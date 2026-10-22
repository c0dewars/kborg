#!/usr/bin/env python3
"""Live TUI acceptance inside the lifecycle runner's explicitly owned namespace.

Imported by test-helm-lifecycle.py, never executed as a standalone cluster tool.
The caller owns namespace creation, credentials, fixture charts and cleanup.
Terminal transcripts and stored release payloads are never written to disk.
"""
import base64
import codecs
import errno
import fcntl
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
import tempfile
import termios
import time


Screen = runpy.run_path(str(Path(__file__).with_name("test-terminal.py")))["Screen"]


def _spawn(argv, environment, slave):
    """Give the actual binary its own controlling terminal, without a shell."""
    def setup():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)

    return subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave,
                            env=environment, preexec_fn=setup)


def _outcome_title(text):
    """The modal starts at row3; footers may describe an earlier operation."""
    lines = text.splitlines()
    return lines[3] if len(lines) > 3 else ""


def run(binary, helm, kubeconfig, context, namespace, chart1, chart2,
        environment, screenshots, *, canary, storage="secret"):
    """Review and execute all four UI actions; capture upgraded two-Pod logs.

    Only the supplied connection and kborg-lifecycle-* namespace are used.
    Each write requires complete painted review and exact namespace/release
    confirmation. No failed or uncertain write is automatically retried.
    """
    paths = [Path(value).resolve(strict=True) for value in
             (binary, helm, kubeconfig, chart1, chart2)]
    binary, helm, kubeconfig, chart1, chart2 = paths
    if (not namespace.startswith("kborg-lifecycle-") or not context
            or storage not in ("secret", "configmap") or not canary
            or any(not path.is_file() for path in paths)):
        raise ValueError("Live UI requires owned lifecycle fixtures and an explicit connection")
    screenshots = Path(screenshots).resolve() if screenshots else None
    release = "ui-lifecycle"
    confirmation = f"{namespace}/{release}"
    forbidden = (canary.encode(), base64.b64encode(canary.encode()))
    started, captures = time.monotonic(), []
    with tempfile.TemporaryDirectory(prefix="kborg-live-lifecycle-ui-") as temporary:
        master, slave = pty.openpty()
        original_mode = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
        child_environment = dict(environment)
        child_environment.update(TERM="xterm-256color", COLORTERM="truecolor",
                                 KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        child_environment.pop("NO_COLOR", None)

        argv = [str(binary), "--context", context, "--kubeconfig", str(kubeconfig),
                "--namespace", namespace, "--helm-storage", storage,
                "--write", "--allow-helm", "--allow-helm-write",
                "--helm-tool", str(helm), "--helm-timeout", "90", "--allow-logs",
                "--theme", "catppuccin-mocha", "--no-splash", "--no-mouse",
                "--color=always"]
        # The master retains termios after the controlling session exits;
        # macOS can revoke slave ioctls when the session leader exits.
        process = _spawn(argv, child_environment, slave)
        screen, output = Screen(140, 45), bytearray()
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")

        def check_private():
            visible = screen.text()
            if any(value in output or value.decode() in visible for value in forbidden):
                raise RuntimeError("Live UI exposed a fixture secret; terminal output suppressed")

        def drain(seconds):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                if time.monotonic() - started > 900:
                    raise RuntimeError("Live UI exceeded its session deadline; output suppressed")
                if not select.select([master], [], [], min(.05, max(0, until-time.monotonic())))[0]:
                    continue
                try:
                    data = os.read(master, 65536)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output.extend(data)
                if len(output) > 16 * 1024 * 1024:
                    raise RuntimeError("Live UI exceeded its terminal capture bound")
                screen.feed(decoder.decode(data))
                check_private()

        def wait(expected, timeout=15, *, outcome=False):
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                text = screen.text()
                # An earlier operation's footer can remain visible while a
                # new one runs. Only the active modal's row3 is an outcome.
                active = _outcome_title(text) if outcome else text
                if expected.casefold() in active.casefold():
                    return
                if process.poll() is not None:
                    raise RuntimeError(f"Live UI exited with status {process.returncode} before {expected!r}; output suppressed")
                if outcome and any(title in active for title in
                                   ("HELM OUTCOME UNKNOWN", "HELM OPERATION FAILED", "HELM NOT STARTED")):
                    raise RuntimeError("Live UI Helm action failed or has an unknown outcome; output suppressed")
                if expected == "HELM REVIEW" and "HELM PREPARATION FAILED" in text:
                    raise RuntimeError("Live UI Helm preparation failed; no execution requested; output suppressed")
                drain(.1)
            raise RuntimeError(f"Live UI did not reach {expected!r}; terminal output suppressed")

        def keys(value, expected=None, timeout=15, *, outcome=False):
            os.write(master, value)
            drain(.12)
            if expected:
                wait(expected, timeout, outcome=outcome)

        def capture(name, required):
            for value in required:
                wait(value)
            check_private()
            captures.append({"name": name, "width": screen.width, "height": screen.height})
            if screenshots:
                screen.png(screenshots / f"{name}.png")

        def review(name):
            keys(b"\r", "HELM REVIEW", timeout=130)
            capture(f"live-ui-{name}-review", ["HELM REVIEW", confirmation, "unread"])

        def execute_review(name):
            keys(confirmation.encode())
            keys(b"\x1b[H")
            for _ in range(1024):
                if re.search(r"\b0 unread\b", screen.text()):
                    break
                keys(b"\x1b[6~")
            else:
                raise RuntimeError("Live UI review coverage did not complete; no execution requested")
            capture(f"live-ui-{name}-confirmed", ["HELM REVIEW", "0 unread", confirmation])
            keys(b"\r", "HELM OPERATION SUCCEEDED", timeout=240, outcome=True)
            capture(f"live-ui-{name}-result", ["HELM OPERATION SUCCEEDED"])

        def return_releases():
            keys(b"\x1b", "HELM / releases")
            wait(release)

        try:
            # Startup discovery and the first list are asynchronous.
            wait("CTX ", timeout=30)
            wait("NAME", timeout=30)
            startup_deadline = time.monotonic() + 30
            while ("LOADING" in screen.text().splitlines()[2]
                   or "Request in progress" in screen.text().splitlines()[-1]):
                if time.monotonic() > startup_deadline:
                    raise RuntimeError("Live UI startup did not become idle; output suppressed")
                drain(.1)
            keys(b":helm\r", "HELM / releases", timeout=30)
            keys(b"i", "INSTALL HELM RELEASE")
            keys(b"\t" + release.encode() + b"\t" + str(chart1).encode())
            capture("live-ui-install-form", ["INSTALL HELM RELEASE", release, "Packaged chart"])
            review("install-cancel")
            keys(b"\x1b", "INSTALL HELM RELEASE")
            capture("live-ui-install-cancelled", ["INSTALL HELM RELEASE", release])
            review("install")
            execute_review("install")
            return_releases()
            keys(b"u", "UPGRADE HELM RELEASE")
            keys(str(chart2).encode())
            capture("live-ui-upgrade-form", ["UPGRADE HELM RELEASE", "revision 1"])
            review("upgrade")
            execute_review("upgrade")
            return_releases()
            keys(b"\x1b")
            keys(b":deploy /ui-lifecycle-worker\r", "deployments", timeout=30)
            wait("ui-lifecycle-worker", timeout=30)
            # The lifecycle command verifies accepted resources, not rollout
            # readiness. Wait using the real UI, then recapture a fresh fixed
            # membership until both upgraded ordinary sources are available.
            logs_deadline = time.monotonic() + 120
            while time.monotonic() < logs_deadline:
                if "2/2" not in screen.text():
                    drain(1)
                    continue
                keys(b"l", "FIXED", timeout=30)
                source_deadline = time.monotonic() + 15
                while time.monotonic() < source_deadline:
                    text = screen.text()
                    sources = set(re.findall(r"\[" + re.escape(namespace) + r"/(ui-lifecycle-worker-[^:\]\s]+):worker\]", text))
                    if "FIXED 2 Pods" in text and len(sources) == 2 and "KBORG_LIFECYCLE" in text:
                        break
                    drain(.2)
                else:
                    keys(b"\x1b")
                    drain(1)
                    continue
                break
            else:
                raise RuntimeError("Live UI did not capture both upgraded Pod log sources; output suppressed")
            capture("live-ui-workload-two-pod-logs", ["FIXED 2 Pods", "2 containers", "KBORG_LIFECYCLE"])
            keys(b"3", "last 300s", timeout=30)
            capture("live-ui-workload-log-range", ["FIXED 2 Pods", "last 300s", "KBORG_LIFECYCLE"])
            keys(b"\x1b")
            keys(b":helm\r", "HELM / releases", timeout=30)
            wait(release)
            keys(b"\r", "HELM HISTORY", timeout=30)
            keys(b"\x1b[Br", "ROLL BACK HELM RELEASE")
            capture("live-ui-rollback-form", ["ROLL BACK HELM RELEASE", "Target revision 1"])
            review("rollback")
            execute_review("rollback")
            return_releases()
            keys(b"\x04", "UNINSTALL HELM RELEASE")
            capture("live-ui-uninstall-form", ["UNINSTALL HELM RELEASE", "DESTRUCTIVE"])
            review("uninstall")
            execute_review("uninstall")
            keys(b"\x1b", "HELM / releases", timeout=30)
            keys(b"\x1b")
            keys(b":q\r")
            deadline = time.monotonic() + 10
            while process.poll() is None and time.monotonic() < deadline:
                drain(.1)
            if (process.poll() != 0
                    or b"\x1b[?1049l" not in output
                    or termios.tcgetattr(master) != original_mode):
                raise RuntimeError("Live UI failed exit or terminal restoration checks")
            check_private()
            process.wait(timeout=5)
            return {"passed": True, "scope": "real Docker Desktop TUI; parent-owned namespace",
                    "release": release, "storage": storage, "screens": captures,
                    "actions": ["install", "upgrade", "rollback", "uninstall"],
                    "cancelled_review_no_execution": True, "complete_painted_review": True,
                    "fixed_workload_log_pods": 2, "fixed_workload_log_sources": 2,
                    "secret_canary_absent": True, "terminal_restored": True,
                    "seconds": round(time.monotonic()-started, 3),
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
        finally:
            if process.poll() is None:
                # Let the UI cancel its supervised Helm group and reconcile the
                # outcome before terminating the terminal process. Do not use
                # the normal capture path here: it may itself have failed its
                # canary/output/deadline assertion.
                try:
                    os.write(master, b"\x03")
                    cleanup_deadline = time.monotonic() + 15
                    trailing, quit_sent = bytearray(), False
                    while process.poll() is None and time.monotonic() < cleanup_deadline:
                        if select.select([master], [], [], .1)[0]:
                            try:
                                trailing.extend(os.read(master, 65536))
                            except OSError as error:
                                if error.errno != errno.EIO:
                                    raise
                            del trailing[:-262144]
                        if not quit_sent and any(title in trailing for title in
                                                 (b"HELM OUTCOME UNKNOWN", b"HELM OPERATION FAILED", b"HELM OPERATION SUCCEEDED")):
                            os.write(master, b"\x03")
                            quit_sent = True
                except (OSError, subprocess.TimeoutExpired):
                    pass
                if process.poll() is None:
                    try:
                        try:
                            os.killpg(process.pid, signal.SIGTERM)
                        except PermissionError:
                            process.terminate()
                        process.wait(timeout=3)
                    except (ProcessLookupError, subprocess.TimeoutExpired):
                        pass
                    if process.poll() is None:
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except PermissionError:
                            process.kill()
                        process.wait(timeout=3)
            os.close(master)
            os.close(slave)
