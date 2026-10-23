#!/usr/bin/env python3
"""Owned local acceptance: OCI tags/provenance, helm test, CRD update/delete.

Extends the existing isolated two-cluster extension harness and cleanup. No
personal signing keys, raw release bodies or terminal transcripts are retained.
"""
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tarfile

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("completion_base", Path(__file__).with_name("test-helm-extensions.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
require = base.require


def changed_chart(source, target, change):
    with tarfile.open(source, "r:gz") as archive:
        files = {member.name: archive.extractfile(member).read().decode() for member in archive.getmembers() if member.isfile()}
    change(files)
    with tarfile.open(target, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for name, content in files.items():
            data = content.encode(); member = tarfile.TarInfo(name)
            member.size, member.mode = len(data), 0o600
            archive.addfile(member, io.BytesIO(data))
    target.chmod(0o600)


def extra_checks(**ctx):
    args, record, namespace = (ctx[key] for key in ("args", "record", "namespace"))
    directory, kube, operation = (ctx[key] for key in ("directory", "kube", "operation"))
    flags = ["--allow-helm-hooks", "--allow-helm-crds"]
    signatures = args.signature_fixtures.resolve(strict=True)
    manifest = json.loads((signatures / "fixtures.json").read_text())
    # The fixture generator's manifest contains filenames/public trust only.
    def fixture(key):
        name = manifest[key]
        require(isinstance(name, str) and Path(name).name == name, "invalid signature fixture path")
        path = (signatures / name).resolve(strict=True)
        require(path.parent == signatures, "signature fixture escaped its directory")
        return path
    first_signed, second_signed = fixture("first"), fixture("second")
    keyring, wrong_keyring = fixture("keyring"), fixture("wrong_keyring")
    with ctx["registry_module"].Registry(directory, {"signed-first": first_signed, "signed-second": second_signed}) as registry:
        signed_flags = ["--helm-registry-ca", registry.ca_file, "--helm-keyring", keyring]
        tag = registry.tag_refs["signed-first"]
        operation("install", "signed", tag, flags=signed_flags,
                  callback=lambda: registry.move_tag("signed-first", "signed-second"))
        require(json.loads(kube("-n", namespace, "get", "configmap", "signed-settings", "-o", "json"))["data"]["message"] == "revision-1",
                "tag movement after review changed the sealed install")
        operation("upgrade", "signed", tag, flags=signed_flags)
        require(json.loads(kube("-n", namespace, "get", "configmap", "signed-settings", "-o", "json"))["data"]["message"] == "revision-2",
                "fresh tag review did not resolve the updated chart")
        operation("uninstall", "signed")
        operation("install", "wrong-key", registry.refs["signed-first"],
                  flags=["--helm-registry-ca", registry.ca_file, "--helm-keyring", wrong_keyring], success=False)
        record["checks"].append("signed OCI tag resolves once; post-review tag movement does not change installation; fresh upgrade resolves new content; wrong key rejected")
        record["completion_registry"] = registry.report()
    operation("install", "signed-local", first_signed, flags=["--helm-keyring", keyring])
    operation("uninstall", "signed-local")
    operation("install", "unsigned", ctx["simple"], flags=["--helm-keyring", keyring], success=False)
    tampered_dir = directory / "tampered-signature"; tampered_dir.mkdir(mode=0o700)
    tampered = tampered_dir / first_signed.name
    shutil.copy2(first_signed, tampered)
    provenance = Path(str(tampered) + ".prov")
    provenance.write_bytes(b"invalid OpenPGP provenance\n"); provenance.chmod(0o600)
    operation("install", "bad-provenance", tampered, flags=["--helm-keyring", keyring], success=False)
    shutil.copy2(Path(str(first_signed) + ".prov"), provenance)
    changed_chart(first_signed, tampered, lambda files: files.update({
        "fixture/templates/unsigned-extra.yaml": "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: unsigned-extra\n"}))
    operation("install", "changed-content", tampered, flags=["--helm-keyring", keyring], success=False)
    sealed_keyring = directory / "reviewed-public.gpg"
    shutil.copy2(keyring, sealed_keyring); sealed_keyring.chmod(0o600)
    operation("install", "changed-keyring", first_signed, flags=["--helm-keyring", sealed_keyring], success=False,
              callback=lambda: sealed_keyring.write_bytes(sealed_keyring.read_bytes() + b"changed"))
    record["checks"].append("local trusted OpenPGP chart accepted; missing/corrupt provenance, changed chart content and post-review keyring drift rejected")

    test_chart, failed_chart = directory / "tests.tgz", directory / "failed-tests.tgz"
    def tests(files):
        text = files["fixture/templates/pre-install.yaml"].replace("pre-install", "smoke")
        text = text.replace("helm.sh/hook: smoke", "helm.sh/hook: test")
        files["fixture/templates/test.yaml"] = text
        files["fixture/templates/legacy-test.yaml"] = text.replace("}}-smoke", "}}-legacy").replace("helm.sh/hook: test", "helm.sh/hook: test-success")
    changed_chart(ctx["first"], test_chart, tests)
    operation("install", "chart-tests", test_chart, flags=flags)
    require(not kube("-n", namespace, "get", "job", "chart-tests-smoke", "--ignore-not-found", "-o", "name").strip(), "test hook ran during installation")
    operation("test", "chart-tests", flags=flags)
    for name in ("smoke", "legacy"):
        base.completed_hook_record(json.loads(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.chart-tests.v1", "-o", "json")), namespace, "chart-tests", 1, "chart-tests-" + name, "Succeeded")
    before = json.loads(kube("-n", namespace, "get", "job", "chart-tests-smoke", "-o", "json"))["metadata"]["uid"]
    operation("test", "chart-tests", flags=flags)
    require(json.loads(kube("-n", namespace, "get", "job", "chart-tests-smoke", "-o", "json"))["metadata"]["uid"] != before, "repeated helm test did not replace stored test hook")
    operation("test", "chart-tests", flags=flags, success=False,
              callback=lambda: kube("-n", namespace, "annotate", "job", "chart-tests-smoke", "kborg.dev/test-drift=true"))
    require(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.chart-tests.v1", "-o", "jsonpath={.metadata.labels.status}") == "deployed", "helm test unexpectedly changed release deployment status")
    operation("uninstall", "chart-tests", flags=flags)
    def fail_test(files):
        files["fixture/templates/test.yaml"] = files["fixture/templates/test.yaml"].replace("echo KBORG_HOOK_smoke", "echo KBORG_TEST_FAILED; exit 1")
    changed_chart(test_chart, failed_chart, fail_test)
    operation("install", "failed-tests", failed_chart, flags=flags)
    operation("test", "failed-tests", flags=flags, success=False)
    base.completed_hook_record(json.loads(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.failed-tests.v1", "-o", "json")), namespace, "failed-tests", 1, "failed-tests-smoke", "Failed")
    require(kube("-n", namespace, "get", "configmap", "failed-tests-settings", "-o", "name").strip(), "failed release test removed ordinary resources")
    operation("uninstall", "failed-tests", flags=flags)
    record["checks"].append("stored test/test-success hooks run only on explicit helm test; repeated tests replace hooks; stale-hook approval rejected; failed test recorded without removing release resources")

    updated, incompatible = directory / "schema-update.tgz", directory / "schema-incompatible.tgz"
    def schema_update(files):
        crd = json.loads(files["fixture/crds/widgets.yaml"])
        crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["note"] = {"type": "string"}
        files["fixture/crds/widgets.yaml"] = json.dumps(crd)
    changed_chart(ctx["first"], updated, schema_update)
    def incompatible_schema(files):
        crd = json.loads(files["fixture/crds/widgets.yaml"])
        crd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["required"].append("note")
        files["fixture/crds/widgets.yaml"] = json.dumps(crd)
    changed_chart(updated, incompatible, incompatible_schema)
    instance = {"apiVersion": ctx["group"] + "/v1", "kind": "Widget", "metadata": {"name": "schema-owned", "namespace": namespace,
                "labels": {"app.kubernetes.io/managed-by": "Helm", "kborg.dev/run-id": ctx["run_id"]},
                "annotations": {"meta.helm.sh/release-name": "schema-ops", "meta.helm.sh/release-namespace": namespace}}, "spec": {"message": "retained"}}
    kube("create", "-f", "-", data=json.dumps(instance))
    operation("crds-upgrade", "schema-ops", updated, flags=flags)
    schema = json.loads(kube("get", "crd", ctx["crd_name"], "-o", "json"))
    require(schema["metadata"]["uid"] == ctx["crd_uid"] and "note" in schema["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"], "CRD update did not preserve identity/add optional property")
    operation("crds-upgrade", "schema-ops", incompatible, flags=flags, success=False)
    foreign_ns = namespace + "-foreign"
    foreign_namespace = {"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": foreign_ns,
                         "labels": {"kborg.dev/run-id": ctx["run_id"], "pod-security.kubernetes.io/enforce": "restricted"}}}
    foreign_uid = json.loads(kube("create", "-f", "-", "-o", "json", data=json.dumps(foreign_namespace)))["metadata"]["uid"]
    try:
        foreign = {"apiVersion": ctx["group"] + "/v1", "kind": "Widget", "metadata": {"name": "foreign", "namespace": foreign_ns}, "spec": {"message": "outside selected namespace"}}
        kube("create", "-f", "-", data=json.dumps(foreign))
        operation("crds-delete", "schema-ops", updated, flags=flags, success=False)
        require(kube("-n", foreign_ns, "get", "widgets." + ctx["group"], "foreign", "-o", "name").strip(), "foreign instance was deleted")
    finally:
        current = json.loads(kube("get", "namespace", foreign_ns, "-o", "json"))
        require(current["metadata"]["uid"] == foreign_uid and current["metadata"]["labels"]["kborg.dev/run-id"] == ctx["run_id"], "foreign fixture namespace ownership changed")
        kube("delete", "--raw", "/api/v1/namespaces/" + foreign_ns, "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": foreign_uid}}))
        kube("wait", "--for=delete", "namespace/" + foreign_ns, "--timeout=60s")
    operation("crds-delete", "schema-ops", updated, flags=flags, confirm=namespace + "/schema-ops", success=False)
    operation("crds-delete", "schema-ops", updated, flags=flags, confirm="DELETE CRDS " + namespace + "/schema-ops", success=False,
              callback=lambda: kube("-n", namespace, "annotate", "widgets." + ctx["group"], "schema-owned", "kborg.dev/stale-instance=true"))
    require(kube("-n", namespace, "get", "widgets." + ctx["group"], "schema-owned", "-o", "name").strip(), "stale schema deletion removed an instance")
    operation("crds-delete", "schema-ops", updated, flags=flags, confirm="DELETE CRDS " + namespace + "/schema-ops")
    require(not kube("get", "crd", ctx["crd_name"], "--ignore-not-found", "-o", "name").strip(), "reviewed CRD deletion did not finish")
    record["checks"].append("CRD update preserves UID and existing instance; incompatible schema refused; foreign instance in another namespace blocks deletion; ordinary confirmation/stale instance rejected; explicit owned-instance/schema deletion completes")
    if ctx["target"] == "desktop" and args.ui_screenshots:
        operation("crds", "ui-completion", ctx["first"], flags=flags)
        ui = base.module("completion_live_ui", "test-live-completion-ui.py")
        def verify_ui_tests():
            stored = json.loads(kube("-n", namespace, "get", "secret", "sh.helm.release.v1.ui-completion.v1", "-o", "json"))
            for name in ("smoke", "legacy"):
                base.completed_hook_record(stored, namespace, "ui-completion", 1, "ui-completion-" + name, "Succeeded")
            require(stored["metadata"]["labels"]["status"] == "deployed", "UI test changed deployment status")
        record["live_ui"] = ui.run(args.binary, args.helm, ctx["credential"], ctx["reader_context"], namespace,
                                    test_chart, updated, ctx["environment"], args.ui_screenshots, canary=ctx["canary"], verify_tests=verify_ui_tests)
        require(not kube("get", "crd", ctx["crd_name"], "--ignore-not-found", "-o", "name").strip(), "live UI schema deletion did not complete")
        record["live_ui"]["native_test_results_verified"] = True
        record["live_ui"]["native_schema_absence_verified"] = True


def configure(parser):
    parser.description = __doc__
    parser.add_argument("--completion-only", action="store_true", help="Run new completion workflows without repeating the original extension suite")
    parser.add_argument("--signature-fixtures", type=Path, required=True,
                        help="Public fixture charts/provenance/keyrings produced by fixture-helm-signatures.py")


if __name__ == "__main__":
    try:
        base.main(extra_checks=extra_checks,
                  extra_sources=("scripts/test-helm-completion.py", "scripts/fixture-helm-signatures.py", "scripts/test-live-completion-ui.py"),
                  configure_parser=configure)
    except (RuntimeError, OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error) if isinstance(error, RuntimeError) else "Completion acceptance failed; private diagnostics suppressed") from None
