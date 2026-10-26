#!/usr/bin/env python3
"""PTY acceptance for live config reload (demo mode, private temp config).

Saves ui.yaml the way editors do (write a temp file and rename it over, then
truncate and write) and checks the running screen changes within 1 s without
`:ui reload`; an invalid save keeps the last good look and reports file, line
and next step. Optional PNG captures require Pillow.
"""
import argparse
import errno
import faulthandler
import fcntl
import os
from pathlib import Path
import pty
import runpy
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

Screen = runpy.run_path(str(Path(__file__).with_name("test-terminal.py")))["Screen"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    faulthandler.dump_traceback_later(60, exit=True)
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="kborg-reload-") as temporary:
        config = Path(temporary).resolve()
        os.chmod(config, 0o700)
        ui = config / "ui.yaml"
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 120, 0, 0))
        environment = os.environ.copy()
        environment.pop("KBORG_CONFIG_RELOAD", None)
        environment.update(TERM="xterm-256color", COLORTERM="truecolor", KBORG_CONFIG_DIR=str(config))

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        process = subprocess.Popen(
            [str(binary), "--demo", "--theme", "catppuccin-mocha", "--no-splash", "--no-mouse", "--color=always"],
            stdin=slave, stdout=slave, stderr=slave, env=environment, preexec_fn=child_setup)
        screen = Screen(120, 30)
        total = 0

        def drain(seconds):
            nonlocal total
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], min(.02, max(0, until - time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    total += len(data)
                    assert total <= 8 * 1024 * 1024, "terminal output exceeded 8 MiB"
                    screen.feed(data)

        def wait_for(check, limit, what):
            started = time.monotonic()
            while time.monotonic() - started < limit:
                drain(.02)
                if check(screen.text().splitlines()):
                    return time.monotonic() - started
            raise AssertionError(f"{what} not seen within {limit}s\n{screen.text()}")

        def capture(name):
            if args.screenshots:
                args.screenshots.mkdir(parents=True, exist_ok=True)
                screen.png(args.screenshots / f"{name}.png")

        def save(text, rename=True):
            if rename:
                temp = config / ".ui.yaml.tmp"
                temp.write_text(text)
                os.chmod(temp, 0o600)
                os.replace(temp, ui)
            else:
                with open(ui, "w") as handle:
                    handle.flush()
                    time.sleep(.05)
                    handle.write(text)
                os.chmod(ui, 0o600)

        table_top = lambda lines: next((line for line in lines if "pods" in line.lower() and line[:1] in "╭┌+"), "")
        try:
            wait_for(lambda lines: "api-7d4f9" in "\n".join(lines), 10, "demo pods")
            drain(1.5)  # let startup settle so the first poll baseline is quiet
            assert table_top(screen.text().splitlines()).startswith("╭"), screen.text()
            capture("reload-before")
            save("version: 1\nborders: ascii\n")
            took = wait_for(lambda lines: table_top(lines).startswith("+"), 1.0, "ascii borders after rename save")
            print(f"rename save applied in {took:.2f}s")
            assert "Applied ui.yaml" in screen.text()
            capture("reload-ascii")
            save("version: 1\nborders: square\n", rename=False)
            took = wait_for(lambda lines: table_top(lines).startswith("┌"), 1.0, "square borders after truncate+write")
            print(f"truncate+write save applied in {took:.2f}s")
            save("version: 1\nborders: ascii\nbordrs: none\n")
            took = wait_for(lambda lines: "ui.yaml not applied" in "\n".join(lines), 1.0, "invalid-file report")
            text = screen.text()
            assert "line 3" in text and "Next:" in text, text
            assert table_top(text.splitlines()).startswith("┌"), "last good look kept"
            print(f"invalid save reported in {took:.2f}s")
            capture("reload-invalid")
        finally:
            os.write(master, b"\x1b:q\r")
            # Keep reading so the child never blocks on a full PTY buffer.
            until = time.monotonic() + 5
            while process.poll() is None and time.monotonic() < until:
                drain(.1)
            if process.poll() is None:
                process.kill()
                drain(.5)
            os.close(master)
            os.close(slave)
    print("live config reload PTY check passed")


if __name__ == "__main__":
    sys.exit(main())
