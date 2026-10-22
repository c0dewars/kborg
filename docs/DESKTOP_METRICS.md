# Docker Desktop metrics

KBorg reads CPU and memory samples from the Kubernetes Metrics API. Docker
Desktop does not always provide that API. The optional
[`setup-desktop-metrics.py`](../scripts/setup-desktop-metrics.py) helper installs
the pinned Metrics Server chart into an explicitly identified Docker Desktop
cluster. KBorg itself does not install a provider or require Helm at runtime.

The helper verifies both TLS connections: API aggregation to Metrics Server, and
Metrics Server to the kubelet. It never sets `--kubelet-insecure-tls` or an
insecure APIService. Metrics Server supplies recent usage snapshots for resource
management; it is not a historical monitoring system. See the upstream
[requirements](https://github.com/kubernetes-sigs/metrics-server#requirements).

## Identify the cluster and public certificate

Use Python 3.9+, kubectl, OpenSSL, and an explicitly selected trusted Helm 4
executable. Installation needs administrator rights for the rendered resources.
The helper always names `docker-desktop`; it never changes the current context.
An unrelated Minikube context is not a target for this helper.

Inspect these public identity fields and keep the results for the command below:

```sh
kubectl --context docker-desktop config view --minify -o 'jsonpath={.clusters[0].cluster.server}'
kubectl --context docker-desktop get namespace kube-system -o 'jsonpath={.metadata.uid}'
kubectl --context docker-desktop get nodes -o wide
```

Supply the node's current InternalIP and its exact hostname DNS SAN. The helper
checks the endpoint, embedded API CA, `kube-system` UID, node identity, certificate
SAN and at least 24 hours of remaining certificate validity before installing.
It refuses endpoint/TLS overrides from `HELM_KUBE*` environment variables.

Only the **public** kubelet serving certificate is needed. On the validated
Docker Desktop node its certificate-only file is
`/var/lib/kubelet/pki/kubelet.crt`. Do not read a private key, a combined
`kubelet-server-current.pem`, or mount the containing host directory. An
administrator can export that single public file through a temporary probe with:

- A new, uniquely named namespace and Pod, with their UIDs recorded for cleanup.
- The selected node's `nodeName`; an `activeDeadlineSeconds: 60` deadline and
  `restartPolicy: Never`.
- One `hostPath` volume whose path is exactly the file above and whose type is
  `File`, mounted read-only at `/public/kubelet.crt`.
- A digest-pinned minimal image, a command equivalent to
  `cat /public/kubelet.crt`, no service-account token, UID/GID 65534,
  `allowPrivilegeEscalation: false`, all capabilities dropped, a read-only root
  filesystem and `seccompProfile.type: RuntimeDefault`.

Capture only that Pod's finite output into a local public certificate file;
verify it contains only public `BEGIN CERTIFICATE` blocks and no `PRIVATE KEY`.
The validated node supplies its leaf and CA together; the helper accepts up to
eight public certificates within 32 KiB, with the serving leaf first. Delete the
probe and its owned namespace promptly, checking their recorded identities
before cleanup. This is an administrator-only, narrowly scoped host-file read,
not a regular KBorg inspection action. If the host file or permissions differ,
obtain the public certificate through your platform's supported administrator
workflow instead of widening the mount or running a privileged shell.

## Review and install

Use the actual values inspected above; sample placeholders are not defaults:

```sh
python3 scripts/setup-desktop-metrics.py \
  --helm "$PWD/.local/platform-tools/helm" \
  --chart "$PWD/.local/metrics-desktop/chart.tgz" \
  --public-kubelet-cert "$PWD/.local/metrics-desktop/kubelet-ca.crt" \
  --node-hostname '<CURRENT_HOSTNAME_DNS_SAN>' \
  --node-ip '<CURRENT_INTERNAL_IP>' \
  --expected-server '<EXACT_HTTPS_SERVER>' \
  --system-uid '<CURRENT_KUBE_SYSTEM_UID>'
```

This default is a read/render preflight. It may download the pinned public chart
into the specified cache, but does not install Kubernetes resources. Its summary
sets `tls_verification_configured: true` and
`both_tls_hops_verified: false`; a render is not a live health check. Review the
listed resources, then repeat the same command with `--install` to create or
upgrade them. Helm waits for readiness and rolls back a failed upgrade.

The helper owns namespace `kborg-system` through the label
`app.kubernetes.io/part-of=kborg-desktop-metrics`. Release
`kborg-metrics-server` manages its namespaced resources and required cluster
resources. The namespace itself is not a Helm chart resource. Existing objects
must already have the exact Helm release ownership; the helper never adopts
foreign resources. Generated serving private keys remain in the trusted render/install processes,
private temporary render files and Kubernetes Secret storage, including Helm
release history. Diagnostics omit them; Helm storage still requires sensitive
read access even when KBorg redacts its projections.

The pinned upstream chart is `metrics-server` 3.14.0, SHA-256
`c2ca1185c01e6e7f53dd1b7d131f0c9b3fa50e003ed068b784563a1b5a3422a1`.
The image is `v0.9.0` at digest
`sha256:d9862115e7c7881280d3d75ca26bda8ffc0fc213315979575bf23ce9826205c0`.
The helper adds a public-trust ConfigMap and a host alias so the kubelet DNS SAN
is verified at the node's InternalIP. It overrides the chart's APIService TLS
default and uses its Helm-managed serving certificate support; see the pinned
[chart values](https://github.com/kubernetes-sigs/metrics-server/blob/metrics-server-helm-chart-3.14.0/charts/metrics-server/values.yaml).

## Check and maintain

After installation, `both_tls_hops_verified: true` is reported only after the
APIService is Available, its CA is configured with verification enabled, and the
target node has a real Metrics API sample. Confirm from either client:

```sh
kubectl --context docker-desktop top nodes
./target/release/kborg --context docker-desktop metrics nodes --json
./target/release/kborg --context docker-desktop metrics pods --json
```

CPU/memory table columns and the metrics view show missing, stale or denied
samples explicitly. They do not turn a provider error into zero usage.

Refresh the **public kubelet pin** after a node/certificate replacement, and its
IP after an address change. Export the new certificate, inspect the cluster
identity again, and rerun preflight and `--install`. A Docker Desktop reset
changes the cluster UID; review that new identity explicitly. Do not bypass a
failure by disabling TLS.

The chart's aggregation serving certificate lasts 365 days. Normal upgrades
reuse it. Before it expires, repeat the reviewed setup command with
`--renew-serving-certificate` for preflight, then with that flag and `--install`.
This deliberately generates a new serving Secret and matching APIService CA in
the same Helm upgrade. Check readiness and real metrics afterward. Changing
only one side of that certificate/CA pair breaks aggregation.

For an unavailable API, inspect the APIService condition and the owned Metrics
Server Pod's status. A `Forbidden` response is an RBAC problem; a missing API is a
provider problem; kubelet `x509` errors require renewing the verified public pin
or matching hostname/IP. Keep application logs and private configuration out of
shared diagnostics.

To remove this installation, first verify the exact release ownership and
cluster identity, then uninstall only `kborg-metrics-server` from `kborg-system`
with the same explicit Docker Desktop Helm context and Secret storage driver.
The namespace remains; inspect its contents before any separate deletion.
