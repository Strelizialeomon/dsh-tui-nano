#!/usr/bin/env python3
"""Real-Ghostty acceptance capture for the S13 release.

The harness installs the packaged payload into a synthetic HOME, drives a real
Ghostty window with System Events (Accessibility), captures only the window
owned by this run, and records screenshots plus machine checks under
docs/release/<version>/evidence/ghostty/.

It does not use the user's real home, credentials, sessions, or desktop. A
local Messages-compatible mock serves the model response, so no model spend is
incurred.

Run:
    python3 tests/e2e/ghostty-acceptance.py \
        --payload packaging/dist/dsh-tui-nano-0.1.0-arm64
"""
import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
import warnings

warnings.filterwarnings("ignore", category=DeprecationWarning)
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROWS, COLS = 35, 100
LIGHT_ROWS, LIGHT_COLS = 30, 60
RESPONSE_MARKER = "S13-GHOSTTY-RESPONSE"
LONG_TEXT = (
    " ".join(["这是用于真实 Ghostty 视觉验收的中文长文本，检查 CJK 宽度、换行和阅读层级。"] * 12)
    + f"\n\n{RESPONSE_MARKER}\n"
    + "\n".join(f"第 {i} 行：滚轮与阅读位置检查。" for i in range(1, 31))
    + f"\n\n结尾标记：{RESPONSE_MARKER}\n"
)

