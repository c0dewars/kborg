#!/usr/bin/env python3
"""Install the pinned Metrics Server chart on an explicitly identified Desktop cluster.

Supply the PUBLIC kubelet serving certificate, its DNS SAN, and the node's internal
IP. No private kubelet key is needed. The certificate pin and IP must be refreshed
after node/certificate replacement. This helper never disables TLS verification.
Without --install, only render, ownership checks and the resource summary run.
"""
import argparse
import base64
import hashlib
import ipaddress
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
from urllib.parse import urlparse
from urllib.request import urlopen

CHART_URL = "https://github.com/kubernetes-sigs/metrics-server/releases/download/metrics-server-helm-chart-3.14.0/metrics-server-3.14.0.tgz"
CHART_SHA = "c2ca1185c01e6e7f53dd1b7d131f0c9b3fa50e003ed068b784563a1b5a3422a1"
IMAGE = "v0.9.0@sha256:d9862115e7c7881280d3d75ca26bda8ffc0fc213315979575bf23ce9826205c0"
NAMESPACE = "kborg-system"
RELEASE = "kborg-metrics-server"
OWNER = {"app.kubernetes.io/part-of": "kborg-desktop-metrics"}


def run(argv, data=None, timeout=180, environment=None):
    p = subprocess.run(argv, input=data, text=True, capture_output=True, timeout=timeout, env=environment)
    if p.returncode:
        # Helm render/diagnostics can contain private serving keys. Never print them.
        raise RuntimeError(f"{Path(argv[0]).name} {argv[1]} failed (exit {p.returncode}; diagnostics suppressed)")
    return p.stdout


def read_bounded(path, maximum):
    if not path.is_file():
        raise RuntimeError("expected a regular local input file")
    with path.open("rb") as source:
        content = source.read(maximum + 1)
    if len(content) > maximum:
        raise RuntimeError("local input file exceeds its size limit")
    return content


