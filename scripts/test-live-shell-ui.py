#!/usr/bin/env python3
"""Full interactive shell (F1) acceptance through the actual TUI on Docker Desktop.

Creates only the owned namespace `kborg-f1` with one BusyBox Pod, gives the UI a
short-lived namespace-limited service-account credential, and drives `s` in a
real PTY with a small VT emulator that answers cursor-position queries:

* the review names the exact Pod, container, UID and argv before connecting;
* BusyBox vi opens, edits with arrow keys and saves a file (checked by a second
  KBorg session and by kubectl exec);
* top draws full screen and quits with q;
* a window resize reaches the remote TTY (`stty size`);
* a hostile program leaves mouse/paste/keypad/cursor modes, the alternate
  screen and an unterminated OSC behind, Ctrl-] disconnects, and KBorg's
  reset sequence clears all of it before the redraw;
* exit from the remote shell also returns cleanly, and quitting KBorg restores
  the original terminal mode.

Captures contain screen text only, never the credential. Cleanup checks the
namespace UID and run label before deleting it.
"""
import argparse
import codecs
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

NAMESPACE = "kborg-f1"
POD = "editor"
CONTAINER = "shell"
IMAGE = "busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"
RESET_START = b"\x18\x1b\\\x1b[?2026l"
RESET_END = b"\x1b[23;0t"


