#!/usr/bin/env python3
"""Live Unicode review display/cancellation on one explicitly owned fixture Pod.

Setup uses the named local context; KBorg receives only a short-lived Pod-read
service account. No forward is started and no resource mutation is approved.
Credentials/transcripts remain in a private temporary directory and memory.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, spawn = helpers["Screen"], helpers["_spawn"]
IMAGE = "busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--kubeconfig", type=Path, required=True)
    parser.add_argument("--context", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.context not in ("docker-desktop", "kborg-soak"):
        raise ValueError("Use only the authorized local fixture contexts")
    binary = args.binary.resolve(strict=True)
    config = args.kubeconfig.resolve(strict=True)
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    namespace, owner, run_id = "kborg-terminal-" + uuid.uuid4().hex[:10], None, uuid.uuid4().hex
    context_name = "review-東京-café-\\u{e9}"
    args.output.mkdir(parents=True, exist_ok=True)
    report = {"passed": False, "binary_sha256": digest, "context": args.context,
              "namespace": namespace, "checks": [], "screenshots": [], "fixture_cleanup": False,
              "native_emulator": False, "port_forward_started": False}

    def kube(*words, payload=None, timeout=60):
        result = subprocess.run(["kubectl", "--kubeconfig", str(config), "--context", args.context,
                                 "--request-timeout=20s", *words], input=json.dumps(payload) if payload is not None else None,
                                capture_output=True, text=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError("Unicode fixture kubectl operation failed: " + words[0])
        return result.stdout

    try:
        with tempfile.TemporaryDirectory(prefix="kborg-unicode-review-") as temporary:
            temporary = Path(temporary).resolve()
            cluster = json.loads(kube("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))["clusters"][0]["cluster"]
            assert cluster["server"].startswith("https://") and cluster.get("certificate-authority-data") and not cluster.get("insecure-skip-tls-verify")
            created = json.loads(kube("create", "-f", "-", "-o", "json", payload={"apiVersion": "v1", "kind": "Namespace",
                "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}}}))
            owner = created["metadata"]["uid"]
            items = [
                {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "review-target", "namespace": namespace},
                 "spec": {"automountServiceAccountToken": False, "terminationGracePeriodSeconds": 0,
                    "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
                    "containers": [{"name": "app", "image": IMAGE, "imagePullPolicy": "IfNotPresent", "command": ["sleep", "300"],
                        "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                        "resources": {"requests": {"cpu": "5m", "memory": "8Mi"}, "limits": {"cpu": "50m", "memory": "24Mi"}}}]}},
                {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"name": "review", "namespace": namespace}, "automountServiceAccountToken": False},
                {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"name": "review", "namespace": namespace},
                 "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list", "watch"]}]},
                {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"name": "review", "namespace": namespace},
                 "subjects": [{"kind": "ServiceAccount", "name": "review", "namespace": namespace}],
                 "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "review"}},
            ]
            kube("create", "-f", "-", payload={"apiVersion": "v1", "kind": "List", "items": items})
            kube("-n", namespace, "wait", "--for=condition=Ready", "pod/review-target", "--timeout=90s", timeout=100)
            token = kube("-n", namespace, "create", "token", "review", "--duration=10m").strip()
            scoped = temporary / "review.kubeconfig"
            with scoped.open("x") as file:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": context_name,
                    "clusters": [{"name": "fixture", "cluster": cluster}], "users": [{"name": "fixture", "user": {"token": token}}],
                    "contexts": [{"name": context_name, "context": {"cluster": "fixture", "user": "fixture", "namespace": namespace}}]}, file)
            master, slave = pty.openpty()
            original = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 132, 0, 0))
            environment = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(temporary / "preferences"))
            environment.pop("NO_COLOR", None)
            process = spawn([str(binary), "--kubeconfig", str(scoped), "--context", context_name, "--namespace", namespace,
                "--write", "--allow-forward", "--no-splash", "--no-mouse", "--theme", "rose-pine", "--color=always"], environment, slave)
            screen = Screen(132, 36)

            def drain(seconds=.15):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    if select.select([master], [], [], min(.05, max(0, deadline-time.monotonic())))[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            break
                        if not data:
                            break
                        if token.encode() in data:
                            raise RuntimeError("Credential appeared in UI; output suppressed")
                        screen.feed(data)

            def keys(value=b"", expected=None):
                if value:
                    os.write(master, value)
                deadline = time.monotonic() + 30
                while time.monotonic() < deadline:
                    drain()
                    text = screen.text()
                    if expected is None or (expected(text) if callable(expected) else expected in text):
                        return
                    if process.poll() is not None:
                        break
                raise RuntimeError(f"Unicode review UI did not reach {expected!r}")

            def capture(name):
                path = args.output / (name + ".png")
                screen.png(path)
                report["screenshots"].append(str(path.resolve()))

            def resize(width, height):
                nonlocal screen
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", height, width, 0, 0))
                screen = Screen(width, height)
                os.killpg(process.pid, signal.SIGWINCH)
                drain(.4)

            try:
                keys(expected=lambda text: "review-target" in text and "LOADING" not in text)
                keys(b"F", "PORT FORWARD / loopback")
                keys(b"\r", "REVIEW PORT FORWARD")
                keys(expected="context: review-")
                text = screen.text()
                assert "\\u{6771}\\u{4eac}" in text and "caf\\u{e9}" in text and "\\\\u{e9}" in text
                assert "Unicode" in text or "ASCII" in text or "code point" in text or "Escapes" in text
                capture("unicode-forward-review")
                report["checks"].append("distinct Unicode and literal escape spelling visibly preserved in actual live review")
                resize(48, 12)
                keys(expected="REVIEW PORT FORWARD")
                # A deliberately incomplete confirmation cannot start a session, even if a bug exists in size gating.
                keys(b"\r", "Enlarge terminal")
                capture("unicode-review-narrow-blocked")
                report["checks"].append("undersize review rejects Enter before target confirmation")
                resize(132, 36)
                keys(expected="\\u{6771}\\u{4eac}")
                keys(b"\x1b", expected=lambda text: "REVIEW PORT FORWARD" not in text)
                keys(b"f", "PORT FORWARDS")
                keys(expected="No forwards.")
                capture("unicode-review-cancelled")
                report["checks"].append("Escape returns to resource scope with no forward session created")
                keys(b"\x1b", expected=lambda text: "PORT FORWARDS" not in text)
                keys(b":q\r")
                deadline = time.monotonic() + 10
                while process.poll() is None and time.monotonic() < deadline:
                    drain()
                assert process.poll() == 0, "review session did not exit cleanly"
                assert termios.tcgetattr(master) == original, "review session did not restore termios"
                report["checks"].append("normal exit restores original terminal settings")
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=3)
                os.close(master)
                os.close(slave)
            assert hashlib.sha256(binary.read_bytes()).hexdigest() == digest
            report["passed"] = True
    finally:
        if owner:
            current = json.loads(kube("get", "namespace", namespace, "-o", "json"))
            if current["metadata"]["uid"] != owner or current["metadata"]["labels"].get("kborg.dev/run-id") != run_id:
                raise RuntimeError("Fixture cleanup refused: namespace owner changed")
            kube("delete", "namespace", namespace, "--wait=true", "--timeout=90s", timeout=100)
            report["fixture_cleanup"] = not kube("get", "namespace", namespace, "--ignore-not-found", "-o", "name").strip()
        (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
