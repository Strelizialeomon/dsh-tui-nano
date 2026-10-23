#!/usr/bin/env python3
"""Headless PTY evidence for Nano's own theme colors.

Runs the real binary under a pseudo-terminal, feeds keys through the PTY, and
parses the emitted SGR sequences. It creates no windows, never changes focus,
and never touches the clipboard.

Run after `cargo build -p nano-app`:
    python3 tests/e2e/theme-pty.py
"""
import argparse
import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROWS, COLS = 32, 100
MARKER = "PTY-MARKER"
PROMPT = f"只回复一行 {MARKER}，不要调用任何工具。"

THEMES = {
    "dark": {
        "fg": "#e8e6e0", "muted": "#8d95a6", "accent": "#7da1de",
        "error": "#da8a93", "warning": "#d8b270", "success": "#9ac4a6",
        "rule": "#424953", "hover_bg": "#242b35", "selected_bg": "#3b4a66",
        "diff_add_bg": "#24382c", "diff_del_bg": "#38252a",
        "diff_add_word_bg": "#385b42", "diff_del_word_bg": "#653641",
    },
    "light": {
        "fg": "#343945", "muted": "#626978", "accent": "#365f9d",
        "error": "#a33e4c", "warning": "#855b18", "success": "#315f3e",
        "rule": "#c4c6c9", "hover_bg": "#e8e8e5", "selected_bg": "#d5def2",
        "diff_add_bg": "#dcebdd", "diff_del_bg": "#f1dcdf",
        "diff_add_word_bg": "#b6d6bc", "diff_del_word_bg": "#e9bac3",
    },
}


def text_events():
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": MARKER}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]


def tool_use_events(name, payload):
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": f"toolu_{name}", "name": name, "input": {}},
        },
        {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": payload},
        },
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]


def sse(events):
    return "".join(
        f"event: {event['type']}\ndata: {json.dumps(event, ensure_ascii=False)}\n\n"
        for event in events
    ).encode()


class MockModel(BaseHTTPRequestHandler):
    mode = "text"
    edit_path = None

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", "0")))
        if MockModel.mode == "ask":
            payload = json.dumps({
                "questions": [{
                    "id": "q1", "question": "选择 A 还是 B？", "header": "主题验收",
                    "options": [
                        {"label": "A", "description": "第一个选项"},
                        {"label": "B", "description": "第二个选项"},
                    ],
                    "multiSelect": False,
                }]
            }, ensure_ascii=False)
            body = sse(tool_use_events("ask_user_question", payload))
        elif MockModel.mode == "subagent":
            body = sse(tool_use_events("subagent", json.dumps({
                "description": "pty-dock",
                "prompt": "只回复一行 PTY-DOCK，不要调用任何工具。",
            }, ensure_ascii=False)))
        elif MockModel.mode == "edit":
            body = sse(tool_use_events("edit", json.dumps({
                "file_path": MockModel.edit_path,
                "old_string": "alpha",
                "new_string": "beta",
            }, ensure_ascii=False)))
        else:
            body = sse(text_events())
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


SGR = re.compile(rb"\x1b\[([0-9;]*)m")


def parse_colors(data):
    fg = set()
    bg = set()
    named = set()
    indexed = set()
    for match in SGR.finditer(data):
        params = [int(value) for value in match.group(1).split(b";") if value]
        i = 0
        while i < len(params):
            value = params[i]
            if value == 38 and i + 4 < len(params) and params[i + 1] == 2:
                fg.add((params[i + 2], params[i + 3], params[i + 4]))
                i += 5
                continue
            if value == 48 and i + 4 < len(params) and params[i + 1] == 2:
                bg.add((params[i + 2], params[i + 3], params[i + 4]))
                i += 5
                continue
            if value == 38 and i + 2 < len(params) and params[i + 1] == 5:
                indexed.add(params[i + 2])
                i += 3
                continue
            if value == 48 and i + 2 < len(params) and params[i + 1] == 5:
                indexed.add(params[i + 2])
                i += 3
                continue
            if 30 <= value <= 37 or 90 <= value <= 97:
                named.add(value)
            if 40 <= value <= 47 or 100 <= value <= 107:
                named.add(value)
            i += 1
    return fg, bg, named, indexed


def rgb(hex_value):
    value = hex_value.lstrip("#")
    return (int(value[0:2], 16), int(value[2:4], 16), int(value[4:6], 16))