class Vt:
    """Enough of a VT/xterm emulator for vi, top, a shell and KBorg itself."""

    def __init__(self, width, height, reply):
        self.reply = reply
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.pending = ""
        self.modes = {7, 25}
        self.keypad = False
        self.title = ""
        self.resize(width, height)
        self.main = self.grid()
        self.alt = self.grid()
        self.cells = self.main
        self.x = self.y = 0
        self.saved = (0, 0)
        self.wrap_next = False

    def grid(self):
        return [[" "] * self.width for _ in range(self.height)]

    def resize(self, width, height):
        old = getattr(self, "cells", None)
        self.width, self.height = width, height
        self.top, self.bottom = 0, height - 1
        if old is not None:
            fit = lambda g: [(row + [" "] * width)[:width] for row in (g + self.grid())[:height]]
            alternate = self.cells is self.alt
            self.main, self.alt = fit(self.main), fit(self.alt)
            self.cells = self.alt if alternate else self.main
            self.x, self.y = min(self.x, width - 1), min(self.y, height - 1)

    @property
    def alternate(self):
        return self.cells is self.alt

    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.cells)

    def scroll_up(self, count=1):
        for _ in range(count):
            del self.cells[self.top]
            self.cells.insert(self.bottom, [" "] * self.width)

    def scroll_down(self, count=1):
        for _ in range(count):
            del self.cells[self.bottom]
            self.cells.insert(self.top, [" "] * self.width)

    def linefeed(self):
        if self.y == self.bottom:
            self.scroll_up()
        elif self.y < self.height - 1:
            self.y += 1

    def put(self, char):
        if self.wrap_next:
            self.x, self.wrap_next = 0, False
            self.linefeed()
        self.cells[self.y][self.x] = char
        if self.x == self.width - 1:
            self.wrap_next = 7 in self.modes
        else:
            self.x += 1

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        text, i = self.pending, 0
        while i < len(text):
            char = text[i]
            if char == "\x1b":
                end = self.escape(text, i)
                if end is None:
                    break
                i = end
                continue
            i += 1
            if char == "\r":
                self.x, self.wrap_next = 0, False
            elif char in "\n\x0b\x0c":
                self.wrap_next = False
                self.linefeed()
            elif char == "\b":
                self.x, self.wrap_next = max(0, self.x - 1), False
            elif char == "\t":
                self.x = min(self.width - 1, (self.x // 8 + 1) * 8)
            elif char >= " " and char != "\x7f":
                self.put(char)
        self.pending = text[i:]

    def escape(self, text, i):
        if i + 1 >= len(text):
            return None
        kind = text[i + 1]
        if kind == "[":
            j = i + 2
            while j < len(text) and not ("\x40" <= text[j] <= "\x7e"):
                if text[j] in "\x18\x1a":
                    return j + 1
                j += 1
            if j >= len(text):
                return None
            self.csi(text[i + 2:j], text[j])
            return j + 1
        if kind in "]P^_X":
            j = i + 2
            while j < len(text):
                if text[j] == "\x07":
                    end = j + 1
                    break
                if text[j] in "\x18\x1a":
                    return j + 1
                if text[j] == "\x1b" and j + 1 < len(text) and text[j + 1] == "\\":
                    end = j + 2
                    break
                j += 1
            else:
                return None
            body = text[i + 2:j]
            if kind == "]" and body[:2] in ("0;", "2;"):
                self.title = body[2:]
            return end
        if kind in "()*+-./":
            return i + 3 if i + 2 < len(text) else None
        if kind == "7":
            self.saved = (self.x, self.y)
        elif kind == "8":
            self.x, self.y = self.saved
        elif kind == "=":
            self.keypad = True
        elif kind == ">":
            self.keypad = False
        elif kind == "M":
            if self.y == self.top:
                self.scroll_down()
            else:
                self.y = max(0, self.y - 1)
        elif kind == "D":
            self.linefeed()
        elif kind == "E":
            self.x = 0
            self.linefeed()
        elif kind == "c":
            self.__init__(self.width, self.height, self.reply)
        return i + 2

    def csi(self, body, final):
        private = body[:1] if body[:1] in "?<=>" else ""
        raw = body[len(private):].rstrip(" !\"$'")
        intermediate = body[len(private) + len(raw):]
        try:
            values = [int(v) if v else 0 for v in raw.replace(":", ";").split(";")] if raw else []
        except ValueError:
            values = []
        n = values[0] if values and values[0] else 1
        self.wrap_next = False
        if private == "?" and final in "hl":
            for mode in values:
                self.private_mode(mode, final == "h")
            return
        if private or intermediate:
            return  # kitty keyboard, modifyOtherKeys, cursor shape, DECSTR...
        if final in "Hf":
            self.y = min(self.height - 1, max(0, (values[0] if values and values[0] else 1) - 1))
            self.x = min(self.width - 1, max(0, (values[1] if len(values) > 1 and values[1] else 1) - 1))
        elif final == "A":
            self.y = max(0, self.y - n)
        elif final == "B":
            self.y = min(self.height - 1, self.y + n)
        elif final == "C":
            self.x = min(self.width - 1, self.x + n)
        elif final == "D":
            self.x = max(0, self.x - n)
        elif final == "G":
            self.x = min(self.width - 1, n - 1)
        elif final == "d":
            self.y = min(self.height - 1, n - 1)
        elif final == "J":
            mode = values[0] if values else 0
            rows = range(self.height) if mode in (2, 3) else range(self.y + 1, self.height) if mode == 0 else range(0, self.y)
            for row in rows:
                self.cells[row] = [" "] * self.width
            if mode == 0:
                self.cells[self.y][self.x:] = [" "] * (self.width - self.x)
            elif mode == 1:
                self.cells[self.y][:self.x + 1] = [" "] * (self.x + 1)
        elif final == "K":
            mode = values[0] if values else 0
            row = self.cells[self.y]
            if mode == 0:
                row[self.x:] = [" "] * (self.width - self.x)
            elif mode == 1:
                row[:self.x + 1] = [" "] * (self.x + 1)
            else:
                self.cells[self.y] = [" "] * self.width
        elif final == "X":
            self.cells[self.y][self.x:self.x + n] = [" "] * min(n, self.width - self.x)
        elif final == "P":
            row = self.cells[self.y]
            del row[self.x:self.x + n]
            row.extend([" "] * (self.width - len(row)))
        elif final == "@":
            row = self.cells[self.y]
            row[self.x:self.x] = [" "] * n
            del row[self.width:]
        elif final in "LM" and self.top <= self.y <= self.bottom:
            saved_top, self.top = self.top, self.y
            (self.scroll_down if final == "L" else self.scroll_up)(n)
            self.top = saved_top
        elif final == "S":
            self.scroll_up(n)
        elif final == "T":
            self.scroll_down(n)
        elif final == "r":
            top = (values[0] if values and values[0] else 1) - 1
            bottom = (values[1] if len(values) > 1 and values[1] else self.height) - 1
            if top < bottom < self.height:
                self.top, self.bottom = top, bottom
            self.x = self.y = 0
        elif final == "n" and values[:1] == [6]:
            self.reply(f"\x1b[{self.y + 1};{self.x + 1}R".encode())
        elif final == "c":
            self.reply(b"\x1b[?1;2c")

    def private_mode(self, mode, enabled):
        if mode in (47, 1047, 1049):
            if enabled and not self.alternate:
                if mode == 1049:
                    self.saved = (self.x, self.y)
                self.alt = self.grid()
                self.cells = self.alt
            elif not enabled and self.alternate:
                self.cells = self.main
                if mode == 1049:
                    self.x, self.y = self.saved
        elif enabled:
            self.modes.add(mode)
        else:
            self.modes.discard(mode)

    def png(self, path):
        from PIL import Image, ImageDraw, ImageFont
        font_path = "/System/Library/Fonts/Menlo.ttc"
        font = ImageFont.truetype(font_path, 16) if Path(font_path).exists() else ImageFont.load_default()
        cell_width, cell_height = int(font.getlength("M") + .5), 22
        picture = Image.new("RGB", (self.width * cell_width + 24, self.height * cell_height + 24), (24, 24, 37))
        draw = ImageDraw.Draw(picture)
        for y, row in enumerate(self.cells):
            draw.text((12, 12 + y * cell_height), "".join(row), font=font, fill=(205, 214, 244))
        path.parent.mkdir(parents=True, exist_ok=True)
        picture.save(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--kubeconfig", type=Path, default=Path.home() / ".kube" / "config")
    parser.add_argument("--context", default="docker-desktop", choices=("docker-desktop",))
    parser.add_argument("--screenshots", required=True, type=Path)
    args = parser.parse_args()
    binary, config = (p.resolve(strict=True) for p in (args.binary, args.kubeconfig))
    screenshots = args.screenshots.resolve()
    screenshots.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex[:12]
    kube = ["kubectl", "--kubeconfig", str(config), "--context", args.context]

    def kubectl(*arguments, value=None):
        result = subprocess.run(kube + list(arguments), input=json.dumps(value) if value else None,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=150)
        if result.returncode:
            raise RuntimeError("Owned F1 fixture kubectl request failed: " + arguments[0])
        return result.stdout

    created_uid = None
    passed = False
    checks, captures = [], []
    started = time.monotonic()
    try:
        cluster_config = json.loads(kubectl("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
        cluster = cluster_config["clusters"][0]["cluster"]
        if not cluster["server"].startswith("https://") or cluster.get("insecure-skip-tls-verify"):
            raise RuntimeError("Local test requires TLS verification")
        ns = json.loads(kubectl("create", "-f", "-", "-o", "json", value={"apiVersion": "v1", "kind": "Namespace", "metadata": {
            "name": NAMESPACE, "labels": {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}}}))
        created_uid = ns["metadata"]["uid"]
        kubectl("create", "-f", "-", value={"apiVersion": "v1", "kind": "List", "items": [
            {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": NAMESPACE, "name": "operator"}, "automountServiceAccountToken": False},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": NAMESPACE, "name": "operator"}, "rules": [
                {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": [""], "resources": ["pods/exec"], "verbs": ["get", "create"]}]},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": NAMESPACE, "name": "operator"},
             "subjects": [{"kind": "ServiceAccount", "name": "operator", "namespace": NAMESPACE}],
             "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "operator"}},
            {"apiVersion": "v1", "kind": "Pod", "metadata": {"namespace": NAMESPACE, "name": POD}, "spec": {
                "automountServiceAccountToken": False, "terminationGracePeriodSeconds": 1,
                "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
                "volumes": [{"name": "tmp", "emptyDir": {"medium": "Memory", "sizeLimit": "1Mi"}}],
                "containers": [{"name": CONTAINER, "image": IMAGE, "imagePullPolicy": "IfNotPresent",
                    "command": ["sleep", "3600"], "env": [{"name": "HOME", "value": "/tmp"}],
                    "volumeMounts": [{"name": "tmp", "mountPath": "/tmp"}],
                    "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                    "resources": {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "200m", "memory": "32Mi"}}}]}},
        ]})
        kubectl("-n", NAMESPACE, "wait", "--for=condition=Ready", "pod/" + POD, "--timeout=120s")
        pod_uid = json.loads(kubectl("-n", NAMESPACE, "get", "pod", POD, "-o", "json"))["metadata"]["uid"]
        token = kubectl("-n", NAMESPACE, "create", "token", "operator", "--duration=10m").strip()
        with tempfile.TemporaryDirectory(prefix="kborg-f1-ui-") as temporary:
            temporary = Path(temporary).resolve()
            scoped = temporary / "operator.kubeconfig"
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": args.context,
                    "clusters": [{"name": "local", "cluster": cluster}],
                    "users": [{"name": "operator", "user": {"token": token}}],
                    "contexts": [{"name": args.context, "context": {"cluster": "local", "user": "operator", "namespace": NAMESPACE}}]}, stream)
            token = cluster_config = None
            environment = os.environ.copy()
            environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(temporary / "app"))
            master, slave = pty.openpty()
            width, height = 150, 46
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            original = termios.tcgetattr(slave)
            screen = Vt(width, height, lambda data: os.write(master, data))
            stream_log = bytearray()

            def setup():
                os.setsid()
                fcntl.ioctl(0, termios.TIOCSCTTY, 0)

            process = subprocess.Popen([str(binary), "--kubeconfig", str(scoped), "--context", args.context, "--namespace", NAMESPACE,
                "--write", "--allow-exec", "--session-seconds", "600", "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"],
                stdin=slave, stdout=slave, stderr=slave, env=environment, preexec_fn=setup)

            def drain(seconds=.1):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    if select.select([master], [], [], min(.05, max(0, until - time.monotonic())))[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            return
                        if not data:
                            return
                        stream_log.extend(data)
                        screen.feed(data)

            def wait(expected, timeout=25, absent=None):
                until = time.monotonic() + timeout
                while time.monotonic() < until:
                    drain()
                    text = screen.text()
                    if (expected(text) if callable(expected) else expected in text) and not (absent and absent in text):
                        drain(.3)
                        return text
                    if process.poll() is not None:
                        break
                raise RuntimeError(f"Shell UI did not reach expected state: {expected}\n{screen.text()}")

            def keys(value, expected=None, timeout=25):
                os.write(master, value)
                return wait(expected, timeout) if expected else drain(.3)

            def capture(name):
                path = screenshots / (name + ".png")
                try:
                    screen.png(path)
                except ImportError:
                    path = screenshots / (name + ".txt")
                    path.write_text(screen.text() + "\n")
                captures.append({"name": name, "path": str(path)})

            def prompt_ready():
                lines = [line for line in screen.text().splitlines() if line.strip()]
                return bool(lines) and lines[-1].rstrip().endswith("$")

            def open_session(confirm_review):
                wait(POD)
                keys(b"s", "POD SHELL / choose ordinary container")
                keys(b"\r", "REVIEW POD SHELL")
                text = screen.text()
                if confirm_review:
                    flat = " ".join(text.replace("\u2502", " ").split())
                    for part in (f"target: {NAMESPACE}/{POD}", f"UID: {pod_uid}", f"container: {CONTAINER}",
                                 '["/bin/sh","-i"]', "pass through unfiltered", "Ctrl-] disconnects", "16 MiB received output"):
                        if part not in flat:
                            raise RuntimeError("Session review is missing " + part + "\n" + text)
                    capture("f1-review")
                    checks.append("review shows exact Pod, container, UID, argv, passthrough trust note and limits")
                keys(f"{NAMESPACE}/{POD}".encode())
                mark = len(stream_log)
                keys(b"\r", f"KBorg session: {NAMESPACE}/{POD} ({CONTAINER}). Ctrl-] returns to KBorg.")
                wait(lambda _: prompt_ready())
                return mark

            try:
                wait(POD)
                open_session(True)
                checks.append("approved session leaves KBorg's alternate screen and shows the banner then the remote prompt")

                # vi: open, insert with arrow keys, save.
                keys(b"vi /tmp/f1.txt\r", lambda t: screen.alternate and "~" in t)
                capture("f1-vi-open")
                keys(b"i")
                keys(b"world")
                for _ in range(5):
                    keys(b"\x1b[D")
                keys(b"hello ", "hello world")
                capture("f1-vi-edit")
                keys(b"\x1b")
                drain(.5)
                keys(b":wq\r", lambda t: not screen.alternate and prompt_ready())
                saved = kubectl("-n", NAMESPACE, "exec", POD, "-c", CONTAINER, "--", "cat", "/tmp/f1.txt")
                if saved != "hello world\n":
                    raise RuntimeError("vi did not save the edited file: " + repr(saved))
                checks.append("BusyBox vi opened full screen, edited with arrow-key escape sequences and saved (kubectl exec cat)")

                # top: full-screen refresh, quit with q.
                keys(b"top\r", lambda t: "PID" in t and "Mem:" in t, timeout=20)
                capture("f1-top")
                keys(b"q", lambda t: prompt_ready())
                checks.append("top drew full screen and quit with q")

                # resize: the remote TTY sees the new size.
                width, height = 132, 40
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
                screen.resize(width, height)
                drain(.6)
                keys(b"clear; stty size\r", f"{height} {width}")
                capture("f1-resize")
                checks.append(f"window resize forwarded to the remote TTY (stty size -> {height} {width})")

                # hostile leftovers, then Ctrl-] with an unterminated OSC pending.
                hostile = (r"printf '\033[?1049h\033[?1000h\033[?1003h\033[?1006h\033[?2004h\033[?1h\033=\033[?25l"
                           r"\033[?7l\033[5;10r\033]0;KBORG_EVIL_TITLE\007HOSTILE_LEFTOVER\033]2;unterminated'" + "\r")
                keys(hostile.encode(), lambda t: "HOSTILE_LEFTOVER" in t and screen.alternate)
                leftover = sorted(screen.modes & {1000, 1003, 1006, 2004, 1})
                if leftover != [1, 1000, 1003, 1006, 2004] or not screen.keypad or 25 in screen.modes:
                    raise RuntimeError("Hostile modes were not applied in the fixture terminal")
                mark = len(stream_log)
                keys(b"\x1d", lambda t: POD in t and "Interactive session ended" in t and "HOSTILE_LEFTOVER" not in t)
                returned = bytes(stream_log[mark:])
                start = returned.find(RESET_START)
                end = returned.find(RESET_END, max(start, 0))
                redraw = returned.find(b"\x1b[?1049h", max(end, 0))
                if not (0 <= start < end < redraw):
                    raise RuntimeError("Reset sequence did not precede KBorg's redraw")
                for mode in (1000, 1003, 1006, 1):
                    if mode in screen.modes:
                        raise RuntimeError(f"Mode {mode} survived the disconnect")
                if screen.keypad or (screen.top, screen.bottom) != (0, height - 1):
                    raise RuntimeError("Keypad or scroll region survived the disconnect")
                if "local Ctrl-] or cancellation" not in " ".join(screen.text().split()):
                    raise RuntimeError("Disconnect outcome was not shown")
                capture("f1-returned")
                checks.append("Ctrl-] disconnected mid-OSC; mouse/paste/keypad/cursor-key/scroll-region/alt-screen reset before a clean KBorg redraw")

                # second session: file is there; exiting the shell also returns cleanly.
                open_session(False)
                keys(b"cat /tmp/f1.txt\r", lambda t: "hello world" in t and prompt_ready())
                keys(b"exit\r", lambda t: POD in t and "Interactive session ended" in t)
                if "remote command completed" not in " ".join(screen.text().split()):
                    raise RuntimeError("Remote exit outcome was not shown")
                capture("f1-exit")
                checks.append("second session read the saved file; remote exit returned to KBorg with the outcome")

                keys(b":q\r")
                until = time.monotonic() + 8
                while process.poll() is None and time.monotonic() < until:
                    drain()
                if process.poll() != 0:
                    raise RuntimeError("KBorg did not exit successfully")
                if termios.tcgetattr(master) != original:
                    raise RuntimeError("KBorg did not restore the terminal mode")
                checks.append("quit restored the original terminal mode")
                passed = True
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                os.close(master)
                os.close(slave)
    finally:
        cleaned = created_uid is None
        if created_uid:
            current = json.loads(kubectl("get", "namespace", NAMESPACE, "-o", "json"))
            if current["metadata"]["uid"] != created_uid or current["metadata"].get("labels", {}).get("kborg.dev/run-id") != run_id:
                raise RuntimeError("F1 fixture cleanup ownership changed")
            kubectl("delete", "namespace", NAMESPACE, "--wait=true", "--timeout=90s")
            cleaned = True
        report = {"passed": passed, "context": args.context, "namespace": NAMESPACE,
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "elapsed_seconds": round(time.monotonic() - started, 2),
            "checks": checks, "captures": captures, "fixture_cleanup": cleaned}
        (screenshots / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({key: report[key] for key in ("passed", "context", "checks", "fixture_cleanup")}, indent=2))


if __name__ == "__main__":
    main()
