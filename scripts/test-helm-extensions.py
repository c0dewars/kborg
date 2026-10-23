#!/usr/bin/env python3
"""Owned OCI, hook and CRD acceptance on explicitly selected local clusters.

Uses temporary short-lived credentials, unique schemas/namespaces and UID-checked
cleanup. No kubeconfigs, tokens, hook payloads or terminal transcripts are saved.
"""
import argparse
import base64
import gzip
import hashlib
import importlib.util
import io
import ipaddress
import json
import os
import re
from pathlib import Path
import shutil
import tarfile
import tempfile
import time
from urllib.parse import urlparse
import uuid

PROJECT = Path(__file__).resolve().parent.parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, PROJECT / "scripts" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


lifecycle = module("extension_lifecycle", "test-helm-lifecycle.py")
base = lifecycle.base
require, execute, object_, write_private = base.require, base.execute, base.object_, base.write_private


def completed_hook_record(stored, namespace, release, revision, hook_name, phase):
    """Privately verify a completed hook; never return release contents."""
    try:
        metadata = stored["metadata"]
        labels = metadata["labels"]
        require(isinstance(metadata, dict) and isinstance(labels, dict), "invalid hook completion metadata")
        require(stored.get("type") == "helm.sh/release.v1"
                and metadata["name"] == f"sh.helm.release.v1.{release}.v{revision}"
                and metadata["namespace"] == namespace and metadata.get("uid") and metadata.get("resourceVersion")
                and labels.get("owner") == "helm" and labels.get("name") == release
                and labels.get("version") == str(revision), "hook completion storage identity mismatch")
        encoded = stored["data"]["release"]
        require(isinstance(encoded, str) and len(encoded) <= 2 * 1024 * 1024,
                "hook completion storage payload exceeds bound")
        helm_base64 = base64.b64decode(encoded, validate=True)
        require(len(helm_base64) <= 1024 * 1024, "hook completion encoded payload exceeds bound")
        compressed = base64.b64decode(helm_base64, validate=True)
        require(compressed.startswith(b"\x1f\x8b"), "hook completion expected Helm gzip storage")
        with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
            decoded = stream.read(8 * 1024 * 1024 + 1)
        require(len(decoded) <= 8 * 1024 * 1024, "hook completion decoded payload exceeds bound")
        payload = json.loads(decoded)
        require(isinstance(payload, dict), "invalid hook completion release envelope")
        require(payload.get("name") == release and payload.get("namespace") == namespace
                and payload.get("version") == revision, "hook completion payload identity mismatch")
        hooks = payload.get("hooks")
        require(isinstance(hooks, list) and len(hooks) <= 32 and all(isinstance(hook, dict) for hook in hooks), "hook completion records malformed")
        selected = [hook for hook in hooks if hook.get("name") == hook_name and hook.get("kind") == "Job"]
        require(len(selected) == 1, "hook completion record missing or ambiguous")
        run = selected[0].get("last_run", {})
        require(isinstance(run, dict) and run.get("phase") == phase
                and all(isinstance(run.get(key), str) and 1 <= len(run[key]) <= 64
                        and not run[key].startswith("0001-") for key in ("started_at", "completed_at")),
                "hook never reached its expected completed phase")
    except (OSError, ValueError, TypeError, KeyError, RecursionError):
        raise RuntimeError("cannot verify bounded private hook completion record; contents suppressed") from None


