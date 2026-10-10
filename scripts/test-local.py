#!/usr/bin/env python3
"""Native macOS/Linux acceptance against an explicitly trusted Desktop endpoint.

Uses only temporary, uniquely owned namespaces. Credentials expire in ten minutes,
stay in a private temporary directory, and are removed even after a test failure.
No changes to the selected context, default kubeconfig or preexisting workloads.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from urllib.parse import urlparse
import uuid


def execute(argv, *, data=None, environment=None, timeout=180):
    result = subprocess.run(argv, input=data, text=True, capture_output=True,
                            env=environment, timeout=timeout)
    if result.returncode:
        if Path(argv[0]).name == "cargo":
            print(result.stdout, file=sys.stderr)
            print(result.stderr, file=sys.stderr)
        # Neither kubectl diagnostics nor partially generated tokens are printed.
        raise RuntimeError(f"{Path(argv[0]).name} operation failed (exit {result.returncode})")
    return result.stdout


def resource(kind, name, namespace=None, **fields):
    group = "rbac.authorization.k8s.io/v1" if kind in ("Role", "RoleBinding") else "v1"
    metadata = {"name": name}
    if namespace:
        metadata["namespace"] = namespace
    return dict(apiVersion=group, kind=kind, metadata=metadata, **fields)


def role(name, namespace, rules):
    return [resource("ServiceAccount", name, namespace, automountServiceAccountToken=False),
            resource("Role", name, namespace, rules=rules),
            resource("RoleBinding", name, namespace,
                     subjects=[{"kind": "ServiceAccount", "name": name, "namespace": namespace}],
                     roleRef={"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": name})]


def rule(group, resources, verbs):
    return {"apiGroups": [group], "resources": resources, "verbs": verbs}


def pod_spec(image, container="sample", server=False):
    command = ('printf KBORG_FORWARD_OK > /www/index.html; exec httpd -f -p 8080 -h /www'
               if server else 'echo "KBorg fixture"; while true; do echo "INFO local test heartbeat"; sleep 2; done')
    spec = {"automountServiceAccountToken": False,
            "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "fsGroup": 1000,
                                "seccompProfile": {"type": "RuntimeDefault"}},
            "containers": [{"name": container, "image": image, "imagePullPolicy": "IfNotPresent",
                            "command": ["sh", "-c", command],
                            "securityContext": {"allowPrivilegeEscalation": False,
                                                "readOnlyRootFilesystem": True,
                                                "capabilities": {"drop": ["ALL"]}},
                            "resources": {"requests": {"cpu": "10m", "memory": "8Mi"},
                                          "limits": {"cpu": "100m", "memory": "32Mi"}}}]}
    if server:
        spec["volumes"] = [{"name": "www", "emptyDir": {"medium": "Memory", "sizeLimit": "1Mi"}}]
        spec["containers"][0]["volumeMounts"] = [{"name": "www", "mountPath": "/www"}]
    return spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-server", required=True)
    parser.add_argument("--cargo", default="cargo")
    parser.add_argument("--suite", choices=("desktop", "watch", "native", "all"), default="all")
    parser.add_argument("--output", type=Path, default=Path("test-results/local-acceptance.json"))
    args = parser.parse_args()
    kubectl = shutil.which("kubectl")
    docker = shutil.which("docker")
    cargo = shutil.which(args.cargo)
    if not all((kubectl, docker, cargo)):
        raise RuntimeError("kubectl, Docker Desktop and Rust cargo must be available")
    prefix = [kubectl, "--context", "docker-desktop", "--request-timeout=30s"]

    def kube(*argv, data=None, timeout=180):
        return execute([*prefix, *argv], data=data, timeout=timeout)

    configuration = json.loads(kube("config", "view", "--minify", "--raw", "-o", "json"))
    if len(configuration["contexts"]) != 1 or configuration["contexts"][0]["name"] != "docker-desktop":
        raise RuntimeError("expected explicitly selected docker-desktop context")
    cluster = configuration["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    if (cluster["server"] != args.expected_server or endpoint.scheme != "https"
            or endpoint.hostname not in ("127.0.0.1", "localhost", "kubernetes.docker.internal")
            or not endpoint.port or endpoint.port < 1024 or endpoint.username or endpoint.password
            or endpoint.path not in ("", "/") or endpoint.query or endpoint.fragment
            or cluster.get("insecure-skip-tls-verify") or not cluster.get("certificate-authority-data")
            or cluster.get("proxy-url")):
        raise RuntimeError("exact local TLS endpoint/CA verification failed")
    ca_hash = hashlib.sha256(base64.b64decode(cluster["certificate-authority-data"], validate=True)).hexdigest()
    version = json.loads(kube("get", "--raw=/version"))["gitVersion"]
    execute([docker, "pull", "busybox:1.37.0"])
    image = execute([docker, "image", "inspect", "--format", "{{index .RepoDigests 0}}", "busybox:1.37.0"]).strip()
    digest = image.rsplit("@sha256:", 1)
    if len(digest) != 2 or len(digest[1]) != 64 or any(c not in "0123456789abcdef" for c in digest[1]):
        raise RuntimeError("fixture image must be digest-pinned")
    project = Path(__file__).resolve().parent.parent
    source_paths = sorted((project / "src").glob("*.rs")) + [project / "Cargo.toml", project / "Cargo.lock", project / "rust-toolchain.toml"]
    source_hashes = {str(path.relative_to(project)): hashlib.sha256(path.read_bytes()).hexdigest() for path in source_paths}
    report = {"schema": 1, "tested_source_sha256": source_hashes,
              "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "platform": os.uname().sysname, "architecture": os.uname().machine,
              "kubernetes": version, "server": cluster["server"], "ca_sha256": ca_hash,
              "image": image, "suites": [], "temporary_credentials_removed": False}
    suites = ("desktop", "watch", "native") if args.suite == "all" else (args.suite,)
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-local-") as temporary:
            directory = Path(temporary).resolve()
            os.chmod(directory, 0o700)
            for suite in suites:
                run = uuid.uuid4().hex[:12]
                ns = f"kborg-{'ops' if suite == 'native' else 'watch' if suite == 'watch' else 'tests'}-{run}"
                record = {"suite": suite, "namespace": ns, "run_id": run, "passed": False, "cleanup": False}
                report["suites"].append(record)
                namespace = resource("Namespace", ns)
                namespace["metadata"]["labels"] = {"kborg.dev/run-id": run, "pod-security.kubernetes.io/enforce": "restricted"}
                owned = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(namespace)))
                uid = owned["metadata"]["uid"]
                started = time.monotonic()
                try:
                    if suite == "native":
                        resources = []
                        for name in ("reader", "operator", "fixture"):
                            rules = [rule("", ["pods"], ["get", "create", "delete"] if name == "fixture" else ["get"])]
                            if name == "operator":
                                rules.append(rule("", ["pods/exec", "pods/attach", "pods/portforward"], ["get", "create"]))
                            resources += role(name, ns, rules)
                        resources.append(resource("Pod", "server", ns, spec=pod_spec(image, "server", True)))
                        identities = ("reader", "operator", "fixture")
                        context = "native-test"
                        target = "native_operations"
                        environment_prefix = "KBORG_OPS"
                    else:
                        reader = [rule("", ["pods", "services", "events", "configmaps"], ["get", "list", "watch"]),
                                  rule("", ["pods/log"], ["get"]),
                                  rule("apps", ["deployments"], ["get", "list", "watch"])]
                        operator = [rule("", ["pods", "services", "events", "configmaps"], ["get", "list", "watch", "delete"]),
                                    rule("", ["pods/log"], ["get"]),
                                    rule("apps", ["deployments"], ["get", "list", "watch", "patch", "delete"]),
                                    rule("apps", ["deployments/scale"], ["get", "patch"])]
                        resources = role("reader", ns, reader) + role("operator", ns, operator)
                        resources.append(resource("ServiceAccount", "denied", ns, automountServiceAccountToken=False))
                        if suite == "desktop":
                            deployment = resource("Deployment", "sample", ns, spec={"replicas": 1,
                                "selector": {"matchLabels": {"app": "kborg-sample"}},
                                "template": {"metadata": {"labels": {"app": "kborg-sample"}}, "spec": pod_spec(image)}})
                            deployment["apiVersion"] = "apps/v1"
                            resources += [deployment,
                                resource("ConfigMap", "canary", ns, data={"sample": "KBORG_TEST_CANARY"}),
                                resource("Secret", "fake-canary", ns, stringData={"sample": "KBORG_TEST_CANARY"}),
                                resource("Service", "delete-me", ns, spec={"selector": {"app": "kborg-sample"}, "ports": [{"port": 8080}]})]
                        else:
                            resources.append(resource("ConfigMap", "initial", ns, data={"sample": "KBORG_WATCH_PAYLOAD_CANARY"}))
                        identities = ("reader", "operator", "denied")
                        context = "kborg-watch-test" if suite == "watch" else "kborg-test"
                        target = "watch_metrics_acceptance" if suite == "watch" else "docker_desktop"
                        environment_prefix = "KBORG_WATCH" if suite == "watch" else "KBORG_TEST"
                    kube("create", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "List", "items": resources}))
                    if suite == "desktop":
                        kube("-n", ns, "rollout", "status", "deployment/sample", "--timeout=120s")
                    elif suite == "native":
                        kube("-n", ns, "wait", "--for=condition=Ready", "pod/server", "--timeout=120s")
                    environment = os.environ.copy()
                    environment["KBORG_CONFIG_DIR"] = str(directory / f"settings-{run}")
                    environment["KBORG_WATCH_NAMESPACE"] = ns
                    environment["KBORG_WATCH_RUN_ID"] = run
                    environment["KBORG_WATCH_KUBECTL"] = kubectl
                    for name in identities:
                        token = kube("-n", ns, "create", "token", name, "--duration=10m").strip()
                        config = {"apiVersion": "v1", "kind": "Config", "current-context": context,
                                  "clusters": [{"name": "local", "cluster": {"server": cluster["server"],
                                      "certificate-authority-data": cluster["certificate-authority-data"]}}],
                                  "users": [{"name": name, "user": {"token": token}}],
                                  "contexts": [{"name": context, "context": {"cluster": "local", "user": name, "namespace": ns}}]}
                        path = directory / f"{suite}-{name}.kubeconfig"
                        with path.open("x", encoding="utf-8") as stream:
                            os.chmod(path, 0o600)
                            json.dump(config, stream)
                        environment[f"{environment_prefix}_{name.upper()}_CONFIG"] = str(path)
                        token = config = None
                    output = execute([cargo, "test", "--locked", "--offline", "--test", target, "--", "--ignored", "--nocapture"],
                                     environment=environment, timeout=300)
                    if "test result: ok." not in output:
                        raise RuntimeError("acceptance result was not confirmed")
                    record["passed"] = True
                    print(f"{suite}: acceptance passed", flush=True)
                finally:
                    current = json.loads(kube("get", "namespace", ns, "-o", "json"))
                    if current["metadata"]["uid"] != uid or current["metadata"].get("labels", {}).get("kborg.dev/run-id") != run:
                        raise RuntimeError("cleanup blocked: namespace ownership changed")
                    kube("delete", "namespace", ns, "--wait=true", "--timeout=90s", timeout=100)
                    record["cleanup"] = True
                    record["seconds"] = round(time.monotonic() - started, 3)
        report["temporary_credentials_removed"] = True
    finally:
        report["temporary_credentials_removed"] = directory is not None and not directory.exists()
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if any(hashlib.sha256((project / path).read_bytes()).hexdigest() != digest for path, digest in source_hashes.items()):
        raise RuntimeError("source changed during acceptance; rerun with a stable tree")
    print("All selected suites passed; owned fixtures and credentials removed.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, subprocess.TimeoutExpired) as error:
        raise SystemExit(str(error)) from None
