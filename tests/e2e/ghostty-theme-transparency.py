#!/usr/bin/env python3
"""Real-Ghostty evidence for 2026-10-22 theme transparency.

Captures the 8-group color-independence matrix, the ordinary-background /
transparency fixtures, an opaque regression, and component/exit regressions.
The harness uses a throwaway HOME, an isolated Ghostty config, a local model
mock, and window-only screenshots. It never reads or writes the user's config.

Run after `cargo build -p nano-app`:
    python3 tests/e2e/ghostty-theme-transparency.py
"""
import argparse
import hashlib
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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

warnings.filterwarnings("ignore", category=DeprecationWarning)

try:
    from PIL import Image
except ImportError:
    sys.exit("ghostty-theme-transparency: Pillow is required (pip install pillow)")

ROWS, COLS = 32, 100
EDIT_PATH = None
SMALL_ROWS, SMALL_COLS = 26, 60
PROMPT = "只回复一行 THEME-TRANSPARENCY，不要调用任何工具。"
MARKER = "THEME-TRANSPARENCY"

# Deliberately mismatched terminal palettes. Nano's own semantic RGB must not
# change when these change.
PALETTE_A = {
    "bg": "#101010", "fg": "#f0f0f0",
    "0": "#000000", "1": "#ff0000", "2": "#00ff00", "3": "#ffff00",
    "6": "#00ffff", "8": "#808080", "9": "#ff5555", "10": "#55ff55",
}
PALETTE_B = {
    "bg": "#fafafa", "fg": "#101010",
    "0": "#ffffff", "1": "#00aa00", "2": "#aa00aa", "3": "#0000aa",
    "6": "#aa5500", "8": "#404040", "9": "#00aa00", "10": "#aa00aa",
}
THEMES = {
    "dark": {"fg": "#e8e6e0", "accent": "#7da1de", "rule": "#424953"},
    "light": {"fg": "#343945", "accent": "#365f9d", "rule": "#c4c6c9"},
}


def text_events(text):
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
        {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}},
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


def ask_events():
    payload = json.dumps({
        "questions": [{
            "id": "q1",
            "question": "选择 A 还是 B？",
            "header": "主题验收",
            "options": [
                {"label": "A", "description": "第一个选项"},
                {"label": "B", "description": "第二个选项"},
            ],
            "multiSelect": False,
        }]
    }, ensure_ascii=False)
    return [
        {"type": "message_start", "message": {"usage": {"input_tokens": 1, "output_tokens": 0}}},
        {
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "toolu_ask", "name": "ask_user_question", "input": {}},
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

    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", "0")))
        if MockModel.mode == "ask":
            body = sse(ask_events())
        elif MockModel.mode == "subagent":
            body = sse(tool_use_events("subagent", json.dumps({
                "description": "theme-dock",
                "prompt": "只回复一行 THEME-DOCK，不要调用任何工具。",
            }, ensure_ascii=False)))
        elif MockModel.mode == "edit":
            body = sse(tool_use_events("edit", json.dumps({
                "file_path": EDIT_PATH,
                "old_string": "alpha",
                "new_string": "beta",
            }, ensure_ascii=False)))
        else:
            body = sse(text_events(MARKER))
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
    return path


def window_exists(title):
    result = run([str(LIST_WINDOWS)])
    return any(
        line.split("\t")[2:4] == ["Ghostty", title]
        for line in result.stdout.splitlines()
        if len(line.split("\t")) >= 4
    )


def kill_window(title):
    result = run(["pgrep", "-f", f"--title={title}"])
    for line in result.stdout.split():
        try:
            os.kill(int(line), signal.SIGKILL)
        except (ValueError, ProcessLookupError):
            pass
    if wait_for_window_gone(title, 8):
        return True
    return not run(["pgrep", "-f", f"--title={title}"]).stdout.strip()


def quit_window(title):
    for _ in range(2):
        if not window_exists(title):
            return True
        try:
            focus(title)
        except RuntimeError:
            break
        applescript('tell application "System Events" to key code 8 using control down')
        if wait_for_window_gone(title, 6):
            return True
    return kill_window(title)


SPAWNED = set()


def ghostty_pids():
    result = run(["pgrep", "-f", "Ghostty.app/Contents/MacOS/ghostty"])
    return {int(line) for line in result.stdout.split() if line.strip().isdigit()}


def palette_args(palette):
    return [
        f"--background={palette['bg']}",
        f"--foreground={palette['fg']}",
        *[
            f"--palette={slot}={palette[slot]}"
            for slot in ("0", "1", "2", "3", "6", "8", "9", "10")
        ],
    ]


def launch(title, script, cols, rows, palette, *, opacity=1.0, cells=False,
           background_image=None, fullscreen=False, font_size=None, config_file=None,
           focus_after=True):
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
        "--config-default-files=false",
        *palette_args(palette),
        f"--background-opacity={opacity}",
        f"--background-opacity-cells={'true' if cells else 'false'}",
    ]
    if config_file is not None:
        args.append(f"--config-file={config_file}")
    if background_image is not None:
        args.extend([f"--background-image={background_image}", "--background-image-opacity=1"])
    if fullscreen:
        args.append("--fullscreen=true")
    if font_size is not None:
        args.append(f"--font-size={font_size}")
    run(args, check=True)
    for pid in ghostty_pids() - before:
        command = run(["ps", "-o", "command=", "-p", str(pid)]).stdout
        if f"--title={title}" in command:
            SPAWNED.add(pid)
    wait_for_window(title)
    time.sleep(10)
    if focus_after:
        focus(title)


