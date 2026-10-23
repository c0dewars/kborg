#!/usr/bin/env python3
"""Capture actual live node cordon review, then cancel; no scheduling write."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import struct
import subprocess
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parent.parent
HELPERS = runpy.run_path(str(ROOT / "scripts/test-live-lifecycle-ui.py"))
Screen, spawn = HELPERS["Screen"], HELPERS["_spawn"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/kborg")
    parser.add_argument("--context", default="docker-desktop")
    parser.add_argument("--node", default="desktop-control-plane")
    parser.add_argument("--kubeconfig", type=Path)
    parser.add_argument("--screenshots", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    prefix = ["kubectl", "--context", args.context, "--request-timeout=15s"]
    argv = [str(binary), "--context", args.context, "--write", "--allow-node-maintenance", "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"]
    if args.kubeconfig:
        prefix += ["--kubeconfig", str(args.kubeconfig.resolve())]
        argv += ["--kubeconfig", str(args.kubeconfig.resolve())]

    def node():
        result = subprocess.run([*prefix, "get", "node", args.node, "-o", "json"], text=True, capture_output=True, timeout=20)
        assert result.returncode == 0, "cannot inspect exact node before/after UI capture"
        return json.loads(result.stdout)

    before = node()
    captures = []
    with tempfile.TemporaryDirectory(prefix="kborg-node-ui-") as temporary:
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 120, 0, 0))
        environment = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        environment.pop("NO_COLOR", None)
        process = spawn(argv, environment, slave)
        screen = Screen(120, 34)
        total = 0

        def drain(seconds=.15):
            nonlocal total
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], min(.04, max(0, until-time.monotonic())))[0]:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError:
                        break
                    if not chunk:
                        break
                    total += len(chunk)
                    assert total < 8 * 1024 * 1024, "UI output exceeded capture bound"
                    screen.feed(chunk)

        def keys(data=b"", expected=None):
            if data:
                os.write(master, data)
            until = time.monotonic() + 25
            while time.monotonic() < until:
                drain()
                if expected is None or expected in screen.text():
                    drain(.2)
                    return
                assert process.poll() is None, "UI exited before target state"
            raise AssertionError("node UI did not reach expected state: " + str(expected))

        def capture(name):
            path = args.screenshots / (name + ".png")
            screen.png(path)
            captures.append({"name": name, "path": str(path.resolve()), "width": 120, "height": 34})

        try:
            keys(expected="CTX ")
            keys(b":nodes\r", args.node)
            capture("nodes")
            keys(b":cordon\r", "REVIEW CHANGES / approval required")
            assert "node-cordon" in screen.text(), "wrong node action review"
            capture("cordon-review")
            # Paint the whole review without entering the confirmation phrase.
            for _ in range(20):
                if "0 unread" in screen.text():
                    break
                keys(b"\x1b[6~")
            assert "0 unread" in screen.text(), "node review has unseen lines"
            capture("cordon-review-complete")
            keys(b"\x1b", args.node)
            assert "REVIEW CHANGES / approval required" not in screen.text(), "review failed to cancel"
            capture("cordon-cancelled")
            keys(b":q\r")
            until = time.monotonic() + 6
            while process.poll() is None and time.monotonic() < until:
                drain()
            assert process.poll() == 0, "UI failed to exit cleanly"
            assert termios.tcgetattr(master) == original, "UI did not restore terminal mode"
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)
    after = node()
    assert before["metadata"]["uid"] == after["metadata"]["uid"] and before["spec"] == after["spec"], "canceled UI review changed node scheduling"
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "binary changed during UI acceptance"
    args.screenshots.mkdir(parents=True, exist_ok=True)
    (args.screenshots / "report.json").write_text(json.dumps({"passed": True, "binary_sha256": digest, "context": args.context, "node": args.node, "node_spec_unchanged": True, "terminal_restored": True, "captures": captures}, indent=2) + "\n")


if __name__ == "__main__":
    main()
