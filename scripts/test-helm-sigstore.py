#!/usr/bin/env python3
"""Real signature/log-proof chart acceptance inside owned local cluster fixtures.

Uses the established namespace/RBAC/PTY/UID-cleanup harness. Public policies and
bundles are explicit fixture artifacts; no signing key or credential is retained
in the report. The subject is chart bytes, not an OCI registry manifest.
"""
import importlib.util
import json
from pathlib import Path
import shutil
import base64

ROOT=Path(__file__).resolve().parent.parent
spec=importlib.util.spec_from_file_location("sigstore_base",Path(__file__).with_name("test-helm-extensions.py"))
base=importlib.util.module_from_spec(spec);spec.loader.exec_module(base)
require=base.require


def extra_checks(**ctx):
    args,record,namespace=(ctx[key] for key in ("args","record","namespace"))
    directory,kube,operation=(ctx[key] for key in ("directory","kube","operation"))
    fixtures=args.sigstore_fixtures.resolve(strict=True)
    manifest=json.loads((fixtures/"fixtures.json").read_text())
    def fixture(section,name):
        basename=manifest[section][name]
        require(isinstance(basename,str) and Path(basename).name==basename,"invalid public Sigstore fixture basename")
        path=(fixtures/basename).resolve(strict=True)
        require(path.parent==fixtures,"Sigstore fixture escaped public directory")
        return path
    first,second=fixture("charts","first"),fixture("charts","second")
    first_policy,second_policy=fixture("policies","first"),fixture("policies","second")
    def flags(policy):return ["--helm-sigstore-policy",policy]
    def policy(name,change):
        value=json.loads(first_policy.read_text());change(value)
        path=directory/(name+".policy.json");path.write_text(json.dumps(value));path.chmod(0o600);return path
    def absent(name):
        secrets=json.loads(kube("-n",namespace,"get","secrets","-l","owner=helm,name="+name,"-o","json"))["items"]
        require(not secrets,"rejected Sigstore operation created Helm storage")
        require(not kube("-n",namespace,"get","configmap",name+"-settings","--ignore-not-found","-o","name").strip(),"rejected Sigstore operation wrote chart resources")
    operation("install","sigstore-local",first,flags=flags(first_policy),text_required=("Required Sigstore chart-blob verification","NOT an OCI manifest"))
    require(json.loads(kube("-n",namespace,"get","configmap","sigstore-local-settings","-o","json"))["data"]["message"]=="revision-1","signed local chart content mismatch")
    operation("upgrade","sigstore-local",second,flags=flags(second_policy))
    require(json.loads(kube("-n",namespace,"get","configmap","sigstore-local-settings","-o","json"))["data"]["message"]=="revision-2","signed upgrade content mismatch")
    operation("uninstall","sigstore-local")
    with ctx["registry_module"].Registry(directory,{"sigstore":first}) as registry:
        operation("install","sigstore-oci",registry.tag_refs["sigstore"],flags=[*flags(first_policy),"--helm-registry-ca",registry.ca_file])
        operation("uninstall","sigstore-oci")
        record["sigstore_registry"]=registry.report()
    for name,change in [
        ("sigstore-bad-pin",lambda value:value.update(sha256="0"*64)),
        ("sigstore-wrong-key",lambda value:value["trust"].update(key=str(fixtures/"chart-1.pub"))),
        ("sigstore-wrong-root",lambda value:value["trust"].update(trusted_root=str(fixtures/"wrong-root.json"))),
        ("sigstore-missing",lambda value:value.update(bundle=str(directory/"absent-bundle.json"))),
    ]:
        selected=policy(name,change)
        operation("install",name,first,flags=flags(selected),success=False);absent(name)
    original=json.loads(first_policy.read_text())
    raw=json.loads(Path(original["bundle"]).read_text());signature=bytearray(base64.b64decode(raw["messageSignature"]["signature"],validate=True));signature[-1]^=1
    raw["messageSignature"]["signature"]=base64.b64encode(signature).decode()
    tampered=directory/"tampered.sigstore.json";tampered.write_text(json.dumps(raw));tampered.chmod(0o600)
    selected=policy("sigstore-bad-signature",lambda value:value.update(bundle=str(tampered)))
    operation("install","sigstore-bad-signature",first,flags=flags(selected),success=False);absent("sigstore-bad-signature")
    changed=directory/"changed-chart.tgz";changed.write_bytes(first.read_bytes()+b"unsigned");changed.chmod(0o600)
    operation("install","sigstore-changed-chart",changed,flags=flags(first_policy),success=False);absent("sigstore-changed-chart")
    selected=policy("sigstore-policy-drift",lambda value:None)
    operation("install","sigstore-policy-drift",first,flags=flags(selected),success=False,
              callback=lambda:selected.write_text(selected.read_text()+"\n"));absent("sigstore-policy-drift")
    local_key=directory/"reviewed-public.pem";shutil.copyfile(original["trust"]["key"],local_key);local_key.chmod(0o600)
    selected=policy("sigstore-key-drift",lambda value:value["trust"].update(key=str(local_key)))
    operation("install","sigstore-key-drift",first,flags=flags(selected),success=False,
              callback=lambda:local_key.write_bytes((fixtures/"chart-1.pub").read_bytes()));absent("sigstore-key-drift")
    record["checks"].extend([
        "actual Cosign3.1.3 verified chart-byte signature plus transparency proof before local install/upgrade and OCI install",
        "wrong executable hash/public key/trusted root, missing bundle, tampered cryptographic signature and changed chart bytes rejected without release or resource writes",
        "post-review policy and public-key replacement rejected without writes; exact policy/source/private snapshots rechecked",
    ])
    record["sigstore_subject"]="exact packaged chart bytes; OCI manifest signatures are not claimed"
    record["sigstore_fixture_authority"]="ephemeral upstream sigstore-go virtual services, actual cryptographic proofs; not public-service account acceptance"


def configure(parser):
    parser.description=__doc__
    parser.add_argument("--sigstore-fixtures",type=Path,required=True)
    parser.set_defaults(completion_only=True)


if __name__=="__main__":
    try:
        base.main(extra_checks=extra_checks,extra_sources=("scripts/test-helm-sigstore.py","scripts/fixture-helm-sigstore.py"),configure_parser=configure)
    except (RuntimeError,OSError,ValueError,KeyError) as error:
        raise SystemExit(str(error) if isinstance(error,RuntimeError) else "Sigstore acceptance failed; private diagnostics suppressed") from None
