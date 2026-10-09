# Native pod operations

Native Kubernetes exec, output-only attach and port forwarding are independent opt-in capabilities. Each requires both write mode and its explicit allow flag. System namespaces are prohibited. An operator profile alone does not grant these flags. No kubectl subprocess or shell command construction is involved.

Every operation reads the Pod, displays endpoint/context/namespace/name/UID/resourceVersion and its complete bounded request, and requires exact namespace/name confirmation within 60 seconds. Approval is consumed by moving the sealed review value into execution. Execution rechecks UID/resourceVersion and writes a durable metadata-only audit intent before opening a websocket. Audit excludes arguments, output and forwarding content. A crash may leave an unresolved intent; never automatically retry it.

Exec accepts direct argv with at most 64 arguments and 2048 combined bytes (serialized review line at most 3000 bytes). No input or terminal is attached. Attach reads stdout/stderr without stdin or a TTY. Each stream has a 256 KiB retained output limit and a 30-second capture deadline. Terminal controls are sanitized. Kubernetes command errors are summarized without reflecting server details. A disconnected websocket does **not** guarantee termination of a process already started in the container; timeout or lost status is an unknown remote outcome.

Forwarding binds only IPv4 127.0.0.1, with local ports at least 1024 and a user-selected lifetime of 1 to 300 seconds. At most four active clients are serviced. Each client's websocket is closed on completion, error, cancellation or session expiry. Every accepted client rechecks Pod UID before opening its stream. Other local users can access the loopback listener. A forwarding session can transport application requests that change remote state.

Kubernetes exec/attach/portforward subresources have no atomic UID precondition. The preflight checks reduce the name-recreation window but cannot eliminate it. Organizations requiring atomic workload identity must leave these capabilities disabled. Server RBAC/admission and pod security policy remain authoritative.

## Dependencies and acceptance

The existing kube dependency's `ws` feature and Tokio `io-util` feature are required for native protocol support. This adds upstream websocket transport and handshake dependencies instead of a custom security-sensitive WebSocket implementation. New dependency versions remain pinned in Cargo.lock and require the release dependency audit. WebSocket framing has upstream allocation limits separate from the application's retained output bounds; large-frame/fault and cancellation tests remain production gates.

Unit tests cover malformed requests, bounded output and terminal sanitation. Live exec/attach/forward interoperability, concurrent-client cancellation, denied RBAC and name-recreation race drills must be completed in an owned fixture before these optional capabilities are production accepted. Interactive TTY shell sessions are not implemented by this bounded interface.

## Local acceptance evidence (2026-10-10)

Owned Docker Desktop run `a3e8a3f4b925` passed against `https://127.0.0.1:64170`, with the configured CA verified. Namespace `kborg-ops-a3e8a3f4b925` and its short-lived credential files were deleted after testing. Machine-readable record: [native-operations-desktop.json](evidence/native-operations-desktop.json).

The live test covered approved direct `printf` exec with control-character output sanitized, read-only and missing-capability rejection, system namespace rejection, wrong typed confirmation, reader RBAC denial, a real HTTP request over a three-second IPv4 loopback forward, refusal of new connections after expiry, and bounded output-only attach termination. The pinned restricted nonroot busybox fixture used digest `sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e` and an isolated memory-backed web directory. No existing workload or default fixture RBAC was modified.

An initial run was blocked by an unrelated concurrent compile change. Two subsequent runs correctly failed HTTP403 with only `get` on streaming subresources: Docker Desktop Kubernetes 1.36 requires `create` authorization as well as the client's websocket GET request. The final operator role grants `get,create` only on `pods/exec`, `pods/attach`, `pods/portforward`, plus ordinary Pod `get`. The reader has only ordinary Pod `get`. All failed-run namespaces and token files were cleaned up.

This validates local interoperability, not all production gates. Concurrent four-client saturation, packet faults, deliberate Pod recreation, long remote process reconciliation, and cross-platform native protocol acceptance remain unverified. Interactive stdin/TTY support remains outside the current interface.

## Upstream WebSocket allocation limits

Inspected the exact locked source: kube-client 4.2.0 `src/client/mod.rs` constructs `WebSocketStream::from_raw_socket` with `None` configuration. Its public `Api::exec`, `Api::attach` and `Api::portforward` helpers expose no `WebSocketConfig` parameter. Their internal protocol constructors are private; reducing their transport caps requires an upstream API change or maintaining a protocol adapter/fork.

Tungstenite 0.29.0 `src/protocol/mod.rs` defaults to a 16 MiB incoming frame payload limit, 64 MiB incoming assembled message limit, and 128 KiB read/write buffers per WebSocket. The configured maximum write buffer is `usize::MAX`; this is not a total memory bound. Upstream stream loops propagate failed writes rather than continuously retrying them. Four forwarded clients can each own a separate WebSocket and a 1 MiB Kubernetes forwarding pipe.

These are source-confirmed protocol limits, **not measured peak RSS**. KBorg's 256 KiB output retention cap does not lower the upstream frame/message allocation limits. Hostile-frame memory measurements and a smaller upstream-configurable transport budget remain a performance/security gate for enabling native streams in high-risk environments. The optional capabilities remain disabled by default; no custom WebSocket or cryptographic implementation was introduced.

## Expanded Windows and WSL2 acceptance

The expanded native protocol test passed on Windows (21.32 seconds) and WSL2 Ubuntu 26.04 Linux (19.49 seconds), against the same owned Docker Desktop fixture `bd052d47d134`. In addition to the initial checks, four incomplete HTTP requests occupied all active forward slots; a fifth client received no data while those slots were occupied, then received the fixture HTTP response after one client disconnected. The three remaining open client sockets closed when the five-second session expired.

A separate fixture-only identity with namespaced Pod get/create/delete permissions then deleted and recreated the exact owned Pod between prepare and execution. The new Pod had a different UID, and the old sealed approval was rejected with `target changed` before remote execution. This verifies the preflight defense; the atomic precondition gap between that final check and Kubernetes subresource connection still exists.

Gateway unit tests additionally cover all three capability indices under writable/readonly mode, missing individual flags and prohibited namespaces. Strict Clippy for the native integration target passed. A first WSL harness attempt failed before running Cargo because the inherited Windows PATH contained spaces; the corrected harness uses an explicit Linux tool PATH. No application failure was suppressed.

The earlier list of unverified concurrent-client and Windows/Linux protocol checks is superseded by this expanded evidence. Deliberate network fault injection, hostile maximum-size WebSocket traffic, and macOS native interoperability remain unverified.
