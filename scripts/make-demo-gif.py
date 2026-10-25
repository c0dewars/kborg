#!/usr/bin/env python3
"""Record the README demo GIF from the real binary in demo mode (no cluster).

Spawns KBorg in its own pseudo-terminal (120x32, truecolor), answers terminal
queries like a real emulator, drives a fixed key script, renders the emulated
screen to PNG after each step and assembles an animated GIF with Pillow.

  python3 scripts/make-demo-gif.py --binary target/release/kborg
  python3 scripts/make-demo-gif.py --binary ... --frames out/frames   # keep PNGs for review

Every run uses a private temporary HOME, KBORG_CONFIG_DIR and XDG directories,
so the user's own settings are never read or written. No kubeconfig is used.
Standard library plus Pillow. Unix only (macOS, Linux).
"""
import argparse
import fcntl
import os
from pathlib import Path
import pty
import re
import runpy
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

SCRIPTS = Path(__file__).resolve().parent
REPO = SCRIPTS.parent
_lifecycle = runpy.run_path(str(SCRIPTS / "test-live-lifecycle-ui.py"))
Screen, _spawn = _lifecycle["Screen"], _lifecycle["_spawn"]

WIDTH, HEIGHT = 120, 32
ARGS = ["--demo", "--no-splash", "--theme", "flexoki-dark",
        "--allow-logs", "--no-mouse", "--color=always"]

# The emulator in test-terminal.py understands CSI [0-9;?]* <letter> only and
# stalls on anything else, so OSC/DCS strings, charset selections and CSI
# sequences with other intermediates are removed before feeding it.
_FOREIGN = re.compile(
    rb"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)"      # OSC
    rb"|\x1b[P^_][^\x1b]*\x1b\\"               # DCS / PM / APC
    rb"|\x1b\[[0-9;?]*[^0-9;?A-Za-z\x1b][ -/]*[@-~]"  # CSI with >, <, =, space...
    rb"|\x1b[()][A-Z0-9]|\x1b[=>78NOM]")
_INCOMPLETE = re.compile(rb"\x1b(?:\][^\x07]*|[P^_].*|\[[0-9;?<>= -/]*|[()])?$", re.S)


