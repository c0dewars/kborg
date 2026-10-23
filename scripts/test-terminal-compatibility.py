#!/usr/bin/env python3
"""Bounded Mac PTY + isolated real tmux compatibility checks, demo data only.

TERM profiles test emitted bytes and input behavior, not native emulator/font
rendering. Native application screenshots are recorded separately. No SSH
server or remote machine is implied by a screen/tmux TERM value.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import runpy
import select
import shlex
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time

helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, spawn = helpers["Screen"], helpers["_spawn"]


PROFILES = [
    ("truecolor", "xterm-256color", "truecolor", "en_US.UTF-8", None, "always", True, "rgb"),
    ("256-color", "xterm-256color", "", "en_US.UTF-8", None, "always", False, "indexed"),
    ("basic-16", "xterm", "", "en_US.UTF-8", None, "always", False, "basic"),
    ("screen-256", "screen-256color", "", "en_US.UTF-8", None, "always", False, "indexed"),
    ("tmux-256", "tmux-256color", "", "en_US.UTF-8", None, "always", False, "indexed"),
    ("ascii-c-locale", "xterm-256color", "truecolor", "C", None, "always", False, "rgb"),
    ("no-color-env", "xterm-256color", "truecolor", "en_US.UTF-8", "1", "auto", False, "none"),
    ("color-never", "xterm-256color", "truecolor", "en_US.UTF-8", None, "never", False, "none"),
    ("explicit-color-overrides-env", "xterm-256color", "truecolor", "en_US.UTF-8", "1", "always", False, "rgb"),
    ("term-unset", None, "", "C", None, "always", False, "basic"),
]


def environment(config, term, colorterm, locale, no_color):
    env = os.environ.copy()
    for key in ("TERM", "COLORTERM", "NO_COLOR", "LC_CTYPE"):
        env.pop(key, None)
    env.update(KBORG_CONFIG_DIR=str(config), LC_ALL=locale, LANG=locale)
    if term is not None:
        env["TERM"] = term
    if colorterm:
        env["COLORTERM"] = colorterm
    if no_color is not None:
        env["NO_COLOR"] = no_color
    return env


def profile(binary, output_dir, settings):
    name, term, colorterm, locale, no_color, color, mouse, depth = settings
    with tempfile.TemporaryDirectory(prefix="kborg-term-") as temporary:
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 34, 120, 0, 0))
        env = environment(Path(temporary).resolve(), term, colorterm, locale, no_color)
        argv = [str(binary), "--demo", "--no-splash", "--theme", "rose-pine"]
        if color != "auto":
            argv.append(f"--color={color}")
        if not mouse:
            argv.append("--no-mouse")
        process = spawn(argv, env, slave)
        screen, raw = Screen(120, 34), bytearray()
        checks, sizes, screenshots = [], [], []

        def drain(seconds=.12):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], min(.04, max(0, deadline-time.monotonic())))[0]:
                    try:
                        value = os.read(master, 65536)
                    except OSError:
                        break
                    if not value:
                        break
                    raw.extend(value)
                    assert len(raw) < 32 * 1024 * 1024, "bounded terminal output exceeded"
                    screen.feed(value)

        def keys(value=b"", expected=None):
            if value:
                os.write(master, value)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                drain()
                text = screen.text()
                if expected is None or (expected(text) if callable(expected) else expected in text):
                    return
                if process.poll() is not None:
                    break
            raise AssertionError(f"{name}: missing {expected!r}\n{screen.text()}")

        def resize(width, height):
            nonlocal screen
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            screen = Screen(width, height)
            os.killpg(process.pid, signal.SIGWINCH)
            drain(.25)
            sizes.append([width, height])

        def capture(label):
            path = output_dir / f"{name}-{label}.png"
            screen.png(path)
            screenshots.append(str(path.resolve()))

        try:
            keys(expected=lambda text: "api-7d4f9" in text and "LOADING" not in text)
            assert ":pods" not in screen.text() and ":ctx" not in screen.text()
            checks.append("quiet resource header")
            if name in ("truecolor", "ascii-c-locale", "no-color-env"):
                capture("resources")
            if mouse:
                keys(b"\x1b[<0;118;1M\x1b[<0;118;1m", "HELP /")
                checks.append("SGR mouse help hit target")
            else:
                keys(b"?", "HELP /")
            keys(b"\x1b", expected=lambda text: "HELP /" not in text)
            keys(b":nodes\x1b", expected=lambda text: "Command >" not in text and "api-7d4f9" in text)
            checks.append("help and cancelled command return to resources")
            keys(b"D", "CONTEXT OVERVIEW")
            keys(expected="observed")
            for width, height in [(80, 24), (48, 16), (28, 10), (20, 8), (120, 34)]:
                resize(width, height)
                keys(expected="needs 28x10" if width < 28 else "KBORG")
                if width == 20:
                    assert "needs 28x10" in screen.text()
            keys(expected="CONTEXT OVERVIEW")
            checks.append("dashboard survives minimum, undersize and recovery resizes")
            keys(b"s", "Save selection / name")
            keys(b"compat-workspace\r", "saved locally")
            keys(b"B", "Saved selections")
            keys(expected="d remove")
            drain(1.2)
            assert "d remove" in screen.text(), "saved-picker hint lost during background update"
            if name == "truecolor":
                capture("saved-picker")
            keys(b"d", "Delete selection / type exact name")
            keys(b"\x1b", "CONTEXT OVERVIEW")
            keys(b"B", "compat-workspace")
            keys(b"\x1b", "CONTEXT OVERVIEW")
            checks.append("saved picker keeps delete hint; cancellation preserves selection")
            keys(b"\x1b", expected=lambda text: "CONTEXT OVERVIEW" not in text)
            # Half the profiles exercise Ctrl-C; the others use the supported quit command.
            keys(b"\x03" if mouse or name in ("basic-16", "no-color-env") else b":q\r")
            deadline = time.monotonic() + 8
            while process.poll() is None and time.monotonic() < deadline:
                drain()
            assert process.poll() == 0, f"{name}: nonzero exit {process.poll()}"
            assert termios.tcgetattr(master) == original, "TTY settings not restored"
            for sequence in (b"\x1b[?1049l", b"\x1b[?25h", b"\x1b[?2004l", b"\x1b[?2026l", b"\x1b[?7h"):
                assert sequence in raw, f"missing exit mode restoration {sequence!r}"
            assert (b"\x1b[?1000h" in raw) == mouse
            codes = re.findall(rb"\x1b\[([0-9;]+)m", bytes(raw))
            rgb = any(re.search(rb"(?:^|;)(?:38|48);2;", code) for code in codes)
            indexed = any(re.search(rb"(?:^|;)(?:38|48);5;", code) for code in codes)
            basic = any(int(part) in list(range(30, 38)) + list(range(40, 48)) + list(range(90, 98)) + list(range(100, 108)) for code in codes for part in code.split(b";"))
            if depth == "rgb":
                assert rgb
            elif depth == "indexed":
                assert indexed and not rgb
            elif depth == "basic":
                assert basic and not rgb and not indexed
            else:
                assert all(set(code.split(b";")).issubset({b"0", b"39", b"49"}) for code in codes)
            if locale == "C":
                assert raw.decode("utf-8").isascii(), "ASCII locale emitted non-ASCII glyphs"
            else:
                assert "╭".encode() in raw and "│".encode() in raw
            checks.extend([f"{depth} color output", "locale border fallback", "normal exit restores modes and termios"])
            return {"name": name, "passed": True, "term": term, "color_depth": depth,
                    "locale": locale, "mouse": mouse, "resize_sizes": sizes,
                    "checks": checks, "screenshots": screenshots}
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)


def dumb_terminal(binary):
    with tempfile.TemporaryDirectory(prefix="kborg-dumb-") as temporary:
        env = environment(Path(temporary).resolve(), "dumb", "", "C", None)
        result = subprocess.run([str(binary), "--demo", "--no-splash"], env=env, capture_output=True, timeout=8)
        assert result.returncode != 0 and b"TERM=dumb" in result.stderr
        assert b"\x1b[?1049h" not in result.stdout
        result = subprocess.run([str(binary), "--demo", "list", "pods", "--json"], env=env, capture_output=True, timeout=8)
        assert result.returncode == 0 and isinstance(json.loads(result.stdout), (dict, list))
        return {"passed": True, "interactive_refused_before_terminal_mutation": True, "cli_json_works": True}


def tmux_session(binary):
    executable = shutil.which("tmux")
    if executable is None:
        return {"tested": False, "reason": "tmux unavailable"}
    with tempfile.TemporaryDirectory(prefix="kborg-tmux-") as temporary:
        root = Path(temporary).resolve()
        socket = root / "isolated.sock"
        prefix = [executable, "-S", str(socket), "-f", "/dev/null"]
        env = environment(root / "preferences", "xterm-256color", "", "en_US.UTF-8", None)

        def run(*args):
            return subprocess.run(prefix + list(args), env=env, check=True, text=True, capture_output=True, timeout=10).stdout

        def wait(needle):
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                value = run("capture-pane", "-p", "-t", "compat:0.0")
                if needle in value:
                    return value
                time.sleep(.1)
            raise AssertionError(f"tmux pane did not show {needle!r}\n{value}")

        # This shell belongs only to this private server; the literal command is quoted.
        command = shlex.join([str(binary), "--demo", "--no-splash", "--no-mouse", "--theme", "rose-pine"])
        try:
            run("new-session", "-d", "-s", "compat", "-x", "120", "-y", "34", "/bin/sh")
            run("send-keys", "-t", "compat:0.0", "-l", command)
            run("send-keys", "-t", "compat:0.0", "Enter")
            wait("SAMPLE")
            wait("api-7d4f9")
            # Wait for the optional initialization metrics request too.
            deadline = time.monotonic() + 10
            while "LOADING" in run("capture-pane", "-p", "-t", "compat:0.0"):
                assert time.monotonic() < deadline, "tmux initialization did not settle"
                time.sleep(.1)
            run("send-keys", "-t", "compat:0.0", "-l", "D")
            wait("CONTEXT OVERVIEW")
            run("resize-window", "-t", "compat:0", "-x", "48", "-y", "16")
            wait("CONTEXT OVERVIEW")
            run("send-keys", "-t", "compat:0.0", "-l", "?")
            wait("DASHBOARD KEYS")
            run("send-keys", "-t", "compat:0.0", "Escape", "Escape")
            run("send-keys", "-t", "compat:0.0", "C-c")
            run("send-keys", "-t", "compat:0.0", "-l", "printf 'KBORG_TMUX_SHELL_RESTORED\\n'")
            run("send-keys", "-t", "compat:0.0", "Enter")
            wait("KBORG_TMUX_SHELL_RESTORED")
            flags = run("display-message", "-p", "-t", "compat:0.0", "#{alternate_on}|#{cursor_flag}|#{mouse_any_flag}|#{pane_dead}").strip()
            assert flags == "0|1|0|0", f"tmux pane modes not restored: {flags}"
            return {"tested": True, "passed": True, "version": subprocess.check_output([executable, "-V"], text=True).strip(),
                    "checks": ["private real tmux server", "resource and dashboard navigation", "resize and contextual help", "Ctrl-C returns to shell", "alternate screen/cursor/mouse restored"],
                    "native_emulator_attached": False, "socket_removed": True}
        finally:
            subprocess.run(prefix + ["kill-server"], env=env, capture_output=True, timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--screenshots", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    args.screenshots.mkdir(parents=True, exist_ok=True)
    profiles = []
    for settings in PROFILES:
        profiles.append(profile(binary, args.screenshots, settings))
        print(f"PASS {settings[0]}", flush=True)
    report = {"passed": True, "binary_sha256": digest, "evidence_kind": "PTY byte/input matrix; real isolated tmux server",
              "profiles": profiles, "term_dumb": dumb_terminal(binary), "tmux": tmux_session(binary),
              "limits": ["PTY screenshots use the shared cell renderer; they are not native emulator screenshots", "No remote SSH server tested", "No screen-reader or font accessibility certification", "Forced SIGKILL cannot execute application cleanup"]}
    assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest, "binary changed during validation"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"passed": True, "profiles": len(profiles), "tmux": report["tmux"]}, indent=2))


if __name__ == "__main__":
    main()
