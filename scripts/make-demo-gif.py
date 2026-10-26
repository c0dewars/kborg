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
import json
import hashlib
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
ARGS = ["--demo", "--no-splash", "--theme", "catppuccin-mocha",
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
    def __init__(self, binary, frames_dir, extra_args=(), copilot_account=None, connection_tour=False):
        self.frames_dir = frames_dir
        self.copilot_account = copilot_account
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
        if connection_tour:
            config = home / "no-kubeconfig"
            config.write_text(json.dumps({
                "apiVersion": "v1", "kind": "Config", "current-context": "company-prod",
                "clusters": [{"name": "company", "cluster": {"server": "https://127.0.0.1:9"}}],
                "users": [{"name": "demo-user", "user": {"token": "SYNTHETIC-NOT-A-CREDENTIAL"}}],
                "contexts": [
                    {"name": "company-prod", "context": {"cluster": "company", "user": "demo-user", "namespace": "payments"}},
                    {"name": "company-dev", "context": {"cluster": "company", "user": "demo-user", "namespace": "development"}},
                ],
            }))
            config.chmod(0o600)
        if copilot_account:
            # The pinned gh lookup reads the selected stored account from the
            # real HOME; KBorg's own config and state stay in the temp dirs.
            env["HOME"] = os.environ["HOME"]
        self.master, self.slave = pty.openpty()
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, 0, 0))
        self.screen = Screen(WIDTH, HEIGHT)
        self.pending = b""
        self.started = time.monotonic()
        self.proc = _spawn([str(binary)] + [arg for arg in ARGS if not (connection_tour and arg == "--demo")] + list(extra_args), env, self.slave)

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
            # Answer queries like a real terminal (Catppuccin Mocha colors).
            if b"\x1b]11;?" in data:
                os.write(self.master, b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\")
            if b"\x1b]10;?" in data:
                os.write(self.master, b"\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\")
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
        """Require the scene marker; never publish a silently incomplete tour."""
        try:
            self.wait(pattern, timeout)
        except RuntimeError as error:
            raise RuntimeError(f"Refusing to publish a tour with a missing scene: {pattern!r}") from error

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


def connection_tour(s):
    s.wait(r"Connect to Kubernetes", 15)
    s.caption = "Use the kubeconfig already prepared by your company CLI"
    s.frame(3000)
    s.caption = "Context, cluster and namespace are named before connecting"
    s.frame(2500)
    s.keys(b"n", 0.3)
    s.wait(r"Choose an existing context", 8)
    s.caption = "No opens a searchable list; your kubeconfig stays unchanged"
    s.type("dev", 200)
    s.frame(2500)
    s.keys(b"\r", 0.3)
    s.wait(r"Context: company-dev", 8)
    s.caption = "Enter / Y connects; Esc exits without running a login helper"
    s.frame(3500)
    # Do not connect, contact an API, or run a helper in a public recording.
    s.keys(b"\x1b", 0.3)
    s.proc.wait(timeout=5)
    if s.proc.returncode != 0:
        raise RuntimeError("connection preview did not exit cleanly")


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


def ask_copilot(s):
    """:ask about the Pods screen, through the real contained Copilot runtime."""
    s.caption = ":ask sends Copilot the screen you see, after you review it"
    s.keys(b":", 0.3)
    s.type("ask why is worker crash looping?", 70)
    s.keys(b"\r", 0.4)
    s.wait(r"REVIEW SCREEN TRANSFER", 60)
    s.drain(0.4)
    s.frame(3200)
    for _ in range(30):
        s.keys(b"\x1b[6~", 0.12)
    s.caption = "Read every line, then type the exact host and account"
    s.type(f"SEND SCREEN TO github.com AS {s.copilot_account}", 25)
    s.frame(1400)
    s.keys(b"\r", 0.6)
    s.frame(1200)
    s.wait(r"COPILOT ADVICE|STOPPED|NOT STARTED", 150)
    s.caption = "Copilot answers from the screen; advice is never executed"
    s.drain(0.6)
    s.frame(5200)
    s.keys(b"\x1b", 0.4)


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
    if s.copilot_account:
        ask_copilot(s)
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
    command(":deploy", r"deployments", 1200, "Review before changing; server dry-run where supported")
    s.keys(b"s", 0.5)
    s.type("3", 120)
    s.keys(b"\r", 0.6)
    s.soft_wait(r"Type |REVIEW|DEMO")
    step("Review before changing; server dry-run where supported", 3200)
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
    command(":pods", r"api-7d4f9", 400)
    command(":xray po", r"(?i)xray|relationship", 2400, "Relationships: :xray links workloads, Pods and Services")
    s.keys(b"\x1b", 0.4)
    command(":subjects", r"RBAC SUBJECTS", 2600, "RBAC subjects and bindings: inspect who can do what")
    s.keys(b"\x1b", 0.4)
    command(":helm", r"(?i)helm", 2800, "Helm releases, history and reviewed lifecycle actions")
    s.keys(b"\x1b", 0.4)
    command(":nodes", r"(?i)nodes", 1200, "Node health, metrics and maintenance workflows")
    s.keys(b"?", 0.4)
    step("Node actions: review cordon, drain or shell before execution", 2600)
    s.keys(b"\x1b", 0.4)
    command(":pods", r"api-7d4f9", 500)
    s.keys(b"\x10", 0.4)
    s.type("shell", 90)
    step("Ctrl-P discovers actions: shell, forward, copy, debug and more", 2800)
    s.keys(b"\x1b", 0.4)
    command(":copilot setup", r"COPILOT SETUP", 3200, "Copilot setup explains login, prerequisites and platform availability")
    s.keys(b"\x1b", 0.4)
    command(":themes", r"(?i)theme", 900, ":themes previews palettes; this tour stays in Catppuccin Mocha")
    # Keep the requested Mocha palette throughout the recording.
    s.frame(2400)
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
    out = Image.new("RGB", (image.width, image.height + bar), (30, 30, 46))
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
    draw.rectangle((0, image.height, image.width, image.height + 3), fill=(203, 166, 247))
    caption = "DEMO | " + caption
    while draw.textlength(caption, font=font) > image.width - 36 and getattr(font, "size", 12) > 14:
        font = font.font_variant(size=font.size - 1)
    draw.text((18, image.height + 10), caption, fill=(205, 214, 244), font=font)
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
    parser.add_argument("--copilot-policy", type=Path,
                        help="tour only: include a live :ask step with this policy (sends the synthetic demo screen)")
    parser.add_argument("--tour", action="store_true",
                        help="record the full feature tour (with captions) instead of the short demo")
    parser.add_argument("--connection-tour", action="store_true",
                        help="record the real startup prompt with a synthetic local kubeconfig; never connects")
    args = parser.parse_args()
    if args.tour and args.connection_tour:
        parser.error("choose either --tour or --connection-tour")
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="kborg-demo-frames-") as tmp:
        frames_dir = args.frames.resolve() if args.frames else Path(tmp)
        frames_dir.mkdir(parents=True, exist_ok=True)
        for old in frames_dir.glob("frame-*.png"):
            old.unlink()
        extra = ["--write", "--allow-secret-reveal"] if args.tour else []
        account = None
        if args.tour and args.copilot_policy:
            policy = args.copilot_policy.resolve(strict=True)
            account = json.loads(policy.read_text())["gh_cli"]["account"]
            extra += ["--copilot-policy", str(policy), "--allow-copilot-screen"]
        session = Session(binary, frames_dir, extra, account, args.connection_tour)
        try:
            (connection_tour if args.connection_tour else tour if args.tour else record)(session)
        finally:
            session.close()
        total = assemble(session.frames, args.out, args.scale)
    size = args.out.stat().st_size
    manifest = {
        "schema": 1, "theme": "catppuccin-mocha", "mode": "synthetic local kubeconfig preview (no connection)" if args.connection_tour else "offline demo",
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "frames": len(session.frames), "duration_seconds": total / 1000,
        "gif_bytes": size, "gif_sha256": hashlib.sha256(args.out.read_bytes()).hexdigest(),
        "captions": list(dict.fromkeys(frame[3] for frame in session.frames)),
        "scope": "Synthetic feature-family tour. Operation previews are not live acceptance; no cluster, auth helper, scanner, shell or model request is used without --copilot-policy.",
    }
    args.out.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(f"{args.out}: {len(session.frames)} frames, {total / 1000:.1f} s, {size / 2**20:.2f} MiB")


if __name__ == "__main__":
    sys.exit(main())
