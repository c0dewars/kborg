#!/usr/bin/env python3
"""Read-only PTY checks for the explicitly installed Docker Desktop metrics release.

Requires scripts/setup-desktop-metrics.py to have completed. No cluster mutations,
credential export or persistent preferences. Optional screenshots require Pillow.
"""
import argparse
import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    binary = args.binary.resolve(strict=True)
    spec = importlib.util.spec_from_file_location("terminal_check", root / "scripts/test-terminal.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    captures = []
    with tempfile.TemporaryDirectory(prefix="kborg-live-ui-") as temporary:
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        env = os.environ.copy()
        env.update(TERM="xterm-256color", COLORTERM="truecolor", KBORG_CONFIG_DIR=str(Path(temporary).resolve()))

        def setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        launcher = "import subprocess,sys,time; status=subprocess.call(sys.argv[1:]); print('\\nKBORG_LIVE_EXIT '+str(status),flush=True); time.sleep(2); sys.exit(status)"
        process = subprocess.Popen([sys.executable, "-c", launcher, str(binary), "--context", "docker-desktop", "-n", "kborg-system", "--allow-helm", "--theme", "catppuccin-mocha", "--no-splash", "--no-mouse", "--color=always"], stdin=slave, stdout=slave, stderr=slave, env=env, preexec_fn=setup)
        screen = module.Screen(120, 32)
        data = bytearray()

        def drain(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], min(.1, max(0, end-time.monotonic())))[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            return
                        raise
                    if not chunk:
                        return
                    data.extend(chunk)
                    if len(data) > 4 * 1024 * 1024:
                        raise RuntimeError("PTY byte cap")
                    screen.feed(chunk.decode("utf-8", errors="replace"))

        def capture(name, required, predicate=lambda _: True, timeout=30):
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                drain(.2)
                text = screen.text()
                if all(item.casefold() in text.casefold() for item in required) and predicate(text):
                    captures.append({"name": name, "required": required})
                    if args.screenshots:
                        screen.png(args.screenshots / (name + ".png"))
                    return
            raise RuntimeError(name + " did not reach expected screen: " + screen.text())

        def keys(value):
            os.write(master, value)

        try:
            capture("live-metrics-pod", ["PRODUCTION", "docker-desktop", "kborg-system", "pods [", "kborg-metrics-server"])
            keys(b":nodes\r")
            capture("live-nodes", ["nodes [", "desktop-control-plane", "CPU(m)", "METRICS 1 measured"])
            keys(b":metrics\r")
            capture("live-node-metrics", ["SESSION HISTORY", "desktop-control-plane", "CPU", "MEM", "latest", "min"])

            def multiple_samples(text):
                chart = re.search(r"CPU \|([^|]+)\|", text)
                return chart is not None and len(re.findall(r"[.:=+*#@]", chart[1])) >= 2

            capture("live-node-metrics-history", ["SESSION HISTORY", "CPU", "MEM"], multiple_samples, timeout=50)
            keys(b"\x1b")
            drain(.3)
            keys(b":helm kborg-system\r")
            capture("live-helm-releases", ["HELM / releases", "kborg-metrics-server", "deployed"])
            keys(b"\r")
            capture("live-helm-history", ["HELM HISTORY", "kborg-metrics-server", "revision"])
            keys(b"m")
            capture("live-helm-resources", ["HELM RELEASE RESOURCES", "Deployment", "APIService", "Resource identities only"])
            keys(b"\x1b")
            drain(.3)
            keys(b"\r")
            capture("live-helm-values", ["HELM STORED VALUES", "revision", "Stored release overrides only"])
            for _ in range(3):
                keys(b"\x1b")
                drain(.2)
            keys(b":q\r")
            drain(.8)
            assert b"KBORG_LIVE_EXIT 0" in data, "nonzero or delayed exit"
            assert b"\x1b[?1049l" in data and termios.tcgetattr(slave) == original, "terminal restoration"
            process.wait(timeout=5)
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except PermissionError:
                    process.kill()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)
    report = {"schema": 1, "context": "docker-desktop", "namespace": "kborg-system", "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "read_only": True, "terminal_restored": True, "screens": captures,
              "scope": "Native macOS PTY against real Desktop metrics and Helm release; two distinct provider timestamps in session history; no cluster mutations."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
