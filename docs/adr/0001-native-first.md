# ADR 0001: native-first modular monolith

Accepted for local-testing iteration; revisit before production.

One Rust crate with separate domain/policy, gateway, safety, themes, CLI and terminal modules keeps iteration overhead low. A Cargo workspace can follow when independent interfaces stabilize. The UI uses std threads and bounded channels. A single Tokio current-thread runtime in the API worker is required by the Kubernetes transport. The CLI uses the same gateway/policy.

Direct runtime dependencies: kube (client protocol/authentication/discovery), k8s-openapi (generated Kubernetes types), Tokio (transport runtime), serde/serde_json (bounded config/data serialization), Crossterm (portable input/raw terminal), http/http-body-util/tower (response cap middleware). No TUI layout framework, CLI parser framework, theme SDK or provider SDK is used. These are explicit exceptions to native-first: Rust std does not provide these protocols and portability APIs. Versions are resolved in Cargo.lock and Rust is pinned.

Application code forbids unsafe Rust. Dependencies may use unsafe or build scripts and require separate audit. Kube uses Rustls/ring, with retries, gzip, proxies, exec providers and debug config inference disabled. A Limited HTTP body layer bounds input before deserialization. Standard library handles policy, timestamps, processes (none executed by app), config paths, bounded queues and file handling.

First iteration uses bounded polling rather than watches to keep reconnect/cache semantics simple. First iteration rejects unsupported credential helpers instead of executing user-configured programs without enforceable trust controls. No external commands form the application backend. These are documented feature gaps, not claims of full implementation.