def send_key(title, code):
    focus(title)
    return applescript(f'tell application "System Events" to key code {code}')


def paste(title, text):
    focus(title)
    subprocess.run(["pbcopy"], input=text.encode(), check=True)
    applescript('tell application "System Events" to keystroke "v" using command down')


def analyze(path):
    with Image.open(path) as image:
        image = image.convert("RGB")
        colors = image.getdata()
    return colors


def near_rgb(color, rgb, tolerance=24):
    return all(abs(a - b) <= tolerance for a, b in zip(color, rgb))


def count_near(colors, rgb, tolerance=24):
    return sum(1 for color in colors if near_rgb(color, rgb, tolerance))


def dominant_near(colors, rgb, tolerance=24):
    from collections import Counter
    matches = Counter(color for color in colors if near_rgb(color, rgb, tolerance))
    return matches.most_common(1)[0][0] if matches else None


def close_color(left, right, tolerance=24):
    return (
        left is not None
        and right is not None
        and all(abs(a - b) <= tolerance for a, b in zip(left, right))
    )


def hex_rgb(value):
    value = value.lstrip("#")
    return tuple(int(value[i:i + 2], 16) for i in (0, 2, 4))


def blank_variance(colors, width, height, box):
    x0, y0, x1, y1 = box
    sample = set()
    for y in range(y0, min(y1, height)):
        for x in range(x0, min(x1, width)):
            sample.add(colors[y * width + x])
    return len(sample)


