#!/usr/bin/env python3
"""Exercise the built CLI against an isolated verified-TLS Kubernetes fixture.

Uses only ephemeral loopback listeners, generated local CAs and a synthetic token.
No user kubeconfig, credentials, cluster or trust store is read or changed. This
is deterministic protocol evidence, separate from real-cluster soak testing.
"""
import argparse
import base64
from collections import deque
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import select
import signal
import ssl
import subprocess
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlparse


NAMESPACE = "kborg-transport-fixture"
TOKEN = "KBORG_SYNTHETIC_TRANSPORT_CANARY"
BODY_CANARY = "KBORG_UNTRUSTED_RESPONSE_CANARY"


def pod(name="fixture", namespace=NAMESPACE):
    return {"apiVersion": "v1", "kind": "Pod", "metadata": {
        "name": name, "namespace": namespace, "uid": "uid-" + name,
        "resourceVersion": "1"}, "spec": {"containers": [{"name": "main"}]},
        "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}],
                   "containerStatuses": [{"name": "main", "ready": True,
                                          "restartCount": 0, "state": {"running": {}}}]}}


def certificate(directory, name):
    key, cert = directory / (name + ".key"), directory / (name + ".pem")
    ca_key, ca_cert = directory / (name + "-ca.key"), directory / (name + "-ca.pem")
    csr, extensions = directory / (name + ".csr"), directory / (name + ".ext")
    subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
                    "-days", "1", "-subj", "/CN=kborg-loopback-fixture-" + name,
                    "-addext", "basicConstraints=critical,CA:TRUE",
                    "-keyout", str(ca_key), "-out", str(ca_cert)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    subprocess.run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
                    "-subj", "/CN=127.0.0.1", "-keyout", str(key), "-out", str(csr)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    extensions.write_text("basicConstraints=critical,CA:FALSE\nsubjectAltName=IP:127.0.0.1\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")
    subprocess.run(["openssl", "x509", "-req", "-in", str(csr), "-CA", str(ca_cert),
                    "-CAkey", str(ca_key), "-CAcreateserial", "-days", "1",
                    "-extfile", str(extensions), "-out", str(cert)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    ca_key.chmod(0o600)
    key.chmod(0o600)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(str(cert), str(key))
    return context, ca_cert


class Fixture(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False

    def __init__(self, tls):
        self.tls = tls
        self.lock = threading.Lock()
        self.mode = "normal"
        self.requests = []
        self.pod_watches = 0
        self.pod_lists = 0
        self.unexpected = 0
        super().__init__(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def get_request(self):
        connection, address = super().get_request()
        connection.settimeout(3)
        try:
            return self.tls.wrap_socket(connection, server_side=True), address
        except Exception:
            connection.close()
            raise

    def reset(self, mode):
        with self.lock:
            self.mode = mode
            self.requests = []
            self.pod_watches = self.pod_lists = self.unexpected = 0

    def counts(self):
        with self.lock:
            return {"pod_lists": self.pod_lists, "pod_watches": self.pod_watches,
                    "http_requests": len(self.requests), "unexpected": self.unexpected}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def reply(self, code, body, slow=False):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            if slow:
                self.wfile.write(data[:1])
                self.wfile.flush()
                time.sleep(12)
                self.wfile.write(data[1:])
            else:
                self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            pass
        self.close_connection = True

    def do_GET(self):
        fixture = self.server
        parsed = urlparse(self.path)
        watch = parse_qs(parsed.query).get("watch") == ["true"]
        kind = parsed.path.rsplit("/", 1)[-1]
        allowed = parsed.path in [f"/api/v1/namespaces/{NAMESPACE}/{k}" for k in ("pods", "events")]
        with fixture.lock:
            mode = fixture.mode
            fixture.requests.append({"at": time.monotonic(), "kind": kind, "watch": watch})
            if kind == "pods":
                fixture.pod_watches += int(watch)
                fixture.pod_lists += int(not watch)
            watches, lists = fixture.pod_watches, fixture.pod_lists
            if not allowed:
                fixture.unexpected += 1
        if not allowed or self.headers.get("Authorization") != "Bearer " + TOKEN:
            self.reply(403, BODY_CANARY)
            return
        if kind == "pods" and watch:
            if mode == "throttle" and watches <= 3:
                self.reply(429, "\x1b[2J" + BODY_CANARY)
                return
            if mode == "expired" and watches == 1:
                self.reply(410, BODY_CANARY)
                return
            if mode in ("revoked401", "revoked403"):
                self.reply(int(mode[-3:]), "{invalid " + BODY_CANARY)
                return
            if mode == "outage" and watches <= 2:
                self.reply(503, BODY_CANARY)
                return
        if watch:
            self.reply(200, "")
            return
        items = [] if kind == "events" else [pod()]
        if kind == "pods":
            if mode == "expired" and lists > 1:
                items.append(pod("replacement"))
            elif mode == "cross_scope":
                items = [pod(namespace="foreign-namespace")]
            elif mode == "flood":
                items = [pod("fixture-" + str(i)) for i in range(1001)]
        body = json.dumps({"apiVersion": "v1", "kind": "PodList" if kind == "pods" else "EventList",
                           "metadata": {"resourceVersion": str(lists + 1)}, "items": items})
        self.reply(200, body, slow=mode == "slow_body" and kind == "pods")


class Follow:
    def __init__(self, binary, config):
        self.process = subprocess.Popen([str(binary), "--kubeconfig", str(config),
            "--context", "fixture", "--namespace", NAMESPACE, "overview", "fixture", "--follow", "--json"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
        self.pending = b""
        self.stderr = b""
        self.rows = deque(maxlen=200)
        self.started = time.monotonic()
        self.output_bytes = 0

    def drain(self, duration=.1):
        end = time.monotonic() + duration
        while time.monotonic() < end:
            streams = select.select([self.process.stdout, self.process.stderr], [], [],
                                    min(.05, max(0, end - time.monotonic())))[0]
            for stream in streams:
                data = os.read(stream.fileno(), 65536)
                self.output_bytes += len(data)
                if self.output_bytes > 2 * 1024 * 1024:
                    raise AssertionError("unbounded fixture output")
                if stream is self.process.stderr:
                    self.stderr += data
                else:
                    self.pending += data
                    while b"\n" in self.pending:
                        line, self.pending = self.pending.split(b"\n", 1)
                        if line:
                            assert BODY_CANARY.encode() not in line and TOKEN.encode() not in line
                            values = json.loads(line)
                            assert len(values) == 1
                            self.rows.append(values[0])
            if self.process.poll() is not None:
                break

    def wait(self, predicate, timeout=20):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.drain()
            for row in self.rows:
                if predicate(row):
                    return row
            if self.process.poll() is not None:
                break
        raise AssertionError("expected fixture state was not observed; states=" +
                             repr([(r.get("state"), r.get("pods")) for r in self.rows]))

    def close(self):
        started = time.monotonic()
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGINT)
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=3)
                raise AssertionError("follow did not stop on SIGINT")
        self.drain(.05)
        assert self.process.returncode in (0, -signal.SIGINT), "follow did not stop as expected"
        assert TOKEN.encode() not in self.stderr and BODY_CANARY.encode() not in self.stderr
        self.process.stdout.close()
        self.process.stderr.close()
        return round(time.monotonic() - started, 3)


def write_config(path, port, certificate_path):
    path.write_text(json.dumps({"apiVersion": "v1", "kind": "Config", "current-context": "fixture",
        "clusters": [{"name": "fixture", "cluster": {"server": f"https://127.0.0.1:{port}",
            "certificate-authority-data": base64.b64encode(certificate_path.read_bytes()).decode()}}],
        "contexts": [{"name": "fixture", "context": {"cluster": "fixture", "user": "fixture", "namespace": NAMESPACE}}],
        "users": [{"name": "fixture", "user": {"token": TOKEN}}]}))
    path.chmod(0o600)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    results = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="kborg-transport-faults-") as tmp:
        directory = Path(tmp).resolve()
        tls_a, cert_a = certificate(directory, "a")
        tls_b, cert_b = certificate(directory, "b")
        fixture = Fixture(tls_a)
        config = directory / "kubeconfig.json"
        write_config(config, fixture.server_port, cert_a)
        try:
            for mode in ["throttle", "expired", "outage", "revoked401", "revoked403", "cross_scope", "flood", "slow_body"]:
                fixture.reset(mode)
                follow = Follow(binary, config)
                try:
                    if mode in ("throttle", "outage"):
                        follow.wait(lambda r: r["state"] == "live")
                        follow.wait(lambda r: r["health"] is None and r["state"] != "connecting")
                        follow.rows.clear()
                        follow.wait(lambda r: r["state"] == "live" and r["health"]["ready"] == 1)
                        attempts = [r["at"] for r in fixture.requests if r["kind"] == "pods" and r["watch"]]
                        assert len(attempts) >= (4 if mode == "throttle" else 3)
                        assert all(b-a >= .35 for a, b in zip(attempts, attempts[1:])), "retry hot loop"
                    elif mode == "expired":
                        follow.wait(lambda r: r["state"] == "live" and r["pods"] == 2)
                        assert fixture.counts()["pod_lists"] == 2
                    elif mode.startswith("revoked"):
                        follow.wait(lambda r: r["state"] == "denied" and r["pods"] is None and r["health"] is None)
                        before = fixture.counts()["pod_watches"]
                        follow.drain(2)
                        assert fixture.counts()["pod_watches"] == before == 1
                    elif mode in ("cross_scope", "flood"):
                        follow.wait(lambda r: r["state"] not in ("live", "connecting") and r["pods"] is None and r["health"] is None)
                        before = fixture.counts()["pod_lists"]
                        follow.drain(.6)
                        assert fixture.counts()["pod_lists"] == before == 1
                    else:
                        follow.wait(lambda r: r["state"] not in ("live", "connecting") and r["health"] is None, timeout=18)
                        assert time.monotonic() - follow.started < 17
                    assert fixture.counts()["unexpected"] == 0
                    evidence = {"case": mode, "passed": True, "seconds": round(time.monotonic()-follow.started, 3), **fixture.counts()}
                finally:
                    stop_seconds = follow.close()
                evidence["interrupt_exit_seconds"] = stop_seconds
                results.append(evidence)
                print(json.dumps(evidence), flush=True)
            # Change the served trust root while a client retains its original CA.
            fixture.reset("normal")
            follow = Follow(binary, config)
            try:
                follow.wait(lambda r: r["state"] == "live")
                fixture.tls = tls_b
                follow.rows.clear()
                follow.wait(lambda r: r["state"] not in ("live", "connecting") and r["health"] is None)
                # Writing a new config cannot silently change an existing client's trust.
                write_config(config, fixture.server_port, cert_b)
                count = fixture.counts()["http_requests"]
                follow.drain(2)
                assert fixture.counts()["http_requests"] == count
                assert follow.rows[-1]["state"] != "live"
            finally:
                follow.close()
            replacement = Follow(binary, config)
            try:
                replacement.wait(lambda r: r["state"] == "live" and r["health"]["ready"] == 1)
            finally:
                replacement.close()
            results.append({"case": "tls_ca_rotation_requires_explicit_reconnect", "passed": True})
            print(json.dumps(results[-1]), flush=True)
        finally:
            fixture.shutdown()
            fixture.server_close()
            fixture.thread.join(timeout=2)
    report = {"schema": 1, "passed": True, "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "seconds": round(time.monotonic()-started, 3), "checks": results,
              "scope": "Synthetic HTTPS loopback Kubernetes protocol fixture with generated CAs; no real cluster or credential access. Static-token revocation, not a provider identity-rotation test."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2)+"\n")


if __name__ == "__main__":
    main()
