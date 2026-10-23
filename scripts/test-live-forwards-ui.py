#!/usr/bin/env python3
"""Exercise independent reviewed forwards through the actual TUI on an owned local fixture.

Creates only a unique restricted namespace and two HTTP Pods. The UI receives a
short-lived namespace-limited service-account credential. Captures contain no
credential or terminal transcript. Cleanup checks namespace ownership and UID.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import stat
import runpy
import select
import signal
import socket
import struct
import subprocess
import tempfile
import termios
import time
import uuid

_helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, _spawn = (_helpers[name] for name in ("Screen", "_spawn"))
IMAGE = "busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--kubeconfig", required=True, type=Path)
    parser.add_argument("--context", required=True, choices=("docker-desktop", "kborg-v1"))
    parser.add_argument("--screenshots", required=True, type=Path)
    args = parser.parse_args()
    binary, config = (p.resolve(strict=True) for p in (args.binary, args.kubeconfig))
    screenshots = args.screenshots.resolve()
    screenshots.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex[:12]
    namespace = "kborg-forward-" + run_id
    kube = ["kubectl", "--kubeconfig", str(config), "--context", args.context]

    def kubectl(*arguments, value=None):
        result = subprocess.run(kube + list(arguments), input=json.dumps(value) if value else None,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=150)
        if result.returncode:
            raise RuntimeError("Owned forward fixture kubectl request failed: " + arguments[0])
        return result.stdout

    created_uid = None
    passed = False
    captures = []
    checks = []
    started = time.monotonic()
    try:
        cluster_config = json.loads(kubectl("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
        cluster = cluster_config["clusters"][0]["cluster"]
        if not cluster["server"].startswith("https://") or cluster.get("insecure-skip-tls-verify") or not cluster.get("certificate-authority-data"):
            raise RuntimeError("Local test requires TLS verification and a pinned CA")
        ns = json.loads(kubectl("create", "-f", "-", "-o", "json", value={"apiVersion": "v1", "kind": "Namespace", "metadata": {
            "name": namespace, "labels": {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}}}))
        created_uid = ns["metadata"]["uid"]
        resources = [
            {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": namespace, "name": "operator"}, "automountServiceAccountToken": False},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": namespace, "name": "operator"}, "rules": [
                {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": [""], "resources": ["pods/portforward"], "verbs": ["get", "create"]},
                {"apiGroups": [""], "resources": ["pods/log"], "verbs": ["get"]}]},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": namespace, "name": "operator"},
             "subjects": [{"kind": "ServiceAccount", "name": "operator", "namespace": namespace}],
             "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "operator"}},
        ]
        for suffix in ("a", "b"):
            resources.append({"apiVersion": "v1", "kind": "Pod", "metadata": {"namespace": namespace, "name": "server-" + suffix}, "spec": {
                "automountServiceAccountToken": False,
                "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
                "volumes": [{"name": "www", "emptyDir": {"medium": "Memory", "sizeLimit": "1Mi"}}],
                "containers": [{"name": "server", "image": IMAGE, "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "printf KBORG_FORWARD_" + suffix.upper() + " > /www/index.html; printf 'INFO KBORG_LOG_INFO\\nERROR KBORG_LOG_ERROR\\nWARN KBORG_LOG_WARNING\\n'; exec httpd -f -p 8080 -h /www"],
                    "volumeMounts": [{"name": "www", "mountPath": "/www"}],
                    "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                    "resources": {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "100m", "memory": "32Mi"}}}]}})
        kubectl("create", "-f", "-", value={"apiVersion": "v1", "kind": "List", "items": resources})
        kubectl("-n", namespace, "wait", "--for=condition=Ready", "pod", "--all", "--timeout=120s")
        token = kubectl("-n", namespace, "create", "token", "operator", "--duration=10m").strip()
        with tempfile.TemporaryDirectory(prefix="kborg-forward-ui-") as temporary:
            temporary = Path(temporary).resolve()
            scoped = temporary / "operator.kubeconfig"
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": args.context,
                    "clusters": [{"name": "local", "cluster": cluster}],
                    "users": [{"name": "operator", "user": {"token": token}}],
                    "contexts": [{"name": args.context, "context": {"cluster": "local", "user": "operator", "namespace": namespace}}]}, stream)
            token = cluster_config = None
            environment = os.environ.copy()
            environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(temporary / "app"))
            environment.pop("NO_COLOR", None)
            master, slave = pty.openpty()
            original = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
            screen = Screen(140, 40)
            process = _spawn([str(binary), "--kubeconfig", str(scoped), "--context", args.context, "--namespace", namespace,
                "--write", "--allow-forward", "--allow-logs", "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"], environment, slave)

            def drain(seconds=.1):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    if select.select([master], [], [], min(.05, max(0, until - time.monotonic())))[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            break
                        if not data:
                            break
                        screen.feed(data)

            def wait(expected, timeout=25):
                until = time.monotonic() + timeout
                while time.monotonic() < until:
                    drain()
                    if expected in screen.text():
                        drain(.25)
                        return
                    if process.poll() is not None:
                        break
                raise RuntimeError("Forward UI did not reach expected state: " + expected + "\n" + screen.text())

            def keys(value, expected=None):
                os.write(master, value)
                if expected:
                    wait(expected)
                else:
                    drain(.3)

            def idle():
                until = time.monotonic() + 20
                while time.monotonic() < until:
                    drain()
                    text = screen.text()
                    if "LOADING" not in text.splitlines()[2] and "Request in progress" not in text.splitlines()[-1]:
                        drain(.3)
                        return
                raise RuntimeError("Forward UI did not become idle")

            def capture(name):
                path = screenshots / (name + ".png")
                screen.png(path)
                captures.append({"name": name, "path": str(path), "width": screen.width, "height": screen.height})

            def free_ports():
                sockets = [socket.socket() for _ in range(2)]
                try:
                    for sock in sockets:
                        sock.bind(("127.0.0.1", 0))
                    return [sock.getsockname()[1] for sock in sockets]
                finally:
                    for sock in sockets:
                        sock.close()

            def request(port, marker):
                with socket.create_connection(("127.0.0.1", port), 4) as sock:
                    sock.sendall(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                    received = b""
                    while len(received) < 8192:
                        part = sock.recv(8192)
                        if not part:
                            break
                        received += part
                if marker.encode() not in received:
                    raise RuntimeError("Forward returned an unexpected fixture response")

            def closed(port):
                until = time.monotonic() + 5
                while time.monotonic() < until:
                    try:
                        with socket.create_connection(("127.0.0.1", port), .2):
                            pass
                    except OSError:
                        return
                    drain(.1)
                raise RuntimeError("Forward listener remained open after cleanup")

            def hold_client(port):
                sock = socket.create_connection(("127.0.0.1", port), 4)
                sock.sendall(b"GET / HTTP/1.0\r\n")
                drain(.3)
                return sock

            def closed_client(sock):
                try:
                    if sock.recv(1):
                        raise RuntimeError("Canceled client unexpectedly received data")
                except ConnectionResetError:
                    pass

            def start(port, pod, expected="PORT FORWARDS /", capture_review=False):
                idle()
                keys(b"F", "PORT FORWARD / loopback")
                keys(b"\x15" + str(port).encode() + b"\t\x15" + b"8080\t\x15" + b"300")
                keys(b"\r", "REVIEW PORT FORWARD")
                if f"127.0.0.1:{port}" not in screen.text() or f"{namespace}/{pod}" not in screen.text():
                    raise RuntimeError("Forward review did not show complete expected target")
                if capture_review:
                    capture("forward-review")
                keys((namespace + "/" + pod).encode())
                keys(b"\r", expected)

            def log_exports():
                export_dir = temporary / "app" / "exports"
                keys(b"l", "KBORG_LOG_ERROR")
                wait("KBORG_LOG_WARNING")
                capture("logs-all-levels")
                keys(b"s", "PAUSED")
                keys(b"e")
                wait("KBORG_LOG_ERROR")
                title = screen.text().splitlines()[3]
                if "err" not in title.lower():
                    raise RuntimeError("Active error filter was not visible in the log title")
                if "KBORG_LOG_INFO" in screen.text() or "KBORG_LOG_WARNING" in screen.text():
                    raise RuntimeError("Error filter retained non-error application records")
                capture("log-errors-filter")
                before = set(export_dir.glob("*.log"))
                keys(b"\x13", "EXPORT / review local file")
                capture("log-export-review")
                keys(b"\x1b", "LOGS ")
                if set(export_dir.glob("*.log")) != before or "KBORG_LOG_ERROR" not in screen.text():
                    raise RuntimeError("Cancel changed exported files or lost the log source")
                checks.append("log export cancel writes nothing and restores paused filtered source")
                keys(b"\x13", "EXPORT / review local file")
                keys(b"\r", "EXPORT RESULT")
                wait("Saved ")
                capture("log-export-saved-result")
                keys(b"\x1b", "LOGS ")
                saved = set(export_dir.glob("*.log")) - before
                if len(saved) != 1:
                    raise RuntimeError("Log save did not create exactly one snapshot")
                path = saved.pop()
                content = path.read_text()
                if stat.S_IMODE(path.stat().st_mode) != 0o600:
                    raise RuntimeError("Log export was not a private file")
                if ("KBORG_LOG_ERROR" not in content or "KBORG_LOG_INFO" in content or "KBORG_LOG_WARNING" in content
                        or not re.search(r"\d{4}-\d{2}-\d{2}T[^ ]+Z .*KBORG_LOG_ERROR", content)
                        or namespace not in content or "server-a" not in content or args.context not in content):
                    raise RuntimeError("Log export lost its filtered records, timestamp or source attribution")
                if "KBORG_LOG_ERROR" not in screen.text() or "PAUSED" not in screen.text():
                    raise RuntimeError("Saving changed the paused source view")
                capture("log-export-saved")
                checks.append("log save preserves filtering/timestamps/source and creates a mode0600 file with visible success")
                keys(b"\x13", "EXPORT / review local file")
                section = screen.text().split("File (escaped):", 1)[1].split("Size:", 1)[0]
                destination = "".join(line.strip(" │") for line in section.splitlines())
                planned = Path(json.loads(destination))
                if planned.parent.resolve() != export_dir.resolve() or not re.fullmatch(r"logs-\d+-\d+\.log", planned.name):
                    raise RuntimeError("Export review did not expose its exact owned destination")
                descriptor = os.open(planned, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(b"EXISTING_EXPORT_UNCHANGED")
                keys(b"\r", "EXPORT RESULT")
                wait("cannot create file")
                capture("log-export-existing-file-refused")
                keys(b"\x1b", "LOGS ")
                if planned.read_bytes() != b"EXISTING_EXPORT_UNCHANGED" or path.read_text() != content:
                    raise RuntimeError("Export overwrote an existing destination")
                checks.append("existing destination is never overwritten and refusal is visible without losing source")
                keys(b"\x1b")
                idle()

            a, b = free_ports()
            try:
                wait("server-a")
                idle()
                log_exports()
                start(a, "server-a", capture_review=True)
                wait("LISTENING")
                request(a, "KBORG_FORWARD_A")
                keys(b"\x1b")
                keys(b"j")
                start(b, "server-b")
                wait("2/8 active")
                request(a, "KBORG_FORWARD_A")
                request(b, "KBORG_FORWARD_B")
                checks.append("two independent reviewed listeners route to distinct live Pods")
                capture("two-active-forwards")
                keys(b"\x1b")
                start(a, "server-b", expected="reserved by another forward")
                request(a, "KBORG_FORWARD_A")
                request(b, "KBORG_FORWARD_B")
                checks.append("collision rejected without disrupting either listener")
                keys(b":pf\r", "2/8 active")
                with hold_client(b) as held:
                    keys(b"s", "1/8 active")
                    closed(b)
                    closed_client(held)
                request(a, "KBORG_FORWARD_A")
                checks.append("selected stop closes listener/active client and preserves other listener")
                capture("selected-forward-stopped")
                start(b, "server-b")
                wait("2/8 active")
                request(b, "KBORG_FORWARD_B")
                checks.append("cleaned port can be reviewed and reused")
                keys(b"\x1b")
                with hold_client(a) as held_a, hold_client(b) as held_b:
                    keys(b":pods " + namespace.encode() + b" /server-a\r")
                    closed(a)
                    closed(b)
                    closed_client(held_a)
                    closed_client(held_b)
                idle()
                keys(b":pf\r", "0/8 active")
                capture("scope-change-stopped-all")
                checks.append("resource filter scope change closes all listeners and active clients")
                keys(b"\x1b")
                start(a, "server-a")
                wait("1/8 active")
                request(a, "KBORG_FORWARD_A")
                keys(b"\x1b")
                with hold_client(a) as held:
                    keys(b":q\r")
                    until = time.monotonic() + 8
                    while process.poll() is None and time.monotonic() < until:
                        drain()
                    if process.poll() != 0:
                        raise RuntimeError("Forward UI did not exit successfully")
                    closed(a)
                    closed_client(held)
                if termios.tcgetattr(master) != original:
                    raise RuntimeError("Forward UI did not restore terminal mode")
                checks.append("normal exit closes listener/active client and restores terminal")
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
            current = json.loads(kubectl("get", "namespace", namespace, "-o", "json"))
            if current["metadata"]["uid"] != created_uid or current["metadata"].get("labels", {}).get("kborg.dev/run-id") != run_id:
                raise RuntimeError("Forward fixture cleanup ownership changed")
            kubectl("delete", "namespace", namespace, "--wait=true", "--timeout=90s")
            cleaned = True
        report = {"passed": passed, "context": args.context, "namespace": namespace,
            "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "elapsed_seconds": round(time.monotonic() - started, 2),
            "checks": checks, "captures": captures, "fixture_cleanup": cleaned}
        (screenshots / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({key: report[key] for key in ("passed", "context", "checks", "fixture_cleanup")}))


if __name__ == "__main__":
    main()
