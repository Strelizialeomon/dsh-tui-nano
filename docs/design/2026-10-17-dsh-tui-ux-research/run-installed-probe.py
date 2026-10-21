"""Run the installed UI probe with fabricated content and a temporary HOME.

Usage: python3 run-installed-probe.py [100|60]
Requires the existing local dsh-tui/DSH installation named in the JS probe.
Does not start a backend, send prompts, or resume a real conversation.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

script = Path(__file__).with_name('installed-ui-probe.mjs')
widths = [int(sys.argv[1])] if len(sys.argv) > 1 else [100, 60]
for width in widths:
    with tempfile.TemporaryDirectory(prefix='dsh-ux-probe-') as task_home:
        env = {key: value for key, value in os.environ.items() if key in ['PATH', 'TMPDIR', 'LANG']}
        env.update(HOME=task_home, DSH_HOME=task_home + '/.dsh', TERM='xterm-ghostty',
                   TERM_PROGRAM='ghostty', COLORTERM='truecolor')
        result = subprocess.run(['node', str(script), str(width)], env=env,
                                capture_output=True, text=True, timeout=25)
        if result.returncode:
            print(result.stderr[-5000:], file=sys.stderr)
            raise SystemExit(result.returncode)
        # Ignore terminal-restoration escapes emitted during process shutdown.
        data, _ = json.JSONDecoder().raw_decode(result.stdout.lstrip())
        print(json.dumps(data, ensure_ascii=False, indent=2))
