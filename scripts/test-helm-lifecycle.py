#!/usr/bin/env python3
"""Owned live Helm lifecycle acceptance using real terminal confirmation.

Explicit local endpoints only. Uses namespace-scoped, short-lived credentials;
never retains terminal output, kubeconfigs, Helm payloads or private keys.
"""
import re
import argparse
import base64
import errno
import fcntl
import hashlib
import importlib.util
import io
import ipaddress
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import struct
import tarfile
import tempfile
import termios
import time
from urllib.parse import urlparse
import uuid

PROJECT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("local_features", PROJECT / "scripts/test-helm-logs-metrics.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
require, execute, object_, write_private = base.require, base.execute, base.object_, base.write_private


def terminal(argv, environment, confirmation, after_review=None, timeout=150):
    """Real controlling PTY, bounded capture, one review, no automatic retries."""
    pid, master = pty.fork()
    if pid == 0:
        os.execvpe(str(argv[0]), [str(arg) for arg in argv], environment)
    fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack("HHHH", 45, 140, 0, 0))
    original_mode = termios.tcgetattr(master)
    output = bytearray()
    deadline, sent, status = time.monotonic() + timeout, False, None
    try:
        while time.monotonic() < deadline:
            if select.select([master], [], [], .05)[0]:
                try:
                    chunk = os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    chunk = b""
                output.extend(chunk)
                require(len(output) <= 1024 * 1024, "terminal review exceeded its capture bound")
            if not sent and output.endswith(b"> "):
                # The prompt precedes raw input setup by a few instructions.
                if termios.tcgetattr(master)[3] & termios.ICANON:
                    continue
                if after_review:
                    after_review()
                os.write(master, confirmation.encode() + b"\r")
                sent = True
            waited, value = os.waitpid(pid, os.WNOHANG)
            if waited:
                status = value
                break
        require(status is not None, "terminal lifecycle operation exceeded deadline; output suppressed")
        require(termios.tcgetattr(master) == original_mode, "CLI terminal mode was not restored")
        text = output.decode("utf-8", errors="replace")
        return os.waitstatus_to_exitcode(status), text, sent
    finally:
        if status is None:
            # Ctrl-C reaches raw confirmation input or the execution signal
            # handler, allowing the supervised Helm group to stop first.
            try:
                os.write(master,b"\x03")
            except OSError:
                pass
            for grace, termination in ((15,None),(3,signal.SIGTERM),(3,signal.SIGKILL)):
                if termination is not None:
                    try:
                        os.killpg(pid,termination)
                    except PermissionError:
                        os.kill(pid,termination)
                    except ProcessLookupError:
                        pass
                stop_deadline=time.monotonic()+grace
                while time.monotonic()<stop_deadline:
                    waited,value=os.waitpid(pid,os.WNOHANG)
                    if waited:
                        status=value
                        break
                    if select.select([master],[],[],.05)[0]:
                        try:
                            os.read(master,65536)
                        except OSError as error:
                            if error.errno!=errno.EIO:
                                raise
                if status is not None:
                    break
        # A macOS PTY writer can remain blocked while our master is open.
        # Release the terminal before the final reap; never wait indefinitely.
        os.close(master)
        if status is None:
            reap_deadline=time.monotonic()+3
            while time.monotonic()<reap_deadline:
                waited,value=os.waitpid(pid,os.WNOHANG)
                if waited:
                    status=value
                    break
                time.sleep(.02)
            require(status is not None,"terminal child did not exit after cancellation/kill and PTY closure")


