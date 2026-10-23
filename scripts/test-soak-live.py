#!/usr/bin/env python3
"""Bounded real-cluster soak using exclusively owned, namespace-limited fixtures.

Runs resource-watch JSONL, dashboard JSONL and the actual dashboard TUI together.
No raw transcript, kubeconfig or bearer token is persisted in the evidence.
The optional Docker pause fault requires a task-owned minikube profile, never
Docker Desktop. Its exact container ID and minikube labels are rechecked.
"""
import argparse
import base64
import datetime
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import runpy
import select
import shutil
import signal
import statistics
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


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def atomic_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def cpu_seconds(value):
    days, _, rest = value.partition("-")
    if not rest:
        rest, days = days, "0"
    fields = [float(part) for part in rest.split(":")]
    total = 0.0
    for field in fields:
        total = total * 60 + field
    return int(days) * 86400 + total


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--kubeconfig", required=True, type=Path)
    parser.add_argument("--context", required=True)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--seconds", type=int, default=7200)
    parser.add_argument("--mutation-seconds", type=int, default=60)
    parser.add_argument("--sample-seconds", type=int, default=15)
    parser.add_argument("--pause-minikube-profile")
    parser.add_argument("--pause-at-seconds", type=int, default=1800)
    parser.add_argument("--pause-seconds", type=int, default=45)
    args = parser.parse_args()
    require(30 <= args.seconds <= 86400, "duration must be 30..86400 seconds")
    require(5 <= args.mutation_seconds <= 600, "mutation interval must be 5..600 seconds")
    require(1 <= args.sample_seconds <= 60, "sample interval must be 1..60 seconds")
    require(20 <= args.pause_seconds <= 120, "fault duration must be 20..120 seconds")
    require(not args.pause_minikube_profile or (
        args.pause_minikube_profile == args.context and args.context.startswith("kborg-")
        and "desktop" not in args.context.lower()
        and 10 <= args.pause_at_seconds < args.seconds - args.pause_seconds - 30),
        "pause fault needs an explicitly owned kborg-* minikube context and recovery time")
    original_binary = args.binary.resolve(strict=True)
    config = args.kubeconfig.resolve(strict=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    namespace = "kborg-soak-" + uuid.uuid4().hex[:12]
    run_id = uuid.uuid4().hex
    report = {"schema": 1, "passed": False, "started_utc": utc(),
              "context": args.context, "namespace": namespace, "run_id": run_id,
              "requested_seconds": args.seconds, "binary_sha256": hashlib.sha256(original_binary.read_bytes()).hexdigest(),
              "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "checks": [], "captures": [], "mutations": [], "fault": None,
              "limits": {"rss_per_process_kib": 262144, "rss_growth_kib": 65536,
                         "total_output_per_process_bytes": 536870912, "json_record_bytes": 16777216,
                         "average_cpu_core_percent_after_30_seconds": 25},
              "claims_excluded": ["429 throttling", "credential/token rotation", "certificate rotation",
                                  "organizational production pilot", "native terminal font rendering"]}
    owner = None
    sessions = {}
    paused_container = None
    start = None
    screen = None
    sample_rows = []
    expected = 1
    mutation_due = None
    convergence_deadline = None

    def kube(*words, payload=None, timeout=40):
        process = subprocess.run(["kubectl", "--kubeconfig", str(config), "--context", args.context,
                                  "--request-timeout=20s", *words],
                                 input=None if payload is None else json.dumps(payload),
                                 text=True, capture_output=True, timeout=timeout)
        require(process.returncode == 0, "owned fixture kubectl operation failed: " + words[0])
        return process.stdout

    def identity():
        current = json.loads(kube("get", "namespace", namespace, "-o", "json"))
        require(current["metadata"]["uid"] == owner and
                current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run_id,
                "owned namespace identity changed; refusing mutation or cleanup")

    def pod(name, pending=False):
        item = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": name, "namespace": namespace,
                "labels": {"kborg.dev/run-id": run_id}}, "spec": {
                "automountServiceAccountToken": False, "terminationGracePeriodSeconds": 0,
                "securityContext": {"runAsNonRoot": True, "runAsUser": 1000,
                                    "seccompProfile": {"type": "RuntimeDefault"}},
                "containers": [{"name": "app", "image": IMAGE, "imagePullPolicy": "IfNotPresent",
                    "command": ["sh", "-c", "exec sleep 100000"],
                    "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
                                        "capabilities": {"drop": ["ALL"]}},
                    "resources": {"requests": {"cpu": "5m", "memory": "8Mi"},
                                  "limits": {"cpu": "50m", "memory": "24Mi"}}}]}}
        if pending:
            item["spec"]["schedulingGates"] = [{"name": "kborg.dev/soak-owned"}]
        return item

    def inspect_container(container_id=None):
        result = subprocess.run(["docker", "inspect", container_id or args.pause_minikube_profile],
                                capture_output=True, text=True, timeout=15)
        require(result.returncode == 0, "owned minikube container unavailable")
        item = json.loads(result.stdout)[0]
        labels = item["Config"]["Labels"]
        require(item["Name"] == "/" + args.pause_minikube_profile
                and labels.get("created_by.minikube.sigs.k8s.io") == "true"
                and labels.get("name.minikube.sigs.k8s.io") == args.pause_minikube_profile,
                "Docker fault refused: container identity is not the explicit minikube fixture")
        return item

    def snapshot_progress(state):
        elapsed = 0 if start is None else time.monotonic() - start
        atomic_json(output / "progress.json", {
            "state": state, "context": args.context, "namespace": namespace,
            "binary_sha256": report["binary_sha256"], "requested_seconds": args.seconds,
            "elapsed_seconds": round(elapsed, 3), "updated_utc": utc(),
            "samples": len(sample_rows), "mutation_count": len(report["mutations"]),
            "fault": report["fault"], "processes": {
                name: {"pid": item["process"].pid, "alive": item["process"].poll() is None,
                       "output_bytes": item["bytes"], "records": item["records"],
                       "observed_counts": sorted(item["counts"]), "stale_records": item["stale"]}
                for name, item in sessions.items()}})

    def consume(name, data):
        item = sessions[name]
        item["bytes"] += len(data)
        require(item["bytes"] <= report["limits"]["total_output_per_process_bytes"],
                name + " exceeded bounded output budget")
        require(b"panicked at" not in data and b"stack backtrace" not in data, name + " panic output")
        if name == "tui":
            screen.feed(data)
            require(len(screen.pending) <= 1024 * 1024, "terminal parser pending bytes exceeded limit")
            return
        item["pending"].extend(data)
        require(len(item["pending"]) <= report["limits"]["json_record_bytes"], name + " record exceeded budget")
        while b"\n" in item["pending"]:
            line, _, rest = item["pending"].partition(b"\n")
            item["pending"][:] = rest
            if not line:
                continue
            value = json.loads(line)
            item["records"] += 1
            item["last_record"] = time.monotonic()
            if name == "resource":
                require(not value["terminal"] and not value["truncated"], "resource watch terminated or truncated")
                objects = value["items"]
                require(all(obj["metadata"]["namespace"] == namespace for obj in objects), "watch escaped namespace")
                stale = value["stale"]
                count = len(objects)
            else:
                require(len(value) == 1 and value[0]["context"] == args.context, "dashboard context changed")
                row = value[0]
                require(row["namespace"] == namespace or (row["namespace"] == "" and row["state"] == "connecting"), "dashboard namespace changed")
                stale = row["state"] != "live"
                count = row["pods"]
                if stale:
                    require(row["health"] is None and count is None, "stale dashboard presented health counts")
                else:
                    health = row["health"]
                    in_fault_recovery = report["fault"] is not None and not report["fault"]["recovered"]
                    require(row["namespace"] == namespace and health["ready"] + health["unready"] == 1
                            and (in_fault_recovery or health["ready"] == 1), "dashboard live readiness is incorrect")
                    item["last_ready"] = health["ready"]
                    require(row["health"]["pending"] + row["health"]["terminating"] == count - 1, "dashboard pending/terminating count is incorrect")
            if stale:
                item["stale"] += 1
            else:
                require(count in (1, 2), "unexpected fixture count")
                item["counts"].add(count)
                item["last_count"] = count
                item["last_live"] = time.monotonic()

    def drain(seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            descriptors = {item["fd"]: name for name, item in sessions.items()}
            ready, _, _ = select.select(list(descriptors), [], [], min(.2, max(0, deadline - time.monotonic())))
            for fd in ready:
                try:
                    data = os.read(fd, 262144)
                except OSError as error:
                    if error.errno == errno.EIO:
                        data = b""
                    else:
                        raise
                require(data, descriptors[fd] + " ended during soak")
                consume(descriptors[fd], data)
            for name, item in sessions.items():
                require(item["process"].poll() is None, name + " process exited during soak")

    def tui_converged():
        visible = screen.text()
        return (namespace in visible and "READ ONLY" in visible and "LIVE / observed" in visible
                and re.search(r"\b" + str(expected) + r" Pods?\s+1 ready\s+" + str(expected - 1) + r" attention", visible)
                and re.search(r"\b" + str(expected - 1) + r" pending\s+0 failed", visible))

    def capture(name):
        require("CONTEXT OVERVIEW" in screen.text() and namespace in screen.text()
                and "READ ONLY" in screen.text(), "dashboard capture lost scope or mode")
        path = output / (name + ".png")
        screen.png(path)
        report["captures"].append({"name": name, "path": str(path), "columns": 120, "rows": 32})

    snapshot_progress("setting-up")
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-soak-private-") as temporary:
            temporary = Path(temporary).resolve()
            binary = temporary / "kborg"
            shutil.copyfile(original_binary, binary)
            binary.chmod(0o700)
            require(hashlib.sha256(binary.read_bytes()).hexdigest() == report["binary_sha256"], "binary changed while sealing")
            initial_context = json.loads(kube("config", "view", "-o", "json")).get("current-context", "")
            report["configured_current_context_before"] = initial_context
            current = json.loads(kube("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
            cluster = current["clusters"][0]["cluster"]
            require(cluster["server"].startswith("https://") and cluster.get("certificate-authority-data")
                    and not cluster.get("insecure-skip-tls-verify"), "verified cluster TLS is required")
            report["kubernetes_version"] = json.loads(kube("version", "-o", "json"))["serverVersion"]["gitVersion"]
            created = json.loads(kube("create", "-f", "-", "-o", "json", payload={
                "apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace,
                "labels": {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}}}))
            owner = created["metadata"]["uid"]
            report["fixture_namespace_uid"] = owner
            resources = [pod("steady"),
                {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": namespace, "name": "observer"},
                 "automountServiceAccountToken": False},
                {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": namespace, "name": "observer"},
                 "rules": [{"apiGroups": [""], "resources": ["pods", "events"], "verbs": ["get", "list", "watch"]}]},
                {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": namespace, "name": "observer"},
                 "subjects": [{"kind": "ServiceAccount", "namespace": namespace, "name": "observer"}],
                 "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "observer"}}]
            kube("create", "-f", "-", payload={"apiVersion": "v1", "kind": "List", "items": resources})
            kube("-n", namespace, "wait", "--for=condition=Ready", "pod/steady", "--timeout=120s", timeout=130)
            steady = json.loads(kube("-n", namespace, "get", "pod", "steady", "-o", "json"))
            token = kube("-n", namespace, "create", "token", "observer", "--duration=" + str(args.seconds + 600) + "s").strip()
            encoded_claims = token.split(".")[1]
            claims = json.loads(base64.urlsafe_b64decode(encoded_claims + "=" * (-len(encoded_claims) % 4)))
            token_remaining = int(claims["exp"]) - int(time.time())
            require(token_remaining >= args.seconds + 120, "issued test token does not span the requested soak duration")
            report["issued_token_remaining_seconds_at_start"] = token_remaining
            claims = encoded_claims = None
            scoped = temporary / "readonly.kubeconfig"
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": args.context,
                    "clusters": [{"name": args.context, "cluster": cluster}],
                    "users": [{"name": "observer", "user": {"token": token}}],
                    "contexts": [{"name": args.context, "context": {"cluster": args.context, "user": "observer", "namespace": namespace}}]}, stream)
            current = cluster = token = None
            environment = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8",
                               KBORG_CONFIG_DIR=str(temporary / "preferences"))
            environment.pop("NO_COLOR", None)
            base = [str(binary), "--kubeconfig", str(scoped), "--context", args.context, "--namespace", namespace]
            for name, arguments in (("resource", ["--json", "watch", "pods"]),
                                    ("dashboard", ["--json", "--follow", "overview", args.context])):
                process = subprocess.Popen(base + arguments, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=environment)
                sessions[name] = {"process": process, "fd": process.stdout.fileno(), "bytes": 0, "records": 0,
                                  "pending": bytearray(), "counts": set(), "stale": 0,
                                  "last_record": time.monotonic(), "last_live": 0, "last_count": None}
            helpers = runpy.run_path(str(ROOT / "scripts/test-live-lifecycle-ui.py"))
            screen = helpers["Screen"](120, 32)
            master, slave = pty.openpty()
            original_mode = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 32, 120, 0, 0))
            process = helpers["_spawn"](base + ["--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"], environment, slave)
            sessions["tui"] = {"process": process, "fd": master, "slave": slave, "bytes": 0, "records": 0,
                               "counts": set(), "stale": 0, "original_mode": original_mode}
            deadline = time.monotonic() + 45
            while not ("steady" in screen.text() and all(sessions[name]["last_count"] == 1 for name in ("resource", "dashboard"))):
                require(time.monotonic() < deadline, "initial resource watches did not converge")
                drain(.2)
            deadline = time.monotonic() + 30
            next_dashboard_key = 0
            while "CONTEXT OVERVIEW" not in screen.text() or "PODS READY" not in screen.text():
                require(time.monotonic() < deadline, "initial dashboard did not converge")
                if "CONTEXT OVERVIEW" not in screen.text() and time.monotonic() >= next_dashboard_key:
                    os.write(master, b"D")
                    next_dashboard_key = time.monotonic() + 1
                drain(.2)
            capture("soak-start-dashboard")
            report["checks"].append("Three continuous read-only processes converged in the explicit fixture namespace")
            start = time.monotonic()
            report["continuous_started_utc"] = utc()
            next_sample = start
            mutation_due = start + min(5, args.mutation_seconds)
            pause_due = start + args.pause_at_seconds if args.pause_minikube_profile else None
            resume_due = None
            recovered = False
            with (output / "samples.jsonl").open("w") as sample_file:
                while time.monotonic() - start < args.seconds:
                    drain(.2)
                    now = time.monotonic()
                    elapsed = now - start
                    if pause_due is not None and now >= pause_due:
                        identity()
                        container = inspect_container()
                        require(not container["State"].get("Paused"), "fixture already paused")
                        paused_container = container["Id"]
                        subprocess.run(["docker", "pause", paused_container], check=True, stdout=subprocess.DEVNULL, timeout=20)
                        report["fault"] = {"kind": "pause-owned-minikube-container", "container_id": paused_container,
                                           "start_seconds": round(time.monotonic() - start, 3), "requested_pause_seconds": args.pause_seconds,
                                           "recovery_settle_seconds": 20,
                                           "resource_stale_before": sessions["resource"]["stale"],
                                           "dashboard_stale_before": sessions["dashboard"]["stale"], "recovered": False}
                        resume_due = time.monotonic() + args.pause_seconds
                        pause_due = None
                        convergence_deadline = None
                        snapshot_progress("fault-in-progress")
                    if resume_due is not None and now >= resume_due:
                        inspect_container(paused_container)
                        subprocess.run(["docker", "unpause", paused_container], check=True, stdout=subprocess.DEVNULL, timeout=20)
                        report["fault"]["resumed_seconds"] = round(time.monotonic() - start, 3)
                        report["fault"]["actual_pause_seconds"] = round(report["fault"]["resumed_seconds"] - report["fault"]["start_seconds"], 3)
                        report["fault"]["resource_stale_observed"] = sessions["resource"]["stale"] > report["fault"]["resource_stale_before"]
                        report["fault"]["dashboard_stale_observed"] = sessions["dashboard"]["stale"] > report["fault"]["dashboard_stale_before"]
                        paused_container = None
                        resume_due = None
                        convergence_deadline = time.monotonic() + 90
                        mutation_due = time.monotonic() + 95
                    if (report["fault"] and "resumed_seconds" in report["fault"] and not recovered
                            and all(sessions[name]["last_live"] > start + report["fault"]["resumed_seconds"]
                                    and sessions[name]["last_count"] == expected for name in ("resource", "dashboard"))
                            and sessions["dashboard"].get("last_ready") == 1
                            and now - start - report["fault"]["resumed_seconds"] >= 20 and tui_converged()):
                        report["fault"]["recovery_seconds"] = round(now - start - report["fault"]["resumed_seconds"], 3)
                        report["fault"]["recovered"] = recovered = True
                        capture("soak-recovered-dashboard")
                    if resume_due is None and now >= mutation_due and convergence_deadline is None:
                        identity()
                        if expected == 1:
                            pending = json.loads(kube("create", "-f", "-", "-o", "json", payload=pod("changing", pending=True)))
                            pending_uid = pending["metadata"]["uid"]
                            expected = 2
                        else:
                            pending = json.loads(kube("-n", namespace, "get", "pod", "changing", "-o", "json"))
                            require(pending["metadata"]["uid"] == pending_uid and pending["metadata"]["labels"]["kborg.dev/run-id"] == run_id,
                                    "changing Pod identity changed; refusing deletion")
                            kube("-n", namespace, "delete", "pod", "changing", "--grace-period=0", "--wait=false")
                            expected = 1
                        stamp = utc().replace("+00:00", "Z")
                        event = {"apiVersion": "v1", "kind": "Event", "metadata": {"name": "soak-warning", "namespace": namespace,
                                 "labels": {"kborg.dev/run-id": run_id}}, "type": "Warning", "reason": "SoakFixtureWarning",
                                 "message": "owned soak fixture", "count": len(report["mutations"]) + 1,
                                 "firstTimestamp": stamp, "lastTimestamp": stamp, "source": {"component": "kborg-soak"},
                                 "involvedObject": {"apiVersion": "v1", "kind": "Pod", "namespace": namespace,
                                                    "name": "steady", "uid": steady["metadata"]["uid"]}}
                        kube("apply", "-f", "-", payload=event)
                        report["mutations"].append({"elapsed_seconds": round(time.monotonic() - start, 3), "expected_pods": expected})
                        convergence_deadline = time.monotonic() + 30
                        mutation_due = time.monotonic() + args.mutation_seconds
                    if convergence_deadline is not None:
                        if (all(sessions[name]["last_count"] == expected and sessions[name]["last_live"] > now - 20
                                for name in ("resource", "dashboard")) and tui_converged()):
                            convergence_deadline = None
                        else:
                            require(now < convergence_deadline, "resource/dashboard watches did not reconverge")
                    if now >= next_sample:
                        row = {"elapsed_seconds": round(elapsed, 3), "utc": utc(), "processes": {}}
                        for name, item in sessions.items():
                            result = subprocess.run(["ps", "-p", str(item["process"].pid), "-o", "rss=,pcpu=,time="], capture_output=True, text=True, timeout=10)
                            fields = result.stdout.strip().split()
                            require(result.returncode == 0 and len(fields) == 3, "process resource sample unavailable")
                            rss, pcpu, cpu = int(fields[0]), float(fields[1]), cpu_seconds(fields[2])
                            require(rss < report["limits"]["rss_per_process_kib"], name + " RSS exceeded budget")
                            if elapsed >= 30:
                                require(cpu / elapsed * 100 < 25, name + " average CPU exceeded 25% of one core")
                            row["processes"][name] = {"rss_kib": rss, "cpu_percent": pcpu, "cpu_seconds": cpu,
                                                       "output_bytes": item["bytes"], "records": item["records"]}
                        sample_rows.append(row)
                        sample_file.write(json.dumps(row) + "\n")
                        sample_file.flush()
                        snapshot_progress("running")
                        next_sample = time.monotonic() + args.sample_seconds
            report["actual_continuous_seconds"] = round(time.monotonic() - start, 3)
            report["continuous_finished_utc"] = utc()
            require(report["actual_continuous_seconds"] >= args.seconds, "continuous duration was short")
            for name in ("resource", "dashboard"):
                require(sessions[name]["counts"] == {1, 2}, "owned mutations did not reach " + name)
            if args.pause_minikube_profile:
                require(report["fault"] and report["fault"]["recovered"] and report["fault"]["resource_stale_observed"]
                        and report["fault"]["dashboard_stale_observed"], "fault did not show stale and recovered evidence")
            report["processes"] = {}
            for name, item in sessions.items():
                rows = [row["processes"][name] for row in sample_rows]
                quarter = max(1, len(rows) // 4)
                early = statistics.median(row["rss_kib"] for row in rows[:quarter])
                late = statistics.median(row["rss_kib"] for row in rows[-quarter:])
                require(late - early < report["limits"]["rss_growth_kib"], name + " sustained RSS growth exceeded budget")
                report["processes"][name] = {"samples": len(rows), "rss_peak_kib": max(row["rss_kib"] for row in rows),
                    "rss_early_quarter_median_kib": early, "rss_late_quarter_median_kib": late,
                    "rss_growth_kib": late - early, "cpu_peak_percent": max(row["cpu_percent"] for row in rows),
                    "cpu_last_sample_seconds": rows[-1]["cpu_seconds"], "output_bytes": item["bytes"],
                    "records": item["records"], "observed_counts": sorted(item["counts"]), "stale_records": item["stale"]}
            deadline = time.monotonic() + 30
            while not tui_converged():
                require(time.monotonic() < deadline, "final TUI health counts did not converge")
                drain(.2)
            capture("soak-final-dashboard")
            report["checks"].extend(["Continuous requested duration completed without process exit or panic",
                "Owned periodic Pod additions/deletions reached resource and dashboard watches and final TUI counts",
                "RSS stayed below 256 MiB per process and quarter-median growth below 64 MiB",
                "Average CPU stayed below 25% of one core per process after the first 30 seconds",
                "Output and JSON-record budgets held without retaining raw transcripts"])
            os.write(master, b"\x1b")
            deadline = time.monotonic() + 15
            while "CONTEXT OVERVIEW" in screen.text() or "steady" not in screen.text():
                require(time.monotonic() < deadline, "dashboard did not close to resource view")
                drain(.2)
            os.write(master, b"q")
            deadline = time.monotonic() + 10
            while process.poll() is None:
                require(time.monotonic() < deadline, "TUI quit timed out")
                if select.select([master], [], [], .1)[0]:
                    try:
                        data = os.read(master, 262144)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        data = b""
                    if data:
                        consume("tui", data)
            process.wait(timeout=1)
            require(process.returncode == 0 and termios.tcgetattr(master) == original_mode,
                    "TUI exit failed or terminal attributes were not restored")
            sessions["tui"]["intentional_exit"] = True
            report["checks"].append("Dashboard closed and quit successfully with exact terminal attributes restored")
            report["configured_current_context_after"] = json.loads(kube("config", "view", "-o", "json")).get("current-context", "")
            require(report["configured_current_context_after"] == initial_context, "configured current context changed")
            report["passed"] = True
    except BaseException as error:
        # Exception messages are ours or parser/type errors; raw subprocess stderr
        # and authentication material are deliberately never included.
        report["failure"] = {"type": type(error).__name__, "message": str(error)[:500]}
        if screen is not None:
            try:
                screen.png(output / "soak-failure-screen.png")
            except Exception:
                pass
        report["actual_continuous_seconds"] = 0 if start is None else round(time.monotonic() - start, 3)
        atomic_json(output / "report.json", report)
    finally:
        if paused_container:
            try:
                inspect_container(paused_container)
                subprocess.run(["docker", "unpause", paused_container], check=True, stdout=subprocess.DEVNULL, timeout=20)
                report["fault_emergency_unpause"] = True
            except Exception:
                report["fault_emergency_unpause"] = False
                report["passed"] = False
        for name, item in sessions.items():
            process = item["process"]
            if name == "tui":
                os.close(item["fd"])
                os.close(item["slave"])
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        report["passed"] = False
                        report["process_cleanup_failure"] = name
            if name != "tui":
                process.stdout.close()
        report["fixture_cleanup"] = owner is None
        if owner:
            try:
                identity()
                kube("delete", "namespace", namespace, "--wait=true", "--timeout=90s", timeout=100)
                report["fixture_cleanup"] = True
            except Exception:
                report["fixture_cleanup"] = False
                report["passed"] = False
        report["finished_utc"] = utc()
        atomic_json(output / "report.json", report)
        snapshot_progress("passed" if report["passed"] else "failed")
    print(json.dumps({"passed": report["passed"], "context": args.context,
                      "seconds": report.get("actual_continuous_seconds", 0), "report": str(output / "report.json")}))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
