#!/usr/bin/env python3
"""Capture real-Ghostty evidence for the 2026-10-22 visual follow-up.

The harness installs a throwaway dsh profile, serves a local Messages mock that
starts one subagent, drives a real Ghostty window with System Events, and
captures only this run's windows. No credentials or model spend are used.

Run after `cargo build -p nano-app`:
    python3 scripts/ghostty-evidence-followup.py
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import warnings
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

warnings.filterwarnings("ignore", category=DeprecationWarning)

from PIL import Image  # noqa: E402

ROWS, COLS = 35, 100
SMALL_ROWS, SMALL_COLS = 30, 60
DARK = {
    "bg": "#17191d", "fg": "#e8e6e0", "muted": "#8d95a6", "accent": "#7da1de",
    "error": "#da8a93", "warning": "#d8b270", "success": "#9ac4a6",
}
LIGHT = {
    "bg": "#f6f3ed", "fg": "#343945", "muted": "#626978", "accent": "#365f9d",
    "error": "#a33e4c", "warning": "#855b18", "success": "#315f3e",
}
TOOL_INPUT = json.dumps(
    {"description": "followup-tui", "prompt": "只回复一行 S12CHILD，不要调用任何工具。"},
    ensure_ascii=False,
)
PROMPT = "请调用子代理：只回复一行 S12CHILD，不要调用任何工具。"


def sse(events):
    return "".join(
        f"event: {event['type']}\ndata: {json.dumps(event, ensure_ascii=False)}\n\n"
        for event in events
    ).encode()


def text_events(text):
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]


def tool_events():
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "subagent", "input": {}},
        },
        {
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "input_json_delta", "partial_json": TOOL_INPUT},
        },
        {"type": "content_block_stop", "index": 0},
        {"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 1}},
        {"type": "message_stop"},
    ]


class MockModel(BaseHTTPRequestHandler):
    count = 0
    child_delay = 1.0

    def do_POST(self):
        MockModel.count += 1
        request = MockModel.count
        self.rfile.read(int(self.headers.get("content-length", "0")))
        if request == 1:
            body = sse(tool_events())
        elif request == 2:
            time.sleep(MockModel.child_delay)
            body = sse(text_events("S12CHILD"))
        else:
            body = sse(text_events("done"))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def run(cmd, **kwargs):
    return subprocess.run(cmd, capture_output=True, text=True, **kwargs)


RAISE_SCRIPT = """
on run argv
  set targetTitle to item 1 of argv
  tell application "System Events"
    repeat with p in (every process whose name is "Ghostty")
      try
        if (name of windows of p) contains targetTitle then
          set frontmost of p to true
          perform action "AXRaise" of window targetTitle of p
          return (unix id of p) as text
        end if
      end try
    end repeat
  end tell
  return ""
end run
"""

KEY_CODES = {"esc": 53, "return": 36, "tab": 48, "down": 125, "up": 126}


def applescript(script, *args):
    return run(["osascript", "-e", script, *args])


def osa_key(code):
    return applescript(f'tell application "System Events" to key code {code}')


def osa_keystroke(char, modifiers=""):
    using = f" using {modifiers} down" if modifiers else ""
    return applescript(f'tell application "System Events" to keystroke "{char}"{using}')


def focus(title):
    result = applescript(RAISE_SCRIPT, title)
    if result.returncode != 0 or not result.stdout.strip():
        raise RuntimeError(f"could not focus {title!r}: {result.stderr.strip() or 'not found'}")
    time.sleep(0.2)


def wait_for_window(title, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = run([str(LIST_WINDOWS)])
        for line in result.stdout.splitlines():
            fields = line.split("\t")
            if len(fields) >= 4 and fields[2] == "Ghostty" and fields[3] == title:
                return int(fields[0])
        time.sleep(0.25)
    raise RuntimeError(f"Ghostty window {title!r} did not appear")


def wait_for_window_gone(title, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = run([str(LIST_WINDOWS)])
        if not any(
            line.split("\t")[2:4] == ["Ghostty", title]
            for line in result.stdout.splitlines()
            if len(line.split("\t")) >= 4
        ):
            return True
        time.sleep(0.25)
    return False


def capture(title, path):
    window_id = wait_for_window(title)
    subprocess.run(["screencapture", "-x", "-l", str(window_id), "-o", str(path)], check=True)
    with Image.open(path) as image:
        return image.size


def palette_args(theme):
    return [
        f"--background={theme['bg']}",
        f"--foreground={theme['fg']}",
        f"--palette=0={theme['bg']}",
        f"--palette=1={theme['error']}",
        f"--palette=2={theme['success']}",
        f"--palette=3={theme['warning']}",
        f"--palette=6={theme['accent']}",
        f"--palette=8={theme['muted']}",
        f"--palette=9={theme['error']}",
        f"--palette=10={theme['success']}",
    ]


SPAWNED = set()


def ghostty_pids():
    result = run(["pgrep", "-f", "Ghostty.app/Contents/MacOS/ghostty"])
    return {int(line) for line in result.stdout.split() if line.strip().isdigit()}


def launch(title, script, cols, rows, theme, extra_env=""):
    before = ghostty_pids()
    args = [
        "open", "-na", "Ghostty.app", "--args",
        f"--title={title}",
        f"--command={script}",
        f"--window-width={cols}",
        f"--window-height={rows}",
        "--cursor-blink=false",
        "--wait-after-command=false",
        "--macos-option-as-alt=true",
        "--window-save-state=never",
        *palette_args(theme),
    ]
    run(args, check=True)
    for pid in ghostty_pids() - before:
        command = run(["ps", "-o", "command=", "-p", str(pid)]).stdout
        if f"--title={title}" in command:
            SPAWNED.add(pid)
    wait_for_window(title)
    # The host handshake can take several seconds; wait before typing.
    time.sleep(12)
    focus(title)


CLIPBOARD = None


def paste(title, text):
    global CLIPBOARD
    if CLIPBOARD is None:
        CLIPBOARD = subprocess.run(["pbpaste"], capture_output=True).stdout
    focus(title)
    subprocess.run(["pbcopy"], input=text.encode(), check=True)
    osa_keystroke("v", "command")
    time.sleep(0.3)


def close(title):
    focus(title)
    osa_keystroke("c", "control")
    return wait_for_window_gone(title, 8)


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    out = repo / "docs" / "evidence" / "2026-10-22-visual-followup"
    out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="nano-followup-ghostty-"))
    home, dsh_home, data, work = root / "home", root / "dsh-home", root / "data", root / "work"
    for directory in (home, work):
        directory.mkdir(parents=True)

    global LIST_WINDOWS
    source = root / "list-windows.swift"
    source.write_text("""