def chart(path, revision, image, canary, *, dynamic=False, replicas=1):
    metadata = f"apiVersion: v2\nname: kborg-lifecycle-fixture\nversion: 0.1.{revision}\n"
    values = f"message: revision-{revision}\npassword: {canary}\n"
    manifest = '''apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ .Release.Name }}-settings
  namespace: {{ .Release.Namespace }}
data:
  message: {{ .Values.message | quote }}
---
apiVersion: v1
kind: Secret
metadata:
  name: {{ .Release.Name }}-private
  namespace: {{ .Release.Namespace }}
type: Opaque
stringData:
  password: {{ .Values.password | quote }}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: {{ .Release.Name }}-worker
  namespace: {{ .Release.Namespace }}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: {{ .Release.Name }}
  template:
    metadata:
      labels:
        app: {{ .Release.Name }}
      annotations:
        fixture-revision: {{ .Values.message | quote }}
    spec:
      automountServiceAccountToken: false
      securityContext:
        runAsNonRoot: true
        runAsUser: 65534
        runAsGroup: 65534
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: worker
          image: IMAGE_PLACEHOLDER
          command: ["/bin/sh", "-c", "while true; do echo KBORG_LIFECYCLE; sleep 2; done"]
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
          resources:
            requests: {cpu: 5m, memory: 8Mi}
            limits: {cpu: 50m, memory: 24Mi}
'''.replace("IMAGE_PLACEHOLDER", image)
    manifest = manifest.replace("  replicas: 1", f"  replicas: {replicas}")
    if dynamic:
        manifest = manifest.replace("message: {{ .Values.message | quote }}", "message: {{ randAlphaNum 12 | quote }}")
    with tarfile.open(path, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for name, text in (("Chart.yaml", metadata), ("values.yaml", values), ("templates/resources.yaml", manifest)):
            data = text.encode()
            item = tarfile.TarInfo("fixture/" + name)
            item.size, item.mode = len(data), 0o600
            archive.addfile(item, io.BytesIO(data))
    path.chmod(0o600)


def run_target(target, args, temporary, image, report):
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
    run = uuid.uuid4().hex[:12]
    namespace = "kborg-lifecycle-" + run
    record = {"target":target,"context":context,"server":expected,"namespace":namespace,
              "run_id":run,"kubernetes":json.loads(kube("get","--raw=/version"))["gitVersion"],
              "passed":False,"cleanup":False,"checks":[],"scope":"live-tui" if args.ui_only else "cli-lifecycle"}
    report["targets"].append(record)
    namespace_object = object_("Namespace",namespace)
    namespace_object["metadata"]["labels"] = {"kborg.dev/run-id":run,"pod-security.kubernetes.io/enforce":"restricted"}
    owned = json.loads(kube("create","-f","-","-o","json",data=json.dumps(namespace_object)))
    namespace_uid = owned["metadata"]["uid"]
    started = time.monotonic()
    try:
        resources = [object_("ServiceAccount","operator",namespace,automountServiceAccountToken=False),
                     object_("Role","operator",namespace,rules=[
                         {"apiGroups":[""],"resources":["configmaps","secrets","pods","pods/log","services","serviceaccounts"],"verbs":["get","list","watch","create","update","patch","delete"]},
                         {"apiGroups":["apps"],"resources":["deployments","replicasets"],"verbs":["get","list","watch","create","update","patch","delete"]}]),
                     object_("RoleBinding","operator",namespace,subjects=[{"kind":"ServiceAccount","name":"operator","namespace":namespace}],roleRef={"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"operator"})]
        kube("create","-f","-",data=json.dumps({"apiVersion":"v1","kind":"List","items":resources}))
        token = kube("-n",namespace,"create","token","operator","--duration=30m").strip()
        credential, reader_context = base.private_reader(directory,cluster,namespace,token)
        del token, config
        environment = {key:value for key,value in os.environ.items() if not key.startswith("HELM_")}
        environment.update({"TERM":"xterm-256color","KBORG_CONFIG_DIR":str(directory / "settings"),"KBORG_NO_SPLASH":"1"})
        # Deliberately hostile inherited overrides must not redirect the supervised Helm child.
        environment["HELM_KUBEAPISERVER"] = "https://127.0.0.1:1"
        environment["HELM_KUBEINSECURE_SKIP_TLS_VERIFY"] = "true"
        canary = "KBORG_LIFECYCLE_PRIVATE_" + uuid.uuid4().hex
        first, second, dynamic = (directory / name for name in ("first.tgz","second.tgz","dynamic.tgz"))
        chart(first,1,image,canary); chart(second,2,image,canary); chart(dynamic,3,image,canary,dynamic=True)
        common = [args.binary,"--kubeconfig",credential,"--context",reader_context,"-n",namespace,"--allow-helm"]
        writes = [*common,"--write","--allow-helm-write","--helm-tool",args.helm,"--helm-timeout","90"]

        def operation(storage, action, release, *parameters, confirm=None, callback=None, success=True):
            record["phase"] = storage + ":" + action
            print(f"{target}: {storage} {action} {release} (expected {'success' if success else 'refusal/failure'})", flush=True)
            code, text, reviewed = terminal([*writes,"--helm-storage",storage,"helm",action,release,*parameters],environment,
                                             confirm if confirm is not None else namespace + "/" + release,callback)
            require(canary not in text and base64.b64encode(canary.encode()).decode() not in text,
                    "Helm terminal review exposed Secret contents")
            if (code == 0) != success or ((success or confirm is not None or callback is not None) and not reviewed):
                errors = re.findall(r"KBorg: ([^\r\n]{1,1200})", text)
                record["failure_category"] = errors[-1] if errors else "expected review or operation outcome missing"
                summaries = re.findall(r"(Helm [a-z-]+ outcome UNKNOWN[^\r\n]{1,2000})", text)
                if summaries:
                    record["failure_summary"] = summaries[-1]
            require((code == 0) == success, f"{storage} {action} unexpected result; private diagnostics suppressed")
            if success or confirm is not None or callback is not None:
                require(reviewed,"lifecycle command did not reach the expected terminal review")
            return text

        if not args.ui_only:
            for storage in ("secret","configmap"):
                release = "lifecycle-" + storage
                operation(storage,"install",release,first,confirm="incorrect/release",success=False)
                inventory = json.loads(kube("-n",namespace,"get","configmap",release+"-settings","--ignore-not-found","-o","json") or "null")
                require(inventory is None,"canceled installation created a resource")
                operation(storage,"install",release,first)
                require(json.loads(kube("-n",namespace,"get","configmap",release+"-settings","-o","json"))["data"]["message"] == "revision-1","install payload mismatch")
                operation(storage,"upgrade",release,second)
                require(json.loads(kube("-n",namespace,"get","configmap",release+"-settings","-o","json"))["data"]["message"] == "revision-2","upgrade payload mismatch")
                operation(storage,"rollback",release,"1")
                require(json.loads(kube("-n",namespace,"get","configmap",release+"-settings","-o","json"))["data"]["message"] == "revision-1","rollback payload mismatch")
                history = json.loads(execute([*common,"--helm-storage",storage,"--json","helm","history",release],environment=environment)[1])
                require(sorted(item["revision"] for item in history["data"]["releases"]) == [1,2,3],"lifecycle revision history mismatch")
                # Perturb field ownership only after the successful lifecycle cycle.
                def change_resource():
                    kube("-n",namespace,"patch","configmap",release+"-settings","--type=merge","-p",json.dumps({"data":{"message":"external-change"}}))
                operation(storage,"upgrade",release,second,callback=change_resource,success=False)
                require(json.loads(kube("-n",namespace,"get","configmap",release+"-settings","-o","json"))["data"]["message"] == "external-change","stale resource review wrote over newer data")
                def change_history():
                    kube("-n",namespace,"annotate",storage,"sh.helm.release.v1."+release+".v3","kborg.dev/review-changed=true")
                operation(storage,"uninstall",release,callback=change_history,success=False)
                require(kube("-n",namespace,"get","deployment",release+"-worker","-o","name").strip(),"stale release review uninstalled the workload")
                operation(storage,"uninstall",release)
                execute([*prefix,"-n",namespace,"wait","--for=delete","deployment/"+release+"-worker","--timeout=60s"],timeout=70)
                require(not kube("-n",namespace,"get","deployment",release+"-worker","--ignore-not-found","-o","name").strip(),"uninstall left the Deployment")
                require(not kube("-n",namespace,"get","secret",release+"-private","--ignore-not-found","-o","name").strip(),"uninstall left the Secret")
                record["checks"].append({"storage":storage,"install_upgrade_rollback_uninstall":True,"revisions":[1,2,3],"canceled_review_no_write":True,"stale_resource_review_rejected":True,"changed_release_review_rejected":True,"secret_canary_absent":True,"hostile_helm_environment_ignored":True})
            operation("secret","install","dynamic-chart",dynamic,success=False)
            # Source replacement during review must fail rather than execute the changed chart.
            changed = directory / "changed.tgz"; shutil.copyfile(first,changed)
            operation("secret","install","changed-chart",changed,callback=lambda: changed.write_bytes(second.read_bytes()),success=False)
            require(not kube("-n",namespace,"get","configmap","changed-chart-settings","--ignore-not-found","-o","name").strip(),"changed chart created resources")
            record["dynamic_template_rejected"] = True
            record["changed_chart_review_rejected"] = True
        if target == "desktop" and args.ui_screenshots:
            record["phase"] = "live-tui-lifecycle"
            ui_spec = importlib.util.spec_from_file_location("lifecycle_ui", PROJECT / "scripts/test-live-lifecycle-ui.py")
            ui = importlib.util.module_from_spec(ui_spec)
            ui_spec.loader.exec_module(ui)
            ui_chart = directory / "ui-second.tgz"
            chart(ui_chart,2,image,canary,replicas=2)
            record["live_ui"] = ui.run(args.binary,args.helm,credential,reader_context,namespace,first,ui_chart,environment,args.ui_screenshots,canary=canary)
        record["passed"],record["phase"] = True,"complete"
        print(f"{target}: {record['scope']} acceptance passed",flush=True)
    finally:
        current = json.loads(kube("get","namespace",namespace,"-o","json"))
        require(current["metadata"]["uid"] == namespace_uid and current["metadata"].get("labels",{}).get("kborg.dev/run-id") == run,"cleanup ownership changed")
        kube("delete","--raw",f"/api/v1/namespaces/{namespace}","-f","-",data=json.dumps({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":namespace_uid}}))
        execute([*prefix,"wait","--for=delete",f"namespace/{namespace}","--timeout=90s"],timeout=100)
        record["cleanup"],record["seconds"] = True,round(time.monotonic()-started,3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target",choices=("desktop","minikube","both"),default="both")
    parser.add_argument("--expected-desktop-server")
    parser.add_argument("--expected-minikube-server")
    parser.add_argument("--binary",type=Path,default=PROJECT/"target/release/kborg")
    parser.add_argument("--helm",type=Path,default=PROJECT/".local/platform-tools/helm")
    parser.add_argument("--ui-screenshots",type=Path)
    parser.add_argument("--ui-only",action="store_true",help="Only the real Docker Desktop TUI acceptance; requires --target desktop and --ui-screenshots")
    parser.add_argument("--output",type=Path,default=PROJECT/"test-results/helm-lifecycle-local.json")
    args=parser.parse_args()
    require(not args.ui_only or (args.target == "desktop" and args.ui_screenshots),"UI-only acceptance requires --target desktop and --ui-screenshots")
    args.binary,args.helm=args.binary.resolve(strict=True),args.helm.resolve(strict=True)
    targets=("desktop","minikube") if args.target=="both" else (args.target,)
    require(all(args.expected_desktop_server if target=="desktop" else args.expected_minikube_server for target in targets),"exact local endpoints required before writes")
    require(shutil.which("kubectl") and shutil.which("docker"),"kubectl and Docker required")
    image=execute([shutil.which("docker"),"image","inspect","--format","{{index .RepoDigests 0}}","busybox:1.37.0"])[1].strip()
    require("@sha256:" in image and len(image.rsplit(":",1)[1])==64,"fixture image must be digest-pinned")
    paths=sorted((PROJECT/"src").glob("*.rs"))+sorted((PROJECT/"tests").glob("*.rs"))
    paths += [PROJECT/name for name in ("Cargo.toml","Cargo.lock","rust-toolchain.toml","docs/CLI_HELP.txt","scripts/test-helm-lifecycle.py","scripts/test-helm-logs-metrics.py")]
    if args.ui_screenshots:
        paths += [PROJECT / "scripts/test-live-lifecycle-ui.py", PROJECT / "scripts/test-terminal.py"]
    hashes={str(path.relative_to(PROJECT)):hashlib.sha256(path.read_bytes()).hexdigest() for path in paths}
    report={"schema":1,"timestamp_utc":time.strftime("%Y-%m-%dT%H:%M:%SZ",time.gmtime()),"binary_sha256":hashlib.sha256(args.binary.read_bytes()).hexdigest(),"helm_version":execute([args.helm,"version","--short"])[1].strip(),"helm_sha256":hashlib.sha256(args.helm.read_bytes()).hexdigest(),"tested_source_sha256":hashes,"targets":[],"temporary_credentials_removed":False,"source_unchanged":False}
    directory=None
    try:
        with tempfile.TemporaryDirectory(prefix="kborg-lifecycle-acceptance-") as temporary:
            directory=Path(temporary).resolve();directory.chmod(0o700)
            for target in targets:run_target(target,args,directory,image,report)
    finally:
        report["temporary_credentials_removed"]=directory is not None and not directory.exists()
        report["source_unchanged"]=all(hashlib.sha256((PROJECT/path).read_bytes()).hexdigest()==digest for path,digest in hashes.items())
        args.output.parent.mkdir(parents=True,exist_ok=True)
        args.output.write_text(json.dumps(report,indent=2)+"\n")
    require(report["source_unchanged"],"source changed during acceptance; rerun stable source")
    require(report["temporary_credentials_removed"] and all(target["cleanup"] for target in report["targets"]),"fixture cleanup unconfirmed")


if __name__=="__main__":
    try:main()
    except (RuntimeError,OSError,ValueError,KeyError) as error:
        raise SystemExit(str(error) if isinstance(error,RuntimeError) else "Lifecycle acceptance failed; private diagnostics suppressed") from None