class Session:
    def __init__(self, binary, frames_dir):
        self.frames_dir = frames_dir
        self.frames = []  # (png path, duration ms)
        self.tmp = tempfile.TemporaryDirectory(prefix="kborg-demo-gif-")
        home = Path(self.tmp.name).resolve()
        env = {
            "PATH": "/usr/bin:/bin", "HOME": str(home), "LANG": "en_US.UTF-8",
            "TERM": "xterm-256color", "COLORTERM": "truecolor",
            "KBORG_CONFIG_DIR": str(home / "kborg"), "K9S_CONFIG_DIR": str(home / "k9s"),
            "XDG_CONFIG_HOME": str(home / "config"), "XDG_DATA_HOME": str(home / "data"),
            "XDG_STATE_HOME": str(home / "state"), "XDG_CACHE_HOME": str(home / "cache"),
            "KUBECONFIG": str(home / "no-kubeconfig"),
        }
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
        self.screen = Screen(WIDTH, HEIGHT)
        self.pending = b""
        self.started = time.monotonic()
        self.proc = _spawn([str(binary)] + ARGS, env, self.slave)

    # ------------------------------------------------------------ terminal

    def drain(self, seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if time.monotonic() - self.started > 120:
                raise RuntimeError("demo recording exceeded 120 s")
            if not select.select([self.master], [], [], 0.02)[0]:
                continue
            try:
                data = os.read(self.master, 1 << 16)
            except OSError:
                return
            if not data:
                return
            # Answer queries like a real terminal (flexoki-dark colors).
            if b"\x1b]11;?" in data:
                os.write(self.master, b"\x1b]11;rgb:1010/0f0f/0f0f\x1b\\")
            if b"\x1b]10;?" in data:
                os.write(self.master, b"\x1b]10;rgb:cece/cdcd/c3c3\x1b\\")
            if b"\x1b[6n" in data:
                os.write(self.master, b"\x1b[1;1R")
            if b"\x1b[c" in data or b"\x1b[0c" in data:
                os.write(self.master, b"\x1b[?62;22c")
            buffer = _FOREIGN.sub(b"", self.pending + data)
            tail = _INCOMPLETE.search(buffer)
            cut = tail.start() if tail and tail.group(0) else len(buffer)
            self.pending = buffer[cut:]
            self.screen.feed(buffer[:cut])

    def wait(self, pattern, timeout=15):
        regex = re.compile(pattern)
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.drain(0.05)
            if regex.search(self.screen.text()):
                return
            if self.proc.poll() is not None:
                break
        raise RuntimeError(f"marker {pattern!r} did not appear; screen:\n{self.screen.text()}")

    def keys(self, value, settle=0.35):
        os.write(self.master, value)
        self.drain(settle)

    def type(self, text, per_key_ms=140):
        """Type visibly, one frame per keystroke."""
        for char in text:
            self.keys(char.encode(), 0.12)
            self.frame(per_key_ms)

    # --------------------------------------------------------------- frames

    def frame(self, duration_ms):
        """Capture the current screen; identical consecutive frames are merged."""
        text = self.screen.text()
        if self.frames and self.frames[-1][2] == text:
            path, duration, _ = self.frames[-1]
            self.frames[-1] = (path, duration + duration_ms, text)
            return
        path = self.frames_dir / f"frame-{len(self.frames):03d}.png"
        self.screen.png(path)
        self.frames.append((path, duration_ms, text))

    def close(self):
        if self.proc.poll() is None:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(self.proc.pid, sig)
                except (ProcessLookupError, PermissionError):
                    pass
                deadline = time.monotonic() + 3
                while self.proc.poll() is None and time.monotonic() < deadline:
                    self.drain(0.1)
                if self.proc.poll() is not None:
                    break
        os.close(self.master)
        os.close(self.slave)
        self.tmp.cleanup()


def record(session):
    s = session
    s.wait(r"api-7d4f9", 30)
    s.wait(r"METRICS [0-9]+ measured", 15)  # startup metrics finished; D needs idle
    s.drain(0.5)
    s.frame(2200)
    # Browse Pods (the demo has two: a healthy one and a crash-looping one).
    s.keys(b"j", 0.25)
    s.frame(1300)
    s.keys(b"k", 0.25)
    s.frame(800)
    # Logs for the selected Pod.
    s.keys(b"l", 0.3)
    s.wait(r"LOGS", 10)
    s.frame(600)
    s.drain(1.0)
    s.frame(1200)
    s.drain(1.0)
    s.frame(1600)
    s.keys(b"\x1b", 0.4)
    s.frame(600)
    # Context help.
    s.keys(b"?", 0.5)
    s.frame(2600)
    s.keys(b"\x1b", 0.4)
    s.frame(500)
    # Deployments by command.
    s.type(":deploy")
    s.frame(500)
    s.keys(b"\r", 0.3)
    s.wait(r"deployments", 10)
    s.drain(0.5)
    s.frame(2200)
    # Dashboard.
    s.keys(b"D", 0.3)
    s.wait(r"CONTEXT OVERVIEW", 10)
    s.drain(1.0)
    s.frame(3200)
    # Quit.
    s.type(":q")
    s.frame(600)
    s.keys(b"\r", 0.5)
    deadline = time.monotonic() + 5
    while s.proc.poll() is None and time.monotonic() < deadline:
        s.drain(0.1)
    if s.proc.returncode != 0:
        raise RuntimeError(f"kborg did not quit cleanly (status {s.proc.returncode})")


def assemble(frames, out, scale):
    from PIL import Image
    images, durations = [], []
    for path, duration, _ in frames:
        image = Image.open(path).convert("RGB")
        if scale != 1:
            image = image.resize((round(image.width * scale), round(image.height * scale)),
                                 Image.Resampling.LANCZOS)
        # Text needs crisp edges: adaptive palette, no dithering.
        images.append(image.quantize(colors=128, method=Image.Quantize.MEDIANCUT,
                                     dither=Image.Dither.NONE))
        durations.append(duration)
    durations[-1] = max(durations[-1], 1500)
    out.parent.mkdir(parents=True, exist_ok=True)
    images[0].save(out, save_all=True, append_images=images[1:], duration=durations,
                   loop=0, optimize=True, disposal=1)
    return sum(durations)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--out", type=Path, default=REPO / "docs/screenshots/demo.gif")
    parser.add_argument("--frames", type=Path, help="keep the PNG frames in this directory")
    parser.add_argument("--scale", type=float, default=1.0, help="resize frames before encoding")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="kborg-demo-frames-") as tmp:
        frames_dir = args.frames.resolve() if args.frames else Path(tmp)
        frames_dir.mkdir(parents=True, exist_ok=True)
        for old in frames_dir.glob("frame-*.png"):
            old.unlink()
        session = Session(binary, frames_dir)
        try:
            record(session)
        finally:
            session.close()
        total = assemble(session.frames, args.out, args.scale)
    size = args.out.stat().st_size
    print(f"{args.out}: {len(session.frames)} frames, {total / 1000:.1f} s, {size / 2**20:.2f} MiB")


if __name__ == "__main__":
    sys.exit(main())
