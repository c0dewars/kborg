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
    def __init__(self, binary, frames_dir, extra_args=()):
        self.frames_dir = frames_dir
        self.caption = ""
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
        self.proc = _spawn([str(binary)] + ARGS + list(extra_args), env, self.slave)

    # ------------------------------------------------------------ terminal

    def drain(self, seconds):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if time.monotonic() - self.started > 300:
                raise RuntimeError("demo recording exceeded 300 s")
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
        if self.frames and self.frames[-1][2] == text and self.frames[-1][3] == self.caption:
            path, duration, _, caption = self.frames[-1]
            self.frames[-1] = (path, duration + duration_ms, text, caption)
            return
        path = self.frames_dir / f"frame-{len(self.frames):03d}.png"
        self.screen.png(path)
        self.frames.append((path, duration_ms, text, self.caption))

    def soft_wait(self, pattern, timeout=8):
        """Like wait, but carry on (and say so) if the marker never appears."""
        try:
            self.wait(pattern, timeout)
        except RuntimeError:
            print(f"warning: {pattern!r} not seen; continuing", file=sys.stderr)

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


def tour(session):
    """A longer walk through the main features, one caption per feature."""
    s = session

    def step(caption, hold=2400):
        s.caption = caption
        s.drain(0.4)
        s.frame(hold)

    def command(text, marker=None, hold=2400, caption=None):
        if caption:
            s.caption = caption
        s.type(text, 90)
        s.keys(b"\r", 0.4)
        if marker:
            s.soft_wait(marker)
        s.drain(0.6)
        s.frame(hold)

    s.wait(r"api-7d4f9", 30)
    s.soft_wait(r"METRICS [0-9]+ measured", 15)
    step("Browse every resource with k9s keys and commands", 2600)
    s.keys(b"j", 0.25)
    step("Live tables: health, restarts, CPU and memory", 1600)
    s.keys(b"/", 0.3)
    s.caption = "/ filters with regex, !inverse, -f fuzzy or -l labels"
    s.type("worker", 90)
    s.keys(b"\r", 0.4)
    step(s.caption, 2000)
    s.keys(b"\x1b", 0.4)
    s.keys(b"\x1a", 0.4)
    step("Ctrl-Z shows only what needs attention", 2200)
    s.keys(b"\x1a", 0.4)
    s.keys(b"N", 0.2)
    s.keys(b"N", 0.4)
    step("Sort by any column; press again to reverse", 1800)
    s.keys(b"N", 0.3)
    s.keys(b"d", 0.5)
    s.soft_wait(r"OVERVIEW|DESCRIBE|Overview")
    step("d describes, y shows YAML, always redacted", 2200)
    s.keys(b"\x1b", 0.4)
    s.keys(b"l", 0.4)
    s.soft_wait(r"LOGS")
    s.drain(1.2)
    step("l follows logs: search, error and warning filters, time ranges", 2600)
    s.keys(b"\x1b", 0.4)
    command(":lint", r"LINT", 2800, ":lint finds misconfigurations, read-only")
    s.keys(b"\x1b", 0.4)
    command(":metrics", r"METRICS", 600, "Live charts: :metrics then g")
    s.keys(b"g", 0.4)
    s.drain(25.0)  # a few more samples (the demo series steps every refresh)
    step("Live charts: CPU and memory over time, % of request and limit", 3000)
    s.keys(b"\x1b", 0.4)
    command(":deploy", r"deployments", 1200, "Every change is dry-run, reviewed and typed to confirm")
    s.keys(b"s", 0.5)
    s.type("3", 120)
    s.keys(b"\r", 0.6)
    s.soft_wait(r"Type |REVIEW|DEMO")
    step("Every change is dry-run, reviewed and typed to confirm", 3200)
    s.keys(b"\x1b", 0.4)
    s.keys(b"\x1b", 0.4)
    command(":secrets", r"(?i)secret", 1200, ":secrets: masked YAML, reveal one key for 30 s")
    s.keys(b"y", 0.6)
    s.soft_wait(r"hidden")
    step(":secrets: masked YAML, reveal one key for 30 s", 2800)
    s.keys(b"\x1b", 0.4)
    s.keys(b"\x1b", 0.4)
    command(":pods", r"api-7d4f9", 400)
    s.keys(b"D", 0.4)
    s.soft_wait(r"CONTEXT OVERVIEW", 10)
    s.drain(1.0)
    step("Dashboard for up to nine clusters", 2800)
    s.drain(32.0)  # trend charts need samples 15 s apart
    s.keys(b"G", 0.6)
    step("G: trend charts for the focused cluster", 2600)
    s.keys(b"\x1b", 0.4)
    s.keys(b"\x1b", 0.4)
    command(":themes", r"(?i)theme", 900, ":themes previews 20 themes live; ui.yaml styles everything")
    for _ in range(3):
        s.keys(b"j", 0.35)
        s.frame(700)
    s.keys(b"\x1b", 0.4)
    s.keys(b"?", 0.5)
    step("? opens help for the screen you are on", 2400)
    s.keys(b"\x1b", 0.4)
    command(":keymap", r"(?i)keymap|action", 2600, ":keymap: every key, remappable in keymap.yaml")
    s.keys(b"\x1b", 0.4)
    step("KBorg: fast, safe, yours to customize", 2600)
    s.keys(b":", 0.2)
    s.keys(b"q", 0.2)
    s.keys(b"\r", 0.5)
    deadline = time.monotonic() + 5
    while s.proc.poll() is None and time.monotonic() < deadline:
        s.drain(0.1)


def captioned(image, caption):
    """Add a caption bar under a frame."""
    from PIL import Image, ImageDraw, ImageFont
    if not caption:
        return image
    bar = 46
    out = Image.new("RGB", (image.width, image.height + bar), (16, 15, 15))
    out.paste(image, (0, 0))
    draw = ImageDraw.Draw(out)
    font = None
    for candidate in ("/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc",
                      "/System/Library/Fonts/Menlo.ttc", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"):
        try:
            font = ImageFont.truetype(candidate, 24)
            break
        except OSError:
            continue
    font = font or ImageFont.load_default()
    draw.rectangle((0, image.height, image.width, image.height + 3), fill=(255, 122, 26))
    draw.text((18, image.height + 10), caption, fill=(236, 226, 204), font=font)
    return out


def assemble(frames, out, scale):
    from PIL import Image
    images, durations = [], []
    for path, duration, _, caption in frames:
        image = captioned(Image.open(path).convert("RGB"), caption)
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
    parser.add_argument("--tour", action="store_true",
                        help="record the full feature tour (with captions) instead of the short demo")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="kborg-demo-frames-") as tmp:
        frames_dir = args.frames.resolve() if args.frames else Path(tmp)
        frames_dir.mkdir(parents=True, exist_ok=True)
        for old in frames_dir.glob("frame-*.png"):
            old.unlink()
        extra = ["--write", "--allow-secret-reveal"] if args.tour else []
        session = Session(binary, frames_dir, extra)
        try:
            (tour if args.tour else record)(session)
        finally:
            session.close()
        total = assemble(session.frames, args.out, args.scale)
    size = args.out.stat().st_size
    print(f"{args.out}: {len(session.frames)} frames, {total / 1000:.1f} s, {size / 2**20:.2f} MiB")


if __name__ == "__main__":
    sys.exit(main())
