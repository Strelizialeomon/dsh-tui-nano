#!/usr/bin/env python3
"""Drive the Nano TUI in a real PTY for the S06 attachment path.

This is evidence tooling, not part of the product. It installs a throwaway dsh
profile, starts the built `nano` binary at 120x40, exercises `@file` completion,
the attachment picker, a real PNG upload through the official attachment
service, ordered echo, and a clean exit. The model turn fails without a route,
which is enough to prove admission and persistence without a paid request.

Run after `cargo build -p nano-app`:
    python3 scripts/pty-evidence-attachments.py
"""
import base64
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
import unicodedata
from pathlib import Path

ROWS, COLS = 40, 120
PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
)


class Screen:
    """Minimal ANSI screen used only to snapshot the TUI."""

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


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    root = Path(tempfile.mkdtemp(prefix="nano-pty-s06-"))
    home, data, work = root / "dsh-home", root / "data", root / "work"
    (work / "src").mkdir(parents=True, exist_ok=True)
    (work / "src" / "main.rs").write_text("fn main() {}\n", encoding="utf-8")
    (work / "pixel.png").write_bytes(PNG)
    (work / "notes.txt").write_text("hello file\n", encoding="utf-8")
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
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    args = [
        str(binary), "--no-install", "--no-alt-screen",
        "--dsh-home", str(home), "--data-root", str(data), "--cwd", str(work),
    ]
    environment = dict(os.environ)
    environment["TERM"] = "xterm-256color"
    child = subprocess.Popen(
        args, stdin=slave, stdout=slave, stderr=slave, env=environment,
        close_fds=True, preexec_fn=os.setsid,
    )
    os.close(slave)
    screen = Screen()
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    snapshots = {}

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

    checks = []
    pump(4)
    checks.append(("connected", "已连接" in screen.text()))

    # `@` completion: path-only candidates from the official provider.
    send("@mai")
    pump(3.0)
    snapshots["completion"] = screen.text()
    now = screen.text()
    checks.append(("completion visible", "文件补全" in now))
    checks.append(("candidate path", "src/main.rs" in now))
    send("\r")
    pump(0.5)
    checks.append(("mention inserted", "@src/main.rs" in screen.text()))
    checks.append(("completion closed", "文件补全" not in screen.text()))

    # Attachment picker: a real PNG goes through local validation and official
    # admission at submit time.
    send("\x1ba")
    pump(0.5)
    checks.append(("picker visible", "附加文件" in screen.text()))
    send("pixel.png")
    pump(0.4)
    send("\r")
    pump(1.5)
    snapshots["attachment"] = screen.text()
    now = screen.text()
    checks.append(("image card ready", "pixel.png" in now and "已就绪" in now))

    # Submit; the echo keeps text/image order. Without a model route the turn
    # fails afterwards, which still leaves the admitted message durable.
    send(" look")
    pump(0.3)
    send("\r")
    pump(3)
    snapshots["submitted"] = screen.text()
    now = screen.text()
    checks.append(("ordered echo", "[图片 pixel.png]" in now))
    checks.append(("turn failure surfaced", "失败" in now))

    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))
    print(
        json.dumps(
            {"ok": all(ok for _, ok in checks), "checks": checks, "exit": code},
            ensure_ascii=False,
            indent=2,
        )
    )
    for name in ("completion", "attachment", "submitted"):
        print(f"--- SCREEN {name} ---")
        print(snapshots[name])
    return 0 if all(ok for _, ok in checks) else 2


if __name__ == "__main__":
    sys.exit(main())