def chart(path, revision, image, canary, group, run_id, *, extensions=True, crd_scope="Namespaced", failing=False):
    """A deterministic chart with independently observable lifecycle hook Jobs."""
    files = {
        "Chart.yaml": f"apiVersion: v2\nname: kborg-extension-fixture\nversion: 0.1.{revision}\n",
        "values.yaml": f"message: revision-{revision}\npassword: {canary}\n",
        "templates/settings.yaml": '''apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .Release.Name }}-settings
data:
  message: {{ .Values.message | quote }}
---
apiVersion: v1
kind: Secret
metadata:
  name: {{ .Release.Name }}-private
stringData:
  password: {{ .Values.password | quote }}
''',
    }
    if extensions:
        crd = {
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "widgets." + group, "labels": {"kborg.dev/run-id": run_id}},
            "spec": {"group": group, "scope": crd_scope,
                     "names": {"plural": "widgets", "singular": "widget", "kind": "Widget"},
                     "versions": [{"name": "v1", "served": True, "storage": True,
                                   "schema": {"openAPIV3Schema": {
                                       "type": "object", "properties": {"spec": {
                                           "type": "object", "required": ["message"],
                                           "properties": {"message": {"type": "string"}}}}}}}]}}
        files["crds/widgets.yaml"] = json.dumps(crd)
        files["templates/widget.yaml"] = '''apiVersion: GROUP/v1
kind: Widget
metadata:
  name: {{ .Release.Name }}-widget
spec:
  message: {{ .Values.message | quote }}
'''.replace("GROUP", group)
        for event in ("pre-install", "post-install", "pre-upgrade", "post-upgrade",
                      "pre-rollback", "post-rollback", "pre-delete", "post-delete"):
            files["templates/" + event + ".yaml"] = '''apiVersion: batch/v1
kind: Job
metadata:
  name: {{ .Release.Name }}-EVENT
  namespace: {{ .Release.Namespace }}
  labels:
    app.kubernetes.io/managed-by: Helm
  annotations:
    meta.helm.sh/release-name: {{ .Release.Name }}
    meta.helm.sh/release-namespace: {{ .Release.Namespace }}
    helm.sh/hook: EVENT
    helm.sh/hook-weight: "-5"
    helm.sh/hook-delete-policy: before-hook-creation
spec:
  backoffLimit: 0
  activeDeadlineSeconds: 45
  template:
    spec:
      automountServiceAccountToken: false
      securityContext:
        runAsNonRoot: true
        runAsUser: 65534
        runAsGroup: 65534
        seccompProfile: {type: RuntimeDefault}
      restartPolicy: Never
      containers:
        - name: hook
          image: IMAGE
          command: ["/bin/sh", "-c", "echo KBORG_HOOK_EVENT"]
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: {drop: [ALL]}
          resources:
            requests: {cpu: 5m, memory: 8Mi}
            limits: {cpu: 50m, memory: 24Mi}
'''.replace("EVENT", event).replace("IMAGE", image)
        files["templates/cleanup-hook.yaml"] = (files["templates/post-install.yaml"]
            .replace("}}-post-install", "}}-post-cleanup")
            .replace('helm.sh/hook-weight: "-5"', 'helm.sh/hook-weight: "5"')
            .replace("helm.sh/hook-delete-policy: before-hook-creation", "helm.sh/hook-delete-policy: before-hook-creation,hook-succeeded"))
        if failing:
            files["templates/pre-install.yaml"] = (files["templates/pre-install.yaml"]
                .replace("echo KBORG_HOOK_pre-install", "echo KBORG_HOOK_FAILED; exit 1")
                .replace("helm.sh/hook-delete-policy: before-hook-creation", "helm.sh/hook-delete-policy: before-hook-creation,hook-failed"))
    with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for name, content in files.items():
            data = content.encode()
            item = tarfile.TarInfo("fixture/" + name)
            item.size, item.mode = len(data), 0o600
            archive.addfile(item, io.BytesIO(data))
    path.chmod(0o600)


