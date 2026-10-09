# ADR 0001: native-first modular monolith

Accepted for local-testing iteration; revisit before production.

One Rust crate with separate domain/policy, gateway, safety, themes, CLI and terminal modules keeps iteration overhead low. A Cargo workspace can follow when independent interfaces stabilize. The UI uses std threads and bounded channels. A single Tokio current-thread runtime in the API worker is required by the Kubernetes transport. The CLI uses the same gateway/policy.

Direct runtime dependencies: kube (client protocol/authentication/discovery), k8s-openapi (generated Kubernetes types), Tokio (transport runtime), serde/serde_json (bounded config/data serialization), Crossterm (portable input/raw terminal), http/http-body-util/tower (response cap middleware). No TUI layout framework, CLI parser framework, theme SDK or provider SDK is used. These are explicit exceptions to native-first: Rust std does not provide these protocols and portability APIs. Versions are resolved in Cargo.lock and Rust is pinned.

Application code forbids unsafe Rust. Dependencies may use unsafe or build scripts and require separate audit. Kube uses Rustls/ring, with retries, gzip, proxies, exec providers and debug config inference disabled. A Limited HTTP body layer bounds input before deserialization. Standard library handles policy, timestamps, process boundaries, config paths, bounded queues and file handling. The sole optional external process is an explicitly selected trusted Copilot CLI login handoff.

The 0.2 iteration adds native bounded LIST/WATCH state and a coalesced one-slot mailbox; explicit polling remains available. futures-util is now a direct dependency using only std/async-await features, because stable Rust std does not provide StreamExt; the same crate was already transitive. No async macro dependency or kube-runtime/layout framework is added. First iteration rejects unsupported credential helpers instead of executing user-configured programs without enforceable trust controls. No external commands form the application backend. These are documented feature gaps, not claims of full implementation.
