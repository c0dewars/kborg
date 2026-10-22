#!/usr/bin/env python3
"""Ephemeral HTTPS OCI Distribution fixture for owned Helm acceptance.

This is a protocol fixture, not an upstream registry interoperability claim.
Registry(root, {"name": packaged_chart}, mode="anonymous") exposes refs,
tampered_refs, ca_file, auth_file, url and a sanitized report(). All listeners
bind 127.0.0.1; all keys/credentials live beneath a new private child of root.
"""
from __future__ import annotations

import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import secrets
import shutil
import ssl
import subprocess
import threading
from urllib.parse import parse_qs, urlsplit


CONFIG = "application/vnd.cncf.helm.config.v1+json"
CHART = "application/vnd.cncf.helm.chart.content.v1.tar+gzip"
MANIFEST = "application/vnd.oci.image.manifest.v1+json"


def digest(data):
    return "sha256:" + hashlib.sha256(data).hexdigest()


def write_private(path, data):
    with path.open("xb") as stream:
        path.chmod(0o600)
        stream.write(data)


class Registry:
    MODES = {"anonymous", "basic", "bearer", "private-bearer", "token-redirect"}

    def __init__(self, root: Path, charts: dict[str, Path], mode="anonymous"):
        if mode not in self.MODES or not charts:
            raise ValueError("invalid OCI fixture mode or empty chart set")
        self.mode = mode
        self.root = Path(root) / ("oci-registry-" + secrets.token_hex(8))
        self.root.mkdir(mode=0o700)
        self.ca_file = self.root / "ca.pem"
        self.auth_file = None
        self.refs, self.tampered_refs = {}, {}
        self._routes, self._thread, self._server = {}, None, None
        self._lock = threading.Lock()
        self._counts = {key: 0 for key in ("manifest", "blob", "token", "redirect_capture", "denied", "unexpected_authorization")}
        self._auth_counts = {key: 0 for key in ("none", "basic", "bearer", "other")}
        self._password, self._token = secrets.token_urlsafe(32), secrets.token_urlsafe(32)
        self._basic = "Basic " + base64.b64encode(("fixture-reader:" + self._password).encode()).decode()
        self._chart_count = len(charts)
        try:
            key, config = self.root / "tls.key", self.root / "openssl.cnf"
            ca_key, ca_config = self.root / "ca.key", self.root / "ca.cnf"
            leaf, csr = self.root / "server.pem", self.root / "server.csr"
            write_private(ca_config, b"[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n[dn]\nCN=KBorg owned OCI fixture CA\n[ext]\nbasicConstraints=critical,CA:TRUE,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n")
            write_private(config, b"[req]\nprompt=no\ndistinguished_name=dn\nreq_extensions=ext\n[dn]\nCN=KBorg owned OCI fixture\n[ext]\nsubjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n")

            def openssl(*args):
                result = subprocess.run(["/usr/bin/openssl", *[str(arg) for arg in args]],
                                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        timeout=20, env={"PATH": "/usr/bin:/bin", "HOME": str(self.root)})
                if result.returncode:
                    raise RuntimeError("cannot create ephemeral public OCI fixture certificate; diagnostics suppressed")

            # A CA certificate must not also be used as the TLS end entity.
            # Rustls enforces this even where some Go/OpenSSL clients accept it.
            openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-sha256",
                    "-keyout", ca_key, "-out", self.ca_file, "-config", ca_config)
            openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-sha256",
                    "-keyout", key, "-out", csr, "-config", config)
            openssl("x509", "-req", "-in", csr, "-CA", self.ca_file, "-CAkey", ca_key,
                    "-set_serial", "1", "-days", "1", "-sha256", "-extfile", config,
                    "-extensions", "ext", "-out", leaf)
            for path in (key, ca_key, self.ca_file, leaf, csr):
                path.chmod(0o600)
            fixture = self

            class Handler(BaseHTTPRequestHandler):
                protocol_version = "HTTP/1.1"

                def log_message(self, *_):
                    pass

                def do_GET(self):
                    self.connection.settimeout(10)
                    try:
                        fixture._get(self)
                    except (BrokenPipeError, ConnectionResetError, TimeoutError, ssl.SSLError):
                        pass

            class Server(ThreadingHTTPServer):
                daemon_threads = True
                block_on_close = False

                def get_request(self):
                    connection, address = super().get_request()
                    connection.settimeout(5)
                    try:
                        return context.wrap_socket(connection, server_side=True), address
                    except BaseException:
                        connection.close()
                        raise

                def handle_error(self, *_):
                    # Never log requests, authorization, signed URLs or bodies.
                    pass

            self._server = Server(("127.0.0.1", 0), Handler)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(str(leaf), str(key))
            self.registry = "127.0.0.1:" + str(self._server.server_port)
            self.url = "https://" + self.registry
            for name, path in charts.items():
                if not re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,100}", name):
                    raise ValueError("invalid fixture chart name")
                with Path(path).open("rb") as stream:
                    chart = stream.read(8 * 1024 * 1024 + 1)
                if len(chart) > 8 * 1024 * 1024 or not chart.startswith(b"\x1f\x8b"):
                    raise ValueError("fixture chart must be a bounded packaged gzip chart")
                config_bytes = json.dumps({"name": name, "version": "0.1.0"}, separators=(",", ":")).encode()
                manifest = json.dumps({"schemaVersion": 2, "mediaType": MANIFEST,
                                       "config": {"mediaType": CONFIG, "digest": digest(config_bytes), "size": len(config_bytes)},
                                       "layers": [{"mediaType": CHART, "digest": digest(chart), "size": len(chart)}]},
                                      separators=(",", ":")).encode()
                for suffix, tampered in ((name, False), (name + "-tampered", True)):
                    repository = "fixtures/" + suffix
                    prefix = "/v2/" + repository
                    self._routes[prefix + "/manifests/" + digest(manifest)] = (MANIFEST, manifest, repository)
                    self._routes[prefix + "/blobs/" + digest(config_bytes)] = (CONFIG, config_bytes, repository)
                    self._routes[prefix + "/blobs/" + digest(chart)] = (CHART, chart[:-1] + bytes([chart[-1] ^ 1]) if tampered else chart, repository)
                    target = self.tampered_refs if tampered else self.refs
                    target[name] = "oci://" + self.registry + "/" + repository + "@" + digest(manifest)
            if mode in {"basic", "private-bearer", "token-redirect"}:
                self.auth_file = self.root / "registry-auth.json"
                write_private(self.auth_file, json.dumps({"registry": self.registry, "username": "fixture-reader", "password": self._password}).encode())
        except BaseException:
            self.close()
            raise

    def __enter__(self):
        self._thread = threading.Thread(target=self._server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, *_):
        self.close()

    def close(self):
        if self._server is not None:
            if self._thread is not None:
                self._server.shutdown()
                self._thread.join(timeout=3)
            self._server.server_close()
            self._server = None
        shutil.rmtree(self.root, ignore_errors=True)

    def report(self):
        with self._lock:
            return {"mode": self.mode, "tls": True, "loopback_only": True, "digest_pinned": True,
                    "chart_count": self._chart_count, "tamper_routes": len(self.tampered_refs),
                    "requests": dict(self._counts), "authorization_kinds": dict(self._auth_counts),
                    "protocol_fixture_not_upstream_registry": True}

    def _count(self, key):
        with self._lock:
            self._counts[key] += 1

    @staticmethod
    def _reply(handler, status, data=b"", headers=None):
        handler.send_response(status)
        handler.send_header("Content-Length", str(len(data)))
        handler.send_header("Connection", "close")
        for key, value in (headers or {}).items():
            handler.send_header(key, value)
        handler.end_headers()
        if data:
            handler.wfile.write(data)
        handler.close_connection = True

    def _get(self, handler):
        uri = urlsplit(handler.path)
        authorization = handler.headers.get("Authorization", "")
        kind = "none" if not authorization else "basic" if authorization.startswith("Basic ") else "bearer" if authorization.startswith("Bearer ") else "other"
        with self._lock:
            self._auth_counts[kind] += 1
        if uri.path == "/capture-private":
            self._count("redirect_capture")
            return self._reply(handler, 500)
        if uri.path == "/token":
            self._count("token")
            query = parse_qs(uri.query, strict_parsing=True)
            expected_auth = "" if self.mode == "bearer" else self._basic
            scope = query.get("scope", [""])
            if (authorization != expected_auth or query.get("service") != [self.registry]
                    or len(scope) != 1 or not re.fullmatch(r"repository:fixtures/[a-z0-9.-]+:pull", scope[0])):
                self._count("denied")
                return self._reply(handler, 403)
            if self.mode == "token-redirect":
                return self._reply(handler, 307, headers={"Location": self.url + "/capture-private"})
            return self._reply(handler, 200, json.dumps({"token": self._token}).encode(), {"Content-Type": "application/json"})
        route = self._routes.get(uri.path)
        if route is None or uri.query:
            return self._reply(handler, 404)
        media, data, repository = route
        authorized = self.mode == "anonymous" or (self.mode == "basic" and authorization == self._basic) or (self.mode in {"bearer", "private-bearer", "token-redirect"} and authorization == "Bearer " + self._token)
        if not authorized:
            self._count("denied")
            challenge = 'Basic realm="kborg-owned-fixture"' if self.mode == "basic" else ('Bearer realm="' + self.url + '/token",service="' + self.registry + '",scope="repository:' + repository + ':pull"')
            return self._reply(handler, 401, headers={"WWW-Authenticate": challenge})
        if self.mode == "anonymous" and authorization:
            self._count("unexpected_authorization")
        self._count("manifest" if "/manifests/" in uri.path else "blob")
        return self._reply(handler, 200, data, {"Content-Type": media, "Docker-Content-Digest": digest(data)})