def run_target(target, args, temporary, image, report, extra_checks=None):
    context = "docker-desktop" if target == "desktop" else "kborg-validation"
    expected = args.expected_desktop_server if target == "desktop" else args.expected_minikube_server
    prefix = [shutil.which("kubectl"), "--context", context, "--request-timeout=30s"]
    if target == "minikube":
        prefix += ["--kubeconfig", str(PROJECT / ".local/minikube.kubeconfig")]

    def kube(*command, data=None):
        return execute([*prefix, *command], data=data)[1]

    config = json.loads(kube("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
    require(len(config["contexts"]) == 1 and config["contexts"][0]["name"] == context, "context identity mismatch")
    cluster = config["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    local = endpoint.hostname in ("localhost", "127.0.0.1", "kubernetes.docker.internal")
    if target == "minikube" and not local:
        local = ipaddress.ip_address(endpoint.hostname).is_private
    require(cluster["server"] == expected and endpoint.scheme == "https" and local and endpoint.port
            and endpoint.port >= 1024 and not endpoint.username and not endpoint.password
            and endpoint.path in ("", "/") and not endpoint.query and not endpoint.fragment
            and not cluster.get("proxy-url") and not cluster.get("insecure-skip-tls-verify")
            and cluster.get("certificate-authority-data"), "expected local TLS identity mismatch")
    directory = temporary / target
    directory.mkdir(mode=0o700)
    run_id = uuid.uuid4().hex[:12]
    namespace = "kborg-extensions-" + run_id
    group, crd_name = run_id + ".kborg.test", "widgets." + run_id + ".kborg.test"
    record = {"target": target, "context": context, "server": expected, "namespace": namespace,
              "run_id": run_id, "crd": crd_name,
              "kubernetes": json.loads(kube("get", "--raw=/version"))["gitVersion"],
              "passed": False, "cleanup": False, "checks": []}
    report["targets"].append(record)
    ns = object_("Namespace", namespace)
    ns["metadata"]["labels"] = {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}
    namespace_uid = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(ns)))["metadata"]["uid"]
    cluster_owned = []
    started = time.monotonic()
    try:
        resources = [object_("ServiceAccount", "operator", namespace, automountServiceAccountToken=False),
                     object_("Role", "operator", namespace, rules=[
                         {"apiGroups": [""], "resources": ["configmaps", "secrets", "pods", "pods/log", "services", "serviceaccounts"], "verbs": ["get", "list", "watch", "create", "update", "patch", "delete"]},
                         {"apiGroups": ["batch"], "resources": ["jobs"], "verbs": ["get", "list", "watch", "create", "update", "patch", "delete"]},
                         {"apiGroups": [group], "resources": ["widgets"], "verbs": ["get", "list", "watch", "create", "update", "patch", "delete"]}]),
                     object_("RoleBinding", "operator", namespace,
                             subjects=[{"kind": "ServiceAccount", "name": "operator", "namespace": namespace}],
                             roleRef={"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "operator"})]
        kube("create", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "List", "items": resources}))
        # Kubernetes RBAC cannot resourceName-limit create. This test credential
        # starts with CRD create + exact-name get. Completion checks add only
        # exact-schema update/delete and the unique fixture API group inventory.
        cluster_resources = [
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole",
             "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}},
             "rules": [{"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "verbs": ["create"]},
                       {"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "resourceNames": [crd_name], "verbs": ["get"]}]},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding",
             "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}},
             "subjects": [{"kind": "ServiceAccount", "name": "operator", "namespace": namespace}],
             "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": namespace}},
        ]
        if extra_checks:
            cluster_resources[0]["rules"].extend([
                {"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "resourceNames": [crd_name], "verbs": ["update", "delete"]},
                {"apiGroups": [group], "resources": ["widgets"], "verbs": ["get", "list", "delete"]},
            ])
        for obj in cluster_resources:
            current = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(obj)))
            cluster_owned.append((obj["kind"].lower() + "s", obj["metadata"]["name"], current["metadata"]["uid"]))
        token = kube("-n", namespace, "create", "token", "operator", "--duration=30m").strip()
        credential, reader_context = base.private_reader(directory, cluster, namespace, token)
        del token, config
        environment = {key: value for key, value in os.environ.items() if not key.startswith("HELM_")}
        environment.update(TERM="xterm-256color", KBORG_CONFIG_DIR=str(directory / "settings"), KBORG_NO_SPLASH="1",
                           HELM_KUBEAPISERVER="https://127.0.0.1:1", HELM_KUBEINSECURE_SKIP_TLS_VERIFY="true")
        canary = "KBORG_EXT_PRIVATE_" + uuid.uuid4().hex
        first, second, simple, invalid, failed_hook = (directory / name for name in ("first.tgz", "second.tgz", "simple.tgz", "cluster-scope.tgz", "failed-hook.tgz"))
        chart(first, 1, image, canary, group, run_id)
        chart(second, 2, image, canary, group, run_id)
        chart(simple, 1, image, canary, group, run_id, extensions=False)
        chart(invalid, 1, image, canary, group, run_id, crd_scope="Cluster")
        chart(failed_hook, 1, image, canary, group, run_id, failing=True)
        common = [args.binary, "--kubeconfig", credential, "--context", reader_context, "-n", namespace,
                  "--allow-helm", "--write", "--allow-helm-write", "--helm-tool", args.helm, "--helm-timeout", "120"]

        def operation(action, release, *parameters, flags=(), callback=None, success=True, confirm=None, text_required=()):
            record["phase"] = action + ":" + release
            print(f"{target}: {action} {release} (expected {'success' if success else 'refusal/failure'})", flush=True)
            code, text, reviewed = lifecycle.terminal([*common, *flags, "helm", action, release, *parameters], environment,
                                                       confirm if confirm is not None else namespace + "/" + release,
                                                       callback, timeout=260)
            require(all(value not in text and base64.b64encode(value.encode()).decode() not in text
                        for value in (canary, "KBORG_SIGNATURE_FIXTURE_CANARY")),
                    "Helm extension review exposed Secret contents")
            if (code == 0) != success:
                # Application-generated error lines already suppress remote
                # diagnostics. Never retain the surrounding review/transcript.
                errors = re.findall(r"KBorg: ([^\r\n]{1,1200})", text)
                record["failure_category"] = errors[-1] if errors else "operation outcome; inspect scoped fixture state"
                summaries = re.findall(r"(Helm [a-z-]+ outcome UNKNOWN[^\r\n]{1,2000})", text)
                if summaries:
                    record["failure_summary"] = summaries[-1]
                jobs = json.loads(kube("-n", namespace, "get", "jobs", "-o", "json"))["items"]
                record["failure_job_phases"] = [{"name": job["metadata"]["name"], "active": job.get("status", {}).get("active", 0),
                    "succeeded": job.get("status", {}).get("succeeded", 0), "failed": job.get("status", {}).get("failed", 0),
                    "conditions": [{"type": c["type"], "status": c["status"], "reason": c.get("reason")} for c in job.get("status", {}).get("conditions", [])]}
                    for job in jobs]
            require((code == 0) == success, f"{action} {release} unexpected result; private diagnostics suppressed")
            if success or confirm is not None or callback is not None:
                require(reviewed, "extension action did not reach the expected terminal review")
            for expected_text in text_required:
                require(expected_text in text, f"extension review missing expected {expected_text!r}")
            return text

        # OCI transport tests are inserted before extension writes. The fixture
        # exposes OCI manifest/config/chart bytes over verified loopback TLS.
        registry_module = module("extension_oci_fixture", "fixture-helm-oci.py")
        def invoke_extra(crd_uid):
            extra_checks(target=target, args=args, directory=directory, record=record,
                         namespace=namespace, group=group, crd_name=crd_name, crd_uid=crd_uid,
                         run_id=run_id, image=image, canary=canary, kube=kube,
                         operation=operation, environment=environment, credential=credential,
                         reader_context=reader_context, first=first, second=second, simple=simple,
                         registry_module=registry_module)
        if extra_checks and getattr(args, "completion_only", False):
            operation("crds", "extended", first, flags=["--allow-helm-crds"])
            crd_uid = json.loads(kube("get", "crd", crd_name, "-o", "json"))["metadata"]["uid"]
            invoke_extra(crd_uid)
            record["passed"], record["phase"], record["scope"] = True, "complete", "completion-only; original extension regression excluded"
            print(target + ": completion acceptance passed", flush=True)
            return
        record["registry_authentication"] = []
        for mode in ("basic", "bearer", "private-bearer", "token-redirect"):
            with registry_module.Registry(directory, {"simple": simple}, mode=mode) as registry:
                flags = ["--helm-registry-ca", registry.ca_file]
                if registry.auth_file:
                    flags += ["--helm-registry-auth", registry.auth_file]
                release = "oci-" + mode
                operation("install", release, registry.refs["simple"], flags=flags, success=mode != "token-redirect")
                if mode != "token-redirect":
                    operation("uninstall", release)
                state = registry.report()
                require(state["requests"]["redirect_capture"] == 0, "token redirect destination received a request")
                record["registry_authentication"].append(state)
        with registry_module.Registry(directory, {"simple": simple, "first": first, "second": second}) as registry:
            registry_flags = ["--helm-registry-ca", registry.ca_file]
            reference = registry.refs["simple"]
            operation("install", "oci-untrusted-ca", reference, success=False)
            operation("install", "oci-tampered", registry.tampered_refs["simple"], flags=registry_flags, success=False,
                      text_required=("digest/size mismatch",))
            operation("install", "oci-simple", reference, flags=registry_flags,
                      text_required=("OCI source:", "Manifest digest:", "Chart layer digest:"))
            operation("uninstall", "oci-simple")
            record["checks"].append("verified-TLS digest-pinned OCI install/uninstall")
            extensions_flags = [*registry_flags, "--allow-helm-hooks", "--allow-helm-crds"]
            operation("crds", "extended", registry.refs["first"], flags=registry_flags, success=False)
            operation("crds", "extended", invalid, flags=["--allow-helm-crds"], success=False)
            operation("crds", "extended", registry.refs["first"], flags=extensions_flags, confirm="wrong/release", success=False)
            require(not kube("get", "crd", crd_name, "--ignore-not-found", "-o", "name").strip(), "unconfirmed CRD action changed cluster")
            operation("crds", "extended", registry.refs["first"], flags=extensions_flags)
            definition = json.loads(kube("get", "crd", crd_name, "-o", "json"))
            crd_uid = definition["metadata"]["uid"]
            require(any(item["type"] == "Established" and item["status"] == "True" for item in definition["status"]["conditions"]), "CRD did not establish")
            record["checks"].append("reviewed CRD create/Established; disabled/cluster-scope/wrong-confirm rejected")
            operation("install", "extended", registry.refs["first"], flags=[*registry_flags, "--allow-helm-crds"], success=False)
            operation("install", "extended", registry.refs["first"], flags=extensions_flags)

            def assert_hook(event):
                job = json.loads(kube("-n", namespace, "get", "job", "extended-" + event, "-o", "json"))
                require(job.get("status", {}).get("succeeded") == 1, event + " hook did not complete")

            for event in ("pre-install", "post-install"):
                assert_hook(event)
            completed_hook_record(json.loads(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.extended.v1", "-o", "json")),
                                  namespace, "extended", 1, "extended-post-cleanup", "Succeeded")
            require(not kube("-n", namespace, "get", "job", "extended-post-cleanup", "--ignore-not-found", "-o", "name").strip(), "hook-succeeded cleanup did not delete its Job")
            operation("upgrade", "extended", registry.refs["second"], flags=extensions_flags)
            require(json.loads(kube("-n", namespace, "get", "widgets." + group, "extended-widget", "-o", "json"))["spec"]["message"] == "revision-2", "custom resource upgrade mismatch")
            for event in ("pre-upgrade", "post-upgrade"):
                assert_hook(event)
            previous_hook_uid = json.loads(kube("-n", namespace, "get", "job", "extended-pre-upgrade", "-o", "json"))["metadata"]["uid"]
            operation("upgrade", "extended", registry.refs["second"], flags=extensions_flags)
            repeated_hook = json.loads(kube("-n", namespace, "get", "job", "extended-pre-upgrade", "-o", "json"))
            require(repeated_hook["metadata"]["uid"] != previous_hook_uid and repeated_hook.get("status", {}).get("succeeded") == 1,
                    "before-hook-creation did not replace the reviewed existing Job")
            def change_hook():
                kube("-n", namespace, "annotate", "job", "extended-pre-upgrade", "kborg.dev/stale-hook=true")
            operation("upgrade", "extended", registry.refs["second"], flags=extensions_flags, callback=change_hook, success=False,
                      text_required=("hook UID/resourceVersion changed",))
            operation("rollback", "extended", "1", flags=extensions_flags)
            require(json.loads(kube("-n", namespace, "get", "widgets." + group, "extended-widget", "-o", "json"))["spec"]["message"] == "revision-1", "custom resource rollback mismatch")
            for event in ("pre-rollback", "post-rollback"):
                assert_hook(event)
            def change_crd():
                kube("annotate", "crd", crd_name, "kborg.dev/stale-review=true")
            operation("uninstall", "extended", flags=extensions_flags, callback=change_crd, success=False)
            require(kube("-n", namespace, "get", "configmap", "extended-settings", "-o", "name").strip(), "stale CRD review submitted uninstall")
            operation("uninstall", "extended", flags=extensions_flags)
            for event in ("pre-delete", "post-delete"):
                assert_hook(event)
            require(not kube("-n", namespace, "get", "widgets." + group, "extended-widget", "--ignore-not-found", "-o", "name").strip(), "custom resource survived uninstall")
            require(json.loads(kube("get", "crd", crd_name, "-o", "json"))["metadata"]["uid"] == crd_uid, "Helm uninstall removed/replaced retained CRD")
            record["checks"].append("all 8 hook events completed; custom-resource install/upgrade/rollback/uninstall; stale CRD review blocked; CRD retained")
            record["checks"].append("before-hook-creation replaces exact stored hook; stale hook review blocked; hook-succeeded cleanup")
            operation("install", "failed-hook", failed_hook, flags=extensions_flags, success=False,
                      text_required=("outcome UNKNOWN",))
            completed_hook_record(json.loads(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.failed-hook.v1", "-o", "json")),
                                  namespace, "failed-hook", 1, "failed-hook-pre-install", "Failed")
            event_deadline, backoff_observed = time.monotonic() + 12, False
            while time.monotonic() < event_deadline:
                # Event recording can trail the Job failure watch and cleanup.
                # Bound each read separately and retain no event message/body.
                events = json.loads(execute([*prefix, "--request-timeout=3s", "-n", namespace, "get", "events",
                                            "--field-selector", "involvedObject.kind=Job,involvedObject.name=failed-hook-pre-install,reason=BackoffLimitExceeded",
                                            "-o", "json"], timeout=5)[1])
                backoff_observed = any(event.get("reason") == "BackoffLimitExceeded"
                                       and event.get("involvedObject", {}).get("kind") == "Job"
                                       and event.get("involvedObject", {}).get("name") == "failed-hook-pre-install"
                                       and event.get("involvedObject", {}).get("namespace") == namespace
                                       and event.get("involvedObject", {}).get("uid")
                                       for event in events.get("items", []))
                if backoff_observed:
                    break
                time.sleep(0.2)
            require(backoff_observed, "failed hook Job did not report BackoffLimitExceeded before cleanup")
            require(not kube("-n", namespace, "get", "job", "failed-hook-pre-install", "--ignore-not-found", "-o", "name").strip(), "hook-failed cleanup did not delete its Job")
            require(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.failed-hook.v1", "-o", "jsonpath={.metadata.labels.status}") == "failed", "failed pre-install hook did not fail the release")
            require(not kube("-n", namespace, "get", "configmap", "failed-hook-settings", "--ignore-not-found", "-o", "name").strip(), "failed pre-install hook created ordinary resources")
            operation("uninstall", "failed-hook", flags=extensions_flags)
            record["checks"].append("completed success hook deleted; failed Job BackoffLimitExceeded and completed Failed record; hook-failed cleanup; UNKNOWN with failed release; reviewed uninstall reconciles fixture")
            if target == "desktop" and args.ui_screenshots and extra_checks is None:
                record["phase"] = "live-extension-ui"
                ui = module("extension_live_ui", "test-live-extensions-ui.py")
                record["live_ui"] = ui.run(args.binary, args.helm, credential, reader_context, namespace,
                                            registry.refs["first"], environment, args.ui_screenshots,
                                            canary=canary, registry_ca=registry.ca_file)
                for event in ("pre-install", "post-install", "pre-delete", "post-delete"):
                    require(json.loads(kube("-n", namespace, "get", "job", "ui-extensions-" + event, "-o", "json")).get("status", {}).get("succeeded") == 1,
                            "live UI hook did not complete")
                require(not kube("-n", namespace, "get", "widgets." + group, "ui-extensions-widget", "--ignore-not-found", "-o", "name").strip(), "live UI uninstall left custom resource")
                require(json.loads(kube("get", "crd", crd_name, "-o", "json"))["metadata"]["uid"] == crd_uid, "live UI removed/replaced retained schema")
            if extra_checks:
                invoke_extra(crd_uid)
            record["registry"] = registry.report()
        record["passed"], record["phase"] = True, "complete"
        print(target + ": OCI/hooks/CRDs acceptance passed", flush=True)
    finally:
        # Each independently owned target gets a cleanup attempt even when a
        # stuck CRD or namespace cannot finish deleting within its deadline.
        cleanup_errors = []
        def cleanup_part(label, action):
            try:
                action()
            except (RuntimeError, OSError, ValueError, KeyError):
                cleanup_errors.append(label)

        def cleanup_crd():
            definition_text = kube("get", "crd", crd_name, "--ignore-not-found", "-o", "json")
            if not definition_text.strip():
                return
            definition = json.loads(definition_text)
            require(definition["metadata"].get("labels", {}).get("kborg.dev/run-id") == run_id,
                    "CRD cleanup ownership changed")
            kube("delete", "--raw", "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/" + crd_name,
                 "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": definition["metadata"]["uid"]}}))
            execute([*prefix, "wait", "--for=delete", "crd/" + crd_name, "--timeout=60s"], timeout=70)

        def cleanup_namespace():
            current = json.loads(kube("get", "namespace", namespace, "-o", "json"))
            require(current["metadata"]["uid"] == namespace_uid and current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run_id, "namespace cleanup ownership changed")
            kube("delete", "--raw", "/api/v1/namespaces/" + namespace, "-f", "-",
                 data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": namespace_uid}}))
            execute([*prefix, "wait", "--for=delete", "namespace/" + namespace, "--timeout=90s"], timeout=100)

        def cleanup_rbac(kind, name, uid):
            current = json.loads(kube("get", kind, name, "-o", "json"))
            require(current["metadata"]["uid"] == uid and current["metadata"].get("labels", {}).get("kborg.dev/run-id") == run_id, "cluster RBAC cleanup ownership changed")
            kube("delete", "--raw", "/apis/rbac.authorization.k8s.io/v1/" + kind + "/" + name, "-f", "-",
                 data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}}))

        # Revoke the test's cluster-wide create grant first, before waiting for
        # potentially slow garbage collection. Namespace-local grants disappear
        # with the namespace and every token's expiry is independently bounded.
        for kind, name, uid in reversed(cluster_owned):
            cleanup_part(kind, lambda kind=kind, name=name, uid=uid: cleanup_rbac(kind, name, uid))
        cleanup_part("crd", cleanup_crd)
        cleanup_part("namespace", cleanup_namespace)
        record["cleanup_errors"] = cleanup_errors
        record["cleanup"], record["seconds"] = not cleanup_errors, round(time.monotonic() - started, 3)
        require(not cleanup_errors, "extension fixture cleanup incomplete: " + ", ".join(cleanup_errors))