def checkerboard(size, cell=12):
    image = Image.new("RGB", size)
    pixels = image.load()
    for y in range(size[1]):
        for x in range(size[0]):
            pixels[x, y] = (255, 0, 255) if (x // cell + y // cell) % 2 else (0, 255, 255)
    return image


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", default="target/debug/nano")
    parser.add_argument("--out", default="docs/evidence/2026-10-22-theme-transparency")
    parser.add_argument("--capture-only", action="store_true",
                        help="skip keyboard input; capture the initial frame and close by PID")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[2]
    binary = (repo / args.binary).resolve()
    out = (repo / args.out).resolve()
    if not binary.exists():
        sys.exit(f"ghostty-theme-transparency: build first: {binary} is missing")
    out.mkdir(parents=True, exist_ok=True)

    root = Path(tempfile.mkdtemp(prefix="nano-theme-transparency-"))
    home = root / "home"
    dsh_home = root / "dsh-home"
    data = root / "data"
    work = root / "work"
    for path in (home, dsh_home, data, work):
        path.mkdir(parents=True, exist_ok=True)
    global EDIT_PATH
    EDIT_PATH = str(work / "theme-diff.txt")
    Path(EDIT_PATH).write_text("alpha\n")

    config = root / "ghostty-config"
    config.write_text("window-save-state=never\nmacos-option-as-alt=true\n")
    config_before = hashlib.sha256(config.read_bytes()).hexdigest()

    swift_source = root / "list-windows.swift"
    swift_source.write_text(
        'import CoreGraphics\nimport Foundation\n'
        'let options = CGWindowListOption(arrayLiteral: .optionOnScreenOnly, .excludeDesktopElements)\n'
        'if let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] {\n'
        '  for w in windows {\n'
        '    let owner = w[kCGWindowOwnerName as String] as? String ?? ""\n'
        '    let name = w[kCGWindowName as String] as? String ?? ""\n'
        '    let number = w[kCGWindowNumber as String] as? Int ?? 0\n'
        '    let layer = w[kCGWindowLayer as String] as? Int ?? 0\n'
        '    print("\\(number)\\t\\(layer)\\t\\(owner)\\t\\(name)")\n'
        '  }\n'
        '}\n'
    )
    global LIST_WINDOWS
    LIST_WINDOWS = root / "list-windows"
    subprocess.run(["swiftc", "-O", str(swift_source), "-o", str(LIST_WINDOWS)], check=True)

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
            "export DEEPSEEK_API_KEY='nano-theme-test-key'\n"
            f"export DEEPSEEK_BASE_URL='http://127.0.0.1:{server.server_address[1]}'\n"
            + (f"export NANO_THEME='light'\n" if theme == "light" else "")
            + f"export PATH='{os.environ.get('PATH', '')}'\n"
            f"exec '{binary}' --no-install --dsh-home '{dsh_home}' "
            f"--data-root '{data}' --cwd '{work}' --provider deepseek-official --model deepseek-flash "
            f"2>'{root / (name + '.stderr.log')}'\n"
        )
        path.chmod(0o755)
        return path

    checks = []

    def check(label, ok, detail=""):
        checks.append({"check": label, "ok": bool(ok), "detail": detail})
        print(f"CHECK {'ok  ' if ok else 'fail'} {label}{(' — ' + detail) if detail else ''}")

    def drive(title, theme):
        if args.capture_only:
            return
        accent = hex_rgb(THEMES[theme]["accent"])
        for _ in range(10):
            probe = root / "drive-probe.png"
            try:
                capture(title, probe)
                if count_near(analyze(probe), accent) > 5:
                    break
            except (RuntimeError, OSError):
                pass
            time.sleep(2)
        paste(title, PROMPT)
        send_key(title, KEY_CODES["return"])
        time.sleep(4)

    def close(title):
        if args.capture_only:
            closed = kill_window(title)
        else:
            closed = quit_window(title)
        check(f"{title} closed", closed)
        return closed

    try:
        # --- 8-group color independence: 2 TUI themes x 2 palettes x 2 widths. ---
        counts = {}
        for theme in ("dark", "light"):
            for palette_name, palette in (("A", PALETTE_A), ("B", PALETTE_B)):
                for width_name, cols, rows in (
                    ("100", COLS, ROWS),
                    ("60", SMALL_COLS, SMALL_ROWS),
                ):
                    title = f"nano-theme-{theme}-{palette_name}-{width_name}"
                    launch(title, run_script(f"{theme}{palette_name}{width_name}", theme),
                           cols, rows, palette)
                    drive(title, theme)
                    path = out / f"{theme}-{palette_name}-{width_name}.png"
                    expected = THEMES[theme]
                    key = (theme, width_name)
                    entry = counts.setdefault(key, {})
                    for attempt in range(4):
                        capture(title, path)
                        colors = analyze(path)
                        entry[palette_name] = {
                            "fg": count_near(colors, hex_rgb(expected["fg"])),
                            "accent": count_near(colors, hex_rgb(expected["accent"])),
                            "rule": count_near(colors, hex_rgb(expected["rule"])),
                            "rule_color": dominant_near(colors, hex_rgb(expected["rule"])),
                            "accent_color": dominant_near(colors, hex_rgb(expected["accent"])),
                        }
                        if (entry[palette_name]["rule"] > 100
                                and entry[palette_name]["accent"] > 5):
                            break
                        time.sleep(2)
                    check(f"{theme} {palette_name} {width_name} rule RGB present",
                          entry[palette_name]["rule"] > 100, str(entry[palette_name]))
                    check(f"{theme} {palette_name} {width_name} accent RGB present",
                          entry[palette_name]["accent"] > 5, str(entry[palette_name]))
                    close(title)
        for key, entry in counts.items():
            same_rule = close_color(entry["A"]["rule_color"], entry["B"]["rule_color"])
            same_accent = close_color(
                entry["A"]["accent_color"], entry["B"]["accent_color"]
            )
            check(f"palette independence {key}", same_rule and same_accent,
                  f"A={entry['A']} B={entry['B']}")

        # --- Ordinary background / transparency: checkerboard must show through. ---
        checker_path = root / "checker.png"
        checkerboard((320, 240)).save(checker_path)
        for theme in ("dark", "light"):
            title = f"nano-theme-transparent-{theme}"
            launch(title, run_script(f"transparent-{theme}", theme), COLS, ROWS,
                   PALETTE_A, opacity=0.7, cells=False, background_image=checker_path)
            drive(title, theme)
            path = out / f"transparent-{theme}.png"
            capture(title, path)
            colors = analyze(path)
            width, height = Image.open(path).size
            variance = blank_variance(colors, width, height, (2, 2, 40, 20))
            check(f"transparent {theme} background not a solid fill", variance > 8,
                  f"variance={variance}")
            close(title)

        # --- Opaque regression: opacity 1 with the checker image. ---
        title = "nano-theme-opaque"
        launch(title, run_script("opaque", "dark"), COLS, ROWS, PALETTE_A,
               opacity=1.0, cells=False, background_image=checker_path)
        drive(title, "dark")
        path = out / "opaque-dark.png"
        capture(title, path)
        colors = analyze(path)
        magenta = count_near(colors, (255, 0, 255), tolerance=4)
        cyan = count_near(colors, (0, 255, 255), tolerance=4)
        check("opaque checker visible", magenta + cyan > 1000,
              f"magenta={magenta} cyan={cyan}")
        close(title)

        # --- Component pass: root feed, command panel, schedule, terminal. ---
        title = "nano-theme-components"
        launch(title, run_script("components", "dark"), COLS, ROWS, PALETTE_A)
        drive(title, "dark")
        base = out / "components-main.png"
        capture(title, base)
        if not args.capture_only:
            # Command panel (Ctrl+P), schedule (Ctrl+G), terminal (F12).
            for name, action in (
                ("command", lambda: applescript(
                    'tell application "System Events" to keystroke "p" using control down')),
                ("schedule", lambda: applescript(
                    'tell application "System Events" to keystroke "g" using control down')),
                ("terminal", lambda: send_key(title, 111)),
            ):
                if not window_exists(title):
                    check(f"components {name} captured", False, "window closed early")
                    break
                try:
                    focus(title)
                    action()
                    time.sleep(1.0)
                    path = out / f"components-{name}.png"
                    capture(title, path)
                    check(f"components {name} captured", path.exists())
                    if name != "terminal":
                        send_key(title, KEY_CODES["esc"])
                except RuntimeError as error:
                    check(f"components {name} captured", False, str(error))
                    break
        close(title)

        # --- Decision card: the mock asks a question so the card is real. ---
        MockModel.mode = "ask"
        title = "nano-theme-decision"
        launch(title, run_script("decision", "dark"), COLS, ROWS, PALETTE_A)
        drive(title, "dark")
        decision = out / "components-decision.png"
        capture(title, decision)
        check("components decision card captured", decision.exists())
        close(title)
        MockModel.mode = "text"

        # --- Local background boundary: cells=true with a focused dock row. ---
        MockModel.mode = "subagent"
        title = "nano-theme-cells-true"
        launch(title, run_script("cells-true", "dark"), COLS, ROWS, PALETTE_A,
               opacity=0.7, cells=True, background_image=checker_path)
        drive(title, "dark")
        if not args.capture_only:
            try:
                focus(title)
                send_key(title, KEY_CODES["down"])
                time.sleep(1.0)
            except RuntimeError:
                pass
        cells_true = out / "local-cells-true.png"
        capture(title, cells_true)
        check("local background cells=true captured", cells_true.exists())
        close(title)
        MockModel.mode = "text"

        # --- Diff under cells=true: local highlight behavior with the terminal
        # compositing explicit cell backgrounds. ---
        MockModel.mode = "edit"
        title = "nano-theme-diff-cells-true"
        launch(title, run_script("diff-cells-true", "dark"), COLS, ROWS, PALETTE_A,
               opacity=0.7, cells=True, background_image=checker_path)
        drive(title, "dark")
        time.sleep(2)
        diff_cells = out / "local-diff-cells-true.png"
        capture(title, diff_cells)
        check("local diff cells=true captured", diff_cells.exists())
        close(title)
        MockModel.mode = "text"

        # --- Dock / child session: a subagent tool call creates the dock. ---
        MockModel.mode = "subagent"
        title = "nano-theme-dock"
        launch(title, run_script("dock", "dark"), COLS, ROWS, PALETTE_A)
        drive(title, "dark")
        if not args.capture_only:
            try:
                focus(title)
                send_key(title, KEY_CODES["down"])
                time.sleep(1.0)
            except RuntimeError:
                pass
        dock = out / "components-dock.png"
        capture(title, dock)
        check("components dock captured", dock.exists())
        if not args.capture_only:
            try:
                focus(title)
                send_key(title, KEY_CODES["return"])
                time.sleep(1.0)
                child = out / "components-child.png"
                capture(title, child)
                check("components child captured", child.exists())
            except RuntimeError as error:
                check("components child captured", False, str(error))
        close(title)
        MockModel.mode = "text"

        # --- Diff: the mock asks the edit tool to change a fixture file. ---
        MockModel.mode = "edit"
        title = "nano-theme-diff"
        launch(title, run_script("diff", "dark"), COLS, ROWS, PALETTE_A)
        drive(title, "dark")
        time.sleep(2)
        diff_shot = out / "components-diff.png"
        capture(title, diff_shot)
        check("components diff captured", diff_shot.exists())
        close(title)
        MockModel.mode = "text"

        # --- Fullscreen regression (note the actual mode; no transparency promise). ---
        title = "nano-theme-fullscreen"
        launch(title, run_script("fullscreen", "dark"), COLS, ROWS, PALETTE_A,
               opacity=0.7, fullscreen=True, focus_after=False)
        path = out / "fullscreen-dark.png"
        capture(title, path)
        check("fullscreen captured", path.exists() and path.stat().st_size > 0)
        close(title)

        title = "nano-theme-fullscreen-60"
        launch(title, run_script("fullscreen60", "dark"), SMALL_COLS, SMALL_ROWS, PALETTE_A,
               opacity=0.7, fullscreen=True, font_size=45, focus_after=False)
        path = out / "fullscreen-dark-60.png"
        capture(title, path)
        check("fullscreen 60 captured", path.exists() and path.stat().st_size > 0)
        close(title)

        config_after = hashlib.sha256(config.read_bytes()).hexdigest()
        check("config unchanged", config_before == config_after,
              f"{config_before} -> {config_after}")
    finally:
        server.shutdown()
        for pid in SPAWNED:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if not args.keep:
            shutil.rmtree(root, ignore_errors=True)

    passed = all(item["ok"] for item in checks)
    report = {
        "ghostty": run(["ghostty", "--version"]).stdout.splitlines()[0] if run(["ghostty", "--version"]).stdout else "",
        "macos": run(["sw_vers", "-productVersion"]).stdout.strip(),
        "arch": run(["uname", "-m"]).stdout.strip(),
        "binary": str(binary),
        "binary_mtime": binary.stat().st_mtime,
        "checks": checks,
        "color_counts": {
            f"{theme}-{width}": entry for (theme, width), entry in counts.items()
        },
    }
    (out / "theme-transparency.json").write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n"
    )
    print("RESULT", "ok" if passed else "fail")
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())