import CoreGraphics
import Foundation
let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
if let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] {
  for w in list {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    let name = w[kCGWindowName as String] as? String ?? ""
    let number = w[kCGWindowNumber as String] as? Int ?? 0
    let layer = w[kCGWindowLayer as String] as? Int ?? 0
    print("\\(number)\\t\\(layer)\\t\\(owner)\\t\\(name)")
  }
}
""")
    LIST_WINDOWS = root / "list-windows"
    subprocess.run(["swiftc", "-O", str(source), "-o", str(LIST_WINDOWS)], check=True)

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

    def run_script(name, theme):
        path = root / f"run-{name}.sh"
        path.write_text(
            "#!/bin/sh\n"
            f"export HOME='{home}'\n"
            f"export DSH_HOME='{dsh_home}'\n"
            f"export NANO_DATA_ROOT='{data}'\n"
            "export DEEPSEEK_API_KEY='nano-followup-test-key'\n"
            f"export DEEPSEEK_BASE_URL='http://127.0.0.1:{server.server_address[1]}'\n"
            + (f"export NANO_THEME='light'\n" if theme == "light" else "")
            + f"export PATH='{os.environ.get('PATH', '')}'\n"
            f"exec '{binary}' --no-install --no-alt-screen --dsh-home '{dsh_home}' "
            f"--data-root '{data}' --cwd '{work}' --provider deepseek-official --model deepseek-flash\n"
        )
        path.chmod(0o755)
        return path

    checks = []

    def check(label, ok, detail=""):
        checks.append({"check": label, "ok": bool(ok), "detail": detail})
        print(f"CHECK {'ok  ' if ok else 'fail'} {label}{(' — ' + detail) if detail else ''}")

    # 100-column dark run: completed dock, then cleaned dock.
    title = "nano-followup-100-dark"
    launch(title, run_script("dark", "dark"), COLS, ROWS, DARK)
    paste(title, PROMPT)
    osa_key(KEY_CODES["return"])
    time.sleep(8)
    size = capture(title, out / "02-dock-completed-100-dark.png")
    check("dock completed captured", size[0] > 0, str(size))
    time.sleep(36)
    size = capture(title, out / "04-dock-cleaned-100-dark.png")
    check("dock cleaned captured", size[0] > 0, str(size))
    check("100 dark window closed", close(title))

    # 60-column dark run: dock slot and status word.
    title = "nano-followup-60-dark"
    MockModel.count = 0
    launch(title, run_script("dark60", "dark"), SMALL_COLS, SMALL_ROWS, DARK)
    paste(title, PROMPT)
    osa_key(KEY_CODES["return"])
    time.sleep(8)
    size = capture(title, out / "05-dock-60-dark.png")
    check("dock 60 captured", size[0] > 0, str(size))
    check("60 dark window closed", close(title))

    # Light-theme run: focused dock row.
    title = "nano-followup-100-light"
    MockModel.count = 0
    launch(title, run_script("light", "light"), COLS, ROWS, LIGHT)
    paste(title, PROMPT)
    osa_key(KEY_CODES["return"])
    time.sleep(8)
    osa_key(KEY_CODES["down"])
    time.sleep(0.5)
    size = capture(title, out / "07-dock-focus-100-light.png")
    check("dock focus light captured", size[0] > 0, str(size))
    check("light window closed", close(title))

    if CLIPBOARD is not None:
        subprocess.run(["pbcopy"], input=CLIPBOARD, check=False)
    for pid in SPAWNED:
        run(["kill", str(pid)])
    summary = {
        "ok": all(item["ok"] for item in checks),
        "checks": checks,
        "mockRequests": MockModel.count,
    }
    (out / "ghostty-evidence.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2))
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    shutil.rmtree(root, ignore_errors=True)
    return 0 if summary["ok"] else 2


if __name__ == "__main__":
    sys.exit(main())
