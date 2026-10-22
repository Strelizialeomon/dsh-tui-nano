#!/usr/bin/env python3
"""Drive the Nano root input history in a real PTY.

Evidence tooling only. It installs a throwaway dsh profile (no credentials),
starts the built `nano` binary, submits two prompts, then walks Up/Down and
snapshots the composer. The turn itself fails without credentials; the history
file is written before the send, which is exactly what this script checks.

Run after `cargo build -p nano-app`:

    python3 scripts/pty-evidence-history.py
"""
import codecs
import fcntl
import json
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import time
import threading
import unicodedata
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROWS, COLS = 40, 120


class Screen:
    def __init__(self):
        self.grid = [[" "] * COLS for _ in range(ROWS)]
        self.row = 0
        self.col = 0

    @staticmethod
    def width(ch):
        return 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1

    def put(self, ch):
        if ch == "\r":
            self.col = 0
        elif ch == "\n":
            self.row = min(ROWS - 1, self.row + 1)
        elif ch == "\b":
            self.col = max(0, self.col - 1)
        elif ch == "\t":
            self.col = min(COLS - 1, (self.col // 8 + 1) * 8)
        elif ch != "\x07" and ord(ch) >= 32:
            w = self.width(ch)
            if self.col + w > COLS:
                self.col = 0
                self.row = min(ROWS - 1, self.row + 1)
            self.grid[self.row][self.col] = ch
            if w == 2 and self.col + 1 < COLS:
                self.grid[self.row][self.col + 1] = ""
            self.col = min(COLS - 1, self.col + w)

    def feed(self, text):
        index = 0
        while index < len(text):
            if text[index] == "\x1b":
                match = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", text[index:])
                if match:
                    self.csi(match.group(1), match.group(2))
                    index += len(match.group(0))
                    continue
                match = re.match(r"\x1b\][^\x07]*\x07", text[index:])
                if match:
                    index += len(match.group(0))
                    continue
                index += 1
                continue
            self.put(text[index])
            index += 1

    def csi(self, params, final):
        values = (
            [int(value) if value.isdigit() else 0 for value in params.replace("?", "").split(";")]
            if params
            else []
        )

        def arg(position, default=1):
            return values[position] if position < len(values) and values[position] != 0 else default

        if final in "Hf":
            self.row = min(ROWS - 1, arg(0) - 1)
            self.col = min(COLS - 1, arg(1) - 1)
        elif final == "A":
            self.row = max(0, self.row - arg(0))
        elif final == "B":
            self.row = min(ROWS - 1, self.row + arg(0))
        elif final == "C":
            self.col = min(COLS - 1, self.col + arg(0))
        elif final == "D":
            self.col = max(0, self.col - arg(0))
        elif final == "G":
            self.col = min(COLS - 1, arg(0) - 1)
        elif final == "d":
            self.row = min(ROWS - 1, arg(0) - 1)
        elif final == "K":
            mode = values[0] if values else 0
            if mode == 0:
                for column in range(self.col, COLS):
                    self.grid[self.row][column] = " "
            elif mode == 2:
                self.grid[self.row] = [" "] * COLS
        elif final == "J":
            mode = values[0] if values else 0
            if mode == 2:
                self.grid = [[" "] * COLS for _ in range(ROWS)]
            elif mode == 0:
                for column in range(self.col, COLS):
                    self.grid[self.row][column] = " "
                for row in range(self.row + 1, ROWS):
                    self.grid[row] = [" "] * COLS

    def text(self):
        lines = ["".join(row).rstrip() for row in self.grid]
        while lines and not lines[-1]:
            lines.pop()
        return "\n".join(lines)



TOOL_INPUT = json.dumps(
    {"description": "followup-tui", "prompt": "只回复一行 S12CHILD，不要调用任何工具。"},
    ensure_ascii=False,
)


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

    def do_POST(self):
        MockModel.count += 1
        request = MockModel.count
        self.rfile.read(int(self.headers.get("content-length", "0")))
        body = sse(tool_events() if request == 1 else text_events("done"))
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args):
        pass


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    server = ThreadingHTTPServer(("127.0.0.1", 0), MockModel)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    root = Path(tempfile.mkdtemp(prefix="nano-child-history-pty-"))
    home, data, work = root / "dsh-home", root / "data", root / "work"
    work.mkdir(parents=True, exist_ok=True)
    install = subprocess.run(
        [
            "node", str(repo / "bridge" / "profile" / "install.mjs"),
            "--dsh-home", str(home), "--data-root", str(data),
            "--bridge-package", str(repo / "bridge"), "--dsh-bin", shutil.which("dsh"),
            "--json",
        ],
        capture_output=True, text=True,
    )
    if install.returncode != 0:
        print(install.stderr, file=sys.stderr)
        return 2
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
        DEEPSEEK_API_KEY="nano-child-history-test-key",
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

    def composer_line():
        lines = screen.text().splitlines()
        borders = [index for index, line in enumerate(lines) if line.strip().startswith("─")]
        if not borders:
            return ""
        border = borders[-1]
        return lines[border - 1] if border > 0 else ""

    checks = []
    pump(5)
    checks.append(("connected", "已连接" in screen.text()))
    send("请调用子代理。")
    send("\r")
    deadline = time.time() + 60
    while time.time() < deadline and "followup-tui" not in screen.text():
        pump(0.5)
    checks.append(("child started", "followup-tui" in screen.text()))
    send("\x1b[B")
    pump(0.5)
    send("\r")
    deadline = time.time() + 30
    while time.time() < deadline and "子代理" not in screen.text():
        pump(0.5)
    pump(3)
    checks.append(("child view", "子代理" in screen.text()))
    checks.append(("child gate enabled", "可输入" in screen.text()))
    send("child history prompt")
    send("\r")
    pump(3)
    send("\x1b[A")
    pump(0.6)
    checks.append(("child history recalled", "child history prompt" in composer_line()))
    snapshot = screen.text()
    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))
    print(json.dumps({"ok": all(ok for _, ok in checks), "checks": checks, "exit": code}, ensure_ascii=False, indent=2))
    print("--- SCREEN ---")
    print(snapshot)
    shutil.rmtree(root, ignore_errors=True)
    return 0 if all(ok for _, ok in checks) else 2


if __name__ == "__main__":
    sys.exit(main())
