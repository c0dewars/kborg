#!/usr/bin/env python3
"""Reviewed node CLI acceptance. Desktop is review/cancel only.

The owned-worker mode accepts only the dedicated kborg-v1 minikube worker and
refuses unrelated non-system workloads. It never drains the Desktop node.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parent.parent


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def terminal(argv, confirmation, deadline=60, before_confirm=None, environment=None):
    child, master = pty.fork()
    if child == 0:
        os.execvpe(argv[0], argv, dict(os.environ, TERM="xterm-256color", LANG="en_US.UTF-8", **(environment or {})))
    output, status, sent = bytearray(), None, False
    try:
        end = time.monotonic() + deadline
        while time.monotonic() < end:
            if select.select([master], [], [], .03)[0]:
                try:
                    output.extend(os.read(master, 65536))
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                require(len(output) <= 1024 * 1024, "review exceeded 1 MiB")
            if not sent and output.endswith(b"> "):
                if before_confirm:
                    before_confirm()
                os.write(master, confirmation.encode() + b"\n")
                sent = True
            finished, code = os.waitpid(child, os.WNOHANG)
            if finished:
                status = code
                break
        require(status is not None, "node CLI exceeded acceptance deadline")
        return os.waitstatus_to_exitcode(status), output.decode(errors="replace"), sent
    finally:
        if status is None:
            try:
                os.killpg(child, signal.SIGKILL)
            except ProcessLookupError:
                pass
        os.close(master)
        if status is None:
            os.waitpid(child, 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/kborg")
    parser.add_argument("--kubectl", default="kubectl")
    parser.add_argument("--kubeconfig", type=Path)
    parser.add_argument("--context", required=True)
    parser.add_argument("--node", required=True)
    parser.add_argument("--mode", choices=("review-only", "owned-worker"), required=True)
    parser.add_argument("--image", default="busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    kube_base = [args.kubectl, "--context", args.context, "--request-timeout=20s"]
    cli_base = [str(args.binary.resolve()), "--context", args.context, "--write", "--allow-node-maintenance"]
    if args.kubeconfig:
        kube_base += ["--kubeconfig", str(args.kubeconfig.resolve())]
        cli_base += ["--kubeconfig", str(args.kubeconfig.resolve())]

    def kube(*words, payload=None, timeout=90):
        result = subprocess.run([*kube_base, *words], input=json.dumps(payload) if payload else None,
                                capture_output=True, text=True, timeout=timeout)
        require(result.returncode == 0, "kubectl fixture operation failed; output withheld")
        return result.stdout

    def get_node():
        return json.loads(kube("get", "node", args.node, "-o", "json"))

    original = get_node()
    original_uid = original["metadata"]["uid"]
    require(not original["spec"].get("unschedulable", False), "fixture node must begin schedulable")
    report = {"schema": 1, "context": args.context, "node": args.node,
              "mode": args.mode, "node_uid": original_uid, "passed": False,
              "checks": [], "fixture_cleanup": False, "node_restored": False,
              "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "source_sha256": {str(p.relative_to(ROOT)): hashlib.sha256(p.read_bytes()).hexdigest()
                                for p in [ROOT / "src/node_maintenance.rs", Path(__file__).resolve()]}}
    namespace, namespace_uid = None, None
    owned_worker_verified = False
    audit_settings = ROOT / ".local" / ("node-acceptance-" + uuid.uuid4().hex[:12])
    audit_settings.mkdir(mode=0o700)
    report["audit_settings"] = str(audit_settings.relative_to(ROOT))

    def unchanged():
        fresh = get_node()
        require(fresh["metadata"]["uid"] == original_uid and fresh["spec"] == original["spec"],
                "review/dry-run unexpectedly changed node scheduling spec")

    def action(command, *, confirm=True, deadline=60, extra=(), before_confirm=None):
        code, text, sent = terminal([*cli_base, command, args.node, *extra],
                                    f"nodes/{args.node}" if confirm else "cancel", deadline,
                                    before_confirm=before_confirm,
                                    environment={"KBORG_CONFIG_DIR": str(audit_settings)})
        require(sent, f"{command} did not produce a review prompt")
        return code, text

    def set_scheduling(command):
        code, text = action(command)
        require(code == 0, "scheduling action failed; inspect node/audit before retrying")
        require(get_node()["spec"].get("unschedulable", False) == (command == "cordon"),
                "node scheduling postcondition failed")
        # Let the node lifecycle controller settle its corresponding taint
        # before opening the next independently reviewed scheduling operation.
        for _ in range(50):
            tainted = any(t.get("key") == "node.kubernetes.io/unschedulable"
                          for t in get_node()["spec"].get("taints", []))
            if tainted == (command == "cordon"):
                break
            time.sleep(.1)

    try:
        code, text = action("cordon", confirm=False, before_confirm=unchanged)
        require(code != 0 and "confirmation must exactly match" in text, "canceled node review was accepted")
        unchanged()
        report["checks"].append("cordon full review/dry-run/cancel preserved node spec")
        if args.mode == "review-only":
            require(args.context == "docker-desktop", "review-only fixture is Docker Desktop")
            report["checks"].append("Docker Desktop scheduling untouched")
            report["passed"] = True
            return

        require(args.context == "kborg-v1" and args.node == "kborg-v1-m02",
                "write fixture restricted to dedicated kborg-v1-m02 worker")
        require("node-role.kubernetes.io/control-plane" not in original["metadata"].get("labels", {}),
                "refusing control-plane maintenance fixture")
        existing = json.loads(kube("get", "pods", "-A", "--field-selector", f"spec.nodeName={args.node}", "-o", "json"))
        require(all(p["metadata"]["namespace"] in ("kube-system", "kube-public", "kube-node-lease")
                    for p in existing["items"]), "worker contains unrelated workloads; run node acceptance alone")
        owned_worker_verified = True
        namespace = "kborg-node-" + uuid.uuid4().hex[:10]
        created = json.loads(kube("create", "-f", "-", "-o", "json", payload={"apiVersion": "v1", "kind": "Namespace",
                     "metadata": {"name": namespace, "labels": {"kborg.dev/node-fixture": "true", "pod-security.kubernetes.io/enforce": "restricted"}}}))
        namespace_uid = created["metadata"]["uid"]
        image = args.image
        digest = image.rsplit("@sha256:", 1)
        require(len(digest) == 2 and len(digest[1]) == 64 and all(c in "0123456789abcdef" for c in digest[1]),
                "fixture image must be digest-pinned")
        spec = {"nodeSelector": {"kubernetes.io/hostname": args.node}, "automountServiceAccountToken": False,
                "terminationGracePeriodSeconds": 1,
                "securityContext": {"runAsNonRoot": True, "runAsUser": 1000, "seccompProfile": {"type": "RuntimeDefault"}},
                "containers": [{"name": "app", "image": image, "imagePullPolicy": "IfNotPresent", "command": ["sh", "-c", "exec sleep 3600"],
                                "securityContext": {"allowPrivilegeEscalation": False, "readOnlyRootFilesystem": True, "capabilities": {"drop": ["ALL"]}},
                                "resources": {"requests": {"cpu": "5m", "memory": "8Mi"}, "limits": {"cpu": "50m", "memory": "16Mi"}}}]}
        objects = [{"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "app", "namespace": namespace},
                    "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "node-fixture"}},
                             "template": {"metadata": {"labels": {"app": "node-fixture"}}, "spec": spec}}},
                   {"apiVersion": "policy/v1", "kind": "PodDisruptionBudget", "metadata": {"name": "app", "namespace": namespace},
                    "spec": {"minAvailable": 1, "selector": {"matchLabels": {"app": "node-fixture"}}}}]
        kube("create", "-f", "-", payload={"apiVersion": "v1", "kind": "List", "items": objects})
        kube("rollout", "status", "deployment/app", "-n", namespace, "--timeout=120s", timeout=130)
        initial_pod = json.loads(kube("get", "pods", "-n", namespace, "-o", "json"))["items"][0]
        initial_pod_uid = initial_pod["metadata"]["uid"]
        initial_pod_name = initial_pod["metadata"]["name"]
        # Wait for the disruption controller to observe the fixture before testing denial.
        for _ in range(50):
            pdb = json.loads(kube("get", "pdb", "app", "-n", namespace, "-o", "json"))
            if pdb.get("status", {}).get("currentHealthy") == 1 and pdb.get("status", {}).get("disruptionsAllowed") == 0:
                break
            time.sleep(.2)
        else:
            raise RuntimeError("fixture disruption budget did not settle")
        set_scheduling("cordon")
        set_scheduling("uncordon")
        report["checks"].append("reviewed cordon and uncordon confirmed through live Node API")
        code, text = action("drain", extra=("30",), deadline=50)
        require(code != 0 and "PodDisruptionBudget" in text and "Node: cordoned" in text,
                "PDB denial did not surface as an incomplete cordoned drain")
        retained = json.loads(kube("get", "pod", initial_pod_name, "-n", namespace, "-o", "json"))
        require(retained["metadata"]["uid"] == initial_pod_uid and not retained["metadata"].get("deletionTimestamp"),
                "PDB-protected Pod was unexpectedly evicted")
        report["checks"].append("PDB denial preserved Pod, left node cordoned, returned failure")
        kube("delete", "pdb", "app", "-n", namespace)
        code, text = action("drain", extra=("45",), deadline=65)
        require(code == 0 and "Reviewed workload evacuation completed" in text and "Retained:" in text,
                "reviewed live eviction did not confirm complete workload evacuation")
        remaining = json.loads(kube("get", "pods", "-n", namespace, "-o", "json"))["items"]
        require(all(p["metadata"]["uid"] != initial_pod_uid for p in remaining), "old Pod UID still exists after reported completion")
        require(get_node()["spec"].get("unschedulable", False), "drain unexpectedly restored scheduling")
        report["checks"].append("policy/v1 eviction removed reviewed UID; final inventory retained system/daemon Pods")
        set_scheduling("uncordon")
        kube("rollout", "status", "deployment/app", "-n", namespace, "--timeout=90s", timeout=100)
        report["checks"].append("explicit reviewed uncordon restored scheduling and fixture became Ready")
        report["passed"] = True
    finally:
        if namespace:
            actual = json.loads(kube("get", "namespace", namespace, "-o", "json"))
            require(actual["metadata"]["uid"] == namespace_uid, "namespace identity changed; cleanup blocked")
            kube("delete", "namespace", namespace, "--wait=true", "--timeout=90s", timeout=100)
        report["fixture_cleanup"] = True
        fresh = get_node()
        require(fresh["metadata"]["uid"] == original_uid, "node identity changed; scheduling cleanup blocked")
        if owned_worker_verified and fresh["spec"].get("unschedulable", False):
            # Explicit fixture cleanup only; production drain never auto-uncordons.
            set_scheduling("uncordon")
        report["node_restored"] = not get_node()["spec"].get("unschedulable", False)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
