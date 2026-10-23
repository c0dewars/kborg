#!/usr/bin/env python3
"""Exercise the actual YAML editor/review on an owned zero-replica local Deployment."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, _spawn = (helpers[name] for name in ("Screen", "_spawn"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--kubeconfig", required=True, type=Path)
    parser.add_argument("--context", required=True, choices=("docker-desktop", "kborg-v1"))
    parser.add_argument("--screenshots", required=True, type=Path)
    args = parser.parse_args()
    binary, config = (path.resolve(strict=True) for path in (args.binary, args.kubeconfig))
    output = args.screenshots.resolve()
    output.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex[:12]
    namespace = "kborg-yaml-ui-" + run_id
    kube = ["kubectl", "--kubeconfig", str(config), "--context", args.context]

    def kubectl(*arguments, value=None):
        result = subprocess.run(kube + list(arguments), input=json.dumps(value) if value else None,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=100)
        if result.returncode:
            raise RuntimeError("Owned YAML fixture request failed: " + arguments[0])
        return result.stdout

    uid, passed, checks, captures = None, False, [], []
    try:
        configuration = json.loads(kubectl("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
        cluster = configuration["clusters"][0]["cluster"]
        if not cluster["server"].startswith("https://") or cluster.get("insecure-skip-tls-verify") or not cluster.get("certificate-authority-data"):
            raise RuntimeError("Fixture requires verified TLS")
        created = json.loads(kubectl("create", "-f", "-", "-o", "json", value={"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}}}))
        uid = created["metadata"]["uid"]
        items = [
            {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": namespace, "name": "editor"}, "automountServiceAccountToken": False},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": namespace, "name": "editor"}, "rules": [
                {"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get", "list", "watch", "patch"]}]},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": namespace, "name": "editor"}, "subjects": [{"kind": "ServiceAccount", "name": "editor", "namespace": namespace}], "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "editor"}},
            {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"namespace": namespace, "name": "yaml-sample", "annotations": {"example.test/preserve": "original"}},
             "spec": {"replicas": 0, "minReadySeconds": 1, "selector": {"matchLabels": {"app": "yaml-sample"}}, "template": {"metadata": {"labels": {"app": "yaml-sample"}}, "spec": {"automountServiceAccountToken": False, "containers": [{"name": "sample", "image": "nginx:1.27", "env": [{"name": "TOKEN", "value": "yaml-hidden-fixture-canary"}]}]}}}},
        ]
        kubectl("create", "-f", "-", value={"apiVersion": "v1", "kind": "List", "items": items})
        token = kubectl("-n", namespace, "create", "token", "editor", "--duration=10m").strip()
        with tempfile.TemporaryDirectory(prefix="kborg-yaml-ui-") as directory:
            directory = Path(directory).resolve()
            scoped = directory / "editor.kubeconfig"
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": "yaml-fixture", "clusters": [{"name": "local", "cluster": cluster}], "users": [{"name": "editor", "user": {"token": token}}], "contexts": [{"name": "yaml-fixture", "context": {"cluster": "local", "user": "editor", "namespace": namespace}}]}, stream)
            token = configuration = None
            environment = os.environ.copy()
            environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(directory / "app"))
            environment.pop("NO_COLOR", None)
            base = [str(binary), "--kubeconfig", str(scoped), "--context", "yaml-fixture", "-n", namespace]
            template = subprocess.run(base + ["yaml-template", "deployments", "yaml-sample"], env=environment, text=True, capture_output=True, timeout=20)
            if template.returncode or "yaml-hidden-fixture-canary" in template.stdout:
                raise RuntimeError("Safe template unavailable or hidden value exposed")
            target_row = next(index for index, line in enumerate(template.stdout.splitlines()) if '"minReadySeconds": 1' in line)
            master, slave = pty.openpty()
            original = termios.tcgetattr(slave)
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
            screen = Screen(140, 40)
            process = _spawn(base + ["--write", "--allow-yaml-edit", "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"], environment, slave)

            def drain(seconds=.2):
                until = time.monotonic() + seconds
                while time.monotonic() < until:
                    if select.select([master], [], [], min(.05, max(0, until-time.monotonic())))[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            break
                        if not data:
                            break
                        screen.feed(data)

            def wait(expected, timeout=25):
                until = time.monotonic() + timeout
                while time.monotonic() < until:
                    drain()
                    if expected.casefold() in screen.text().casefold():
                        drain(.25)
                        if "yaml-hidden-fixture-canary" in screen.text():
                            raise RuntimeError("Hidden fixture value appeared in UI")
                        return
                    if process.poll() is not None:
                        break
                raise RuntimeError("YAML UI did not reach: " + expected + "\n" + screen.text())

            def keys(value, expected=None):
                os.write(master, value)
                wait(expected) if expected else drain(.3)

            def capture(name):
                path = output / (name + ".png")
                screen.png(path)
                captures.append({"name": name, "path": str(path), "width": 140, "height": 40})

            def live():
                return json.loads(kubectl("-n", namespace, "get", "deployment", "yaml-sample", "-o", "json"))

            def change_seconds():
                keys(b"\x1b[1;5H" + b"\x1b[B" * target_row + b"\x1b[F\x7f2")

            try:
                wait("PODS")
                keys(b":deployments\r", "yaml-sample")
                drain(.8)
                before = live()
                keys(b"e", "YAML EDITOR")
                change_seconds()
                capture("yaml-editor")
                keys(b"\x1b")
                if live()["spec"] != before["spec"]:
                    raise RuntimeError("Cancel changed live spec")
                checks.append("editor cancel preserves live resource")
                keys(b"e", "YAML EDITOR")
                keys(b"[\x13", "invalid or excessive YAML")
                if "YAML EDITOR" not in screen.text() or '["apiVersion"' not in screen.text():
                    raise RuntimeError("Invalid YAML did not retain the editor buffer")
                capture("yaml-invalid-retained")
                keys(b"\x7f")
                change_seconds()
                keys(b"\x13", "REVIEW CHANGES")
                for _ in range(12):
                    if "0 unread" in screen.text():
                        break
                    keys(b"\x1b[6~")
                wait("0 unread")
                if '+ "/spec/minReadySeconds": 2' not in screen.text():
                    keys(b"\x1b[H")
                    wait('+ "/spec/minReadySeconds": 2')
                capture("yaml-reviewed-diff")
                keys((namespace + "/yaml-sample").encode() + b"\r", "YAML edit accepted")
                capture("yaml-applied")
                after = live()
                if after["spec"]["minReadySeconds"] != 2 or after["spec"]["replicas"] != 0:
                    raise RuntimeError("Reviewed field was not applied exactly")
                if after["spec"]["template"] != before["spec"]["template"] or after["metadata"]["annotations"] != before["metadata"]["annotations"]:
                    raise RuntimeError("Hidden fields or protected metadata changed")
                checks.extend(["invalid YAML retains buffer", "fully painted diff and exact target confirmation before apply", "real JSON Patch preserves hidden fields and metadata"])
                keys(b":q\r")
                until = time.monotonic() + 8
                while process.poll() is None and time.monotonic() < until:
                    drain()
                if process.poll() != 0 or termios.tcgetattr(master) != original:
                    raise RuntimeError("YAML UI did not exit cleanly and restore terminal")
                checks.append("TUI exit restores terminal mode")
                # Exercise the explicit-file CLI using its own terminal confirmation.
                latest = subprocess.run(base + ["yaml-template", "deployments", "yaml-sample"], env=environment, text=True, capture_output=True, timeout=20)
                if latest.returncode or "yaml-hidden-fixture-canary" in latest.stdout:
                    raise RuntimeError("CLI safe template failed")
                edited = latest.stdout.replace('"minReadySeconds": 2', '"minReadySeconds": 3')
                if edited == latest.stdout:
                    raise RuntimeError("CLI fixture field missing")
                path = directory / "edited.yaml"
                with path.open("x") as stream:
                    os.chmod(path, 0o600)
                    stream.write(edited)
                os.close(master)
                os.close(slave)
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
                screen = Screen(140, 40)
                process = _spawn(base + ["--write", "--allow-yaml-edit", "yaml-edit", "deployments", "yaml-sample", str(path)], environment, slave)
                wait("REVIEW YAML SPEC EDIT")
                wait('"/spec/minReadySeconds": 3')
                keys((namespace + "/yaml-sample").encode() + b"\r", "YAML edit accepted")
                until = time.monotonic() + 8
                while process.poll() is None and time.monotonic() < until:
                    drain()
                final = live()
                if process.poll() != 0 or final["spec"]["minReadySeconds"] != 3 or final["spec"]["template"] != before["spec"]["template"]:
                    raise RuntimeError("CLI explicit-file edit did not apply only its reviewed field")
                checks.append("interactive explicit-file CLI applies reviewed spec and preserves hidden fields")
                passed = True
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
                os.close(master)
                os.close(slave)
    finally:
        cleaned = uid is None
        if uid:
            current = json.loads(kubectl("get", "namespace", namespace, "-o", "json"))
            if current["metadata"]["uid"] != uid or current["metadata"].get("labels", {}).get("kborg.dev/run-id") != run_id:
                raise RuntimeError("YAML cleanup ownership changed")
            kubectl("delete", "--raw", "/api/v1/namespaces/" + namespace, "-f", "-", value={"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": uid}})
            kubectl("wait", "--for=delete", "namespace/" + namespace, "--timeout=90s")
            cleaned = True
        report = {"passed": passed, "context": args.context, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "checks": checks, "captures": captures, "fixture_cleanup": cleaned}
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({key: report[key] for key in ("passed", "context", "checks", "fixture_cleanup")}))


if __name__ == "__main__":
    main()
