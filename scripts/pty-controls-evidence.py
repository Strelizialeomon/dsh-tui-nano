#!/usr/bin/env python3
"""Drive the Nano TUI in a real PTY and capture S04 model/permission evidence.

This is evidence tooling, not part of the product. It starts the built `nano`
binary against the user's real dsh home (credentials stay in place) with a
throwaway data root and working directory, then walks the model and permission
panes and prints readable screen snapshots.

Run after `cargo build -p nano-app`:
    python3 scripts/pty-controls-evidence.py
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
import tempfile
import termios
import time
from pathlib import Path

ROWS, COLS = 40, 120


def load_screen():
    spec = importlib.util.spec_from_file_location(
        "pty_evidence", Path(__file__).resolve().parent / "pty-evidence.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.Screen


Screen = load_screen()


def main():
    repo = Path(__file__).resolve().parent.parent
    binary = repo / "target" / "debug" / "nano"
    if not binary.exists():
        print(f"missing {binary}; run: cargo build -p nano-app", file=sys.stderr)
        return 2
    dsh_home = Path(os.environ.get("NANO_DSH_HOME", "~/.dsh")).expanduser()
    root = Path(tempfile.mkdtemp(prefix="nano-s04-"))
    data, work = root / "data", root / "work"
    work.mkdir(parents=True, exist_ok=True)

    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    args = [
        str(binary),
        "--no-install",
        "--no-alt-screen",
        "--dsh-home",
        str(dsh_home),
        "--data-root",
        str(data),
        "--cwd",
        str(work),
    ]
    environment = dict(os.environ)
    environment["TERM"] = "xterm-256color"
    child = subprocess.Popen(
        args,
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=environment,
        close_fds=True,
        preexec_fn=os.setsid,
    )
    os.close(slave)
    screen = Screen()
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    snapshots = []

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

    def snapshot(label):
        pump(0.4)
        text = screen.text()
        snapshots.append({"label": label, "screen": text})
        print(f"--- {label} ---")
        print(text)
        print()
        return text

    def wait_for(markers, timeout):
        deadline = time.time() + timeout
        while time.time() < deadline:
            pump(0.2)
            text = screen.text()
            if any(marker in text for marker in markers):
                return text
        return screen.text()

    connected = wait_for(["已连接", "失败", "启动失败", "错误"], 30)
    snapshot("connected")
    checks = [("connected", "已连接" in connected)]

    # Command palette: the list must come from the official command service.
    send("\x10")
    palette = snapshot("command-palette")
    checks.append(("command palette opened", "命令 · 查询" in palette))
    checks.append(("official commands listed", "/permission" in palette))
    send("\x1b")
    pump(0.3)

    # A no-op preset switch succeeds without claiming a model notification.
    send("/permission\r")
    pump(0.5)
    send("workspace")
    pump(0.4)
    send("\r")
    noop = snapshot("permission-noop")
    checks.append(("no-op receipt shown", "权限已切换为 workspace-write（官方回执）" in noop))
    checks.append(("no-op does not claim a model notice", "模型已收到" not in noop))

    # Cold start: no model selection yet. The request must use the official
    # deployment default that the bridge injected into the selection ref.
    cold_turn = ""
    if os.environ.get("NANO_COLD_PROMPT") == "1":
        send("\x1b[200~只回复 ok\x1b[201~")
        pump(0.3)
        send("\r")
        cold_turn = wait_for(["助手", "失败", "错误"], 60)
        snapshot("cold-turn")
        checks.append(("cold-start reply visible", "助手" in cold_turn))
        checks.append(("cold-start turn did not fail", "失败" not in cold_turn))

    # Model pane: open, show the catalog, then walk the current model.
    send("/model\r")
    model_panel = snapshot("model-panel")
    checks.append(("model pane opened", "模型 · 查询" in model_panel))
    checks.append(("provider or model rows shown", "强度：" in model_panel or "目录读取失败" in model_panel))
    checks.append(("current model line shown", "当前：" in model_panel))
    # Move to the second model, then choose an explicit advertised effort.
    send("\x1b[B")
    send("\r")
    after_enter = snapshot("model-enter")
    if "推理强度 · 查询" in after_enter:
        checks.append(("effort pane opened", True))
        send("\x1b[B\x1b[B")
        send("\r")
        effort_selected = snapshot("effort-selected")
        checks.append(("effort selection confirmed", "会话内模型已切换" in effort_selected))
    else:
        checks.append(("effort pane opened", False))
        effort_selected = after_enter
        checks.append(("model selection confirmed", "会话内模型已切换" in effort_selected))
    send("\x1b")
    pump(0.3)

    # Re-open the pane: the durable selection and its effort are now shown.
    send("/model\r")
    reopened = snapshot("model-reopened")
    checks.append(("explicit effort shown", "强度 默认" not in reopened))
    checks.append(("selected model marked current", "（当前）" in reopened))
    send("\x1b")
    pump(0.3)

    # Permission pane: show the catalog, filter to danger-full-access, switch.
    send("/permission\r")
    permission_panel = snapshot("permission-panel")
    checks.append(("permission pane opened", "权限预设 · 查询" in permission_panel))
    checks.append(("current permission shown", "当前：" in permission_panel))
    send("danger")
    filtered = snapshot("permission-filtered")
    checks.append(("approval consequence shown", "approval=never" in filtered))
    checks.append(("approval rejection consequence shown", "自动拒绝" in filtered))
    send("\r")
    permission_switched = snapshot("permission-switched")
    checks.append(("permission switch confirmed", "权限已切换为" in permission_switched))
    checks.append(("status bar shows new preset", "权限 danger-full-access" in permission_switched))

    # One authorized real model call: the selected route and the injected
    # changed-by-user notice must reach the next request.
    if os.environ.get("NANO_SKIP_MODEL") != "1" and os.environ.get("NANO_COLD_PROMPT") != "1":
        send("\x1b[200~只回复 ok\x1b[201~")
        pump(0.3)
        send("\r")
        turn = wait_for(["助手", "失败", "错误"], 60)
        snapshot("real-turn")
        checks.append(("assistant reply visible", "助手" in turn))
        checks.append(("turn did not fail", "失败" not in turn))

    # Switch back to the workspace-write preset through the same command path.
    send("/permission\r")
    pump(0.5)
    send("workspace")
    pump(0.4)
    send("\r")
    restored = snapshot("permission-restored")
    checks.append(("permission restored", "权限 workspace-write" in restored))

    # Leave the deployment default where it started: Flash with its default effort.
    send("/model\r")
    pump(0.5)
    send("flash")
    pump(0.4)
    send("\r")
    pump(0.5)
    send("\r")
    restored_model = snapshot("model-restored")
    checks.append(("model restored", "会话内模型已切换" in restored_model))
    send("\x1b")
    pump(0.3)

    send("\x03")
    pump(2)
    try:
        code = child.wait(timeout=20)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "timeout"
    checks.append(("clean exit", code == 0))

    # Collect the official session log for the same run.
    sessions = sorted(
        (data / "sessions").rglob("session*.jsonl.zstd"),
        key=lambda path: path.stat().st_mtime,
    )
    events = []
    if sessions:
        decompressed = subprocess.run(
            ["zstd", "-dc", str(sessions[-1])], capture_output=True, check=True
        )
        for line in decompressed.stdout.decode("utf-8").splitlines():
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                continue

    def event_data(event):
        return event.get("data", event)

    extracted = []
    for event in events:
        kind = event.get("type")
        data = event_data(event)
        if kind == "model/selection":
            extracted.append({"type": kind, "data": data})
        elif kind == "request/header":
            extracted.append({"type": kind, "config": data.get("header", {}).get("config")})
        elif kind in ("assistant/message", "turn/end"):
            extracted.append({"type": kind, "data": data})
        elif kind in ("permission/preset", "sandbox/mode", "approval/policy"):
            extracted.append({"type": kind, "data": data})
        elif kind in ("command/run", "command/done"):
            if data.get("name") == "permission" or data.get("commandId") in {
                entry["data"].get("commandId") for entry in extracted if entry["type"] == "command/run"
            }:
                extracted.append({"type": kind, "data": data})
        elif kind == "user/message":
            source = data.get("source") or {}
            content = data.get("content") or []
            text = " ".join(
                block.get("text", "")
                for block in content
                if isinstance(block, dict) and block.get("type") == "text"
            )
            if source.get("kind") == "user-approval" or "changed by the user" in text:
                extracted.append(
                    {"type": kind, "source": source.get("kind"), "text": text}
                )

    routes = [entry.get("config") for entry in extracted if entry["type"] == "request/header"]
    if os.environ.get("NANO_COLD_PROMPT") == "1":
        checks.append((
            "cold-start request used an official route",
            any(
                route is not None
                and isinstance(route.get("provider"), str)
                and route.get("provider") != ""
                and isinstance(route.get("model"), str)
                and route.get("model") != ""
                for route in routes
            ),
        ))
    elif os.environ.get("NANO_SKIP_MODEL") != "1":
        checks.append((
            "next request used the switched route",
            any(
                route is not None
                and route.get("model") == "deepseek-v4-pro"
                and route.get("reasoningEffort") == "low"
                for route in routes
            ),
        ))
        checks.append((
            "approval notice reached the model",
            any(
                entry["type"] == "user/message" and entry.get("source") == "user-approval"
                for entry in extracted
            ),
        ))

    report = {
        "ok": all(ok for _, ok in checks),
        "checks": checks,
        "sessionFile": str(sessions[-1]) if sessions else None,
        "officialEvents": extracted,
        "snapshots": snapshots,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["ok"] else 2


if __name__ == "__main__":
    sys.exit(main())
