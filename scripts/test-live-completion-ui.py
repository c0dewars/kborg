#!/usr/bin/env python3
"""Real Helm test and schema lifecycle UI acceptance in the parent-owned namespace.

Imported only; the parent supplies TLS registry, charts, short-lived credentials,
an already Established fixture CRD, and UID-checked cleanup. No transcripts saved.
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


_terminal = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, _spawn, _outcome_title = (_terminal[name] for name in ("Screen", "_spawn", "_outcome_title"))


def _close_and_reap(process, master, slave):
    """Release PTY writers before the last bounded reap of a stopped child."""
    os.close(master)
    os.close(slave)
    if process.poll() is None:
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            raise RuntimeError("Completion UI child did not exit after cancellation/kill and PTY closure") from None


def run(binary, helm, kubeconfig, context, namespace, test_chart, updated_crd_chart,
        environment, screenshots, *, canary, verify_tests=None):
    """Install, run stored tests, uninstall, then review CRD upgrade and deletion.

    The parent owns all fixtures, API postconditions and UID-checked cleanup.
    This module requests no automatic retry after failed or uncertain writes.
    """
    paths = [Path(value).resolve(strict=True) for value in
             (binary, helm, kubeconfig, test_chart, updated_crd_chart)]
    binary, helm, kubeconfig, test_chart, updated_crd_chart = paths
    if (not namespace.startswith("kborg-extensions-") or not context or not canary
            or any(not value.is_file() for value in paths)):
        raise ValueError("Completion UI requires parent-owned fixtures and an explicit connection")
    screenshots = Path(screenshots).resolve() if screenshots else None
    release, captures = "ui-completion", []
    confirmation = f"{namespace}/{release}"
    forbidden = (canary.encode(), base64.b64encode(canary.encode()))
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kborg-live-completion-ui-") as temporary:
        master, slave = pty.openpty()
        original = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
        env = dict(environment)
        env.update(TERM="xterm-256color", COLORTERM="truecolor",
                   KBORG_CONFIG_DIR=str(Path(temporary).resolve()))
        env.pop("NO_COLOR", None)
        process = _spawn([str(binary), "--context", context, "--kubeconfig", str(kubeconfig),
                          "--namespace", namespace, "--helm-storage", "secret",
                          "--write", "--allow-helm", "--allow-helm-write", "--allow-helm-hooks",
                          "--allow-helm-crds", "--helm-tool", str(helm), "--helm-timeout", "120",
                          "--theme", "catppuccin-mocha",
                          "--no-splash", "--no-mouse", "--color=always"], env, slave)
        screen, output = Screen(140, 45), bytearray()
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")

        def check_private():
            visible = screen.text()
            if any(value in output or value.decode() in visible for value in forbidden):
                raise RuntimeError("Completion UI exposed a fixture secret; diagnostics suppressed")

        def drain(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if time.monotonic()-started > 900:
                    raise RuntimeError("Completion UI exceeded session deadline; diagnostics suppressed")
                if not select.select([master], [], [], min(.05, max(0, end-time.monotonic())))[0]:
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
                if len(output) > 24*1024*1024:
                    raise RuntimeError("Completion UI exceeded capture bound; diagnostics suppressed")
                screen.feed(decoder.decode(data))
                check_private()

        def wait(expected, timeout=20, *, outcome=False):
            end = time.monotonic()+timeout
            while time.monotonic() < end:
                text = screen.text()
                active = _outcome_title(text) if outcome else text
                if expected.casefold() in active.casefold():
                    return
                if process.poll() is not None:
                    raise RuntimeError(f"Completion UI exited {process.returncode} before {expected!r}; diagnostics suppressed")
                if outcome and any(title in active for title in
                                   ("HELM OUTCOME UNKNOWN", "HELM OPERATION FAILED", "HELM NOT STARTED")):
                    raise RuntimeError("Completion UI action failed or is uncertain; diagnostics suppressed")
                if expected == "HELM REVIEW" and "HELM PREPARATION FAILED" in text:
                    raise RuntimeError("Completion UI preparation failed; no action requested; diagnostics suppressed")
                drain(.1)
            raise RuntimeError(f"Completion UI did not reach {expected!r}; diagnostics suppressed")

        def keys(value, expected=None, timeout=20, *, outcome=False):
            os.write(master, value)
            drain(.12)
            if expected:
                wait(expected, timeout, outcome=outcome)

        def capture(name, required):
            for value in required:
                wait(value)
            check_private()
            captures.append({"name": name, "width": 140, "height": 45})
            if screenshots:
                screen.png(screenshots/f"{name}.png")

        def review(action, *, hooks=False, schema=False, destructive=False):
            keys(b"\r", "HELM REVIEW", timeout=130)
            expected_confirmation=("DELETE CRDS " if destructive else "")+confirmation
            capture(f"live-ui-{action}-review", ["HELM REVIEW", expected_confirmation, "CHANGE PLAN"])
            keys(b"\x1b[H")
            found_hooks,found_schema=False,False
            for _ in range(1024):
                text=screen.text()
                if hooks and not found_hooks and " weight " in text and "Job " in text:
                    capture(f"live-ui-{action}-hook-plan", ["Job", "weight", "HELM REVIEW"])
                    found_hooks=True
                if schema and not found_schema and ("CustomResourceDefinition" in text or "CRD " in text):
                    capture(f"live-ui-{action}-schema-plan", ["HELM REVIEW"])
                    found_schema=True
                if re.search(r"\b0 unread\b",text): break
                keys(b"\x1b[6~")
            else:
                raise RuntimeError("Completion UI review coverage did not complete; no action requested")
            if hooks and not found_hooks:
                raise RuntimeError("Completion UI did not show expected stored hook Job plan; no action requested")
            if schema and not found_schema:
                raise RuntimeError("Completion UI did not show expected CRD plan; no action requested")
            keys(expected_confirmation.encode())
            capture(f"live-ui-{action}-confirmed",["HELM REVIEW","0 unread",expected_confirmation])
            keys(b"\r","HELM OPERATION SUCCEEDED",timeout=270,outcome=True)
            capture(f"live-ui-{action}-result",["HELM OPERATION SUCCEEDED"])

        try:
            wait("CTX ", timeout=30)
            wait("NAME", timeout=30)
            idle_deadline = time.monotonic()+30
            while ("LOADING" in screen.text().splitlines()[2]
                   or "Request in progress" in screen.text().splitlines()[-1]):
                if time.monotonic() > idle_deadline:
                    raise RuntimeError("Completion UI startup did not become idle; diagnostics suppressed")
                drain(.1)
            keys(b":helm\r","HELM / releases")
            keys(b"i","INSTALL HELM RELEASE")
            keys(b"\t"+release.encode()+b"\t\x15"+str(test_chart).encode())
            capture("live-ui-completion-install-form",["INSTALL HELM RELEASE",release,"hooks ON"])
            review("completion-install")
            keys(b"\x1b","HELM / releases",timeout=30)
            wait(release)
            # The isolated namespace has only this live release; filter by its
            # exact name before dispatching a captured-current-release action.
            keys(b"/"+release.encode()+b"\r")
            keys(b"t","TEST HELM RELEASE")
            capture("live-ui-stored-tests-form",["TEST HELM RELEASE",release,"revision"])
            review("stored-tests",hooks=True)
            if verify_tests is not None:
                verify_tests()
            keys(b"\x1b","HELM / releases",timeout=30)
            keys(b"\x04","UNINSTALL HELM RELEASE")
            review("completion-uninstall")
            keys(b"\x1b","HELM / releases",timeout=30)
            keys(b":helm-crds-upgrade\r","UPGRADE CHART CRDS")
            keys(b"\t"+release.encode()+b"\t\x15"+str(updated_crd_chart).encode())
            capture("live-ui-schema-upgrade-form",["UPGRADE CHART CRDS","Cluster-wide schemas",release])
            review("schema-upgrade",schema=True)
            keys(b"\x1b","HELM / releases",timeout=30)
            keys(b":helm-crds-delete\r","DELETE CHART CRDS")
            keys(b"\t"+release.encode()+b"\t\x15"+str(updated_crd_chart).encode())
            capture("live-ui-schema-delete-form",["DELETE CHART CRDS","DESTRUCTIVE",release])
            review("schema-delete",schema=True,destructive=True)
            keys(b"\x1b","HELM / releases",timeout=30)
            keys(b"\x1b")
            keys(b":q\r")
            deadline = time.monotonic()+10
            while process.poll() is None and time.monotonic() < deadline:
                drain(.1)
            if (process.poll() != 0 or b"\x1b[?1049l" not in output
                    or termios.tcgetattr(master) != original):
                raise RuntimeError("Completion UI terminal restoration/exit failed")
            check_private()
            return {"passed": True, "release": release, "screens": captures,
                    "actions": ["install", "stored-tests", "uninstall", "upgrade-crds", "delete-crds"],
                    "complete_painted_review_before_confirmation": True,
                    "secret_canary_absent": True, "terminal_restored": True,
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "seconds": round(time.monotonic()-started, 3),
                    "scope": "real UI; parent-owned namespace/charts/CRD; parent verifies hook and API outcomes/cleanup"}
        finally:
            if process.poll() is None:
                # Responsive local cancellation first; do not reuse a capture
                # function that may have failed a private-data or output bound.
                try:
                    os.write(master, b"\x03")
                    deadline = time.monotonic()+15
                    trailing, quit_sent = bytearray(), False
                    while process.poll() is None and time.monotonic() < deadline:
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
                except OSError:
                    pass
                for sig in (signal.SIGTERM, signal.SIGKILL):
                    if process.poll() is not None:
                        break
                    try:
                        try:
                            os.killpg(process.pid, sig)
                        except PermissionError:
                            process.send_signal(sig)
                        process.wait(timeout=3)
                    except (ProcessLookupError, subprocess.TimeoutExpired):
                        pass
            _close_and_reap(process, master, slave)
