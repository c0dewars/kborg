#!/usr/bin/env python3
"""Owned local CRD storage migration acceptance; no context/provider changes.

Desktop validates honest native staging when SVM is absent. An explicitly owned
minikube with its official StorageVersionMigrator enabled validates controller
completion. GET apiVersion is never recorded as persisted-storage proof.
"""
import argparse
import hashlib
import importlib.util
import io
import ipaddress
import json
import os
from pathlib import Path
import re
import shutil
import tarfile
import tempfile
import time
from urllib.parse import urlparse
import uuid

PROJECT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("migration_extensions", PROJECT / "scripts/test-helm-extensions.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
execute, require, object_ = base.execute, base.require, base.object_


def chart(path, schema):
    files = {"Chart.yaml": b"apiVersion: v2\nname: kborg-migration-fixture\nversion: 0.1.0\n",
             "crds/widgets.yaml": json.dumps(schema).encode()}
    with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for name, data in files.items():
            item = tarfile.TarInfo("fixture/" + name)
            item.size, item.mode = len(data), 0o600
            archive.addfile(item, io.BytesIO(data))
    path.chmod(0o600)


def run_target(target, args, temporary, report):
    context = "docker-desktop" if target == "desktop" else "kborg-validation"
    expected = args.expected_desktop_server if target == "desktop" else args.expected_minikube_server
    config_path = Path.home() / ".kube/config" if target == "desktop" else PROJECT / ".local/minikube.kubeconfig"
    prefix = [shutil.which("kubectl"), "--kubeconfig", config_path, "--context", context, "--request-timeout=20s"]
    def kube(*command, data=None, success=True):
        return execute([*prefix, *command], data=data, timeout=40, success=success)[1]
    config = json.loads(kube("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
    require(len(config["contexts"]) == 1 and config["contexts"][0]["name"] == context, "migration fixture context mismatch")
    cluster = config["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    local = endpoint.hostname in ("127.0.0.1", "localhost", "kubernetes.docker.internal")
    if target == "minikube" and not local:
        local = ipaddress.ip_address(endpoint.hostname).is_private
    require(cluster["server"] == expected and endpoint.scheme == "https" and local and endpoint.port and endpoint.port >= 1024
            and not endpoint.username and not endpoint.password and endpoint.path in ("", "/") and not endpoint.query and not endpoint.fragment
            and not cluster.get("insecure-skip-tls-verify") and not cluster.get("proxy-url") and cluster.get("certificate-authority-data"), "migration fixture exact TLS endpoint mismatch")
    directory = temporary / target
    directory.mkdir(mode=0o700)
    run_id = uuid.uuid4().hex[:12]
    namespace = "kborg-migration-" + run_id
    other_namespace = namespace + "-other"
    group = run_id + ".migration.kborg.test"
    crd_name = "widgets." + group
    record = {"target": target, "context": context, "server": expected, "run_id": run_id,
              "namespace": namespace, "kubernetes": json.loads(kube("get", "--raw=/version"))["gitVersion"],
              "passed": False, "cleanup": False, "checks": [], "controller_migrations": []}
    report["targets"].append(record)
    namespaces, rbac = [], []
    crd_uid = None
    controller_api = None
    started = time.monotonic()
    try:
        for name in (namespace, other_namespace):
            obj = object_("Namespace", name)
            obj["metadata"]["labels"] = {"kborg.dev/run-id": run_id, "pod-security.kubernetes.io/enforce": "restricted"}
            live = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(obj)))
            namespaces.append((name, live["metadata"]["uid"]))
        operator = object_("ServiceAccount", "operator", namespace, automountServiceAccountToken=False)
        kube("create", "-f", "-", data=json.dumps(operator))
        rules = [{"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "verbs": ["create"]},
                 {"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions"], "resourceNames": [crd_name], "verbs": ["get", "update"]},
                 {"apiGroups": [group], "resources": ["widgets"], "verbs": ["get", "list", "update"]},
                 {"apiGroups": ["storagemigration.k8s.io"], "resources": ["storageversionmigrations"], "verbs": ["create", "get"]}]
        role = {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole", "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}}, "rules": rules}
        binding = {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding", "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}},
                   "subjects": [{"kind": "ServiceAccount", "name": "operator", "namespace": namespace}],
                   "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "ClusterRole", "name": namespace}}
        for obj in (role, binding):
            live = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(obj)))
            rbac.append((obj["kind"].lower() + "s", namespace, live["metadata"]["uid"]))
        token = kube("-n", namespace, "create", "token", "operator", "--duration=30m").strip()
        credential, reader_context = base.base.private_reader(directory, cluster, namespace, token)
        del token, config
        environment = {key: value for key, value in os.environ.items() if not key.startswith("HELM_")}
        environment.update(TERM="xterm-256color", KBORG_CONFIG_DIR=str(directory / "settings"), KBORG_NO_SPLASH="1")
        common = [args.binary, "--kubeconfig", credential, "--context", reader_context, "-n", namespace,
                  "--write", "--allow-helm", "--allow-helm-write", "--allow-helm-crds", "--helm-tool", args.helm, "--helm-timeout", "240"]
        def operation(action, packaged, *, success=True, confirm=None, callback=None):
            record["phase"] = action
            phrase = ("MIGRATE CRDS " if action == "crds-migrate" else "") + namespace + "/migration"
            code, text, reviewed = base.lifecycle.terminal([*common, "helm", action, "migration", packaged], environment,
                                                          phrase if confirm is None else confirm, callback, timeout=390)
            if (code == 0) != success:
                categories = re.findall(r"(?:KBorg: |CRD migration PARTIAL/UNKNOWN: )([^\r\n]{1,1200})", text)
                record["failure_category"] = categories[-1] if categories else "migration result mismatch; private transcript suppressed"
            require((code == 0) == success, "migration action had unexpected result; private diagnostics suppressed")
            if success or callback or confirm is not None:
                require(reviewed, "migration did not reach exact controlling-terminal review")
            return text
        schema = {"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition", "metadata": {"name": crd_name, "labels": {"kborg.dev/run-id": run_id}},
                  "spec": {"group": group, "scope": "Namespaced", "names": {"kind": "Widget", "plural": "widgets"}, "versions": [{"name": "v1", "served": True, "storage": True,
                  "schema": {"openAPIV3Schema": {"type": "object", "properties": {"spec": {"type": "object", "properties": {"message": {"type": "string"}}}}}}}]}}
        desired = json.loads(json.dumps(schema))
        second = json.loads(json.dumps(desired["spec"]["versions"][0]))
        second["name"] = "v2"
        desired["spec"]["versions"][0]["storage"] = False
        desired["spec"]["versions"].append(second)
        old_chart, new_chart, invalid_chart, removed_chart = [directory / name for name in ("old.tgz", "new.tgz", "invalid.tgz", "removed.tgz")]
        chart(old_chart, schema)
        chart(new_chart, desired)
        invalid = json.loads(json.dumps(desired))
        invalid["spec"]["versions"][1]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["message"]["type"] = "integer"
        chart(invalid_chart, invalid)
        removed = json.loads(json.dumps(desired)); removed["spec"]["versions"].pop(0)
        chart(removed_chart, removed)
        operation("crds", old_chart)
        live_schema = json.loads(kube("get", "crd", crd_name, "-o", "json"))
        crd_uid = live_schema["metadata"]["uid"]
        instances = {}
        for index, name in enumerate((namespace, other_namespace)):
            obj = {"apiVersion": group + "/v1", "kind": "Widget", "metadata": {"name": "owned-" + str(index), "namespace": name,
                   "labels": {"app.kubernetes.io/managed-by": "Helm", "kborg.dev/run-id": run_id},
                   "annotations": {"meta.helm.sh/release-name": "migration", "meta.helm.sh/release-namespace": namespace}}, "spec": {"message": "preserved-" + str(index)}}
            live = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(obj)))
            instances[(name, obj["metadata"]["name"])] = {"uid": live["metadata"]["uid"], "rv": live["metadata"]["resourceVersion"], "spec": live["spec"]}
        operation("crds-migrate", invalid_chart, success=False)
        operation("crds-migrate", removed_chart, success=False)
        foreign = {"apiVersion": group + "/v1", "kind": "Widget", "metadata": {"name": "foreign", "namespace": other_namespace, "labels": {"kborg.dev/run-id": run_id}}, "spec": {"message": "do not rewrite"}}
        foreign = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(foreign)))
        operation("crds-migrate", new_chart, success=False)
        require(kube("-n", other_namespace, "get", "widgets." + group, "foreign", "-o", "jsonpath={.metadata.resourceVersion}") == foreign["metadata"]["resourceVersion"], "foreign instance was rewritten")
        kube("delete", "--raw", "/apis/" + group + "/v1/namespaces/" + other_namespace + "/widgets/foreign", "-f", "-",
             data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": foreign["metadata"]["uid"], "resourceVersion": foreign["metadata"]["resourceVersion"]}}))
        for version in ("v1", "v1beta1", "v1alpha1"):
            result = execute([*prefix, "get", "--raw=/apis/storagemigration.k8s.io/" + version], success=False)
            if result[0] == 0:
                controller_api = version
                break
        if controller_api:
            operation("crds-migrate", new_chart, success=False)  # exact status UPDATE permission absent
            rules.append({"apiGroups": ["apiextensions.k8s.io"], "resources": ["customresourcedefinitions/status"], "resourceNames": [crd_name], "verbs": ["update"]})
            role_live = json.loads(kube("get", "clusterrole", namespace, "-o", "json"))
            require(role_live["metadata"]["uid"] == rbac[0][2], "fixture role identity changed")
            role_live["rules"] = rules
            kube("replace", "-f", "-", data=json.dumps(role_live))
        require(controller_api is not None if target == "minikube" else controller_api is None, "expected minikube controller/Desktop fallback API scope changed")
        operation("crds-migrate", new_chart, confirm=namespace + "/migration", success=False)
        operation("crds-migrate", new_chart, callback=lambda: kube("-n", namespace, "annotate", "widgets." + group, "owned-0", "kborg.dev/stale-migration=true"), success=False)
        current = json.loads(kube("get", "crd", crd_name, "-o", "json"))
        require(next(version["name"] for version in current["spec"]["versions"] if version["storage"]) == "v1", "refused migration switched schema storage")
        if args.ui_review_script:
            def identity_snapshot():
                schema_identity = json.loads(kube("get", "crd", crd_name, "-o", "json"))["metadata"]
                instance_identities = {}
                for name, instance in instances:
                    metadata = json.loads(kube("-n", name, "get", "widgets." + group, instance, "-o", "json"))["metadata"]
                    instance_identities[name + "/" + instance] = (metadata["uid"], metadata["resourceVersion"])
                return (schema_identity["uid"], schema_identity["resourceVersion"], instance_identities)
            before_ui = identity_snapshot()
            ui_spec = importlib.util.spec_from_file_location("migration_trust_ui", args.ui_review_script)
            ui = importlib.util.module_from_spec(ui_spec)
            ui_spec.loader.exec_module(ui)
            record["phase"] = "live-migration-review"
            record["live_ui"] = ui.run(args.binary, environment, common[1:], namespace, "migration", new_chart,
                                         args.screenshots / target)
            require(identity_snapshot() == before_ui, "canceled live migration UI changed schema/instance UID or resourceVersion")
            record["live_ui"]["canceled_review_schema_and_instance_uid_rv_unchanged"] = True
            record["ui_capture_count"] = len(record["live_ui"].get("screens", []))
            record["ui_capture_paths"] = [screen["path"] for screen in record["live_ui"].get("screens", [])]
            record["ui_report_path"] = str((args.screenshots / target / "report.json").resolve())
        text = operation("crds-migrate", new_chart)
        match = re.search(r"Migration report: (\{[^\r\n]+\})", text)
        require(match is not None, "migration omitted structured outcome")
        outcome = json.loads(match.group(1))
        require(outcome["rewritten"] == 2 and outcome["switched"] == 1, "migration did not rewrite complete captured membership")
        current = json.loads(kube("get", "crd", crd_name, "-o", "json"))
        require(current["metadata"]["uid"] == crd_uid and current["metadata"]["labels"]["kborg.dev/run-id"] == run_id, "migration changed schema identity/metadata")
        if controller_api:
            require(outcome["complete"] and outcome["finalized"] == 1 and current["status"]["storedVersions"] == ["v2"], "controller-backed migration did not finalize storage status")
            require(len(outcome["controller_migrations"]) == 1, "controller migration identity missing")
            for migration in outcome["controller_migrations"]:
                svm = json.loads(kube("get", "storageversionmigrations.storagemigration.k8s.io", migration["name"], "-o", "json"))
                require(svm["metadata"]["uid"] == migration["uid"] and svm["spec"]["resource"]["group"] == group and svm["spec"]["resource"]["resource"] == "widgets"
                        and any(condition["type"] == "Succeeded" and condition["status"] == "True" for condition in svm["status"]["conditions"]), "authoritative controller proof missing or mismatched")
                record["controller_migrations"].append(migration)
        else:
            require(not outcome["complete"] and outcome["pending"] == 1 and set(current["status"]["storedVersions"]) == {"v1", "v2"}, "native fallback falsely finalized storage proof")
            resumed_text = operation("crds-migrate", new_chart)
            resumed = json.loads(re.search(r"Migration report: (\{[^\r\n]+\})", resumed_text).group(1))
            require(not resumed["complete"] and resumed["switched"] == 0 and resumed["rewritten"] == 2 and resumed["pending"] == 1, "fresh review did not resume honest native stage")
        for (name, instance), before in instances.items():
            live = json.loads(kube("-n", name, "get", "widgets." + group, instance, "-o", "json"))
            require(live["metadata"]["uid"] == before["uid"] and live["metadata"]["resourceVersion"] != before["rv"] and live["spec"] == before["spec"]
                    and "kborg.dev/storage-migration" not in live["metadata"].get("annotations", {}), "instance content/identity/marker preservation failed")
        record["checks"] = ["incompatible schemas and old-version removal refused", "ordinary confirmation and foreign cross-namespace instance refused",
                            "stale instance review refused before storage switch", "all captured owned instances across two namespaces rewritten with content and UID preservation",
                            "official controller Succeeded+checkpoint and guarded status finalization" if controller_api else "native stage and fresh review retain historical storedVersions without claiming complete storage proof"]
        record["controller_api"] = controller_api
        record["outcome"] = outcome
        record["passed"], record["phase"] = True, "complete"
    finally:
        cleanup_errors = []
        # Controller objects can continue after client cancellation. Query only
        # kborg-managed objects, then bind cleanup to this exact owned schema/ns.
        if crd_uid:
            listed = execute([*prefix, "get", "storageversionmigrations.storagemigration.k8s.io", "-l", "app.kubernetes.io/managed-by=kborg", "-o", "json"], success=False)
            if listed[0] == 0:
                for migration in json.loads(listed[1]).get("items", []):
                    metadata = migration["metadata"]
                    annotations = metadata.get("annotations", {})
                    if (annotations.get("kborg.dev/crd-uid") == crd_uid and annotations.get("kborg.dev/confirmation-namespace") == namespace
                            and annotations.get("kborg.dev/confirmation-release") == "migration" and metadata["name"].startswith("kborg-migrate-")
                            and migration.get("spec", {}).get("resource", {}).get("group") == group
                            and migration.get("spec", {}).get("resource", {}).get("resource") == "widgets"):
                        try:
                            kube("delete", "--raw", "/apis/" + migration["apiVersion"] + "/storageversionmigrations/" + metadata["name"], "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": metadata["uid"]}}))
                            kube("wait", "--for=delete", "storageversionmigrations.storagemigration.k8s.io/" + metadata["name"], "--timeout=30s")
                        except RuntimeError:
                            cleanup_errors.append("owned migration controller object cleanup failed")
            elif controller_api:
                cleanup_errors.append("owned migration controller inventory cleanup could not be verified")
            try:
                live = json.loads(kube("get", "crd", crd_name, "-o", "json"))
                require(live["metadata"]["uid"] == crd_uid and live["metadata"]["labels"]["kborg.dev/run-id"] == run_id, "schema cleanup ownership changed")
                kube("delete", "--raw", "/apis/apiextensions.k8s.io/v1/customresourcedefinitions/" + crd_name, "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": crd_uid}}))
                kube("wait", "--for=delete", "crd/" + crd_name, "--timeout=30s")
            except RuntimeError:
                cleanup_errors.append("owned schema cleanup failed")
        for resource, name, uid in reversed(rbac):
            try:
                live = json.loads(kube("get", resource, name, "-o", "json"))
                require(live["metadata"]["uid"] == uid and live["metadata"]["labels"]["kborg.dev/run-id"] == run_id, "RBAC cleanup ownership changed")
                kube("delete", "--raw", "/apis/rbac.authorization.k8s.io/v1/" + resource + "/" + name, "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}}))
            except RuntimeError:
                cleanup_errors.append("owned RBAC cleanup failed")
        for name, uid in reversed(namespaces):
            try:
                live = json.loads(kube("get", "namespace", name, "-o", "json"))
                require(live["metadata"]["uid"] == uid and live["metadata"]["labels"]["kborg.dev/run-id"] == run_id, "namespace cleanup ownership changed")
                kube("delete", "--raw", "/api/v1/namespaces/" + name, "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}}))
                kube("wait", "--for=delete", "namespace/" + name, "--timeout=30s")
            except RuntimeError:
                cleanup_errors.append("owned namespace cleanup failed")
        record["cleanup_errors"], record["cleanup"] = cleanup_errors, not cleanup_errors
        record["seconds"] = round(time.monotonic() - started, 3)
        require(not cleanup_errors, "migration fixture cleanup unconfirmed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--helm", type=Path, default=PROJECT / ".local/platform-tools/helm")
    parser.add_argument("--target", choices=("desktop", "minikube", "both"), default="both")
    parser.add_argument("--expected-desktop-server")
    parser.add_argument("--expected-minikube-server")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--ui-review-script", type=Path)
    parser.add_argument("--screenshots", type=Path)
    args = parser.parse_args()
    args.binary, args.helm = args.binary.resolve(strict=True), args.helm.resolve(strict=True)
    if args.ui_review_script:
        args.ui_review_script = args.ui_review_script.resolve(strict=True)
        require(args.screenshots is not None, "UI review hook requires --screenshots")
        args.screenshots = args.screenshots.resolve()
    targets = ("desktop", "minikube") if args.target == "both" else (args.target,)
    require(all(args.expected_desktop_server if target == "desktop" else args.expected_minikube_server for target in targets), "explicit expected endpoints required")
    sources = sorted((PROJECT / "src").glob("*.rs")) + [PROJECT / path for path in ("Cargo.toml", "Cargo.lock", "scripts/test-helm-migration.py", "scripts/test-helm-extensions.py", "scripts/test-helm-lifecycle.py", "scripts/test-helm-logs-metrics.py")]
    if args.ui_review_script:
        sources.append(args.ui_review_script)
        sources += [PROJECT / "scripts/test-terminal.py", PROJECT / "scripts/test-live-lifecycle-ui.py"]
    hashes = {str(path.relative_to(PROJECT)): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources}
    report = {"schema": 1, "timestamp_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
              "helm_sha256": hashlib.sha256(args.helm.read_bytes()).hexdigest(), "helm_version": execute([args.helm, "version", "--short"])[1].strip(),
              "targets": [], "tested_source_sha256": hashes, "temporary_credentials_removed": False, "source_unchanged": False}
    directory = None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-migration-") as temporary:
            directory = Path(temporary).resolve(); directory.chmod(0o700)
            for target in targets:
                run_target(target, args, directory, report)
    finally:
        report["temporary_credentials_removed"] = directory is not None and not directory.exists()
        report["source_unchanged"] = all(hashlib.sha256((PROJECT / path).read_bytes()).hexdigest() == value for path, value in hashes.items())
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    require(report["source_unchanged"], "source changed during migration acceptance; rerun frozen source")
    require(report["temporary_credentials_removed"] and all(target["cleanup"] and target["passed"] for target in report["targets"]), "migration acceptance or cleanup failed")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error) if isinstance(error, RuntimeError) else "Migration acceptance failed; private diagnostics suppressed") from None
