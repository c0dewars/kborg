#!/usr/bin/env python3
"""Real OCI/hook/CRD UI acceptance in the extension runner's owned namespace.

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


def run(binary, helm, kubeconfig, context, namespace, chart_source,
        environment, screenshots, *, canary, registry_ca):
    """Review existing CRDs, install pinned OCI with hooks, then uninstall.

    No mutation retries; every line is painted before exact confirmation.
    Namespace/CRD ownership and post-action API assertions belong to the parent.
    """
    paths = [Path(value).resolve(strict=True) for value in (binary, helm, kubeconfig, registry_ca)]
    binary, helm, kubeconfig, registry_ca = paths
    if (not namespace.startswith("kborg-extensions-") or not context or not canary
            or not isinstance(chart_source, str) or len(chart_source) > 1024
            or not re.fullmatch(r"oci://[^\s]+@sha256:[0-9a-f]{64}", chart_source)
            or any(not value.is_file() for value in paths)):
        raise ValueError("Extension UI requires owned fixtures and an exact digest-pinned OCI source")
    screenshots = Path(screenshots).resolve() if screenshots else None
    release, captures = "ui-extensions", []
    confirmation = f"{namespace}/{release}"
    forbidden = (canary.encode(), base64.b64encode(canary.encode()))
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kborg-live-extensions-ui-") as temporary:
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
                          "--helm-registry-ca", str(registry_ca), "--theme", "catppuccin-mocha",
                          "--no-splash", "--no-mouse", "--color=always"], env, slave)
        screen, output = Screen(140, 45), bytearray()
        decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")

        def check_private():
            visible = screen.text()
            if any(value in output or value.decode() in visible for value in forbidden):
                raise RuntimeError("Extension UI exposed a fixture secret; diagnostics suppressed")

        def drain(seconds):
            end = time.monotonic() + seconds
            while time.monotonic() < end:
                if time.monotonic()-started > 900:
                    raise RuntimeError("Extension UI exceeded session deadline; diagnostics suppressed")
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
                    raise RuntimeError("Extension UI exceeded capture bound; diagnostics suppressed")
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
                    raise RuntimeError(f"Extension UI exited {process.returncode} before {expected!r}; diagnostics suppressed")
                if outcome and any(title in active for title in
                                   ("HELM OUTCOME UNKNOWN", "HELM OPERATION FAILED", "HELM NOT STARTED")):
                    raise RuntimeError("Extension UI action failed or is uncertain; diagnostics suppressed")
                if expected == "HELM REVIEW" and "HELM PREPARATION FAILED" in text:
                    raise RuntimeError("Extension UI preparation failed; no action requested; diagnostics suppressed")
                drain(.1)
            raise RuntimeError(f"Extension UI did not reach {expected!r}; diagnostics suppressed")

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

        def review(action, *, hooks=False):
            keys(b"\r", "HELM REVIEW", timeout=130)
            capture(f"live-ui-{action}-review", ["HELM REVIEW", confirmation])
            keys(b"\x1b[H")
            found_hooks, found_source = False, False
            for _ in range(1024):
                text = screen.text()
                if action != "hook-uninstall" and not found_source and "OCI source:" in text:
                    capture(f"live-ui-{action}-source", ["OCI source:", "HELM REVIEW"])
                    found_source = True
                if hooks and not found_hooks and " weight " in text and "Job " in text:
                    capture(f"live-ui-{action}-hook-plan", ["Job", "weight", "HELM REVIEW"])
                    found_hooks = True
                if re.search(r"\b0 unread\b", text):
                    break
                keys(b"\x1b[6~")
            else:
                raise RuntimeError("Extension UI review coverage did not complete; no action requested")
            if hooks and not found_hooks:
                raise RuntimeError("Extension UI review did not show expected hook Job plan; no action requested")
            if action != "hook-uninstall" and not found_source:
                raise RuntimeError("Extension UI review did not show expected pinned OCI source; no action requested")
            # Approval follows complete painted coverage, never an End jump.
            keys(confirmation.encode())
            capture(f"live-ui-{action}-confirmed", ["HELM REVIEW", "0 unread", confirmation])
            keys(b"\r", "HELM OPERATION SUCCEEDED", timeout=270, outcome=True)
            capture(f"live-ui-{action}-result", ["HELM OPERATION SUCCEEDED"])

        try:
            wait("CTX ", timeout=30)
            wait("NAME", timeout=30)
            idle_deadline = time.monotonic()+30
            while ("LOADING" in screen.text().splitlines()[2]
                   or "Request in progress" in screen.text().splitlines()[-1]):
                if time.monotonic() > idle_deadline:
                    raise RuntimeError("Extension UI startup did not become idle; diagnostics suppressed")
                drain(.1)
            keys(b":helm-crds\r", "INSTALL CHART CRDS")
            keys(b"\t"+release.encode()+b"\t\x15"+chart_source.encode())
            capture("live-ui-existing-crds-form", ["INSTALL CHART CRDS", "Cluster-wide schema creation", "retained on uninstall"])
            review("existing-crds")
            keys(b"\x1b", "HELM / releases", timeout=30)
            keys(b"i", "INSTALL HELM RELEASE")
            keys(b"\t"+release.encode()+b"\t\x15"+chart_source.encode())
            capture("live-ui-pinned-oci-form", ["INSTALL HELM RELEASE", "OCI sha256:", "hooks ON", "CRDs ON"])
            review("oci-install", hooks=True)
            keys(b"\x1b", "HELM / releases", timeout=30)
            wait(release)
            keys(b"\x04", "UNINSTALL HELM RELEASE")
            capture("live-ui-hook-uninstall-form", ["UNINSTALL HELM RELEASE", "DESTRUCTIVE"])
            review("hook-uninstall", hooks=True)
            keys(b"\x1b", "HELM / releases", timeout=30)
            keys(b"\x1b")
            keys(b":q\r")
            deadline = time.monotonic()+10
            while process.poll() is None and time.monotonic() < deadline:
                drain(.1)
            if (process.poll() != 0 or b"\x1b[?1049l" not in output
                    or termios.tcgetattr(master) != original):
                raise RuntimeError("Extension UI terminal restoration/exit failed")
            check_private()
            return {"passed": True, "release": release, "screens": captures,
                    "actions": ["review-existing-crds", "install-oci-with-hooks", "uninstall-with-hooks"],
                    "complete_painted_review_before_confirmation": True,
                    "secret_canary_absent": True, "terminal_restored": True,
                    "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                    "seconds": round(time.monotonic()-started, 3),
                    "scope": "real UI; parent-owned namespace and existing CRD; parent verifies API outcomes/cleanup"}
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
            os.close(master)
            os.close(slave)
