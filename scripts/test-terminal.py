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


def lifecycle_session(binary, args, captures):
    """Exercise simulated writes; the demo backend never runs Helm or a cluster request."""
    with tempfile.TemporaryDirectory(prefix="kborg-lifecycle-terminal-") as temporary:
        master, slave = pty.openpty()
        original_mode = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        environment = os.environ.copy()
        environment.update(TERM="xterm-256color", COLORTERM="truecolor", KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        def child_setup():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        launcher = ("import json,resource,subprocess,sys; "
                    "status=subprocess.call(sys.argv[1:]); "
                    "usage=resource.getrusage(resource.RUSAGE_CHILDREN); "
                    "print('\\nKBORG_PTY_RESULT '+json.dumps({'status':status,'cpu':usage.ru_utime+usage.ru_stime,'rss':usage.ru_maxrss}),flush=True); "
                    "sys.stdin.readline(); sys.exit(status)")
        process = subprocess.Popen([sys.executable, "-c", launcher, str(binary), "--demo", "--theme", "catppuccin-mocha", "--write", "--allow-helm", "--allow-helm-write", "--helm-tool", "/demo/helm", "--allow-logs", "--no-splash", "--no-mouse", "--color=always"], stdin=slave, stdout=slave, stderr=slave, env=environment, preexec_fn=child_setup)
        screen = Screen(120, 32)
        output = bytearray()
        def drain(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if select.select([master], [], [], min(.05, max(0, until-time.monotonic())))[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not data:
                        break
                    output.extend(data)
                    assert len(output) <= 8*1024*1024, "lifecycle terminal output exceeded bound"
                    screen.feed(data.decode("utf-8", errors="replace"))
        def keys(value, expected=None):
            idle_deadline=time.monotonic()+6
            while ("LOADING" in screen.text().splitlines()[2] or "Request in progress" in screen.text().splitlines()[-1]) and time.monotonic()<idle_deadline:
                drain(.1)
            os.write(master, value)
            deadline = time.monotonic()+6
            while time.monotonic() < deadline:
                drain(.12)
                if expected is None or expected.casefold() in screen.text().casefold():
                    return
            raise AssertionError(f"missing {expected!r} after keys {value!r}\n{screen.text()}")
        def capture(name, required):
            deadline=time.monotonic()+6
            while any(value.casefold() not in screen.text().casefold() for value in required) and time.monotonic()<deadline:
                drain(.1)
            text = screen.text()
            for value in required:
                assert value.casefold() in text.casefold(), f"{name}: missing {value!r}\n{text}"
            captures.append({"name":name, "width":screen.width, "height":screen.height})
            if args.screenshots:
                screen.png(args.screenshots/f"{name}.png")
        def resize(width, height):
            nonlocal screen
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
            screen = Screen(width,height)
            os.killpg(process.pid,signal.SIGWINCH)
            drain(.25)
        def complete_review(target):
            keys(target.encode())
            keys(b"\x1b[H")
            for _ in range(100):
                if re.search(r"\b0 unread\b",screen.text()):
                    break
                keys(b"\x1b[6~")
            else:
                raise AssertionError(f"review did not reach complete painted coverage\n{screen.text()}")
            keys(b"\r","HELM OPERATION SUCCEEDED")
        try:
            drain(1)
            keys(b"","api-7d4f9")
            keys(b":helm\r","HELM / releases")
            keys(b"i","INSTALL HELM RELEASE")
            keys(b"\tdemo-new")
            capture("helm-install-form",["INSTALL HELM RELEASE","demo-new","/demo/chart.tgz"])
            keys(b"\r","HELM REVIEW")
            capture("helm-install-review",["SIMULATED HELM install","CONFIRM demo/demo-new"])
            keys(b"\x1b","INSTALL HELM RELEASE")
            capture("helm-review-cancelled",["INSTALL HELM RELEASE","demo-new"])
            pinned = "oci://registry.example/team/chart@sha256:" + "a"*64
            keys(b"\t\x15" + pinned.encode())
            capture("helm-oci-default-off",["INSTALL HELM RELEASE","OCI sha256:","hooks OFF","CRDs OFF","publisher signatures"])
            keys(b"\x15/demo/chart.tgz")
            keys(b"\r","HELM REVIEW")
            resize(48,12)
            capture("helm-review-narrow",["HELM REVIEW","unread"])
            keys(b"demo/demo-new\r","HELM REVIEW")
            capture("helm-unread-review-denied",["Read every review line","unread"])
            # End cannot acknowledge unseen middle pages.
            keys(b"\x1b[F\r","HELM REVIEW")
            assert not re.search(r"\b0 unread\b",screen.text()), "End incorrectly approved skipped review content"
            keys(b"\x15")
            complete_review("demo/demo-new")
            capture("helm-install-simulated",["HELM OPERATION SUCCEEDED","SIMULATED"])
            resize(120,32)
            keys(b"\x1b","HELM / releases")
            keys(b"u","UPGRADE HELM RELEASE")
            capture("helm-upgrade-form",["UPGRADE HELM RELEASE","revision 2","/demo/chart.tgz"])
            keys(b"\r","HELM REVIEW")
            complete_review("demo/demo-api")
            capture("helm-upgrade-simulated",["HELM OPERATION SUCCEEDED","SIMULATED"])
            keys(b"\x1b","HELM / releases")
            keys(b"\r","HELM HISTORY")
            keys(b"\x1b[Br","ROLL BACK HELM RELEASE")
            capture("helm-rollback-form",["ROLL BACK HELM RELEASE","Target revision 1"])
            keys(b"\r","HELM REVIEW")
            complete_review("demo/demo-api")
            capture("helm-rollback-simulated",["HELM OPERATION SUCCEEDED","SIMULATED"])
            keys(b"\x1b","HELM / releases")
            keys(b"\x04","UNINSTALL HELM RELEASE")
            capture("helm-uninstall-form",["UNINSTALL HELM RELEASE","DESTRUCTIVE"])
            keys(b"\r","HELM REVIEW")
            complete_review("demo/demo-api")
            capture("helm-uninstall-simulated",["HELM OPERATION SUCCEEDED","SIMULATED"])
            keys(b"\x1b","HELM / releases")
            keys(b"c")
            capture("helm-crds-denied",["HELM / releases","--allow-helm-crds"])
            keys(b"\x1b")
            drain(.3)
            keys(b":deploy\r","deployments")
            drain(.5)
            keys(b"l","FIXED")
            capture("workload-logs",["FIXED 1 Pods","[demo/api-7d4f9:app]","INFO"])
            keys(b"\x1b")
            keys(b":logs 'app in (api,worker)'\r","FIXED 2 Pods")
            capture("multi-pod-logs",["FIXED 2 Pods","[demo/api-7d4f9:app]","[demo/worker-6a1bc:app]"])
            keys(b"3","last 300s")
            capture("multi-pod-log-range",["FIXED 2 Pods","last 300s","[demo/worker-6a1bc:app]"])
            keys(b"\x1b")
            keys(b":q\r")
            drain(.5)
            result = re.search(rb"KBORG_PTY_RESULT (\{[^\r\n]+\})",output)
            assert result,"simulated lifecycle session did not exit"
            usage = json.loads(result.group(1))
            assert usage["status"] == 0
            assert b"\x1b[?1049l" in output
            assert termios.tcgetattr(slave) == original_mode,"lifecycle terminal mode was not restored"
            keys(b"\n")
            process.wait(timeout=3)
            return usage
        finally:
            if process.poll() is None:
                os.killpg(process.pid,signal.SIGKILL)
                process.wait(timeout=3)
            os.close(master)
            os.close(slave)


def extension_form_session(binary, args, captures):
    """Real demo UI frames for explicit flags; no chart or registry is accessed."""
    with tempfile.TemporaryDirectory(prefix="kborg-extension-terminal-") as temporary:
        master, slave = pty.openpty()
        original_mode = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
        environment = os.environ.copy()
        environment.update(TERM="xterm-256color", COLORTERM="truecolor", KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        environment.pop("NO_COLOR",None)
        def setup():
            os.setsid()
            fcntl.ioctl(0,termios.TIOCSCTTY,0)
        process=subprocess.Popen([str(binary),"--demo","--theme","catppuccin-mocha","--write","--allow-helm","--allow-helm-write","--allow-helm-hooks","--allow-helm-crds","--helm-tool","/demo/helm","--no-splash","--no-mouse","--color=always"],stdin=slave,stdout=slave,stderr=slave,env=environment,preexec_fn=setup)
        screen=Screen(120,32)
        output=bytearray()
        def drain(seconds):
            until=time.monotonic()+seconds
            while time.monotonic()<until:
                if select.select([master],[],[],min(.05,max(0,until-time.monotonic())))[0]:
                    try:data=os.read(master,65536)
                    except OSError as error:
                        if error.errno==errno.EIO:break
                        raise
                    if not data:break
                    output.extend(data)
                    assert len(output)<=4*1024*1024,"extension form terminal output exceeded bound"
                    screen.feed(data.decode("utf-8",errors="replace"))
        def keys(value,expected=None):
            os.write(master,value)
            deadline=time.monotonic()+6
            while time.monotonic()<deadline:
                drain(.1)
                if expected is None or expected.casefold() in screen.text().casefold():return
            raise AssertionError(f"missing {expected!r} in demo extension frame\n{screen.text()}")
        def capture(name,required):
            deadline=time.monotonic()+6
            while any(value.casefold() not in screen.text().casefold() for value in required) and time.monotonic()<deadline:
                drain(.1)
            for value in required:
                assert value.casefold() in screen.text().casefold(),f"{name}: missing {value!r}\n{screen.text()}"
            captures.append({"name":name,"width":120,"height":32})
            if args.screenshots:screen.png(args.screenshots/f"{name}.png")
        try:
            keys(b"","api-7d4f9")
            keys(b":helm\r","HELM / releases")
            keys(b"i","INSTALL HELM RELEASE")
            pinned="oci://registry.example/team/chart@sha256:"+"a"*64
            keys(b"\toci-demo\t\x15"+pinned.encode())
            capture("helm-oci-explicit-policy",["INSTALL HELM RELEASE","OCI sha256:","hooks ON","CRDs ON","publisher signatures"])
            keys(b"\x1b","HELM / releases")
            keys(b"c","INSTALL CHART CRDS")
            keys(b"\tcrd-demo\t\x15"+pinned.encode())
            capture("helm-crds-form",["INSTALL CHART CRDS","Cluster-wide schema creation","retained on uninstall","No release installed"])
            assert "Values file" not in screen.text(),"CRD bootstrap must not offer release values"
            keys(b"\x1b","HELM / releases")
            keys(b"\x1b")
            keys(b":q\r")
            deadline=time.monotonic()+6
            usage=None
            while time.monotonic()<deadline:
                waited,status,child_usage=os.wait4(process.pid,os.WNOHANG)
                if waited:
                    process.returncode=os.waitstatus_to_exitcode(status)
                    usage=child_usage
                    break
                drain(.1)
            drain(.1)
            assert process.returncode==0 and usage is not None,"extension form session did not exit"
            assert b"\x1b[?1049l" in output and termios.tcgetattr(master)==original_mode,"extension form terminal restoration failed"
            return {"status":0,"cpu":usage.ru_utime+usage.ru_stime,"rss":usage.ru_maxrss}
        finally:
            if process.poll() is None:
                os.write(master,b"\x03")
                try:process.wait(timeout=3)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=3)
            os.close(master)
            os.close(slave)


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
            deadline=time.monotonic()+6
            while any(value.casefold() not in screen.text().casefold() for value in required) and time.monotonic()<deadline:
                drain(.1)
            text = screen.text()
            for value in required:
                assert value.casefold() in text.casefold(), f"{name}: missing {value!r} in actual terminal frame\n{text}"
            captures.append({"name": name, "width": screen.width, "height": screen.height})
            if args.screenshots:
                screen.png(args.screenshots / f"{name}.png")

        def keys(value, wait=.5):
            deadline=time.monotonic()+6
            while ("LOADING" in screen.text().splitlines()[2] or "Request in progress" in screen.text().splitlines()[-1]) and time.monotonic()<deadline:
                drain(.1)
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
            assert "RESOURCES [Tab]" not in screen.text(), "permanent sidebar returned"
            keys(b":dep")
            capture("command-completion", [":dep", ":deploy", "Tab complete"])
            keys(b"\t\r")
            capture("deployments", ["deployments", "2/2 ready"])
            keys(b":po /api\r")
            capture("filtered-pods", ["pods", "FILTERED", "api-7d4f9"])
            assert "CrashLoopBackOff" not in screen.text(), "regex filter retained the excluded pod"
            keys(b"\x1b")
            keys(b"d", 1)
            capture("object-overview", ["OBJECT OVERVIEW", "app", "Running"])
            keys(b"J")
            capture("json", ["apiVersion", "containers"])
            keys(b"\x1b")
            keys(b"\r", 1)
            capture("containers", ["CONTAINERS", "app", "running"])
            keys(b"\x1b")
            keys(b":actions\r")
            capture("actions", ["ACTIONS", "[OFF]", "READ ONLY"])
            keys(b"\x1b")
            keys(b"\x01")
            capture("aliases", ["Aliases", "deployments.apps", "deploy"])
            keys(b"\x1b")
            keys(b":po\r")
            keys(b"S")
            capture("sorted-pods", ["sort:status", "api-7d4f9"])
            keys(b"C", 1)
            capture("metrics-table", ["CPU(m)", "MEM", "125", "64Mi", "sort:cpu"])
            keys(b"N")
            keys(b"m", 1)
            capture("metrics", ["METRICS", "CPU", "MEM", "SESSION HISTORY"])
            keys(b"M", 1)
            capture("allocations", ["requests", "50.00%", "Resource comparisons"])
            keys(b"\x1b")
            keys(b"l", 1)
            capture("log-detail", ["LOGS", "demo", "INFO"])
            keys(b"a", 1)
            capture("merged-logs", ["ALL ordinary", "[demo/api-7d4f9:app]", "INFO"])
            keys(b"tws")
            capture("log-controls", ["LOGS", "PAUSED"])
            keys(b"\x1b")
            keys(b":helm\r", 1)
            capture("helm-releases", ["HELM", "demo-api", "deployed", "Secret"])
            keys(b"u")
            capture("helm-lifecycle-denied", ["--write", "--allow-helm-write"])
            keys(b"\r", 1)
            capture("helm-history", ["HELM HISTORY", "superseded", "1.1.0"])
            keys(b"\r", 1)
            capture("helm-values", ["HELM STORED VALUES", "replicaCount", "confidential"])
            keys(b"\x1b")
            keys(b"m", 1)
            capture("helm-resources", ["HELM RELEASE RESOURCES", "Deployment", "demo/api"])
            keys(b"\x1b")
            keys(b"\x1b")
            keys(b"\x1b")
            keys(b":pu\r", 1)
            capture("pulses", ["PULSES", "ATTENTION", "pods"])
            keys(b"\x1b")
            keys(b":xray dp\r", 1)
            capture("xray", ["XRAY", "Deployment", "DEMO"])
            keys(b"\x1b")
            keys(b"\x13")
            keys(b":sd\r")
            capture("saved-resources", ["Saved resources", "pods"])
            keys(b"\x1b")
            keys(b":overview\r")
            keys(b"\r", 1)
            capture("live-overview", ["LIVE", "demo", "UNHEALTHY"])
            keys(b"\x1b")
            keys(b":themes\r")
            capture("themes", ["Themes", "Catppuccin Mocha"])
            keys(b"N")
            capture("create-theme", ["theme", "Enter"])
            keys(b"\x1b")
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
    lifecycle_usage = lifecycle_session(binary, args, captures)
    extension_usage = extension_form_session(binary, args, captures)
    native_usage["cpu"] += lifecycle_usage["cpu"] + extension_usage["cpu"]
    native_usage["rss"] = max(native_usage["rss"], lifecycle_usage["rss"], extension_usage["rss"])
    report = {"schema": 1, "platform": os.uname().sysname, "architecture": os.uname().machine,
              "binary": str(binary), "binary_sha256": __import__("hashlib").sha256(binary.read_bytes()).hexdigest(),
              "elapsed_seconds": round(time.monotonic() - started, 3),
              "child_cpu_seconds": round(native_usage["cpu"], 6),
              "peak_rss_bytes": native_usage["rss"] if os.uname().sysname == "Darwin" else native_usage["rss"] * 1024,
              "idle_output_bytes_over_2_seconds": idle_bytes,
              "terminal_restored": True, "screens": captures,
              "scope": "three native PTY sessions, offline samples, OCI/CRD forms and SIMULATED Helm lifecycle; no registry/tool/cluster writes; ASCII truecolor; not a real-terminal/platform matrix"}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
