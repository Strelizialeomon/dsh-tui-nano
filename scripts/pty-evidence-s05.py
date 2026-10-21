#!/usr/bin/env python3
"""Drive the skills and workspace-instruction panels in a real PTY.

This is S05 evidence tooling, not part of the product. It installs a throwaway
dsh profile, starts the built `nano` binary at 120x40 with a temporary project
that contains real skill files and real AGENTS.md/CLAUDE.md files, points the
official DeepSeek route at a local Messages-compatible mock (no real model
spend), and snapshots the resulting screens.

Run after `cargo build -p nano-app`:
    python3 scripts/pty-evidence-s05.py
"""
import codecs
import fcntl
import importlib.util
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.dont_write_bytecode = True

ROWS, COLS = 40, 120
SKILL_MARKER = "S05_SKILL_BODY_MARKER"
RULE_MARKER = "S05_PROJECT_RULE_MARKER"


def load_screen():
    path = Path(__file__).with_name("pty-evidence.py")
    spec = importlib.util.spec_from_file_location("nano_pty_evidence", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Screen


SCREEN_EVENTS = [
    {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "ok"}},
    {"type": "content_block_stop", "index": 0},
    {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
    {"type": "message_stop"},
]


class MockModel(BaseHTTPRequestHandler):
    """One Anthropic-Messages-shaped SSE response; no model is called."""

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        self.rfile.read(length)
        body = "".join(
            f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in SCREEN_EVENTS
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def write_skill(root: Path, name: str, description: str, body: str, user_only: bool = False):
    directory = root / ".agents" / "skills" / name
    directory.mkdir(parents=True, exist_ok=True)
    flag = "disable-model-invocation: true\n" if user_only else ""
    (directory / "SKILL.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n{flag}---\n# {name}\n{body}\n",
        encoding="utf-8",
    )


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    Screen = load_screen()

    root = Path(tempfile.mkdtemp(prefix="nano-s05-pty-"))
    home, agents_home, data, work = root / "dsh-home", root / "agents-home", root / "data", root / "work"
    work.mkdir(parents=True, exist_ok=True)
    (work / ".git").mkdir()
    agents_home.mkdir()
    write_skill(work, "hello", "Says hello for the S05 fixture", SKILL_MARKER)
    write_skill(work, "user-only", "Only the user can invoke this fixture", "USER_ONLY_MARKER", user_only=True)
    project_rules = f"# Project rules\n{RULE_MARKER}\n"
    (work / "AGENTS.md").write_text(project_rules, encoding="utf-8")
    (work / "CLAUDE.md").write_text(project_rules, encoding="utf-8")
    (work / "AGENTS.local.md").write_text(
        f"# Local rules\nLOCAL_RULE_MARKER\n{'x' * 64_900}\n", encoding="utf-8"
    )
    (agents_home / "AGENTS.md").write_text("# Shared rules\nSHARED_RULE_MARKER\n", encoding="utf-8")

    dsh = shutil.which("dsh")
    install = subprocess.run(
        [
            "node", str(repo / "bridge" / "profile" / "install.mjs"),
            "--dsh-home", str(home), "--data-root", str(data),
            "--bridge-package", str(repo / "bridge"), "--dsh-bin", dsh, "--json",
        ],
        capture_output=True, text=True,
    )
    if install.returncode != 0:
        print(install.stderr, file=sys.stderr)
        return 2

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    args = [
        str(binary), "--no-install", "--no-alt-screen",
        "--dsh-home", str(home), "--data-root", str(data), "--cwd", str(work),
        "--provider", "deepseek-official", "--model", "deepseek-flash",
    ]
    environment = dict(os.environ)
    environment.update(
        TERM="xterm-256color",
        DSH_HOME=str(home),
        DSH_AGENTS_HOME=str(agents_home),
        DEEPSEEK_API_KEY="nano-s05-test-key",
        DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_address[1]}",
    )
    child = subprocess.Popen(
        args, stdin=slave, stdout=slave, stderr=slave, env=environment,
        close_fds=True, preexec_fn=os.setsid,
    )
    os.close(slave)
    screen = Screen()
    decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def pump(seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            ready, _, _ = select.select([master], [], [], 0.05)
            if master in ready:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                screen.feed(decoder.decode(chunk))

    def send(text):
        os.write(master, text.encode("utf-8"))

    def snapshot():
        return screen.text()

    def close_panel():
        # Crossterm can merge a lone ESC with the next byte as Alt+key; send a
        # second standalone ESC after a short pause so panel close is reliable.
        for _ in range(3):
            send("\x1b")
            pump(0.35)

    snapshots = {}
    checks = []
    pump(5)
    checks.append(("connected", "已连接" in snapshot()))

    send("\x1bk")  # Alt+K
    pump(1.2)
    now = snapshot()
    snapshots["skills"] = now
    checks.append(("skill panel opens", "技能" in now))
    checks.append(("skill listed", "hello" in now))
    checks.append(("user-only marker", "仅用户可调用" in now))
    checks.append(("description visible", "Says hello" in now))

    send("p")
    pump(1.2)
    now = snapshot()
    snapshots["preview"] = now
    checks.append(("preview shows official file", SKILL_MARKER in now))
    checks.append(("preview title visible", f"预览 /hello" in now))

    close_panel()  # close the preview/panel
    send("\x1bk")  # reopen and keep it open across the watcher signal
    pump(1.0)
    write_skill(work, "added-later", "Appears after the provider watcher fires", "LATER_MARKER")
    time.sleep(2.0)
    pump(2.0)
    now = snapshot()
    snapshots["watcher"] = now
    checks.append(("watcher refreshes the open panel", "added-later" in now))
    close_panel()

    send("\x1br")  # Alt+R before any request
    pump(1.0)
    checks.append(("rules panel opens", "项目规矩" in snapshot()))
    checks.append(("rules not loaded yet", "尚未加载项目规矩" in snapshot()))
    close_panel()

    send("/hello\r")
    pump(6)
    now = snapshot()
    checks.append(("slash invocation sent literally", "/hello" in now))
    snapshots["invocation"] = now
    checks.append(("official injection reported", "技能正文已注入" in now))

    send("\x1br")
    pump(1.2)
    now = snapshot()
    checks.append(("global source visible", "$DSH_AGENTS_HOME/AGENTS.md" in now))
    checks.append(("local overlay source visible", "AGENTS.local.md" in now))
    checks.append(("duplicate CLAUDE.md collapsed", "CLAUDE.md" not in now))
    checks.append(("load time visible", "最近一次加载" in now))
    snapshots["rules"] = now
    close_panel()

    send("/does-not-exist\r")
    pump(6)
    now = snapshot()
    snapshots["unknown"] = now
    checks.append(("unknown name passes as prose", "未注入" in now))

    send("/compact\r")
    pump(1.0)
    now = snapshot()
    snapshots["command"] = now
    # After S04, an official command with the same name executes instead of
    # being reported as a command-wins skill collision; the draft is consumed.
    checks.append(("command wins over skill routing", "命令 compact" in now or "Compacted" in now))
    checks.append(("official command took the draft", "/compact" not in now.split("输入")[-1]))

    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))

    # Second session: an oversized local overlay must surface the official
    # budget notice. The official renderer omits broader files before
    # truncating the most specific one, so this scenario carries one source.
    work_budget = root / "work-budget"
    work_budget.mkdir(parents=True, exist_ok=True)
    (work_budget / ".git").mkdir()
    (work_budget / "AGENTS.local.md").write_text(
        f"# Local rules\nLOCAL_RULE_MARKER\n{'x' * 70_000}\n", encoding="utf-8"
    )
    master2, slave2 = pty.openpty()
    fcntl.ioctl(slave2, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    child2 = subprocess.Popen(
        [str(binary), "--no-install", "--no-alt-screen",
         "--dsh-home", str(home), "--data-root", str(data), "--cwd", str(work_budget),
         "--provider", "deepseek-official", "--model", "deepseek-flash"],
        stdin=slave2, stdout=slave2, stderr=slave2, env=environment,
        close_fds=True, preexec_fn=os.setsid,
    )
    os.close(slave2)
    screen2 = Screen()

    def pump2(seconds):
        deadline = time.time() + seconds
        while time.time() < deadline:
            ready, _, _ = select.select([master2], [], [], 0.05)
            if master2 in ready:
                try:
                    chunk = os.read(master2, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                screen2.feed(decoder.decode(chunk))

    pump2(5)
    os.write(master2, b"hi\r")
    pump2(6)
    os.write(master2, b"\x1br")
    pump2(1.2)
    now = screen2.text()
    snapshots["budget"] = now
    checks.append(("budget notice visible", "Workspace instruction budget" in now))
    checks.append(("truncated local source visible", "AGENTS.local.md" in now))
    os.write(master2, b"\x03")
    pump2(2)
    try:
        code2 = child2.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child2.kill()
        code2 = "timeout"
    checks.append(("second session clean exit", code2 == 0))
    server.shutdown()

    print(json.dumps({"ok": all(ok for _, ok in checks), "checks": checks, "exit": code}, ensure_ascii=False, indent=2))
    for name in ["skills", "preview", "watcher", "invocation", "rules", "unknown", "command", "budget"]:
        print(f"--- {name.upper()} SCREEN ---")
        print(snapshots.get(name, ""))
    return 0 if all(ok for _, ok in checks) else 2


if __name__ == "__main__":
    sys.exit(main())
