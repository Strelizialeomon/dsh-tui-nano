#!/usr/bin/env python3
"""Drive S07 session controls in a real PTY and print a text snapshot.

Evidence tooling only. It installs a throwaway profile, starts the built `nano`
binary, then exercises:
  * the official session panel and its cold/blank facts,
  * active-session rename through the official title service,
  * the reference context read with an explicit unknown occupancy,
  * the official /compact command with no compactable history,
  * a controlled switch to a new session (old claim released, new claim taken),
  * clean exit with an empty mount ledger.

Run after `cargo build -p nano-app`:
    python3 scripts/sessions-evidence.py
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
        lines = ["".join(row).rstrip() for row in self.grid]
        while lines and not lines[-1]:
            lines.pop()
        return "\n".join(lines)


def ledger_sessions(data_root):
    path = Path(data_root) / "session-mounts.json"
    if not path.exists():
        return []
    try:
        ledger = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return []
    sessions = []
    for owner in ledger.get("owners", []):
        sessions.extend(owner.get("sessionIds", []))
    return sessions


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    root = Path(tempfile.mkdtemp(prefix="nano-sessions-pty-"))
    home, data, work = root / "dsh-home", root / "data", root / "work"
    work.mkdir(parents=True, exist_ok=True)
    work = Path(os.path.realpath(work))
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
    pump(5)
    initial = screen.text()
    checks.append(("connected", "已连接" in initial))
    checks.append(("context unknown marker", "ctx 未知" in initial))
    first_sessions = ledger_sessions(data)
    checks.append(("initial claim present", len(first_sessions) == 1))

    # A draft belongs to the current address; it must survive the switch and
    # come back only when that address is shown again.
    send("草稿A")
    pump(0.4)

    # Alt+O opens the official session panel.
    send("\x1bo")
    pump(1.5)
    panel = screen.text()
    checks.append(("panel opened", "新建会话" in panel))
    checks.append(("panel shows context details", "上下文占用（参考值）：未知" in panel))
    checks.append(("panel shows billing usage", "计费用量" in panel))
    checks.append(("panel marks the current session", "· 当前" in panel))

    # Select the current session row (index 1; index 0 is "new session"), then
    # n opens the rename editor for the active session.
    send("\x1b[B")
    pump(0.3)
    send("n")
    pump(0.4)
    send("证据会话")
    pump(0.4)
    send("\r")
    pump(1.5)
    renamed = screen.text()
    checks.append(("rename accepted", "会话已重命名" in renamed or "证据会话" in renamed))
    send("\x1b")
    pump(0.5)

    # Alt+C runs the official /compact command on the idle, empty session.
    send("\x1bc")
    pump(2)
    compact = screen.text()
    checks.append(("official compact text", "No compactable history yet." in compact))

    # Alt+O, then Enter on the selected "new session" row performs a controlled
    # restart: the old host must exit and release its claim before the new one
    # claims its own root.
    send("\x1bo")
    pump(1.2)
    send("\r")
    pump(8)
    switched = screen.text()
    second_sessions = ledger_sessions(data)
    checks.append(("switch completed", "已切换到会话" in switched or "已连接" in switched))
    checks.append(("new claim differs", len(second_sessions) == 1 and second_sessions != first_sessions))
    checks.append(("old claim released", not (set(first_sessions) & set(second_sessions))))
    checks.append(("old draft not shown in the new session", "草稿A" not in switched))

    # Reopen the panel after the switch: the new session is a real official
    # session row, unnamed until a title exists.
    send("\x1bo")
    pump(1.5)
    after_switch_panel = screen.text()
    checks.append(("new session is unnamed", "未命名" in after_switch_panel))
    checks.append(("new session is current", "· 当前" in after_switch_panel))

    # Rows: new(0), the just-created session(1), the renamed original(2).
    # Return to the original session; its draft must reappear.
    send("\x1b[B")
    pump(0.2)
    send("\x1b[B")
    pump(0.2)
    send("\r")
    pump(8)
    returned = screen.text()
    returned_sessions = ledger_sessions(data)
    checks.append(("return switch completed", "已切换到会话" in returned or "已连接" in returned))
    checks.append(("return claim restored", returned_sessions == first_sessions))
    checks.append(("draft restored with its address", "草稿A" in returned))
    snapshot = screen.text()

    send("\x1b")
    pump(0.4)
    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=15)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))
    checks.append(("claim released", ledger_sessions(data) == []))
    print(json.dumps({"ok": all(ok for _, ok in checks), "checks": checks, "exit": code}, ensure_ascii=False, indent=2))
    print("--- SCREEN ---")
    print(snapshot)
    return 0 if all(ok for _, ok in checks) else 2


if __name__ == "__main__":
    sys.exit(main())
