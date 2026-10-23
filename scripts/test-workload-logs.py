#!/usr/bin/env python3
"""Owned workload-log acceptance on exact Docker Desktop/minikube endpoints.

Creates only unique, labelled fixture namespaces. KBorg receives a short-lived
namespace reader; fixture changes use the explicitly selected administrator
context. No current-context changes, raw logs or credentials enter the report.
Provider installation is separate. Do not run until the intended source freezes.
"""
import argparse
import base64
import hashlib
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
import uuid

sys.dont_write_bytecode = True
PROJECT = Path(__file__).resolve().parent.parent
_spec = importlib.util.spec_from_file_location("feature_harness", PROJECT / "scripts/test-helm-logs-metrics.py")
base = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(base)
execute, require = base.execute, base.require


def pod_spec(image, *, completed=False):
    container = {"name": "main", "image": image, "imagePullPolicy": "IfNotPresent",
                 "command": ["sh", "-c", "echo KBORG_CRONJOB" if completed else "while true; do echo KBORG_WORKLOAD_MAIN; sleep 1; done"],
                 "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                 "resources": {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "100m", "memory": "32Mi"}}}
    containers = [container]
    if not completed:
        sidecar = json.loads(json.dumps(container))
        sidecar.update(name="sidecar", command=["sh", "-c", "while true; do echo KBORG_WORKLOAD_SIDE; sleep 1; done"])
        containers.append(sidecar)
    return {"automountServiceAccountToken": False, "terminationGracePeriodSeconds": 1,
            "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
            "containers": containers, "restartPolicy": "Never" if completed else "Always"}


def follow_change(argv, environment, change, expected_members):
    """Run one bounded live follow; mutate the owned fixture after first data."""
    process = subprocess.Popen([str(arg) for arg in argv], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, env=environment)
    records, pending, errors = [], bytearray(), bytearray()
    changed = False
    total = 0
    started = time.monotonic()
    try:
        with selectors.DefaultSelector() as selected:
            selected.register(process.stdout, selectors.EVENT_READ, "stdout")
            selected.register(process.stderr, selectors.EVENT_READ, "stderr")
            while selected.get_map():
                require(time.monotonic() - started < 35, "workload follow exceeded its acceptance deadline")
                for event, _ in selected.select(0.2):
                    chunk = os.read(event.fileobj.fileno(), 65536)
                    if not chunk:
                        selected.unregister(event.fileobj)
                        continue
                    total += len(chunk)
                    require(total <= base.MAX_CAPTURE, "workload follow exceeded bounded capture")
                    if event.data == "stderr":
                        errors.extend(chunk)
                        continue
                    pending.extend(chunk)
                    while b"\n" in pending:
                        raw, _, remainder = pending.partition(b"\n")
                        pending = bytearray(remainder)
                        if not raw:
                            continue
                        record = json.loads(raw)
                        require(record.get("pod_scope") == "fixed-membership", "live follow lost fixed-membership output")
                        require(len(record["membership"]["pods"]) == expected_members, "initial workload membership count changed")
                        if records:
                            require(record["membership"] == records[0]["membership"], "live follow silently changed captured membership")
                        records.append(record)
                        if not changed and "KBORG_WORKLOAD" in record["line"]:
                            change(record["membership"])
                            require(process.poll() is None, "follow ended before its owned fixture change was confirmed")
                            changed = True
            process.wait(timeout=5)
        require(changed and records, "follow never delivered attributed fixture data")
        names = {pod["name"] for pod in records[0]["membership"]["pods"]}
        containers = {pod["name"]: set(pod["containers"]) for pod in records[0]["membership"]["pods"]}
        namespace = records[0]["membership"]["namespace"]
        for record in records:
            if record["line"].startswith("[Log stream "):
                continue
            match = re.search(r"^(?:\d{4}-\d\d-\d\dT[^ ]+ )?\[([a-z0-9-]+)/([a-z0-9.-]+):([a-z0-9-]+)\]", record["line"])
            require(match, "follow application record omitted its exact source prefix")
            if match:
                require(match[1] == namespace and match[2] in names, "follow admitted an uncaptured or foreign Pod")
                require(match[3] in containers[match[2]], "follow admitted an uncaptured container")
        return process.returncode, errors.decode(errors="replace"), round(time.monotonic() - started, 3)
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        process.stdout.close()
        process.stderr.close()


def target_run(target, args, root, kubectl, binary, image, report):
    context = "docker-desktop" if target == "desktop" else "kborg-validation"
    prefix = [kubectl]
    if target == "minikube":
        prefix += ["--kubeconfig", str(PROJECT / ".local/minikube.kubeconfig")]
    prefix += ["--context", context, "--request-timeout=30s"]

    def kube(*command, data=None, timeout=90):
        return execute([*prefix, *command], data=data, timeout=timeout)[1]

    config = json.loads(kube("config", "view", "--raw", "--minify", "--flatten", "-o", "json"))
    require(len(config.get("contexts", [])) == 1 and config["contexts"][0]["name"] == context, "explicit local context mismatch")
    cluster = config["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    expected = args.expected_desktop_server if target == "desktop" else args.expected_minikube_server
    local = endpoint.hostname in ("127.0.0.1", "localhost", "kubernetes.docker.internal")
    if target == "minikube" and not local:
        try:
            local = ipaddress.ip_address(endpoint.hostname).is_private
        except ValueError:
            local = False
    require(expected and cluster["server"] == expected and endpoint.scheme == "https" and local and endpoint.port
            and endpoint.port >= 1024 and not endpoint.username and not endpoint.password and endpoint.path in ("", "/")
            and not endpoint.query and not endpoint.fragment and not cluster.get("proxy-url")
            and not cluster.get("insecure-skip-tls-verify") and cluster.get("certificate-authority-data"), "exact local endpoint and verified CA required")
    ca = base64.b64decode(cluster["certificate-authority-data"], validate=True)
    require(0 < len(ca) <= 1024 * 1024, "CA bundle bound exceeded")
    run = uuid.uuid4().hex[:12]
    namespace, foreign = "kborg-workload-" + run, "kborg-workload-other-" + run
    record = {"target": target, "context": context, "namespace": namespace, "foreign_namespace": foreign,
              "run_id": run, "server": cluster["server"], "ca_sha256": hashlib.sha256(ca).hexdigest(),
              "kubernetes": json.loads(kube("get", "--raw=/version"))["gitVersion"], "phase": "creating-fixtures", "passed": False,
              "cleanup": False, "namespace_cleanup": []}
    report["targets"].append(record)
    owned = []
    started = time.monotonic()
    directory = root / target
    directory.mkdir(mode=0o700)
    try:
        for name in (namespace, foreign):
            obj = base.object_("Namespace", name)
            obj["metadata"]["labels"] = {"kborg.dev/run-id": run, "pod-security.kubernetes.io/enforce": "restricted"}
            value = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(obj)))
            owned.append((name, value["metadata"]["uid"]))
        labels = {"app": "kborg-workload", "kborg.dev/run-id": run}
        spec = pod_spec(image)
        initialization = json.loads(json.dumps(spec["containers"][0]))
        initialization.update(name="initialization", command=["sh", "-c", "echo KBORG_WORKLOAD_INIT"])
        spec["initContainers"] = [initialization]
        deployment = {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"namespace": namespace, "name": "different-name"},
                      "spec": {"replicas": 2, "selector": {"matchLabels": labels}, "template": {"metadata": {"labels": labels}, "spec": spec}}}
        service = base.object_("Service", "by-labels", namespace, spec={"selector": labels, "ports": [{"port": 80}]})
        cron = {"apiVersion": "batch/v1", "kind": "CronJob", "metadata": {"namespace": namespace, "name": "nightly"},
                "spec": {"schedule": "0 2 * * *", "suspend": True, "jobTemplate": {"spec": {"template": {"spec": pod_spec(image, completed=True)}}}}}
        resources = [deployment, service, cron, base.object_("ServiceAccount", "reader", namespace, automountServiceAccountToken=False),
                     base.object_("Role", "reader", namespace, rules=[
                         {"apiGroups": [""], "resources": ["pods", "pods/log", "services"], "verbs": ["get", "list"]},
                         {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get", "list"]},
                         {"apiGroups": ["batch"], "resources": ["cronjobs", "jobs"], "verbs": ["get", "list"]}]),
                     base.object_("RoleBinding", "reader", namespace, subjects=[{"kind": "ServiceAccount", "name": "reader", "namespace": namespace}],
                                  roleRef={"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "reader"})]
        kube("create", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "List", "items": resources}))
        outside = base.object_("Pod", "foreign-writer", foreign, spec=pod_spec(image))
        outside["metadata"]["labels"] = labels
        kube("create", "-f", "-", data=json.dumps(outside))
        kube("-n", namespace, "rollout", "status", "deployment/different-name", "--timeout=120s", timeout=135)
        cron_uid = json.loads(kube("-n", namespace, "get", "cronjob", "nightly", "-o", "json"))["metadata"]["uid"]
        job = {"apiVersion": "batch/v1", "kind": "Job", "metadata": {"namespace": namespace, "name": "owned-child",
               "ownerReferences": [{"apiVersion": "batch/v1", "kind": "CronJob", "name": "nightly", "uid": cron_uid, "controller": True}]},
               "spec": {"template": {"spec": pod_spec(image, completed=True)}}}
        kube("create", "-f", "-", data=json.dumps(job))
        kube("-n", namespace, "wait", "--for=condition=complete", "job/owned-child", "--timeout=90s", timeout=105)
        token = kube("-n", namespace, "create", "token", "reader", "--duration=10m").strip()
        reader, reader_context = base.private_reader(directory, cluster, namespace, token)
        del token, config
        environment = os.environ.copy()
        environment["KBORG_CONFIG_DIR"] = str(directory / "settings")
        app_prefix = [binary, "--kubeconfig", reader, "--context", reader_context, "-n", namespace, "--allow-logs", "--json"]

        def app(*command, success=True):
            return execute([*app_prefix, *command], environment=environment, timeout=45, success=success)

        record["phase"] = "workload-selector-service-cronjob"
        selected = None
        for command, count in [(["logs", "deploy/different-name"], 2), (["logs", "svc/by-labels"], 2),
                               (["logs", "--selector", "kborg.dev/run-id=" + run], 2), (["logs", "cronjob/nightly"], 1)]:
            result = json.loads(app(*command)[1])
            require(result.get("pod_scope") == "fixed-membership", "workload output omitted fixed membership")
            membership = result["membership"]
            require(membership["namespace"] == namespace and len(membership["pods"]) == count, "workload membership escaped namespace or omitted Pods")
            require(all(pod["name"] != "foreign-writer" for pod in membership["pods"]), "matching foreign-namespace Pod entered membership")
            text = result["logs"]
            require("KBORG_WORKLOAD_INIT" not in text, "workload merge admitted init logs")
            for pod in membership["pods"]:
                for container in pod["containers"]:
                    require(f"[{namespace}/{pod['name']}:{container}]" in text, "workload snapshot omitted an ordinary source")
            if "deploy/different-name" in command:
                selected = membership
        tail = json.loads(app("--tail", "3", "logs", "deploy/different-name")[1])["logs"]
        require(len([line for line in tail.splitlines() if re.match(r"\d{4}-\d\d-\d\dT", line)]) <= 3, "workload tail multiplied across Pods")
        require(selected and sum(len(pod["containers"]) for pod in selected["pods"]) == 4, "complete two-Pod/four-source membership missing")
        record["snapshot"] = {"pods": 2, "ordinary_sources": 4, "actual_workload_selector": True, "service_selector": True,
                              "explicit_selector": True, "cronjob_controller_uid_chain": True, "global_tail": True, "foreign_namespace_excluded": True}

        def scale(membership):
            kube("-n", namespace, "scale", "deployment/different-name", "--replicas=3")
            captured = {pod["uid"] for pod in membership["pods"]}
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                current = json.loads(kube("-n", namespace, "get", "pods", "-l", "kborg.dev/run-id=" + run, "-o", "json"))
                if len(current["items"]) == 3 and any(pod["metadata"]["uid"] not in captured for pod in current["items"]):
                    return
                time.sleep(.2)
            raise RuntimeError("third owned Pod did not appear during the live follow")

        record["phase"] = "fixed-membership-new-pod"
        code, error, elapsed = follow_change([*app_prefix, "--follow", "--log-seconds", "20", "logs", "deploy/different-name"], environment, scale, 2)
        require(code == 1 and "session deadline" in error, "fixed-membership follow did not end at its bounded deadline")
        kube("-n", namespace, "rollout", "status", "deployment/different-name", "--timeout=60s", timeout=75)
        current = json.loads(kube("-n", namespace, "get", "pods", "-l", "kborg.dev/run-id=" + run, "-o", "json"))
        require(len(current["items"]) == 3, "owned scale fixture did not create the third Pod")
        record["fixed_membership"] = {"captured_pods": 2, "scaled_pods": 3, "new_pod_confirmed_while_follow_alive": True,
                                      "new_pod_not_admitted": True, "captured_containers_only": True, "seconds": elapsed}

        def replace(membership):
            pod = membership["pods"][0]
            current = json.loads(kube("-n", namespace, "get", "pod", pod["name"], "-o", "json"))
            require(current["metadata"]["uid"] == pod["uid"] and current["metadata"]["labels"]["kborg.dev/run-id"] == run, "Pod deletion ownership changed")
            kube("delete", "--raw", f"/api/v1/namespaces/{namespace}/pods/{pod['name']}", "-f", "-",
                 data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": pod["uid"]}}))

        record["phase"] = "captured-pod-deletion"
        code, error, elapsed = follow_change([*app_prefix, "--follow", "--log-seconds", "20", "logs", "deploy/different-name"], environment, replace, 3)
        require(code == 1 and ("all fixed-membership log streams stopped" in error or "all merged streams stopped" in error)
                and "404" in error, "captured Pod deletion did not stop the fixed session")
        record["identity"] = {"captured_pod_deletion_stops_all_sources": True, "no_out_of_membership_data_observed": True,
                              "captured_containers_only": True, "seconds": elapsed}
        record["passed"] = True
        record["phase"] = "complete"
        print(f"{target}: workload selectors, fixed membership and deletion guards passed", flush=True)
    finally:
        for name, uid in reversed(owned):
            current = json.loads(kube("get", "namespace", name, "-o", "json"))
            require(current["metadata"]["uid"] == uid and current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run, "namespace cleanup ownership changed")
            kube("delete", "--raw", f"/api/v1/namespaces/{name}", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}}))
            kube("wait", "--for=delete", "namespace/" + name, "--timeout=90s", timeout=100)
            record["namespace_cleanup"].append({"namespace": name, "removed": True})
        record["cleanup"] = len(owned) == len(record["namespace_cleanup"])
        record["seconds"] = round(time.monotonic() - started, 3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("desktop", "minikube", "both"), default="both")
    parser.add_argument("--expected-desktop-server")
    parser.add_argument("--expected-minikube-server")
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/release/kborg")
    parser.add_argument("--output", type=Path, default=PROJECT / "test-results/workload-logs-local.json")
    args = parser.parse_args()
    targets = ("desktop", "minikube") if args.target == "both" else (args.target,)
    require(all(args.expected_desktop_server if target == "desktop" else args.expected_minikube_server for target in targets), "supply exact selected local endpoints before fixture writes")
    kubectl, docker = shutil.which("kubectl"), shutil.which("docker")
    require(kubectl and docker, "kubectl and Docker are required")
    binary = args.binary.resolve(strict=True)
    require(os.access(binary, os.X_OK), "explicit KBorg executable is required")
    execute([docker, "pull", "busybox:1.37.0"], timeout=120)
    image = execute([docker, "image", "inspect", "--format", "{{index .RepoDigests 0}}", "busybox:1.37.0"])[1].strip()
    require(re.fullmatch(r"(?:docker.io/library/)?busybox@sha256:[0-9a-f]{64}", image), "fixture image must be digest-pinned")
    paths = sorted((PROJECT / "src").glob("*.rs")) + sorted((PROJECT / "tests").glob("*.rs"))
    paths += [PROJECT / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "scripts/test-helm-logs-metrics.py")]
    paths.append(Path(__file__).resolve())
    hashes = {str(path.relative_to(PROJECT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    report = {"schema": 1, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "platform": os.uname().sysname,
              "architecture": os.uname().machine, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "kborg_version": execute([binary, "--version"])[1].strip(), "image": image, "targets": [],
              "tested_source_sha256": hashes, "source_unchanged": False, "temporary_credentials_removed": False}
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-workload-acceptance-") as temporary:
            directory = Path(temporary).resolve()
            os.chmod(directory, 0o700)
            for target in targets:
                target_run(target, args, directory, kubectl, binary, image, report)
    finally:
        report["temporary_credentials_removed"] = directory is not None and not directory.exists()
        report["source_unchanged"] = all(hashlib.sha256((PROJECT / path).read_bytes()).hexdigest() == digest for path, digest in hashes.items())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    require(report["source_unchanged"], "source changed during acceptance; rerun stable source")
    require(report["temporary_credentials_removed"] and all(target["cleanup"] for target in report["targets"]), "owned fixture cleanup not confirmed")
    print("Workload log suites passed; owned namespaces and temporary credentials removed.")


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        raise SystemExit(str(error)) from None
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired):
        raise SystemExit("Workload acceptance failed; inspect the recorded phase. Credentials and command diagnostics suppressed.") from None
