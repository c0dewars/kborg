# Read workflows: logs and YAML

KBorg supports log snapshots and bounded follow sessions only after `--allow-logs`.
Logs can contain application secrets. Sanitization removes terminal control characters;
it is not content redaction. Container selection accepts a Kubernetes container name.
The pod container list includes normal, init, and ephemeral containers.

`Gateway::follow_logs(namespace, pod, container, previous)` opens a direct authenticated
Kubernetes log request. The caller pulls sanitized lines using `LogFeed::next()`.
Dropping the feed cancels it without a background producer. Each session stops after
60 seconds, 10 seconds idle, 256 KiB received, or a line exceeding 4096 bytes. It has no
automatic reconnect, so resumed sessions cannot silently duplicate or omit logs.
The caller should display the terminal error and let the user explicitly start again.
Previous-container logs use snapshot mode; previous and follow together are rejected.
The transport retains existing certificate verification, deadlines, and response limits.

`inspect::yaml(value)` provides readable YAML 1.2 output with every key and string
quoted using JSON escapes. This avoids implicit boolean/date/null interpretation and
requires no new serialization dependency. Data is redacted before rendering. The
renderer rejects nesting above 64 levels, more than 100000 values, or input/output
above 8 MiB; terminal controls and bidirectional formatting characters are escaped.
This is an inspection/export format, not an editable input parser or full YAML roundtrip.

Verification includes line boundaries, control bytes, invalid/partial UTF-8 conversion,
line and aggregate limits, EOF, ambiguous scalar quoting, redaction, and depth limits.
Live Kubernetes acceptance and TUI command integration belong to the release gate.

## TUI integration and acceptance (2026-10-10)

- `y`: bounded redacted YAML inspection; `a`: discoverable action menu.
- `l` / `L` / `f`: choose a normal/init/ephemeral container, then snapshot,
  previous snapshot, or bounded follow. `--allow-logs` is required before the chooser.
- The live log modal has one shared bounded mailbox, never an unbounded per-line queue.
  Esc, scope changes, and worker destruction invalidate the generation and abort the
  producer. Late messages from canceled sessions cannot repopulate the display.
- `e`: deployment container/image form captures the selected name and UID before typing;
  admission dry-run and typed target approval remain in the gateway workflow.
- `o`: explicitly enter up to four comma-separated contexts for independent read-only
  snapshots. The CLI supports larger explicitly named context sets.

Focused tests pass: 4 log feed tests, 3 YAML renderer tests, and 12 TUI reducer/layout
checks, including bounded mailbox growth, stale producer rejection, Esc/scope
cancellation, read-only image form denial, and captured name/UID identity.

A native Windows pseudo-terminal session exercised YAML, container selection, demo
follow with explicit EOF, Esc, context overview, and the action picker. It exited with
code 0 and restored terminal state. This was a developer acceptance run, not an
independent usability study or a real macOS terminal test. The overview initially
lost tab separators through terminal sanitization; display separators were changed
to visible bars, and detail views now wrap to the terminal width. The app footer was
updated to reflect the expanded feature set.
