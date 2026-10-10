#!/usr/bin/env python3
"""Native Unix PTY acceptance; optional PNG captures require Pillow.

Exercises the actual release binary, keyboard paths and resize events. Runs demo
only, with private temporary preferences and a bounded subprocess lifetime.
"""
import argparse
import errno
import fcntl
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


class Screen:
    def __init__(self, width, height):
        self.width, self.height = width, height
        self.x = self.y = 0
        self.fg, self.bg = (205, 214, 244), (30, 30, 46)
        self.cells = [[(" ", self.fg, self.bg) for _ in range(width)] for _ in range(height)]
        self.pending = ""

    def feed(self, text):
        self.pending += text
        position = 0
        while position < len(self.pending):
            char = self.pending[position]
            if char == "\x1b":
                match = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", self.pending[position:])
                if not match:
                    break
                raw, command = match.groups()
                values = [int(value) if value else 0 for value in raw.lstrip("?").split(";")]
                if command in ("H", "f"):
                    self.y = max(0, (values[0] or 1) - 1)
                    self.x = max(0, (values[1] if len(values) > 1 and values[1] else 1) - 1)
                elif command == "J" and values[0] == 2:
                    self.cells = [[(" ", self.fg, self.bg) for _ in range(self.width)] for _ in range(self.height)]
                elif command == "m":
                    index = 0
                    while index < len(values):
                        value = values[index]
                        if value == 0:
                            self.fg, self.bg = (205, 214, 244), (30, 30, 46)
                        elif value == 39:
                            self.fg = (205, 214, 244)
                        elif value == 49:
                            self.bg = (30, 30, 46)
                        elif value in (38, 48) and index + 4 < len(values) and values[index + 1] == 2:
                            color = tuple(values[index + 2:index + 5])
                            if value == 38:
                                self.fg = color
                            else:
                                self.bg = color
                            index += 4
                        index += 1
                position += len(match.group(0))
                continue
            if char == "\r":
                self.x = 0
            elif char == "\n":
                self.y += 1
            elif char >= " " and self.x < self.width and self.y < self.height:
                self.cells[self.y][self.x] = (char, self.fg, self.bg)
                self.x += 1
            position += 1
        self.pending = self.pending[position:]

    def text(self):
        return "\n".join("".join(cell[0] for cell in row) for row in self.cells)

    def png(self, path):
        from PIL import Image, ImageDraw, ImageFont
        font_path = "/System/Library/Fonts/Menlo.ttc"
        font = ImageFont.truetype(font_path, 17) if Path(font_path).exists() else ImageFont.load_default()
        cell_width = int(font.getlength("M") + .5)
        cell_height = 24
        picture = Image.new("RGB", (self.width * cell_width + 32, self.height * cell_height + 32), (30, 30, 46))
        draw = ImageDraw.Draw(picture)
        for y, row in enumerate(self.cells):
            for x, (char, fg, bg) in enumerate(row):
                left, top = 16 + x * cell_width, 16 + y * cell_height
                draw.rectangle((left, top, left + cell_width, top + cell_height), fill=bg)
                draw.text((left, top), char, font=font, fill=fg)
        path.parent.mkdir(parents=True, exist_ok=True)
        picture.save(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", default=Path("test-results/terminal.json"), type=Path)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    captures = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kborg-terminal-") as temporary:
        master, slave = pty.openpty()
        original_mode = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        environment = os.environ.copy()
        environment["NO_COLOR"] = "1"
        environment.update(TERM="xterm-256color", COLORTERM="truecolor", KBORG_CONFIG_DIR=str(Path(temporary).resolve()))

        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        # Keep the controlling PTY session alive after KBorg exits, so macOS
        # does not invalidate tcgetattr before restoration can be inspected.
        launcher = ("import json,resource,subprocess,sys; "
                    "status=subprocess.call(sys.argv[1:]); "
                    "usage=resource.getrusage(resource.RUSAGE_CHILDREN); "
                    "print('\\nKBORG_PTY_RESULT '+json.dumps({'status':status,'cpu':usage.ru_utime+usage.ru_stime,'rss':usage.ru_maxrss}),flush=True); "
                    "sys.stdin.readline(); sys.exit(status)")
        process = subprocess.Popen([sys.executable, "-c", launcher, str(binary), "--demo", "--theme", "catppuccin-mocha", "--no-splash", "--allow-logs", "--no-mouse", "--color=always"],
                                   stdin=slave, stdout=slave, stderr=slave, env=environment, preexec_fn=child_setup)
        screen = Screen(120, 32)
        all_output = bytearray()

        def drain(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], min(.1, max(0, until - time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not data:
                        break
                    all_output.extend(data)
                    if len(all_output) > 4 * 1024 * 1024:
                        raise RuntimeError("terminal output exceeded 4 MiB test bound")
                    screen.feed(data.decode("utf-8", errors="replace"))

        def capture(name, required):
            text = screen.text()
            for value in required:
                assert value.casefold() in text.casefold(), f"{name}: missing {value!r} in actual terminal frame\n{text}"
            captures.append({"name": name, "width": screen.width, "height": screen.height})
            if args.screenshots:
                screen.png(args.screenshots / f"{name}.png")

        def keys(value, wait=.5):
            os.write(master, value)
            drain(wait)

        try:
            drain(1)
            capture("demo-pods", ["KBorg", "DEMO", "api-7d4f9", "CrashLoopBackOff"])
            assert b"\x1b[38;2;" in all_output, "explicit truecolor output was suppressed"
            # A quiet interval measures actual idle redraw/output behavior.
            idle_bytes = len(all_output)
            drain(2)
            idle_bytes = len(all_output) - idle_bytes
            keys(b"\r")
            capture("object-overview", ["OBJECT OVERVIEW", "app", "Running"])
            keys(b"J")
            capture("json", ["apiVersion", "containers"])
            keys(b"\x1b")
            keys(b"a")
            capture("actions", ["ACTIONS", "[OFF]", "READ ONLY"])
            keys(b"\x1b")
            keys(b":deploy\r")
            capture("deployments", ["deployments", "2/2 ready"])
            keys(b":pods\r")
            keys(b"S")
            capture("sorted-pods", ["sort:namespace", "api-7d4f9"])
            keys(b"m", 1)
            capture("metrics", ["METRICS", "CPU", "MEM", "SESSION HISTORY"])
            keys(b"M", 1)
            capture("allocations", ["requests", "50.00%", "Resource comparisons"])
            keys(b"\x1b")
            keys(b"l", 1)
            capture("logs", ["container", "app"])
            keys(b"\r", 1)
            capture("log-detail", ["demo", "INFO"])
            keys(b"\x1b")
            keys(b"o")
            keys(b"demo\r", 1)
            capture("live-overview", ["LIVE", "demo", "UNHEALTHY"])
            keys(b"\x1b")
            keys(b"t")
            capture("themes", ["Themes", "Catppuccin Mocha"])
            keys(b"N")
            capture("create-theme", ["theme", "Enter"])
            keys(b"\x1b")
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 12, 48, 0, 0))
            screen = Screen(48, 12)
            os.killpg(process.pid, signal.SIGWINCH)
            drain(.5)
            capture("narrow", ["DEMO", "api-7d4f9"])
            keys(b"q")
            result_match = re.search(rb"KBORG_PTY_RESULT (\{[^\r\n]+\})", all_output)
            assert result_match, "KBorg did not exit within the keyboard deadline"
            native_usage = json.loads(result_match.group(1))
            assert native_usage["status"] == 0, "terminal session failed"
            assert b"\x1b[?1049l" in all_output, "alternate screen was not restored"
            assert termios.tcgetattr(slave) == original_mode, "terminal input mode was not restored"
            keys(b"\n", .1)
            process.wait(timeout=3)
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)
    report = {"schema": 1, "platform": os.uname().sysname, "architecture": os.uname().machine,
              "binary": str(binary), "binary_sha256": __import__("hashlib").sha256(binary.read_bytes()).hexdigest(),
              "elapsed_seconds": round(time.monotonic() - started, 3),
              "child_cpu_seconds": round(native_usage["cpu"], 6),
              "peak_rss_bytes": native_usage["rss"] if os.uname().sysname == "Darwin" else native_usage["rss"] * 1024,
              "idle_output_bytes_over_2_seconds": idle_bytes,
              "terminal_restored": True, "screens": captures,
              "scope": "native PTY, offline samples, ASCII truecolor; not a real-terminal/platform matrix"}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
