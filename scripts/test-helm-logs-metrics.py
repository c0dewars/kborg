#!/usr/bin/env python3
"""Bounded, owned Helm/merged-log/metrics acceptance on explicit local clusters.

No current-context changes. Writes are limited to a unique owned namespace.
The application receives a ten-minute namespace-scoped reader credential in a
private temporary directory; captured credentials and diagnostics are never
printed or retained in the evidence report. Namespace cleanup checks UID and
run ownership. Metrics Server installation is a separate explicit operation.
"""
import argparse
import base64
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import tempfile
import time
from urllib.parse import urlparse
import uuid


PROJECT = Path(__file__).resolve().parent.parent
MAX_CAPTURE = 8 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def execute(argv, *, data=None, environment=None, timeout=90, success=True):
    """Drain both pipes with a finite capture/deadline; never echo diagnostics."""
    require(data is None or len(data.encode()) <= 32768, "fixture input exceeded its bound")
    process = subprocess.Popen([str(arg) for arg in argv], stdin=subprocess.PIPE if data is not None else subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment)
    output = {"stdout": bytearray(), "stderr": bytearray()}
    deadline = time.monotonic() + timeout
    try:
        if data is not None:
            process.stdin.write(data.encode())
            process.stdin.close()
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ, "stdout")
            selector.register(process.stderr, selectors.EVENT_READ, "stderr")
            total = 0
            while selector.get_map():
                remaining = deadline - time.monotonic()
                require(remaining > 0, f"{Path(argv[0]).name} exceeded its operation deadline; diagnostics suppressed")
                for event, _ in selector.select(min(remaining, 0.2)):
                    chunk = os.read(event.fileobj.fileno(), 65536)
                    if not chunk:
                        selector.unregister(event.fileobj)
                        continue
                    total += len(chunk)
                    require(total <= MAX_CAPTURE, "fixture command exceeded its bounded output capture")
                    output[event.data].extend(chunk)
            process.wait(timeout=max(0.1, deadline - time.monotonic()))
        if success:
            require(process.returncode == 0, f"{Path(argv[0]).name} operation failed (exit {process.returncode}; diagnostics suppressed)")
        return process.returncode, output["stdout"].decode("utf-8", errors="strict"), output["stderr"].decode("utf-8", errors="replace")
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        for pipe in (process.stdin, process.stdout, process.stderr):
            if pipe is not None:
                pipe.close()


def write_private(path, content):
    with path.open("x", encoding="utf-8") as handle:
        os.chmod(path, 0o600)
        handle.write(content)


def object_(kind, name, namespace=None, **fields):
    metadata = {"name": name}
    if namespace:
        metadata["namespace"] = namespace
    api = "rbac.authorization.k8s.io/v1" if kind in ("Role", "RoleBinding") else "v1"
    return {"apiVersion": api, "kind": kind, "metadata": metadata, **fields}


def fixture_pod(namespace, image):
    security = {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True,
                "capabilities": {"drop": ["ALL"]}}
    resources = {"requests": {"cpu": "10m", "memory": "8Mi"}, "limits": {"cpu": "100m", "memory": "32Mi"}}

    def container(name, command):
        return {"name": name, "image": image, "imagePullPolicy": "IfNotPresent", "command": ["sh", "-c", command],
                "securityContext": security.copy(), "resources": resources.copy()}

    first = container("writer-a", 'if [ ! -f /state/once ]; then touch /state/once; echo KBORG_PREVIOUS_A; exit 0; fi; '
                      'while true; do echo KBORG_WRITER_A; sleep 1; done')
    first["volumeMounts"] = [{"name": "state", "mountPath": "/state"}]
    second = container("writer-b", 'while true; do echo KBORG_WRITER_B; sleep 1; done')
    return object_("Pod", "writers", namespace, spec={
        "automountServiceAccountToken": False,
        "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000,
                            "seccompProfile": {"type": "RuntimeDefault"}},
        "initContainers": [container("initialization", "echo KBORG_INIT_ONLY")],
        "containers": [first, second],
        "volumes": [{"name": "state", "emptyDir": {"medium": "Memory", "sizeLimit": "1Mi"}}],
        "restartPolicy": "Always"})


