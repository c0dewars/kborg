#!/usr/bin/env python3
"""Read-only dashboard acceptance on owned Docker Desktop/minikube fixtures.

The app sees only short-lived namespace-limited service accounts. Setup/cleanup
use the supplied local cluster credentials. No token or raw config is recorded.
"""
import argparse
import datetime
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

ROOT = Path(__file__).resolve().parent.parent
IMAGE = "busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e"


def require(value, message):
    if not value:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--desktop-config", type=Path, required=True)
    parser.add_argument("--minikube-config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    namespace = "kborg-dashboard-" + uuid.uuid4().hex[:12]
    run_id = uuid.uuid4().hex
    contexts = ("docker-desktop", "kborg-dashboard")
    configs = (args.desktop_config.resolve(strict=True), args.minikube_config.resolve(strict=True))
    owners = {}
    report = {"schema": 1, "passed": False, "namespace": namespace,
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "source_sha256": {str(path.relative_to(ROOT)): hashlib.sha256(path.read_bytes()).hexdigest()
                                for path in (ROOT / "src/overview.rs", ROOT / "src/dashboard_health.rs", Path(__file__).resolve())},
              "checks": [], "snapshots": {}, "captures": [], "fixture_cleanup": {}}

    def kube(index, *words, payload=None, timeout=90):
        result = subprocess.run(["kubectl", "--kubeconfig", str(configs[index]), "--context", contexts[index],
                                 "--request-timeout=20s", *words],
                                input=json.dumps(payload) if payload is not None else None,
                                capture_output=True, text=True, timeout=timeout)
        require(result.returncode == 0, "owned fixture kubectl operation failed: " + words[0])
        return result.stdout

    def pod(name, mode):
        item = {"apiVersion": "v1", "kind": "Pod", "metadata": {"namespace": namespace, "name": name},
                "spec": {"automountServiceAccountToken": False, "terminationGracePeriodSeconds": 0,
                         "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000,
                                             "seccompProfile": {"type": "RuntimeDefault"}},
                         "containers": [{"name": "app", "image": IMAGE, "imagePullPolicy": "IfNotPresent",
                                         "command": ["sh", "-c", "exec sleep 3600"],
                                         "securityContext": {"allowPrivilegeEscalation": False,
                                                             "readOnlyRootFilesystem": True,
                                                             "capabilities": {"drop": ["ALL"]}},
                                         "resources": {"requests": {"cpu": "5m", "memory": "8Mi"},
                                                       "limits": {"cpu": "50m", "memory": "24Mi"}}}]}}
        if mode == "unready":
            item["spec"]["volumes"] = [{"name": "state", "emptyDir": {"medium": "Memory", "sizeLimit": "1Mi"}}]
            item["spec"]["containers"][0].update(
                volumeMounts=[{"name": "state", "mountPath": "/state"}],
                readinessProbe={"exec": {"command": ["sh", "-c", "test -e /state/ready"]},
                                "periodSeconds": 2, "timeoutSeconds": 1})
        if mode == "pending":
            item["spec"]["nodeName"] = "kborg-owned-fixture-absent-node"
        return item

    def event(name, uid, reason, ago=0):
        stamp = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=ago)).isoformat().replace("+00:00", "Z")
        return {"apiVersion": "v1", "kind": "Event", "metadata": {"namespace": namespace, "name": name},
                "type": "Warning", "reason": reason, "message": "owned dashboard fixture",
                "count": 1, "firstTimestamp": stamp, "lastTimestamp": stamp,
                "source": {"component": "kborg-acceptance"},
                "involvedObject": {"apiVersion": "v1", "kind": "Pod", "namespace": namespace,
                                   "name": "ready", "uid": uid}}

    try:
        with tempfile.TemporaryDirectory(prefix="kborg-dashboard-health-") as temporary:
            temporary = Path(temporary).resolve()
            scoped = temporary / "readonly.kubeconfig"
            combined = {"apiVersion": "v1", "kind": "Config", "current-context": contexts[0],
                        "clusters": [], "users": [], "contexts": []}
            for index, context in enumerate(contexts):
                current = json.loads(kube(index, "config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
                cluster = current["clusters"][0]["cluster"]
                require(cluster["server"].startswith("https://") and cluster.get("certificate-authority-data")
                        and not cluster.get("insecure-skip-tls-verify"), "fixture requires verified TLS")
                combined["clusters"].append({"name": context, "cluster": cluster})
                created = json.loads(kube(index, "create", "-f", "-", "-o", "json", payload={
                    "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace,
                    "labels": {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}}}))
                owners[index] = created["metadata"]["uid"]
                resources = [pod("ready", "ready"), pod("unready", "unready"), pod("pending", "pending")]
                accounts = [(context, ["pods", "events"])]
                if index == 0:
                    accounts += [("events-denied", ["pods"]), ("pods-denied", ["events"])]
                for account, allowed in accounts:
                    resources.extend([
                        {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": namespace, "name": account}, "automountServiceAccountToken": False},
                        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": namespace, "name": account},
                         "rules": [{"apiGroups": [""], "resources": allowed, "verbs": ["get", "list", "watch"]}]},
                        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": namespace, "name": account},
                         "subjects": [{"kind": "ServiceAccount", "namespace": namespace, "name": account}],
                         "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": account}}])
                kube(index, "create", "-f", "-", payload={"apiVersion": "v1", "kind": "List", "items": resources})
                kube(index, "-n", namespace, "wait", "--for=condition=Ready", "pod/ready", "--timeout=120s", timeout=130)
                kube(index, "-n", namespace, "wait", "--for=jsonpath={.status.phase}=Running", "pod/unready", "--timeout=120s", timeout=130)
                ready = json.loads(kube(index, "-n", namespace, "get", "pod", "ready", "-o", "json"))
                uid = ready["metadata"]["uid"]
                kube(index, "create", "-f", "-", payload={"apiVersion": "v1", "kind": "List", "items": [
                    event("linked-warning", uid, "DashboardFixtureWarning"),
                    event("wrong-uid-warning", "00000000-0000-0000-0000-000000000000", "WrongUIDCanary"),
                    event("old-warning", uid, "OldWarningCanary", ago=3600)]})
                for account, _ in accounts:
                    token = kube(index, "-n", namespace, "create", "token", account, "--duration=10m").strip()
                    combined["users"].append({"name": account, "user": {"token": token}})
                    combined["contexts"].append({"name": account, "context": {"cluster": context, "user": account, "namespace": namespace}})
                token = current = None
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump(combined, stream)
            combined = None
            environment = dict(os.environ, KBORG_CONFIG_DIR=str(temporary / "preferences"))
            names = [*contexts, "events-denied", "pods-denied"]
            base = [str(binary), "--kubeconfig", str(scoped), "--namespace", namespace]

            def inspect(rows, recovered=False):
                mapped = {row["context"]: row for row in rows}
                require(set(mapped) == set(names), "selected contexts changed")
                for context in contexts:
                    row = mapped[context]
                    require(row["namespace"] == namespace and row["state"] == "live" and row["pods"] == 3,
                            "live scope or Pod counts are incorrect")
                    health = row["health"]
                    expected = (2, 0) if recovered and context == contexts[0] else (1, 1)
                    require((health["ready"], health["unready"]) == expected and health["pending"] == 1,
                            "readiness/phase counts are incorrect")
                    require(health["warnings"]["state"] == "live" and health["warnings"]["count"] >= 1,
                            "recent Warning evidence unavailable")
                    reasons = [item["reason"] for item in health["warnings"]["recent"]]
                    require("DashboardFixtureWarning" in reasons and not any("Canary" in reason for reason in reasons),
                            "Warning UID/recency attribution failed")
                    require(row["observed_unix_seconds"] is not None, "freshness timestamp absent")
                denied = mapped["events-denied"]
                require(denied["state"] == "live" and denied["pods"] == 3 and denied["health"]["warnings"]["state"] == "denied"
                        and denied["health"]["warnings"]["count"] is None, "Event denial spoiled Pod evidence or looked like healthy zero")
                denied = mapped["pods-denied"]
                require(denied["state"] == "denied" and denied["pods"] is None and denied["unhealthy"] is None
                        and denied["health"] is None, "Pod denial exposed stale or healthy-zero counts")
                return mapped

            result = subprocess.run(base + ["--json", "overview", *names], capture_output=True, text=True, env=environment, timeout=35)
            require(result.returncode == 0, "snapshot command failed")
            report["snapshots"]["initial"] = inspect(json.loads(result.stdout))
            report["checks"].extend([
                "Docker Desktop and minikube: explicit namespace, Ready=True, unready and pending counts",
                "Recent Warning Events match exact Pod UID and exclude old/unrelated UID evidence",
                "Event denial retains valid Pod evidence with unknown Warning count",
                "Pod denial produces no health/count values while independent peers remain live"])

            process = subprocess.Popen(base + ["--follow", "--json", "overview", *names], stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, env=environment)
            pending = bytearray()

            def follow_until(recovered=False, timeout=35):
                deadline = time.monotonic() + timeout
                while time.monotonic() < deadline:
                    if select.select([process.stdout], [], [], .2)[0]:
                        data = os.read(process.stdout.fileno(), 65536)
                        require(data, "follow process ended before complete evidence")
                        pending.extend(data)
                        require(len(pending) <= 1024 * 1024, "follow record exceeded budget")
                        while b"\n" in pending:
                            line, _, rest = pending.partition(b"\n")
                            pending[:] = rest
                            rows = json.loads(line)
                            try:
                                return inspect(rows, recovered)
                            except RuntimeError:
                                pass
                raise RuntimeError("independent live watches did not converge")

            try:
                report["snapshots"]["watch_initial"] = follow_until()
                kube(0, "-n", namespace, "exec", "unready", "--", "sh", "-c", "touch /state/ready")
                kube(0, "-n", namespace, "wait", "--for=condition=Ready", "pod/unready", "--timeout=30s")
                report["snapshots"]["watch_recovered"] = follow_until(recovered=True)
                report["checks"].append("Desktop readiness recovery updates its card while minikube and denied peers retain independent state")
            finally:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
                process.stdout.close()
            report["checks"].append("Closing the follow process releases all read-only watches promptly")

            if args.screenshots:
                helpers = runpy.run_path(str(ROOT / "scripts/test-live-lifecycle-ui.py"))
                Screen, spawn = helpers["Screen"], helpers["_spawn"]
                output = args.screenshots.resolve()
                output.mkdir(parents=True, exist_ok=True)
                master, slave = pty.openpty()
                original = termios.tcgetattr(slave)
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 44, 150, 0, 0))
                screen = Screen(150, 44)
                environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8")
                environment.pop("NO_COLOR", None)
                process = spawn(base + ["--context", contexts[0], "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"], environment, slave)

                def drain(seconds=.15):
                    deadline = time.monotonic() + seconds
                    while time.monotonic() < deadline:
                        if select.select([master], [], [], .03)[0]:
                            try:
                                data = os.read(master, 65536)
                            except OSError:
                                break
                            if not data:
                                break
                            screen.feed(data)

                def wait(*needles, timeout=30):
                    deadline = time.monotonic() + timeout
                    while time.monotonic() < deadline:
                        drain()
                        if all(needle.lower() in screen.text().lower() for needle in needles):
                            return
                        require(process.poll() is None, "TUI exited early")
                    raise RuntimeError("TUI did not render expected dashboard evidence")

                def capture(name):
                    # Do not capture the clear pass at the start of a repaint.
                    # Scope and mode are required in every saved dashboard frame.
                    wait("CONTEXT OVERVIEW", namespace, "READ ONLY")
                    path = output / (name + ".png")
                    screen.png(path)
                    report["captures"].append({"name": name, "path": str(path), "columns": 150, "rows": 44})

                try:
                    wait("PODS", "ready")
                    os.write(master, b":overview\r")
                    wait("Overview contexts")
                    os.write(master, b"\x7f" * len(contexts[0]) + ",".join(names).encode() + b"\r")
                    wait("CONTEXT OVERVIEW", "DashboardFixtureWarning", "Denied", "pods-denied")
                    require("docker-desktopdocker-desktop" not in screen.text() and "context not found" not in screen.text(),
                            "dashboard input did not preserve explicit contexts")
                    capture("live-dashboard")
                    os.write(master, b"3?")
                    wait("DASHBOARD KEYS")
                    capture("dashboard-keymap")
                    os.write(master, b"\x1b")
                    drain(.3)
                    os.write(master, b"\x1b")
                    wait("PODS", "ready")
                    os.write(master, b"q")
                    for _ in range(30):
                        if process.poll() is not None:
                            break
                        drain(.1)
                    require(process.poll() == 0, "dashboard exit did not complete")
                    require(termios.tcgetattr(master) == original, "TUI changed terminal attributes")
                    report["checks"].append("Actual live dashboard cards and keymap captured; terminal attributes restored on exit")
                finally:
                    if process.poll() is None:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)
                    os.close(master)
                    os.close(slave)
            report["passed"] = True
    finally:
        for index, uid in owners.items():
            try:
                current = json.loads(kube(index, "get", "namespace", namespace, "-o", "json"))
                require(current["metadata"]["uid"] == uid and current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run_id,
                        "fixture namespace ownership changed; refusing cleanup")
                kube(index, "delete", "namespace", namespace, "--wait=true", "--timeout=90s", timeout=100)
                report["fixture_cleanup"][contexts[index]] = True
            except Exception:
                report["fixture_cleanup"][contexts[index]] = False
                report["passed"] = False
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    require(report["passed"], "dashboard live acceptance did not pass or cleanup failed")
    print(json.dumps({"passed": True, "checks": len(report["checks"]), "report": str(args.output), "captures": len(report["captures"])}))


if __name__ == "__main__":
    main()
