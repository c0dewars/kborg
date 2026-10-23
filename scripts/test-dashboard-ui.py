#!/usr/bin/env python3
"""Capture and verify the actual dashboard TUI with demo or explicit read-only contexts.

Live mode creates no cluster resources. The caller supplies an isolated fixture
namespace and kubeconfig, and retains responsibility for fixture cleanup.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import signal
import struct
import tempfile
import termios
import time

_helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, _spawn = (_helpers[name] for name in ("Screen", "_spawn"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--screenshots", type=Path, required=True)
    parser.add_argument("--kubeconfig", type=Path)
    parser.add_argument("--contexts", default="demo")
    parser.add_argument("--namespace", default="demo")
    parser.add_argument("--theme", default="rose-pine")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    names = args.contexts.split(",")
    live = args.kubeconfig is not None
    if not names or len(names) > 4 or len(set(names)) != len(names):
        raise ValueError("Provide one to four unique explicit contexts")
    if live and not args.namespace.startswith("kborg-dashboard-"):
        raise ValueError("Live capture requires the caller's kborg-dashboard-* fixture namespace")
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    captures, checks = [], []
    args.screenshots.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="kborg-dashboard-ui-") as temporary:
        config = Path(temporary).resolve()
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 120, 0, 0))
        environment = os.environ.copy()
        environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(config))
        environment.pop("NO_COLOR", None)
        argv = [str(binary), "--theme", args.theme, "--no-splash", "--no-mouse", "--color=always"]
        if live:
            argv += ["--kubeconfig", str(args.kubeconfig.resolve(strict=True)), "--context", names[0], "--namespace", args.namespace]
        else:
            argv += ["--demo"]
        process = _spawn(argv, environment, slave)
        screen = Screen(120, 34)

        def drain(seconds=.15):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if select.select([master], [], [], min(.05, max(0, end - time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError:
                        break
                    if not data:
                        break
                    screen.feed(data)

        def keys(value=b"", expected=None, timeout=30):
            if value:
                os.write(master, value)
            end = time.monotonic() + timeout
            while time.monotonic() < end:
                drain()
                text = screen.text()
                if expected is None or (expected in text if isinstance(expected, str) else expected(text)):
                    drain(.15)
                    return
                if process.poll() is not None:
                    break
            raise RuntimeError(f"Dashboard did not reach {expected!r}\n{screen.text()}")

        def capture(name):
            path = args.screenshots / f"{name}.png"
            screen.png(path)
            captures.append({"name": name, "path": str(path.resolve()), "width": screen.width, "height": screen.height})

        def resize(width, height):
            nonlocal screen
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            screen = Screen(width, height)
            os.killpg(process.pid, signal.SIGWINCH)
            drain(.4)

        try:
            keys(expected=lambda text: "LIVE" in text or "SAMPLE" in text)
            # Wait for initialization and the optional table metrics request.
            keys(expected=lambda text: "LOADING" not in text and "Request in progress" not in text)
            capture("resources-clean-header")
            assert ":pods" not in screen.text() and ":ctx" not in screen.text(), "command menu remained visible"
            checks.append("quiet header has no colon-command menu")
            keys(b"D", "CONTEXT OVERVIEW")
            keys(expected=lambda text: "observed" in text)
            capture("dashboard-single-context")
            keys(b"?", "DASHBOARD KEYS")
            capture("dashboard-keymap")
            keys(b"\x1b", "CONTEXT OVERVIEW")
            assert "DASHBOARD KEYS" not in screen.text(), "Escape closed dashboard instead of help"
            checks.append("contextual help returns to live dashboard")
            requested = names if live else ["development", "staging", "production", "sandbox"]
            keys(b"c", "Overview contexts")
            keys(b"\x7f" * len(names[0]) + ",".join(requested).encode() + b"\r", "CONTEXT OVERVIEW")
            keys(expected=lambda text: "observed" in text and all(name in text for name in requested))
            capture("dashboard-context-cards")
            selected = len(requested)
            keys(str(selected).encode(), f"{selected} of {selected}")
            capture("dashboard-numbered-focus")
            checks.append("explicit context cards and numbered focus")
            resize(80, 24)
            keys(expected=f"{selected} of {selected}")
            capture("dashboard-80x24")
            resize(48, 16)
            keys(expected=requested[-1])
            capture("dashboard-narrow-focus")
            assert f"{selected} of {selected}" in screen.text(), "resize changed focused context"
            checks.append("responsive grid preserves focused context")
            resize(120, 34)
            keys(b"s", "Save selection / name")
            keys(b"dashboard-fixture\r", "CONTEXT OVERVIEW")
            keys(expected="saved locally")
            capture("dashboard-save-confirmation")
            keys(b"B", "Saved selections")
            capture("dashboard-saved-selection")
            assert "dashboard-fixture" in screen.text(), "saved dashboard missing from picker"
            keys(b"\x1b", "CONTEXT OVERVIEW")
            checks.append("explicit local dashboard save and picker")
            # The initial demo context is the only kubeconfig-free resource identity.
            if not live:
                keys(b"c", "Overview contexts")
                keys(b"\x7f" * len(",".join(requested)) + b"demo\r", "CONTEXT OVERVIEW")
                keys(expected="observed")
            keys(b"\r", expected=lambda text: "CONTEXT OVERVIEW" not in text and ("LIVE" in text or "SAMPLE" in text))
            capture("dashboard-drilled-pods")
            checks.append("Enter drills into focused context Pods")
            keys(b":q\r")
            end = time.monotonic() + 8
            while process.poll() is None and time.monotonic() < end:
                drain()
            assert process.poll() == 0, "dashboard binary failed to exit cleanly"
            assert termios.tcgetattr(master) == original, "terminal mode not restored"
            checks.append("clean exit restores terminal state")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "binary changed during capture"
    report = {"passed": True, "live": live, "contexts": names, "namespace": args.namespace, "theme": args.theme,
              "binary_sha256": digest, "checks": checks, "screens": captures, "terminal_restored": True}
    (args.screenshots / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
