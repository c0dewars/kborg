#!/usr/bin/env python3
"""Exercise explicit saved scopes in a real terminal using an owned read-only fixture."""
import argparse
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import runpy
import select
import shutil
import signal
import struct
import subprocess
import tempfile
import termios
import time
import uuid

helpers = runpy.run_path(str(Path(__file__).with_name("test-live-lifecycle-ui.py")))
Screen, spawn = (helpers[name] for name in ("Screen", "_spawn"))


class Terminal:
    def __init__(self, command, environment, output, captures):
        self.output, self.captures = output, captures
        self.master, self.slave = pty.openpty()
        self.original = termios.tcgetattr(self.slave)
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", 38, 136, 0, 0))
        self.screen = Screen(136, 38)
        self.process = spawn(command, environment, self.slave)

    def drain(self, seconds=.2):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            if select.select([self.master], [], [], min(.05, max(0, until - time.monotonic())))[0]:
                try:
                    data = os.read(self.master, 65536)
                except OSError:
                    break
                if not data:
                    break
                self.screen.feed(data)

    def wait(self, expected, timeout=30):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            self.drain()
            if expected.casefold() in self.screen.text().casefold():
                self.drain(.3)
                return
            if self.process.poll() is not None:
                break
        raise RuntimeError("Saved-selection UI did not reach: " + expected + "\n" + self.screen.text())

    def keys(self, data, expected=None):
        os.write(self.master, data)
        self.wait(expected) if expected else self.drain(.4)

    def capture(self, name):
        path = self.output / (name + ".png")
        self.screen.png(path)
        self.captures.append({"name": name, "path": str(path), "width": 136, "height": 38})

    def close(self):
        if self.process.poll() is None:
            self.keys(b"\x1b")
            self.keys(b":q\r")
            until = time.monotonic() + 8
            while self.process.poll() is None and time.monotonic() < until:
                self.drain()
        result = self.process.poll()
        restored = termios.tcgetattr(self.master) == self.original
        if result is None:
            os.killpg(self.process.pid, signal.SIGKILL)
            self.process.wait(timeout=5)
        os.close(self.master)
        os.close(self.slave)
        if result != 0 or not restored:
            raise RuntimeError("Saved-selection TUI did not exit cleanly and restore terminal mode")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--kubeconfig", required=True, type=Path)
    parser.add_argument("--context", required=True, choices=("docker-desktop", "kborg-dashboard"))
    parser.add_argument("--screenshots", required=True, type=Path)
    args = parser.parse_args()
    binary, kubeconfig = (path.resolve(strict=True) for path in (args.binary, args.kubeconfig))
    binary_sha256 = hashlib.sha256(binary.read_bytes()).hexdigest()
    output = args.screenshots.resolve()
    output.mkdir(parents=True, exist_ok=True)
    run_id = uuid.uuid4().hex[:12]
    namespace = "kborg-saved-ui-" + run_id
    kube = ["kubectl", "--kubeconfig", str(kubeconfig), "--context", args.context]

    def kubectl(*arguments, value=None):
        result = subprocess.run(kube + list(arguments), input=json.dumps(value) if value else None,
                                text=True, capture_output=True, timeout=100)
        if result.returncode:
            raise RuntimeError("Owned saved-selection fixture request failed: " + arguments[0])
        return result.stdout

    namespace_uid, passed, cleaned = None, False, False
    checks, captures = [], []
    terminal = None
    try:
        configuration = json.loads(kubectl("config", "view", "--minify", "--flatten", "--raw", "-o", "json"))
        cluster = configuration["clusters"][0]["cluster"]
        if not cluster["server"].startswith("https://") or cluster.get("insecure-skip-tls-verify") or not cluster.get("certificate-authority-data"):
            raise RuntimeError("Fixture requires verified TLS")
        created = json.loads(kubectl("create", "-f", "-", "-o", "json", value={"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": namespace, "labels": {"kborg.dev/run-id": run_id}}}))
        namespace_uid = created["metadata"]["uid"]
        deployment = {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"namespace": namespace, "name": "saved-sample", "labels": {"app": "saved-sample"}},
                      "spec": {"replicas": 0, "selector": {"matchLabels": {"app": "saved-sample"}}, "template": {"metadata": {"labels": {"app": "saved-sample"}}, "spec": {"automountServiceAccountToken": False, "containers": [{"name": "sample", "image": "nginx:1.27"}]}}}}
        items = [
            {"apiVersion": "v1", "kind": "ServiceAccount", "metadata": {"namespace": namespace, "name": "reader"}, "automountServiceAccountToken": False},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"namespace": namespace, "name": "reader"}, "rules": [
                {"apiGroups": [""], "resources": ["pods", "events"], "verbs": ["get", "list", "watch"]},
                {"apiGroups": ["apps"], "resources": ["deployments"], "verbs": ["get", "list", "watch"]}]},
            {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"namespace": namespace, "name": "reader"}, "subjects": [{"kind": "ServiceAccount", "name": "reader", "namespace": namespace}], "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "reader"}},
            deployment,
        ]
        kubectl("create", "-f", "-", value={"apiVersion": "v1", "kind": "List", "items": items})
        token = kubectl("-n", namespace, "create", "token", "reader", "--duration=10m").strip()
        with tempfile.TemporaryDirectory(prefix="kborg-saved-ui-") as temporary:
            directory = Path(temporary).resolve()
            tested_binary = directory / "kborg"
            shutil.copyfile(binary, tested_binary)
            os.chmod(tested_binary, 0o700)
            binary_sha256 = hashlib.sha256(tested_binary.read_bytes()).hexdigest()
            scoped = directory / "reader.kubeconfig"
            with scoped.open("x") as stream:
                os.chmod(scoped, 0o600)
                json.dump({"apiVersion": "v1", "kind": "Config", "current-context": "saved-fixture", "clusters": [{"name": "local", "cluster": cluster}], "users": [{"name": "reader", "user": {"token": token}}], "contexts": [{"name": "saved-fixture", "context": {"cluster": "local", "user": "reader", "namespace": namespace}}]}, stream)
            token = configuration = None
            environment = os.environ.copy()
            environment.update(TERM="xterm-256color", COLORTERM="truecolor", LC_ALL="en_US.UTF-8", KBORG_CONFIG_DIR=str(directory / "app"))
            environment.pop("NO_COLOR", None)
            command = [str(tested_binary), "--kubeconfig", str(scoped), "--context", "saved-fixture", "-n", namespace, "--theme", "rose-pine", "--no-splash", "--no-mouse", "--color=always"]
            store = directory / "app" / "selections" / "v1.json"

            def start():
                view = Terminal(command, environment, output, captures)
                view.wait("PODS")
                view.drain(1)
                return view

            def preferences():
                return json.loads(store.read_text())

            terminal = start()
            if store.exists():
                raise RuntimeError("Reading current view unexpectedly persisted selections")
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"\x1b")
            if store.exists():
                raise RuntimeError("Opening an empty picker unexpectedly persisted selections")
            checks.append("startup and empty picker do not create saved preferences")
            terminal.keys(b":deploy app=saved-sample\r", "saved-sample")
            terminal.drain(1)
            terminal.keys(b"b", "Save selection / name")
            terminal.keys(b"Application API\r", "saved locally")
            record = preferences()["selections"][0]
            if record["target"]["kind"] != "deployments.apps" or record["target"]["label_selector"] != "app=saved-sample" or not record["target"]["workload"]["uid"]:
                raise RuntimeError("Saved resource lost its selector or workload identity")
            if set(record) != {"name", "demo", "target"} or record["demo"]:
                raise RuntimeError("Saved resource schema contains unexpected preferences")
            if store.stat().st_mode & 0o777 != 0o600 or store.parent.stat().st_mode & 0o777 != 0o700:
                raise RuntimeError("Saved preferences are not private")
            checks.append("explicit resource save retains selector and UID in private data-only preferences")
            before = store.read_bytes()
            terminal.keys(b"b", "Save selection / name")
            terminal.keys(b"Application API\r", "already exists")
            if store.read_bytes() != before:
                raise RuntimeError("Duplicate save replaced existing preferences")
            terminal.keys(b"\x1b")
            checks.append("duplicate selection name cannot overwrite an existing record")
            terminal.keys(b"D", "CONTEXT OVERVIEW")
            terminal.wait("NO PODS")
            terminal.keys(b"s", "Save selection / name")
            terminal.keys(b"Local dashboard\r", "CONTEXT OVERVIEW")
            if preferences()["selections"][1]["target"] != {"view": "dashboard", "contexts": ["saved-fixture"], "namespace": namespace}:
                raise RuntimeError("Dashboard selection contains incorrect scope")
            terminal.keys(b"B", "Saved selections")
            terminal.capture("saved-selections")
            terminal.keys(b"\x1b")
            terminal.keys(b"\x1b")
            terminal.close()
            terminal = None
            checks.append("dashboard save persists only selected contexts and namespace")

            terminal = start()
            if "DEPLOYMENTS" in terminal.screen.text() or "CONTEXT OVERVIEW" in terminal.screen.text():
                raise RuntimeError("Session startup implicitly restored a saved scope")
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"\r", "Saved workload identity verified")
            terminal.capture("restored-workload")
            checks.append("restart preserves selections but explicit reopen revalidates resource and exact workload UID")
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"j\r", "CONTEXT OVERVIEW")
            terminal.wait("NO PODS")
            terminal.capture("restored-dashboard")
            terminal.keys(b"\x1b")
            terminal.close()
            terminal = None
            checks.append("saved dashboard explicitly reopens its live namespace scope")

            # The parent harness, not the read-only app credential, replaces its own
            # zero-replica Deployment to prove a reused name cannot inherit focus.
            original_uid = record["target"]["workload"]["uid"]
            kubectl("delete", "--raw", f"/apis/apps/v1/namespaces/{namespace}/deployments/saved-sample", "-f", "-", value={"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": original_uid}})
            replacement = json.loads(kubectl("create", "-f", "-", "-o", "json", value=deployment))
            if replacement["metadata"]["uid"] == original_uid:
                raise RuntimeError("Fixture replacement did not change identity")
            document = preferences()
            missing = copy.deepcopy(document["selections"][0])
            missing["name"] = "Unavailable context"
            missing["target"]["context"] = "removed-fixture-context"
            document["selections"].append(missing)
            store.write_text(json.dumps(document, indent=2) + "\n")
            terminal = start()
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"\r", "SAVED WORKLOAD UNAVAILABLE")
            terminal.capture("replaced-workload")
            terminal.keys(b"\x1b")
            checks.append("recreated workload name cannot inherit saved UID focus")
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"jj\r", "saved context is unavailable")
            if "saved-fixture" not in terminal.screen.text() or "Saved selections" not in terminal.screen.text():
                raise RuntimeError("Missing context changed the active scope")
            terminal.capture("unavailable-context")
            checks.append("missing context rejected before changing active scope")
            terminal.keys(b"\x1b")
            before = store.read_bytes()
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"d", "Delete selection / type exact name")
            terminal.keys(b"\x1b")
            if store.read_bytes() != before:
                raise RuntimeError("Canceled deletion changed preferences")
            terminal.keys(b"B", "Saved selections")
            terminal.keys(b"d", "Delete selection / type exact name")
            terminal.keys(b"Wrong\r", "Type the exact saved selection name")
            if store.read_bytes() != before:
                raise RuntimeError("Incorrect confirmation deleted a selection")
            terminal.keys(b"\x7f" * 5 + b"Application API\r", "removed")
            if any(item["name"] == "Application API" for item in preferences()["selections"]):
                raise RuntimeError("Exact selection deletion did not persist")
            checks.append("delete cancel and incorrect confirmation preserve records; exact name removes only selected record")
            valid_preferences = store.read_bytes()
            store.write_text("invalid preferences retained for inspection\n")
            corrupt = store.read_bytes()
            terminal.keys(b"B", "SAVED SELECTIONS UNAVAILABLE")
            terminal.capture("invalid-preferences")
            terminal.keys(b"\x1b", "saved-sample")
            if terminal.process.poll() is not None or store.read_bytes() != corrupt:
                raise RuntimeError("Invalid preferences terminated the app or were overwritten")
            terminal.keys(b"D", "CONTEXT OVERVIEW")
            terminal.wait("NO PODS")
            terminal.keys(b"B", "SAVED SELECTIONS UNAVAILABLE")
            terminal.keys(b"\x1b", "CONTEXT OVERVIEW")
            if terminal.process.poll() is not None or store.read_bytes() != corrupt:
                raise RuntimeError("Invalid preferences lost the dashboard or were overwritten")
            terminal.keys(b"\x1b")
            checks.append("invalid preferences show an in-app notice, retain file and restore underlying resource/dashboard")
            store.write_bytes(valid_preferences)
            os.chmod(store, 0o640)  # group-readable: KBorg must still refuse it
            terminal.keys(b"B", "SAVED SELECTIONS UNAVAILABLE")
            terminal.wait("must be private")
            terminal.keys(b"\x1b", "saved-sample")
            if store.read_bytes() != valid_preferences or store.stat().st_mode & 0o777 != 0o644:
                raise RuntimeError("Nonprivate saved preferences were silently modified")
            os.chmod(store, 0o600)
            checks.append("nonprivate preferences are refused without changing contents or permissions")
            terminal.close()
            terminal = None
            checks.append("all TUI sessions exit cleanly and restore terminal mode")
            passed = True
    finally:
        if terminal is not None:
            try:
                terminal.close()
            except Exception:
                # Continue the UID-guarded fixture cleanup even after a failed UI
                # assertion or terminal-exit assertion. The failing run stays failed.
                passed = False
        if namespace_uid is not None:
            current = json.loads(kubectl("get", "namespace", namespace, "-o", "json"))
            if current["metadata"]["uid"] != namespace_uid or current["metadata"].get("labels", {}).get("kborg.dev/run-id") != run_id:
                raise RuntimeError("Saved-selection cleanup ownership changed")
            kubectl("delete", "--raw", "/api/v1/namespaces/" + namespace, "-f", "-", value={"apiVersion": "v1", "kind": "DeleteOptions", "preconditions": {"uid": namespace_uid}})
            kubectl("wait", "--for=delete", "namespace/" + namespace, "--timeout=90s")
        cleaned = True
        report = {"passed": passed, "context": args.context, "binary_sha256": binary_sha256, "checks": checks, "captures": captures, "fixture_cleanup": cleaned}
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({key: report[key] for key in ("passed", "context", "checks", "fixture_cleanup")}))


if __name__ == "__main__":
    main()