class Session:
    def __init__(self, binary, env, cwd):
        self.binary = binary
        self.env = env
        self.cwd = cwd
        self.buffer = bytearray()
        self.lock = threading.Lock()
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.proc = subprocess.Popen(
            [
                str(binary), "--no-install", "--no-alt-screen",
                "--dsh-home", env["DSH_HOME"],
                "--data-root", env["NANO_DATA_ROOT"],
                "--cwd", str(cwd),
                "--provider", "deepseek-official",
                "--model", "deepseek-flash",
            ],
            stdin=slave, stdout=slave, stderr=slave,
            env=env, preexec_fn=os.setsid, close_fds=True,
        )
        os.close(slave)
        self.pgid = self.proc.pid
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        while True:
            try:
                chunk = os.read(self.master, 4096)
            except OSError:
                break
            if not chunk:
                break
            with self.lock:
                self.buffer.extend(chunk)

    def wait_ready(self, timeout=25):
        deadline = time.time() + timeout
        last = -1
        stable = 0
        while time.time() < deadline:
            with self.lock:
                size = len(self.buffer)
            if size > 500 and size == last:
                stable += 1
                if stable >= 5:
                    return True
            else:
                stable = 0
            last = size
            time.sleep(0.2)
        with self.lock:
            return len(self.buffer) > 0

    def send(self, text):
        os.write(self.master, text.encode())

    def snapshot(self):
        with self.lock:
            return bytes(self.buffer)

    def close(self):
        try:
            self.send("\x11")
            self.proc.wait(timeout=3)
        except (OSError, subprocess.TimeoutExpired):
            pass
        # Kill the whole session group so a lingering host cannot hold the
        # data-root lock for the next scenario.
        try:
            os.killpg(self.pgid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        try:
            os.close(self.master)
        except OSError:
            pass


def run_scenario(base_env, theme, mode, actions, edit_path):
    env = dict(base_env)
    env["NANO_THEME"] = "light" if theme == "light" else "dark"
    MockModel.mode = mode
    MockModel.edit_path = edit_path
    session = Session(Path(env["NANO_BIN"]), env, env["NANO_WORK"])
    if not session.wait_ready(25):
        tail = session.snapshot()[-600:]
        session.close()
        return None, f"TUI did not become ready; tail={tail!r}"
    try:
        for action in actions:
            if action == "prompt":
                session.send(PROMPT + "\r")
                time.sleep(4)
            elif action == "command":
                session.send("\x10")
                time.sleep(1.5)
            elif action == "escape":
                session.send("\x1b")
                time.sleep(0.5)
            elif action == "down":
                session.send("\x1b[B")
                time.sleep(0.5)
    except OSError as error:
        tail = session.snapshot()[-400:]
        session.close()
        return None, f"PTY write failed: {error}; tail={tail!r}"
    time.sleep(1)
    data = session.snapshot()
    session.close()
    return data, ""


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/nano")
    parser.add_argument("--out", default="docs/evidence/2026-10-22-theme-transparency/theme-pty.json")
    parser.add_argument("--only")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[2]
    binary = (repo / args.binary).resolve()
    if not binary.exists():
        sys.exit(f"theme-pty: build first: {binary} is missing")

    root = Path(tempfile.mkdtemp(prefix="nano-theme-pty-"))
    home = root / "home"
    dsh_home = root / "dsh-home"
    data = root / "data"
    work = root / "work"
    for path in (home, dsh_home, data, work):
        path.mkdir(parents=True, exist_ok=True)
    edit_path = str(work / "theme-diff.txt")
    Path(edit_path).write_text("alpha\n")

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    install = subprocess.run(
        [
            "node", str(repo / "bridge" / "profile" / "install.mjs"),
            "--dsh-home", str(dsh_home), "--data-root", str(data),
            "--bridge-package", str(repo / "bridge"), "--dsh-bin", shutil.which("dsh"),
            "--json",
        ],
        capture_output=True, text=True,
    )
    if install.returncode != 0:
        print(install.stderr, file=sys.stderr)
        return 2

    base_env = dict(os.environ)
    base_env.update({
        "HOME": str(home),
        "DSH_HOME": str(dsh_home),
        "NANO_DATA_ROOT": str(data),
        "DEEPSEEK_API_KEY": "nano-theme-pty-key",
        "DEEPSEEK_BASE_URL": f"http://127.0.0.1:{server.server_address[1]}",
        "TERM": "xterm-256color",
        "NANO_REDUCED_MOTION": "1",
        "NANO_BRIDGE_PACKAGE": str(repo / "bridge"),
        "NANO_BIN": str(binary),
        "NANO_WORK": str(work),
    })

    checks = []
    reports = {}

    def check(label, ok, detail=""):
        checks.append({"check": label, "ok": bool(ok), "detail": detail})
        print(f"CHECK {'ok  ' if ok else 'fail'} {label}{(' — ' + detail) if detail else ''}")

    scenarios = [
        ("dark-basic", "dark", "text", ["prompt"]),
        ("light-basic", "light", "text", ["prompt"]),
        ("dark-command", "dark", "text", ["command", "escape"]),
        ("dark-decision", "dark", "ask", ["prompt"]),
        ("dark-diff", "dark", "edit", ["prompt"]),
        ("dark-dock", "dark", "subagent", ["prompt", "down"]),
    ]
    try:
        for name, theme, mode, actions in scenarios:
            if args.only and args.only != name:
                continue
            time.sleep(1)
            data_bytes, error = run_scenario(base_env, theme, mode, actions, edit_path)
            if data_bytes is None:
                check(f"{name} started", False, error)
                continue
            fg, bg, named, indexed = parse_colors(data_bytes)
            (Path("/tmp") / f"pty-{name}.bin").write_bytes(data_bytes)
            reports[name] = {
                "fg": sorted(fg), "bg": sorted(bg),
                "named": sorted(named), "indexed": sorted(indexed),
            }
            check(f"{name} started", True)
            check(f"{name} no named ANSI colors", not named, str(sorted(named)))
            check(f"{name} no indexed colors", not indexed, str(sorted(indexed)))
            expected = {role: rgb(value) for role, value in THEMES[theme].items()}
            allowed_bg = {
                expected["selected_bg"], expected["hover_bg"],
                expected["diff_add_bg"], expected["diff_del_bg"],
                expected["diff_add_word_bg"], expected["diff_del_word_bg"],
            }
            check(f"{name} ordinary background stays Reset", not (bg - allowed_bg),
                  f"bg={sorted(bg)}")
            if name in ("dark-basic", "light-basic"):
                for role in ("fg", "muted", "accent", "rule"):
                    check(f"{name} uses {role}", expected[role] in fg,
                          f"{role}={expected[role]} fg={sorted(fg)}")
            if name == "dark-command":
                check("dark-command uses selected_bg", expected["selected_bg"] in bg,
                      f"bg={sorted(bg)}")
            if name == "dark-decision":
                check("dark-decision uses accent",
                      expected["accent"] in fg,
                      f"fg={sorted(fg)} bg={sorted(bg)}")
            if name == "dark-diff":
                check("dark-diff uses diff backgrounds",
                      expected["diff_add_bg"] in bg or expected["diff_del_bg"] in bg,
                      f"bg={sorted(bg)}")
            if name == "dark-dock":
                check("dark-dock uses hover_bg", expected["hover_bg"] in bg,
                      f"bg={sorted(bg)}")
        # Terminal-palette independence at the protocol level: the same TUI output
        # must carry the same RGB values regardless of TERM.
        env_a = dict(base_env)
        env_a["TERM"] = "xterm-256color"
        env_b = dict(base_env)
        env_b["TERM"] = "screen-256color"
        data_a, err_a = run_scenario(env_a, "dark", "text", ["prompt"], edit_path)
        data_b, err_b = run_scenario(env_b, "dark", "text", ["prompt"], edit_path)
        for _ in range(2):
            if data_a is not None and data_b is not None:
                break
            time.sleep(3)
            data_a, err_a = run_scenario(env_a, "dark", "text", ["prompt"], edit_path)
            data_b, err_b = run_scenario(env_b, "dark", "text", ["prompt"], edit_path)
        if data_a is not None and data_b is not None:
            fg_a, bg_a, _, _ = parse_colors(data_a)
            fg_b, bg_b, _, _ = parse_colors(data_b)
            check("TERM does not change Nano RGB", fg_a == fg_b and bg_a == bg_b,
                  f"fg {sorted(fg_a)} vs {sorted(fg_b)}")
        else:
            check("TERM does not change Nano RGB", False, f"A:{err_a} B:{err_b}")
        if args.only:
            passed = all(item["ok"] for item in checks)
            report = {"mode": "headless-pty", "checks": checks, "scenarios": reports}
            out = repo / args.out
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            print("RESULT", "ok" if passed else "fail")
            return 0 if passed else 1
    finally:
        server.shutdown()
        shutil.rmtree(root, ignore_errors=True)

    passed = all(item["ok"] for item in checks)
    report = {
        "mode": "headless-pty",
        "build_commit": subprocess.run(
            ["git", "-C", str(repo), "rev-parse", "HEAD"],
            capture_output=True, text=True,
        ).stdout.strip(),
        "terminal": "pty",
        "size": [COLS, ROWS],
        "checks": checks,
        "scenarios": reports,
    }
    out = repo / args.out
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print("RESULT", "ok" if passed else "fail")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
