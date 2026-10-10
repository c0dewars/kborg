# ADR 0001: native-first modular monolith

Accepted for local-testing iteration; revisit before production.

One Rust crate with separate domain/policy, gateway, safety, themes, CLI and terminal modules keeps iteration overhead low. A Cargo workspace can follow when independent interfaces stabilize. The UI uses std threads and bounded channels. A single Tokio current-thread runtime in the API worker is required by the Kubernetes transport. The CLI uses the same gateway/policy.

Direct runtime dependencies: kube (client protocol/authentication/discovery), k8s-openapi (generated Kubernetes types), Tokio (transport runtime), serde/serde_json (bounded config/data serialization), Crossterm (portable input/raw terminal), http/http-body/http-body-util/tower (response cap/deadline middleware), ring (SHA-256 trust fingerprints) and base64 (Kubernetes credential/CA encoding). No TUI layout framework, CLI parser framework, theme SDK or provider SDK is used. These are explicit exceptions to native-first: Rust std does not provide these protocols, cryptographic primitives and portability APIs. Versions are resolved in Cargo.lock and Rust is pinned.

Application code forbids unsafe Rust. Dependencies may use unsafe or build scripts and require separate audit. Kube uses Rustls/ring, with retries, gzip, proxies, automatic exec-provider execution and debug config inference disabled. A Limited HTTP body layer bounds input before deserialization. Standard library handles policy, process boundaries, config paths, bounded queues and file handling. Explicitly reviewed native authentication helpers, local tool views and Copilot login/preflight use separate supervised process boundaries; no subprocess forms the Kubernetes backend.

The 0.2 iteration adds native bounded LIST/WATCH state and a coalesced one-slot mailbox; explicit polling remains available. futures-util is now a direct dependency using only std/async-await features, because stable Rust std does not provide StreamExt; the same crate was already transitive. No async macro dependency or kube-runtime/layout framework is added. First iteration rejects unsupported credential helpers instead of executing user-configured programs without enforceable trust controls. No external commands form the application backend. These are documented feature gaps, not claims of full implementation.

## Priority iteration dependency decisions (2026-10-10)

The reviewed helper/view and stream-lifetime workflows promote three already
transitive crates to direct dependencies. Comparing the priority lockfile with
`c54dea9` adds no package name/version pair; both graphs contain 198 locked
package records. The priority lockfile SHA-256 is
`e9d0cd1469ec5fcc409612be616e2dd8e95f04001ccad6ba5db34d7dba9fd028`.

| Direct dependency and exact locked version | Reason | Graph/contract effect |
|---|---|---|
| `ring 0.17.14` | Stream SHA-256 over native executables, CA and exact helper configuration; stable public fingerprints for helper policies, registered views and SDK preflight | Reuses kube's existing ring graph; no custom cryptographic implementation or publisher-signature claim |
| `base64 0.22.1` | Decode pinned kubeconfig CA data and encode ExecCredential certificate/key PEM for the Kubernetes TLS configuration | Reuses the existing codec; credential contents are not printed or retained in trust manifests/evidence |
| `http-body 1.1.0` | Implement the standard body adapter for explicit object idle/deadline limits and separately bounded long log streams | Reuses the existing HTTP graph; standard `Body`/`Limited` composition, with no custom HTTP/TLS/WebSocket protocol parser |

Crossterm's existing `bracketed-paste` feature is enabled for bounded interactive
input; it introduces no new locked package pair. Interactive sessions use kube's
native WebSocket protocol support and generated status types. The metrics history,
resource comparisons, registry and terminal layout use std/existing dependencies.

Kube's unbounded automatic helper refresh is not enabled. KBorg supervises only
a separately approved native helper, removes exec configuration before building
the client, and gates expiry/401/403 before subsequent sends. Registered native
views retain per-invocation transfer approval and supply no cluster credentials.
These supervisors do not establish OS isolation or eliminate same-user path
races. Copilot preflight cannot enable inference; its credential, provenance and
hostname-containment gates remain closed.

The historical [dependency advisory audit](../DEPENDENCY_AUDIT.md) remains tied
to its stated lockfile/database revision. Promoting existing crates and passing
local tests do not constitute a fresh RustSec scan or an independent dependency
security audit for this lockfile.