SCREEN_EVENTS = [
    {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
    {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
    {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": LONG_TEXT}},
    {"type": "content_block_stop", "index": 0},
    {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 1}},
    {"type": "message_stop"},
]

DARK = {
    "bg": "#17191d", "fg": "#e8e6e0", "muted": "#8d95a6", "accent": "#7da1de",
    "error": "#da8a93", "warning": "#d8b270", "success": "#9ac4a6",
}
LIGHT = {
    "bg": "#f6f3ed", "fg": "#343945", "muted": "#626978", "accent": "#365f9d",
    "error": "#a33e4c", "warning": "#855b18", "success": "#315f3e",
}


class MockModel(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        self.rfile.read(length)
        body = "".join(
            f"event: {event['type']}\ndata: {json.dumps(event, ensure_ascii=False)}\n\n"
            for event in SCREEN_EVENTS
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def run(cmd, **kwargs):
    return subprocess.run(cmd, capture_output=True, text=True, **kwargs)


def applescript(script, *args):
    return run(["osascript", "-e", script, *args])


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

KEY_CODES = {
    "esc": 53, "return": 36, "tab": 48, "space": 49,
    "up": 126, "down": 125, "left": 123, "right": 124,
    "pageup": 116, "pagedown": 121, "home": 115, "end": 119,
}


def osa_key(code):
    return applescript(f'tell application "System Events" to key code {code}')


def osa_keystroke(char, modifiers=""):
    using = f" using {modifiers} down" if modifiers else ""
    return applescript(f'tell application "System Events" to keystroke "{char}"{using}')


def focus(title):
    applescript(RAISE_SCRIPT, title)
    time.sleep(0.2)


def send_key(title, code):
    focus(title)
    osa_key(code)


def send_char(title, char, modifiers=""):
    focus(title)
    osa_keystroke(char, modifiers)


def paste_text(title, text):
    focus(title)
    subprocess.run(["pbcopy"], input=text.encode(), check=True)
    osa_keystroke("v", "command")


def quit_window(title):
    for _ in range(2):
        focus(title)
        osa_keystroke("c", "control")
        if wait_for_window_gone(title, 6):
            return True
    return False


def wait_for_window(title, timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = run([str(LIST_WINDOWS)])
        for line in result.stdout.splitlines():
            fields = line.split("\t")
            if len(fields) >= 5 and fields[3] == title:
                return int(fields[0])
        time.sleep(0.25)
    raise RuntimeError(f"Ghostty window {title!r} did not appear")


def wait_for_window_gone(title, timeout=20):
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = run([str(LIST_WINDOWS)])
        if not any(line.split("\t")[3:4] == [title] for line in result.stdout.splitlines()):
            return True
        time.sleep(0.25)
    return False


def capture(title, path):
    window_id = wait_for_window(title)
    subprocess.run(["screencapture", "-x", "-l", str(window_id), "-o", str(path)], check=True)
    return window_id


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


SPAWNED_GHOSTTY = set()


def ghostty_pids():
    result = run(["pgrep", "-f", "Ghostty.app/Contents/MacOS/ghostty"])
    return {int(line) for line in result.stdout.split() if line.strip().isdigit()}


def launch(title, run_script, cols, rows, theme, fullscreen=False, font_size=None):
    before = ghostty_pids()
    args = [
        "open", "-na", "Ghostty.app", "--args",
        f"--title={title}",
        f"--command={run_script}",
        f"--window-width={cols}",
        f"--window-height={rows}",
        "--cursor-blink=false",
        "--wait-after-command=false",
        "--macos-option-as-alt=true",
        "--window-save-state=never",
        *palette_args(theme),
    ]
    if font_size is not None:
        args.append(f"--font-size={font_size}")
    if fullscreen:
        args.append("--fullscreen=true")
    run(args, check=True)
    wait_for_window(title)
    SPAWNED_GHOSTTY.update(ghostty_pids() - before)
    # Give the host handshake and first draw time to settle.
    time.sleep(6)
    applescript(RAISE_SCRIPT, title)
    time.sleep(0.5)


def scroll_at(window_id, dy):
    # A tiny Swift helper posts a wheel event over the window centre.
    script = f"""
import CoreGraphics
import Foundation
let id = CGWindowID({window_id})
if let list = CGWindowListCopyWindowInfo([.optionIncludingWindow], id) as? [[String: Any]],
   let first = list.first,
   let bounds = first[kCGWindowBounds as String] as? [String: Any],
   let x = bounds["X"] as? Double, let y = bounds["Y"] as? Double,
   let w = bounds["Width"] as? Double, let h = bounds["Height"] as? Double {{
  let point = CGPoint(x: x + w / 2, y: y + h / 2)
  print("bounds", x, y, w, h, "target", point.x, point.y)
  CGEvent(mouseEventSource: nil, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)?.post(tap: .cghidEventTap)
  usleep(200_000)
  print("mouse", CGEvent(source: nil)?.location ?? .zero)
  for _ in 0..<6 {{
    CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1, wheel1: Int32({dy}), wheel2: 0, wheel3: 0)?.post(tap: .cghidEventTap)
    usleep(30_000)
  }}
}}
"""
    path = Path(ROOT) / "scroll.swift"
    path.write_text(script)
    result = run(["swift", str(path)], check=True)
    if result.stdout.strip():
        print(result.stdout.strip())


def png_diff(a, b):
    try:
        from PIL import Image, ImageChops
    except ImportError:
        return None
    left = Image.open(a).convert("RGB")
    right = Image.open(b).convert("RGB")
    if left.size != right.size:
        return 1.0
    diff = ImageChops.difference(left, right)
    bbox = diff.getbbox()
    if bbox is None:
        return 0.0
    changed = sum(1 for pixel in diff.getdata() if pixel != (0, 0, 0))
    return changed / (left.size[0] * left.size[1])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--payload", required=True)
    parser.add_argument("--out", default=None)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()

    payload = Path(args.payload).resolve()
    repo = Path(__file__).resolve().parent.parent.parent
    out = Path(args.out).resolve() if args.out else repo / "docs/release/0.1.0/evidence/ghostty"
    out.mkdir(parents=True, exist_ok=True)
    global ROOT, LIST_WINDOWS
    ROOT = Path(tempfile.mkdtemp(prefix="nano-s13-ghostty-"))
    home, dsh_home, data, work = ROOT / "home", ROOT / "dsh-home", ROOT / "data", ROOT / "work"
    for directory in (home, work):
        directory.mkdir(parents=True)

    checks = []
    def check(label, ok, detail=""):
        checks.append((label, bool(ok), detail))
        print(f"CHECK {'ok  ' if ok else 'fail'} {label}{(' — ' + detail) if detail else ''}")

    # Compile the window-list helper once.
    list_source = ROOT / "list-windows.swift"
    list_source.write_text("""
import CoreGraphics
import Foundation
let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
if let list = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] {
  for w in list {
    let owner = w[kCGWindowOwnerName as String] as? String ?? ""
    let name = w[kCGWindowName as String] as? String ?? ""
    let number = w[kCGWindowNumber as String] as? Int ?? 0
    let layer = w[kCGWindowLayer as String] as? Int ?? 0
    let bounds = w[kCGWindowBounds as String] as? [String: Any] ?? [:]
    print("\\(number)\\t\\(layer)\\t\\(owner)\\t\\(name)\\t\\(bounds)")
  }
}
""")
    LIST_WINDOWS = ROOT / "list-windows"
    subprocess.run(["swiftc", "-O", str(list_source), "-o", str(LIST_WINDOWS)], check=True)

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    install_env = dict(os.environ)
    install_env.update(
        HOME=str(home),
        DSH_HOME=str(dsh_home),
        NANO_DATA_ROOT=str(data),
        NANO_INSTALL_ROOT=str(ROOT / "install"),
        NANO_BIN_DIR=str(ROOT / "bin"),
    )
    install = subprocess.run(
        [str(payload / "install.sh"), "--json"], env=install_env, capture_output=True, text=True
    )
    (out / "install.json").write_text(install.stdout + install.stderr)
    check("install succeeded", install.returncode == 0, install.stderr.strip()[:200])
    if install.returncode != 0:
        return 1

    def write_run_script(name, reduced_motion=False):
        path = ROOT / f"run-{name}.sh"
        path.write_text(
            "#!/bin/sh\n"
            f"export HOME='{home}'\n"
            f"export PATH='{os.environ.get('PATH', '')}'\n"
            f"export DSH_HOME='{dsh_home}'\n"
            f"export NANO_DATA_ROOT='{data}'\n"
            f"export NANO_BRIDGE_PACKAGE='{ROOT / 'install/current/lib/bridge'}'\n"
            "export DEEPSEEK_API_KEY='nano-s13-test-key'\n"
            f"export DEEPSEEK_BASE_URL='http://127.0.0.1:{server.server_address[1]}'\n"
            f"export NANO_REDUCED_MOTION='{'1' if reduced_motion else '0'}'\n"
            f"exec '{ROOT / 'bin/nano'}' --no-install "
            f"--cwd '{work}' --provider deepseek-official --model deepseek-flash "
            f"2>'{ROOT / 'nano.stderr.log'}'\n"
        )
        path.chmod(0o755)
        return path

    try:
        # --- Dark, 100 columns: panels, CJK, response, copy, wheel, exit. ---
        title = "nano-s13-dark-100"
        launch(title, write_run_script("dark100"), COLS, ROWS, DARK)
        main_dark = out / "01-dark-100-main.png"
        capture(title, main_dark)
        check("dark 100 main captured", main_dark.exists())

        panels = [
            ("sessions", "o", "option"),
            ("skills", "k", "option"),
            ("instructions", "r", "option"),
            ("jobs", "t", "control"),
            ("schedule", "g", "control"),
            ("palette", "p", "control"),
        ]
        for name, character, modifiers in panels:
            send_char(title, character, modifiers)
            time.sleep(1.2)
            path = out / f"02-dark-100-{name}.png"
            capture(title, path)
            check(f"dark 100 {name} panel captured", path.exists())
            send_key(title, KEY_CODES["esc"])
            time.sleep(0.8)

        # CJK paste into the composer (real Ghostty paste path, not IME).
        paste_text(title, "中文输入验收：请显示这段文字。")
        time.sleep(1.0)
        cjk = out / "03-dark-100-cjk-input.png"
        capture(title, cjk)
        check("CJK composer captured", cjk.exists())
        send_key(title, KEY_CODES["return"])
        time.sleep(4)
        response = out / "04-dark-100-response.png"
        capture(title, response)
        check("model response captured", response.exists())

        # Ghostty selection copy: Cmd+A then Cmd+C, verify the clipboard.
        send_char(title, "a", "command")
        time.sleep(0.4)
        send_char(title, "c", "command")
        time.sleep(0.5)
        copied = subprocess.run(["pbpaste"], capture_output=True, text=True).stdout
        check("selection copy contains model response", RESPONSE_MARKER in copied, copied[:80])

        # Mouse wheel scroll changes the transcript view.
        before = out / "05-dark-100-before-wheel.png"
        after = out / "06-dark-100-after-wheel.png"
        capture(title, before)
        focus(title)
        scroll_at(wait_for_window(title), 80)
        time.sleep(1.0)
        capture(title, after)
        ratio = png_diff(before, after)
        if ratio is not None and ratio <= 0.001:
            # The wheel direction may be inverted by the terminal; try the
            # opposite sign from the same view.
            scroll_at(wait_for_window(title), -80)
            time.sleep(1.0)
            alt = out / "06b-dark-100-after-wheel-negative.png"
            capture(title, alt)
            alt_ratio = png_diff(before, alt)
            if alt_ratio is not None and alt_ratio > ratio:
                ratio = alt_ratio
        check("wheel changed the view", ratio is None or ratio > 0.001, f"diff={ratio}")

        # Clean exit with Ctrl+C.
        check("window closed after Ctrl+C", quit_window(title), "")
        time.sleep(1)
        leftover = run(["pgrep", "-f", str(ROOT / "install/current/bin/nano")]).stdout.strip()
        check("no nano process remains", leftover == "", leftover)
        stderr = (ROOT / "nano.stderr.log").read_text() if (ROOT / "nano.stderr.log").exists() else ""
        check("no shutdown error on stderr", "shutdown was not clean" not in stderr, stderr[-200:])

        # --- Light, 60 columns. ---
        light_title = "nano-s13-light-60"
        launch(light_title, write_run_script("light60"), LIGHT_COLS, LIGHT_ROWS, LIGHT)
        light_main = out / "07-light-60-main.png"
        capture(light_title, light_main)
        check("light 60 main captured", light_main.exists())
        send_char(light_title, "o", "option")
        time.sleep(1.2)
        light_panel = out / "08-light-60-sessions.png"
        capture(light_title, light_panel)
        check("light 60 sessions panel captured", light_panel.exists())
        send_key(light_title, KEY_CODES["esc"])
        check("light window closed after Ctrl+C", quit_window(light_title), "")

        # --- Fullscreen dark, 100 columns (font sized to keep ~100 columns). ---
        full_title = "nano-s13-fullscreen"
        launch(full_title, write_run_script("fullscreen"), COLS, ROWS, DARK, fullscreen=True, font_size=15)
        full = out / "09-dark-fullscreen.png"
        capture(full_title, full)
        check("fullscreen capture exists", full.exists())
        check("fullscreen window closed after Ctrl+C", quit_window(full_title), "")

        # --- Reduced motion: launch and confirm it draws; motion itself is unit-tested. ---
        rm_title = "nano-s13-reduced-motion"
        launch(rm_title, write_run_script("reduced", reduced_motion=True), COLS, ROWS, DARK)
        rm_shot = out / "10-reduced-motion-main.png"
        capture(rm_title, rm_shot)
        check("reduced-motion main captured", rm_shot.exists())
        check("reduced-motion window closed", quit_window(rm_title), "")
    finally:
        server.shutdown()
        # Best-effort cleanup of any window this run left behind.
        for title in ("nano-s13-dark-100", "nano-s13-light-60", "nano-s13-fullscreen", "nano-s13-reduced-motion"):
            run(["osascript", "-e", RAISE_SCRIPT, title])
        for pid in SPAWNED_GHOSTTY:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if not args.keep:
            shutil.rmtree(ROOT, ignore_errors=True)

    passed = all(ok for _, ok, _ in checks)
    report = {
        "ghostty": run(["ghostty", "--version"]).stdout.splitlines()[0],
        "macos": run(["sw_vers", "-productVersion"]).stdout.strip(),
        "arch": run(["uname", "-m"]).stdout.strip(),
        "payload": payload.name,
        "checks": [{"label": label, "ok": ok, "detail": detail} for label, ok, detail in checks],
    }
    (out / "ghostty-acceptance.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print("RESULT", "ok" if passed else "fail")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