def main(extra_checks=None, extra_sources=(), configure_parser=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=("desktop", "minikube", "both"), default="both")
    parser.add_argument("--expected-desktop-server")
    parser.add_argument("--expected-minikube-server")
    parser.add_argument("--binary", type=Path, default=PROJECT / "target/release/kborg")
    parser.add_argument("--helm", type=Path, default=PROJECT / ".local/platform-tools/helm")
    parser.add_argument("--ui-screenshots", type=Path)
    parser.add_argument("--output", type=Path, default=PROJECT / "test-results/helm-extensions-local.json")
    if configure_parser:
        configure_parser(parser)
    args = parser.parse_args()
    args.binary, args.helm = args.binary.resolve(strict=True), args.helm.resolve(strict=True)
    targets = ("desktop", "minikube") if args.target == "both" else (args.target,)
    require(all(args.expected_desktop_server if target == "desktop" else args.expected_minikube_server for target in targets), "exact local endpoints required before writes")
    image = execute([shutil.which("docker"), "image", "inspect", "--format", "{{index .RepoDigests 0}}", "busybox:1.37.0"])[1].strip()
    require("@sha256:" in image and len(image.rsplit(":", 1)[1]) == 64, "fixture image must be digest-pinned")
    paths = sorted((PROJECT / "src").glob("*.rs")) + sorted((PROJECT / "tests").glob("*.rs"))
    paths += [PROJECT / name for name in ("Cargo.toml", "Cargo.lock", "scripts/test-helm-extensions.py", "scripts/fixture-helm-oci.py", "scripts/test-helm-lifecycle.py", "scripts/test-helm-logs-metrics.py")]
    paths += [PROJECT / name for name in extra_sources]
    if args.ui_screenshots:
        paths += [PROJECT / "scripts/test-live-extensions-ui.py", PROJECT / "scripts/test-live-lifecycle-ui.py", PROJECT / "scripts/test-terminal.py"]
    hashes = {str(path.relative_to(PROJECT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    report = {"schema": 1, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "helm_version": execute([args.helm, "version", "--short"])[1].strip(),
              "helm_sha256": hashlib.sha256(args.helm.read_bytes()).hexdigest(),
              "tested_source_sha256": hashes, "targets": [], "temporary_credentials_removed": False, "source_unchanged": False}
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-extension-acceptance-") as temporary:
            directory = Path(temporary).resolve()
            directory.chmod(0o700)
            for target in targets:
                run_target(target, args, directory, image, report, extra_checks=extra_checks)
    finally:
        report["temporary_credentials_removed"] = directory is not None and not directory.exists()
        report["source_unchanged"] = all(hashlib.sha256((PROJECT / path).read_bytes()).hexdigest() == digest for path, digest in hashes.items())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    require(report["source_unchanged"], "source changed during extension acceptance; rerun stable source")
    require(report["temporary_credentials_removed"] and all(target["cleanup"] for target in report["targets"]), "extension fixture cleanup unconfirmed")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error) if isinstance(error, RuntimeError) else "Extension acceptance failed; private diagnostics suppressed") from None