def helm_environment():
    # Helm-only endpoint/token/TLS overrides must not bypass the kubectl
    # endpoint and cluster-UID checks. Both tools retain the same KUBECONFIG.
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith("HELM_KUBE")}
    environment["HELM_DRIVER"] = "secret"
    return environment


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--helm", required=True, type=Path)
    p.add_argument("--chart", required=True, type=Path)
    p.add_argument("--public-kubelet-cert", required=True, type=Path)
    p.add_argument("--node-hostname", required=True)
    p.add_argument("--node-ip", required=True)
    p.add_argument("--expected-server", required=True)
    p.add_argument("--system-uid", required=True, help="Expected kube-system namespace UID")
    p.add_argument("--install", action="store_true")
    p.add_argument("--renew-serving-certificate", action="store_true",
                   help="Generate a new aggregation serving certificate and matching APIService CA together")
    args = p.parse_args()
    helm = args.helm.resolve(strict=True)
    helm_env = helm_environment()
    kubectl = shutil.which("kubectl")
    if not kubectl or not os.access(helm, os.X_OK):
        raise RuntimeError("kubectl and an explicit executable Helm path are required")
    ipaddress.ip_address(args.node_ip)
    if not re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?", args.node_hostname):
        raise RuntimeError("invalid node DNS name")
    prefix = [kubectl, "--context", "docker-desktop", "--request-timeout=30s"]

    def kube(*argv, data=None, timeout=180):
        return run([*prefix, *argv], data, timeout)

    config = json.loads(kube("config", "view", "--minify", "--raw", "-o", "json"))
    cluster = config["clusters"][0]["cluster"]
    endpoint = urlparse(cluster["server"])
    if (cluster["server"] != args.expected_server or endpoint.scheme != "https"
            or endpoint.hostname not in ("127.0.0.1", "localhost", "kubernetes.docker.internal")
            or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment
            or endpoint.path not in ("", "/") or not cluster.get("certificate-authority-data")
            or cluster.get("insecure-skip-tls-verify") or cluster.get("proxy-url")):
        raise RuntimeError("expected local TLS endpoint and CA required")
    system = json.loads(kube("get", "namespace", "kube-system", "-o", "json"))
    if system["metadata"]["uid"] != args.system_uid:
        raise RuntimeError("cluster identity changed")
    node = json.loads(kube("get", "node", args.node_hostname, "-o", "json"))
    if {"type": "InternalIP", "address": args.node_ip} not in node["status"]["addresses"]:
        raise RuntimeError("node IP changed")
    cert = read_bounded(args.public_kubelet_cert, 32768).decode("ascii")
    certificate_pattern = r"-----BEGIN CERTIFICATE-----\r?\n[A-Za-z0-9+/=\r\n]+-----END CERTIFICATE-----"
    certificates = re.findall(certificate_pattern, cert)
    if ("PRIVATE KEY" in cert or not 1 <= len(certificates) <= 8
            or re.sub(certificate_pattern, "", cert).strip()):
        raise RuntimeError("supply only a bounded PUBLIC kubelet serving certificate chain")
    for certificate in certificates:
        run(["openssl", "x509", "-noout"], data=certificate)
    san = run(["openssl", "x509", "-noout", "-ext", "subjectAltName"], data=cert)
    if f"DNS:{args.node_hostname}" not in [s.strip() for s in san.splitlines()[-1].split(",")]:
        raise RuntimeError("node hostname must exactly match a certificate DNS SAN")
    run(["openssl", "x509", "-noout", "-checkend", "86400"], data=cert)
    if not args.chart.exists():
        args.chart.parent.mkdir(parents=True, exist_ok=True)
        with urlopen(CHART_URL, timeout=60) as response:
            archive = response.read(4 * 1024 * 1024 + 1)
        if hashlib.sha256(archive).hexdigest() != CHART_SHA:
            raise RuntimeError("official chart checksum mismatch")
        args.chart.write_bytes(archive)
    chart_bytes = read_bounded(args.chart, 4 * 1024 * 1024)
    if hashlib.sha256(chart_bytes).hexdigest() != CHART_SHA:
        raise RuntimeError("pinned chart checksum mismatch")
    ns_raw = kube("get", "namespace", NAMESPACE, "--ignore-not-found", "-o", "json")
    if ns_raw and not all(json.loads(ns_raw)["metadata"].get("labels", {}).get(k) == v for k, v in OWNER.items()):
        raise RuntimeError("namespace exists without KBorg metrics ownership")
    if ns_raw and json.loads(ns_raw)["metadata"].get("deletionTimestamp"):
        raise RuntimeError("owned metrics namespace is terminating")
    with tempfile.TemporaryDirectory(prefix="kborg-metrics-") as temporary:
        root = Path(temporary).resolve()
        os.chmod(root, 0o700)
        with tarfile.open(fileobj=io.BytesIO(chart_bytes), mode="r:gz") as archive:
            for entry in archive.getmembers():
                target = root / entry.name
                if not entry.isfile() or not target.resolve().is_relative_to(root) or entry.size > 1024 * 1024:
                    raise RuntimeError("unexpected chart archive member")
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(archive.extractfile(entry).read())
        chart = root / "metrics-server"
        deployment = chart / "templates/deployment.yaml"
        original = deployment.read_text()
        anchor = "    spec:\n      {{- with .Values.schedulerName }}"
        if original.count(anchor) != 1:
            raise RuntimeError("upstream Deployment template changed")
        deployment.write_text(original.replace(anchor, "    spec:\n      hostAliases:\n        {{- toYaml .Values.kborgHostAliases | nindent 8 }}\n      {{- with .Values.schedulerName }}"))
        # Minimal local chart extension; stored in the Helm release and upgradeable.
        (chart / "templates/kborg-kubelet-trust.yaml").write_text('''apiVersion: v1
kind: ConfigMap
metadata:
  name: {{ include "metrics-server.fullname" . }}-kubelet-trust
  namespace: {{ include "metrics-server.namespace" . }}
data:
  kubelet-ca.crt: {{ .Values.kborgKubeletCertificate | quote }}
''')
        values = {"fullnameOverride": RELEASE, "commonLabels": OWNER,
                  "image": {"tag": IMAGE}, "apiService": {"insecureSkipTLSVerify": False},
                  "tls": {"type": "helm", "helm": {"lookup": not args.renew_serving_certificate, "certDurationDays": 365}},
                  "defaultArgs": ["--cert-dir=/tmp", "--kubelet-preferred-address-types=Hostname", "--kubelet-use-node-status-port", "--metric-resolution=15s"],
                  "args": ["--kubelet-certificate-authority=/etc/kubelet-trust/kubelet-ca.crt"],
                  "kborgHostAliases": [{"ip": args.node_ip, "hostnames": [args.node_hostname]}],
                  "kborgKubeletCertificate": cert,
                  "extraVolumes": [{"name": "kubelet-trust", "configMap": {"name": RELEASE + "-kubelet-trust"}}],
                  "extraVolumeMounts": [{"name": "kubelet-trust", "mountPath": "/etc/kubelet-trust", "readOnly": True}],
                  "resources": {"requests": {"cpu": "50m", "memory": "100Mi"}, "limits": {"cpu": "250m", "memory": "256Mi"}}}
        values_path = root / "values.json"
        values_path.write_text(json.dumps(values))
        rendered = run([str(helm), "template", RELEASE, str(chart), "--namespace", NAMESPACE, "--values", str(values_path)], environment=helm_env)
        decoded = kube("create", "--dry-run=client", "-f", "-", "-o", "json", data=rendered).strip()
        objects = []
        while decoded:
            obj, end = json.JSONDecoder().raw_decode(decoded)
            objects.extend(obj["items"] if obj.get("kind") == "List" else [obj])
            decoded = decoded[end:].lstrip()
        summary = []
        for obj in objects:
            kind, name = obj["kind"], obj["metadata"]["name"]
            namespace = obj["metadata"].get("namespace")
            scope = ["-n", namespace] if namespace else []
            existing = kube("get", kind, name, *scope, "--ignore-not-found", "-o", "json")
            if existing:
                metadata = json.loads(existing)["metadata"]
                annotations = metadata.get("annotations", {})
                if (annotations.get("meta.helm.sh/release-name") != RELEASE
                        or annotations.get("meta.helm.sh/release-namespace") != NAMESPACE
                        or metadata.get("labels", {}).get("app.kubernetes.io/managed-by") != "Helm"):
                    raise RuntimeError(f"refusing ownership conflict: {kind}/{name}")
            summary.append(f"{kind}/{name}")
        print(json.dumps({"resources": summary, "chart_sha256": CHART_SHA, "image": IMAGE,
                          "kubelet_certificate_sha256": hashlib.sha256(cert.encode()).hexdigest(),
                          "renew_serving_certificate": args.renew_serving_certificate,
                          "tls_verification_configured": True,
                          "both_tls_hops_verified": False, "install": args.install}, indent=2))
        if not args.install:
            return
        if not ns_raw:
            kube("create", "-f", "-", data=json.dumps({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": NAMESPACE, "labels": OWNER}}))
        run([str(helm), "upgrade", "--install", RELEASE, str(chart), "--kube-context", "docker-desktop", "--namespace", NAMESPACE,
             "--values", str(values_path), "--wait", "--rollback-on-failure", "--timeout", "180s"], timeout=390, environment=helm_env)
        kube("wait", "--for=condition=Available", "apiservice/v1beta1.metrics.k8s.io", "--timeout=60s", timeout=75)
        api = json.loads(kube("get", "apiservice", "v1beta1.metrics.k8s.io", "-o", "json"))
        if api["spec"].get("insecureSkipTLSVerify") or not api["spec"].get("caBundle"):
            raise RuntimeError("aggregation TLS verification not enabled")
        metrics = json.loads(kube("get", "--raw=/apis/metrics.k8s.io/v1beta1/nodes"))
        if not any(row["metadata"]["name"] == args.node_hostname for row in metrics["items"]):
            raise RuntimeError("target node has no Metrics API sample")
        print(json.dumps({"both_tls_hops_verified": True, "node_sample_received": True,
                          "release": RELEASE, "namespace": NAMESPACE}))
        print("Metrics Server ready; verified node sample received. Public certificate pin requires renewal after node/certificate rotation.")


if __name__ == "__main__":
    main()