def reader_resources(namespace):
    return [object_("ServiceAccount", "reader", namespace, automountServiceAccountToken=False),
            object_("Role", "reader", namespace, rules=[
                {"apiGroups": [""], "resources": ["pods", "pods/log", "secrets", "configmaps"], "verbs": ["get", "list"]},
                {"apiGroups": ["metrics.k8s.io"], "resources": ["pods"], "verbs": ["get", "list"]}]),
            object_("RoleBinding", "reader", namespace,
                    subjects=[{"kind": "ServiceAccount", "name": "reader", "namespace": namespace}],
                    roleRef={"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "reader"})]


def private_reader(root, cluster, namespace, token):
    path = root / "reader.kubeconfig"
    context = "kborg-feature-reader"
    config = {"apiVersion": "v1", "kind": "Config", "current-context": context,
              "clusters": [{"name": "local", "cluster": {"server": cluster["server"],
                           "certificate-authority-data": cluster["certificate-authority-data"]}}],
              "users": [{"name": "reader", "user": {"token": token}}],
              "contexts": [{"name": context, "context": {"cluster": "local", "user": "reader", "namespace": namespace}}]}
    write_private(path, json.dumps(config))
    return path, context


def helm_checks(helm, prefix, kube, app, root, namespace, record):
    chart = root / "chart"
    (chart / "templates").mkdir(parents=True, mode=0o700)
    os.chmod(chart, 0o700)
    canary = "KBORG_HELM_PRIVATE_CANARY_" + uuid.uuid4().hex
    write_private(chart / "values.yaml", f"visible: default\npassword: {canary}\ntoken: {canary}\n")
    write_private(chart / "templates" / "manifest.yaml",
                  'apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: {{ .Release.Name }}-manifest\n'
                  'data:\n  visible: {{ .Values.visible | quote }}\n'
                  f'  opaque: "{canary}"\n')
    write_private(chart / "templates" / "NOTES.txt", canary + "\n")
    values = root / "values.yaml"
    write_private(values, f"visible: revision-one\npassword: {canary}\ntoken: {canary}\n")
    # Helm's HELM_KUBE* variables can override the endpoint/CA/credential even
    # with an explicit context flag. Keep fixture writes bound to the exact
    # kubeconfig endpoint checked by run_target, including TLS verification.
    helm_environment = {key: value for key, value in os.environ.items() if not key.startswith("HELM_KUBE")}
    for name in ("HELM_CACHE_HOME", "HELM_CONFIG_HOME", "HELM_DATA_HOME", "HELM_PLUGINS"):
        directory = root / name.lower()
        directory.mkdir(mode=0o700)
        helm_environment[name] = str(directory)
    helm_context = []
    # kubectl and Helm accept the same explicit context/kubeconfig flags, but
    # Helm spells its context flag differently and has no request-timeout flag.
    for index, argument in enumerate(prefix):
        if argument == "--context":
            helm_context += ["--kube-context", prefix[index + 1]]
        if argument == "--kubeconfig":
            helm_context += ["--kubeconfig", prefix[index + 1]]
    outcomes = []
    for storage in ("secret", "configmap"):
        release = "feature-" + storage
        helm_environment["HELM_DRIVER"] = storage
        for revision in (1, 2):
            chart_metadata = f"apiVersion: v2\nname: kborg-fixture\nversion: 0.1.{revision - 1}\nappVersion: fixture\n"
            (chart / "Chart.yaml").write_text(chart_metadata, encoding="utf-8")
            os.chmod(chart / "Chart.yaml", 0o600)
            values.write_text(f"visible: revision-{'one' if revision == 1 else 'two'}\npassword: {canary}\ntoken: {canary}\n", encoding="utf-8")
            execute([helm, *helm_context, "install" if revision == 1 else "upgrade", release, chart,
                     "--namespace", namespace, "--values", values, "--timeout", "45s"], environment=helm_environment, timeout=60)

        def read(*command):
            text = app("--allow-helm", "--helm-storage", storage, "--json", *command)
            require(canary not in text, "Helm projection exposed a hidden fixture canary")
            envelope = json.loads(text)
            require(envelope.get("schema_version") == 1 and envelope.get("bounded") is True, "Helm JSON envelope invalid")
            return envelope

        latest = read("helm", "list")
        releases = latest["data"]["releases"]
        selected = [item for item in releases if item["name"] == release]
        require(len(selected) == 1 and selected[0]["revision"] == 2 and selected[0]["storage"] == storage,
                "Helm latest revision/storage identity invalid")
        history = read("helm", "history", release)
        require(sorted(item["revision"] for item in history["data"]["releases"]) == [1, 2], "Helm two-revision history invalid")
        require(all(item["storage"] == storage and item["namespace"] == namespace for item in history["data"]["releases"]),
                "Helm history escaped selected scope/storage")
        detail = read("helm", "detail", release, "2")
        identities = read("helm", "resources", release, "2")
        for envelope in (detail, identities):
            resources = envelope["data"]["resources"]
            require(any(item["kind"] == "ConfigMap" and item["namespace"] == namespace
                        and item["name"] == release + "-manifest" for item in resources), "Helm rendered resource identity missing")
            require(all(set(item) <= {"api_version", "kind", "namespace", "name"} for item in resources), "Helm resource projection exposed object payload")
        values_result = read("helm", "values", release, "2")
        require(values_result["data"]["values"]["visible"] == "revision-two", "Helm explicit values omitted ordinary settings")
        require(values_result["data"]["values"].get("password") != canary and values_result["data"]["values"].get("token") != canary,
                "Helm known credential fields were not redacted")
        old_values = read("helm", "values", release, "1")
        require(old_values["data"]["values"]["visible"] == "revision-one", "Helm selected revision did not preserve historical values")
        outcomes.append({"storage": storage, "revisions": [1, 2], "list_history_detail_resources_values": True,
                         "known_key_redaction": True, "hidden_manifest_notes_canaries": True})
    record["helm"] = outcomes
    del canary


def logs_checks(app, pod, namespace, record):
    uid = pod["metadata"]["uid"]
    require(any(status["name"] == "writer-a" and status.get("restartCount", 0) >= 1
                for status in pod["status"]["containerStatuses"]), "previous-log writer did not restart")
    snapshot = json.loads(app("--allow-logs", "--all-containers", "--json", "logs", "writers"))
    text = snapshot["logs"]
    require(snapshot.get("uid") == uid and snapshot.get("container_scope") == "all-ordinary", "merged snapshot lost captured Pod identity")
    for source, marker in (("writer-a", "KBORG_WRITER_A"), ("writer-b", "KBORG_WRITER_B")):
        require(f"[{namespace}/writers:{source}] {marker}" in text, "merged snapshot missing an attributed ordinary source")
    require("KBORG_INIT_ONLY" not in text, "merged logs included excluded init-container output")
    data_lines = [line for line in text.splitlines() if re.match(r"\d{4}-\d\d-\d\dT", line)]
    require(data_lines and all(" [" in line for line in data_lines), "merged source prefix discarded server timestamps")
    tail = json.loads(app("--allow-logs", "--all-containers", "--tail", "3", "--json", "logs", "writers"))["logs"]
    require(len([line for line in tail.splitlines() if re.match(r"\d{4}-\d\d-\d\dT", line)]) <= 3,
            "merged snapshot multiplied the shared tail across sources")
    previous = json.loads(app("--allow-logs", "--all-containers", "--previous", "--json", "logs", "writers"))["logs"]
    require(f"[{namespace}/writers:writer-a] KBORG_PREVIOUS_A" in previous, "available previous logs were lost")
    require(f"[Log stream {namespace}/writers:writer-b: previous logs unavailable (Kubernetes API 400)" in previous,
            "missing previous logs were not explicitly source-labelled")
    started = time.monotonic()
    code, stdout, stderr = app("--allow-logs", "--all-containers", "--follow", "--log-seconds", "2", "--json", "logs", "writers", result=True)
    elapsed = time.monotonic() - started
    require(code == 1 and "session deadline" in stderr and elapsed < 12, "merged follow did not stop at its shared deadline")
    records = [json.loads(line) for line in stdout.splitlines() if line]
    require(records and all(item.get("uid") == uid and item.get("container_scope") == "all-ordinary" for item in records), "merged follow JSONL lost captured scope")
    for source in ("writer-a", "writer-b"):
        require(any(f"[{namespace}/writers:{source}]" in item["line"] for item in records), "live merged follow starved an ordinary source")
    single = json.loads(app("--allow-logs", "--json", "logs", "writers", "writer-b"))["logs"]
    require("KBORG_WRITER_B" in single and "KBORG_WRITER_A" not in single, "selected-container logs changed scope")
    record["logs"] = {"ordinary_sources": 2, "snapshot_source_timestamps": True, "shared_tail": True,
                      "previous_partial_failure": True, "selected_container_preserved": True,
                      "follow_scope_uid": True, "follow_deadline_seconds": 2, "follow_elapsed_seconds": round(elapsed, 3),
                      "replacement_and_drop": "covered by focused mock/reader tests; no live UID-replacement claim"}


def metrics_checks(app, namespace, record):
    deadline = time.monotonic() + 100
    selected = []
    while time.monotonic() < deadline:
        code, stdout, _ = app("--metrics-resources", "--json", "metrics", "pods", result=True)
        if code == 0:
            result = json.loads(stdout)
            selected = [row for row in result.get("items", []) if row["name"] == "writers" and row.get("namespace") == namespace]
            if {row.get("container") for row in selected} == {"writer-a", "writer-b"} and all(row.get("allocations", {}).get("bases") for row in selected):
                break
        time.sleep(3)
    require(len(selected) == 2, "Metrics API did not provide both ordinary-container samples within the bounded wait")
    for row in selected:
        require(row["memory_bytes"] > 0 and row["cpu_nanocores"] >= 0 and row["timestamp"] and row["window"], "real metrics measurement invalid")
        comparison = row.get("allocations", {})
        require({basis["basis"] for basis in comparison.get("bases", [])} == {"requests", "limits"}, "metrics request/limit basis missing")
        for basis in comparison["bases"]:
            require(basis["cpu"].get("amount", 0) > 0 and basis["memory"].get("amount", 0) > 0,
                    "metrics denominator was not positive declared configuration")
            require("usage_percent" in basis["cpu"] and "usage_percent" in basis["memory"], "metrics comparison unavailable for declared positive resources")
            expected = (10_000_000, 8 * 1024 * 1024) if basis["basis"] == "requests" else (100_000_000, 32 * 1024 * 1024)
            require((basis["cpu"]["amount"], basis["memory"]["amount"]) == expected, "metrics denominator changed the fixture's exact declared quantities")
    record["metrics"] = {"source": "cluster", "ordinary_container_samples": 2, "positive_memory_measurements": True,
                         "current_declared_requests_limits": True,
                         "generation_verified": [row["allocations"]["generation_verified"] for row in selected],
                         "samples": [{key: row[key] for key in ("container", "cpu_nanocores", "memory_bytes", "timestamp", "window")} for row in selected]}


def run_target(target, args, root, tools, image, report):
    kubectl, helm, binary = tools
    if target == "desktop":
        expected = args.expected_desktop_server
        prefix = [kubectl, "--context", "docker-desktop", "--request-timeout=30s"]
        context = "docker-desktop"
    else:
        expected = args.expected_minikube_server
        config_path = PROJECT / ".local" / "minikube.kubeconfig"
        require(config_path.is_file(), "owned minikube kubeconfig is missing")
        prefix = [kubectl, "--kubeconfig", str(config_path), "--context", "kborg-validation", "--request-timeout=30s"]
        context = "kborg-validation"
    require(expected, "an exact expected server is required for every selected target")

    def kube(*command, data=None, timeout=90):
        return execute([*prefix, *command], data=data, timeout=timeout)[1]

    config = json.loads(kube("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
    require(len(config.get("contexts", [])) == 1 and config["contexts"][0]["name"] == context, "explicit local context mismatch")
    cluster = config["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    local = endpoint.hostname in ("localhost", "127.0.0.1", "kubernetes.docker.internal")
    if target == "minikube" and not local:
        try:
            local = ipaddress.ip_address(endpoint.hostname).is_private
        except ValueError:
            local = False
    require(cluster["server"] == expected and endpoint.scheme == "https" and local
            and endpoint.port and endpoint.port >= 1024 and not endpoint.username and not endpoint.password
            and endpoint.path in ("", "/") and not endpoint.query and not endpoint.fragment
            and not cluster.get("proxy-url") and not cluster.get("insecure-skip-tls-verify")
            and cluster.get("certificate-authority-data"), "exact local TLS endpoint/CA verification failed")
    ca = base64.b64decode(cluster["certificate-authority-data"], validate=True)
    require(0 < len(ca) <= 1024 * 1024, "local CA bundle exceeded its bound")
    run = uuid.uuid4().hex[:12]
    namespace = f"kborg-features-{run}"
    record = {"target": target, "context": context, "namespace": namespace, "run_id": run, "server": cluster["server"],
              "ca_sha256": hashlib.sha256(ca).hexdigest(), "kubernetes": json.loads(kube("get", "--raw=/version"))["gitVersion"],
              "passed": False, "cleanup": False}
    report["targets"].append(record)
    namespace_object = object_("Namespace", namespace)
    namespace_object["metadata"]["labels"] = {"kborg.dev/run-id": run, "pod-security.kubernetes.io/enforce": "restricted"}
    owned = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(namespace_object)))
    namespace_uid = owned["metadata"]["uid"]
    started = time.monotonic()
    directory = root / target
    directory.mkdir(mode=0o700)
    try:
        fixture = fixture_pod(namespace, image)
        kube("create", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "List", "items": [fixture, *reader_resources(namespace)]}))
        record["phase"] = "waiting-for-fixture"
        pod_deadline = time.monotonic() + 150
        while time.monotonic() < pod_deadline:
            pod = json.loads(kube("-n", namespace, "get", "pod", "writers", "-o", "json"))
            statuses = pod.get("status", {}).get("containerStatuses", [])
            if (len(statuses) == 2 and all(status.get("ready") for status in statuses)
                    and any(status["name"] == "writer-a" and status.get("restartCount", 0) >= 1 for status in statuses)):
                break
            time.sleep(2)
        else:
            raise RuntimeError("fixture containers did not become ready after the intentional writer-a restart")
        time.sleep(2)
        token = kube("-n", namespace, "create", "token", "reader", "--duration=10m").strip()
        reader, reader_context = private_reader(directory, cluster, namespace, token)
        del token, config
        environment = os.environ.copy()
        environment["KBORG_CONFIG_DIR"] = str(directory / "settings")

        def app(*command, result=False):
            output = execute([binary, "--kubeconfig", reader, "--context", reader_context, "-n", namespace, *command],
                             environment=environment, timeout=45, success=not result)
            return output if result else output[1]

        record["phase"] = "merged-logs"
        logs_checks(app, pod, namespace, record)
        record["phase"] = "helm-two-storage-drivers"
        helm_checks(helm, prefix, kube, app, directory, namespace, record)
        if args.require_metrics in (target, "both"):
            record["phase"] = "real-metrics"
            metrics_checks(app, namespace, record)
        else:
            record["metrics"] = {"tested": False, "reason": "positive metrics not requested for this target"}
        record["passed"] = True
        record["phase"] = "complete"
        print(f"{target}: merged logs, Helm revisions and selected metrics checks passed", flush=True)
    finally:
        current = json.loads(kube("get", "namespace", namespace, "-o", "json"))
        require(current["metadata"]["uid"] == namespace_uid
                and current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run,
                "cleanup blocked: namespace UID or ownership changed")
        # The API UID precondition closes the get/delete race: a replacement
        # namespace must never be removed, even if another actor reused the name.
        kube("delete", "--raw", f"/api/v1/namespaces/{namespace}", "-f", "-",
             data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": namespace_uid}}))
        kube("wait", "--for=delete", f"namespace/{namespace}", "--timeout=90s", timeout=100)
        record["cleanup"] = True
        record["seconds"] = round(time.monotonic() - started, 3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("desktop", "minikube", "both"), default="both")
    parser.add_argument("--expected-desktop-server")
    parser.add_argument("--expected-minikube-server")
    parser.add_argument("--binary", type=Path, default=PROJECT / "target" / "release" / "kborg")
    parser.add_argument("--helm", type=Path, default=PROJECT / ".local" / "platform-tools" / "helm")
    parser.add_argument("--require-metrics", choices=("desktop", "minikube", "both", "none"), default="desktop")
    parser.add_argument("--output", type=Path, default=PROJECT / "test-results" / "helm-logs-metrics-local.json")
    args = parser.parse_args()
    kubectl, docker = shutil.which("kubectl"), shutil.which("docker")
    require(kubectl and docker, "kubectl and Docker are required")
    binary, helm = args.binary.resolve(strict=True), args.helm.resolve(strict=True)
    require(os.access(binary, os.X_OK) and os.access(helm, os.X_OK), "explicit KBorg and Helm executables are required")
    targets = ("desktop", "minikube") if args.target == "both" else (args.target,)
    require(all(args.expected_desktop_server if target == "desktop" else args.expected_minikube_server for target in targets),
            "supply exact expected server endpoints before any fixture writes")
    execute([docker, "pull", "busybox:1.37.0"], timeout=120)
    image = execute([docker, "image", "inspect", "--format", "{{index .RepoDigests 0}}", "busybox:1.37.0"])[1].strip()
    require(re.fullmatch(r"(?:docker.io/library/)?busybox@sha256:[0-9a-f]{64}", image), "fixture image must be digest-pinned")
    paths = sorted((PROJECT / "src").glob("*.rs")) + sorted((PROJECT / "tests").glob("*.rs"))
    paths += [PROJECT / name for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")]
    paths.append(Path(__file__).resolve())
    hashes = {str(path.relative_to(PROJECT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    report = {"schema": 1, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "platform": os.uname().sysname, "architecture": os.uname().machine,
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "kborg_version": execute([binary, "--version"])[1].strip(),
              "helm_version": execute([helm, "version", "--short"])[1].strip(),
              "helm_sha256": hashlib.sha256(helm.read_bytes()).hexdigest(), "image": image,
              "tested_source_sha256": hashes, "source_unchanged": False,
              "targets": [], "temporary_credentials_removed": False}
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-feature-acceptance-") as temporary:
            directory = Path(temporary).resolve()
            os.chmod(directory, 0o700)
            for target in targets:
                run_target(target, args, directory, (kubectl, helm, binary), image, report)
    finally:
        report["temporary_credentials_removed"] = directory is not None and not directory.exists()
        report["source_unchanged"] = all(hashlib.sha256((PROJECT / path).read_bytes()).hexdigest() == digest for path, digest in hashes.items())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    require(report["source_unchanged"], "source changed during acceptance; rerun against a stable tree")
    require(report["temporary_credentials_removed"] and all(target["cleanup"] for target in report["targets"]), "owned fixture cleanup was not confirmed")
    print("All selected feature suites passed; owned namespaces and temporary credentials removed.")


if __name__ == "__main__":
    try:
        main()
    except RuntimeError as error:
        raise SystemExit(str(error)) from None
    except (OSError, ValueError, KeyError, subprocess.TimeoutExpired):
        raise SystemExit("Feature acceptance failed; inspect the bounded phase/check fields in the output report. Credentials and command diagnostics suppressed.") from None
