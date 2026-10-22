#!/usr/bin/env python3
"""Drive the S12 subagent dock and child view in a real PTY.

Evidence tooling only. It uses the real ~/.dsh profile for credentials, starts
the built `nano` binary against a throwaway data root, asks the real model to
spawn one subagent, then snapshots the dock, enters the child view, sends one
follow-up, and returns with Esc. Run after `cargo build -p nano-app`:

    python3 scripts/pty-evidence-s12.py
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
import unicodedata
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
        values = [int(value) if value.isdigit() else 0 for value in params.replace("?", "").split(";")] if params else []

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
        return "\n".join("".join(row).rstrip() for row in self.grid)


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    dsh_home = Path(os.environ.get("DSH_HOME", Path.home() / ".dsh"))
    root = Path(tempfile.mkdtemp(prefix="nano-s12-pty-"))
    data, work = root / "data", root / "work"
    work.mkdir(parents=True, exist_ok=True)
    dsh = shutil.which("dsh")
    install = subprocess.run(
        [
            "node", str(repo / "bridge" / "profile" / "install.mjs"),
            "--dsh-home", str(dsh_home), "--data-root", str(data),
            "--bridge-package", str(repo / "bridge"), "--dsh-bin", dsh,
            "--profile", "dsh-tui-nano", "--json",
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
        "--dsh-home", str(dsh_home), "--data-root", str(data), "--cwd", str(work),
        "--provider", os.environ.get("NANO_EVIDENCE_PROVIDER", "deepseek-official"),
        "--model", os.environ.get("NANO_EVIDENCE_MODEL", "deepseek-flash"),
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

    def wait_for(text, timeout, label):
        deadline = time.time() + timeout
        while time.time() < deadline:
            pump(0.5)
            if text in screen.text():
                return True
        print(json.dumps({"failed": label, "screen": screen.text()}, ensure_ascii=False, indent=2))
        return False

    checks = []
    snapshots = {}
    pump(5)
    checks.append(("connected", "已连接" in screen.text()))
    prompt = (
        "只做一件事：调用 subagent 工具（不要指定 provider），label 设为 s12-tui，"
        "任务内容为「只回复一行 S12CHILD，不要调用任何工具」。等它返回后，用一句很短的话结束。"
    )
    send(prompt)
    send("\r")
    if not wait_for("local agents", 150, "dock"):
        child.kill()
        return 2
    snapshots["dock"] = screen.text()
    checks.append(("dock pill", "local agents" in snapshots["dock"]))
    checks.append(("dock row", "s12-tui" in snapshots["dock"] or "S12CHILD" in snapshots["dock"]))

    # Down enters the dock, Enter opens the child.
    send("\x1b[B")
    pump(0.5)
    send("\r")
    pump(3)
    snapshots["child"] = screen.text()
    checks.append(("child header", "子代理" in snapshots["child"]))
    checks.append(("exit button", "返回" in snapshots["child"]))

    send("请只回复一行 S12TUI，不要调用任何工具。")
    pump(0.4)
    send("\r")
    if not wait_for("S12TUI", 90, "child follow-up"):
        child.kill()
        return 2
    snapshots["followup"] = screen.text()
    checks.append(("child follow-up visible", "S12TUI" in snapshots["followup"]))

    send("\x1b")
    pump(1.5)
    snapshots["return"] = screen.text()
    checks.append(("returned to parent", "local agents" in snapshots["return"] and "子代理" not in snapshots["return"].split("local agents")[0]))

    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))
    print(json.dumps({"ok": all(ok for _, ok in checks), "checks": checks, "exit": code}, ensure_ascii=False, indent=2))
    for name, text in snapshots.items():
        print(f"--- SCREEN {name} ---")
        print(text)
    shutil.rmtree(root, ignore_errors=True)
    return 0 if all(ok for _, ok in checks) else 2


if __name__ == "__main__":
    sys.exit(main())
