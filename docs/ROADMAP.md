# KBorg roadmap

This plan follows 0.5.0-rc.3. Each item has a measurable acceptance bar ("done when"). Items are ordered by impact within each area. [REMAINING_WORK.md](REMAINING_WORK.md) stays the register of what exists today. This file is what we build next.

Updated 2026-10-26: P1, S2, F1, F2, U1 and X1 are implemented; the CI performance gate exists and runs on manual dispatch. Use each row's completion evidence and the current register as the release checklist. New context consent and guided Copilot setup are described in OPERATIONS.md and COPILOT_INTEGRATION.md. Automatic GitHub Actions remains off; a first signed release and longer external acceptance are separate gates.

## Where we stand

| Area | Current scope | Remaining acceptance or work |
|---|---|---|
| Performance | Recorded comparisons, bookmark/gzip traffic improvements and a manually dispatched regression gate | Current-source p95/p99 frame latency, long soak/fault evidence and browsing beyond the 10,000-object cap |
| Security | Operator default with review; company capability restrictions/read-only and strict confirmations; dry-run where supported; macOS ARM64 contained Copilot | First signed release pending; fuzz targets exist but longer campaigns remain; Copilot containment is macOS ARM64 only; no external audit; Windows file ACLs |
| Features | Broad k9s-style workflows with documented limits (K9S_COMPATIBILITY.md), dashboard, charts and platform-limited Copilot | Full-screen shells and plugin variable substitution implemented; cluster-scoped apply and per-context policy remain; enterprise Copilot unvalidated |
| UI | 20 themes, ui.yaml, charts, keymap everywhere | Unicode width implemented with ASCII fallback; no k9s skin import; no automated screenshot matrix for real terminals |
| UX | `?` help per screen, typed command grammar, action menu | Fuzzy palette and actionable errors implemented; first-run tips and automatic config reload remain |

## 1. Performance

| # | Change | Done when |
|---|---|---|
| P1 | **Watch traffic diet**: request `allowWatchBookmarks`, resume from the last bookmark on reconnect instead of relisting, and negotiate gzip on LIST pages | **Done.** Watches renew every 55 s (was 8 s) with bookmarks and keep their cache across disconnects, resuming at the last resourceVersion; only 410 Gone relists. GET reads negotiate gzip, bounded like raw bodies. 5,000-ConfigMap churn: 36.4 KiB/min vs k9s 36.7 (was 101 vs 56); 0 relists after a 30 s outage; a 5,000-object LIST is 195 KiB instead of 1.8 MiB |
| P2 | **Server-side tables** (`Accept: application/json;as=Table`) for resources without custom columns, so the client stops downloading full objects it only shows five columns of | Memory at 5,000 ConfigMaps ≤ 40 MiB (from 71); first page ≤ 0.1 s **Done (as PartialObjectMetadata, not Table).** Kinds whose columns read only metadata (ConfigMaps, ServiceAccounts, RBAC, Endpoints/EndpointSlices, Leases and similar) LIST and WATCH as `PartialObjectMetadata` and keep only identity, timestamps and labels; Pods, workloads, CRDs and any resource with a `views.yaml` column that reads more stay on full objects. Rows are shared between the watch cache and the table, and each initial-list page shows as it arrives. 5,000 ConfigMaps: 33 MiB (was 67 on the same data), first page 0.05 s after Enter; with 2 KiB ConfigMaps, churn 34 KiB/min vs k9s 212 (full objects: 162). Table rows measured larger on the wire than full objects for ConfigMaps (they carry the same metadata plus cells), so they were not used |
| P3 | **Virtual table above the cap**: page through >10,000 objects with continue tokens on demand instead of showing only the first 10,000 | 50,000 Pods browsable, sort and filter by server label/field selectors; banner explains what is client- vs server-side |
| P4 | **Frame budget**: diff-render only changed cells, coalesce watch bursts to one frame per 16 ms, measure p50/p95/p99 key-to-frame latency | p99 key-to-frame < 16 ms at 5,000 rows; idle CPU < 0.3 % |
| P5 | **Performance gate in CI**: the bench job compares against a stored baseline (demo scale 5k/20k load, filter, sort; binary size; RSS) and fails on > 15 % regression | Every PR shows the delta table; a deliberate regression fails CI |
| P6 | **Discovery cache on disk** (private, per server+identity hash, 10 min TTL, revalidated in background) | Cold start to first data on a 300-CRD cluster ≤ 0.15 s |
| P7 | **Long soak and fault runs** (24 h idle, reconnect storms, slow links via a throttling proxy) published like PERFORMANCE_COMPARISON.md | RSS growth < 5 % over 24 h; no stuck views after 100 forced disconnects |

## 2. Security

| # | Change | Done when |
|---|---|---|
| S1 | **Signed, attested releases**: macOS notarization, Sigstore/cosign signatures, SLSA build provenance from GitHub Actions, Homebrew formula pinned to the attested artifact | `gh attestation verify` and `cosign verify-blob` pass for every release asset; README shows how to verify **Done (workflow; first signed run pending)**: manual `.github/workflows/release.yml` builds all six archives plus .deb/.rpm, writes SHA256SUMS, signs every asset with keyless cosign, attests build provenance and creates a draft release; it verifies each bundle itself. macOS codesign + notarization run when the five Apple secrets exist. README and RELEASE_PROCESS.md show how to verify. Homebrew still builds from the source tarball (pinning to attested binaries moves to F7) |
| S2 | **Fuzzing** with cargo-fuzz for the parsers that see untrusted data: kubeconfig, manifests/YAML, tar extraction, terminal sanitizer, k9s views/hotkeys/plugins, PEM/DER walker, Helm storage decoding | **Done.** 12 invariant-checking targets in `src/fuzzing.rs` (also k9s aliases, keymap/ui.yaml, masked Secrets), run on every `cargo test` over the seed corpus `fuzz/seeds/` plus mutations, and coverage-guided via `fuzz/` (cargo-fuzz) and the manual Fuzz workflow (1 CPU-hour per target by default). Local nightly runs: 6.5 min per target, 0 crashes; fixes in import-k9s shell quoting, kubeconfig multi-document duplicates, apply namespace handling and `safe_lines`. A full 1 CPU-hour run per target is left to the workflow |
| S3 | **Copilot containment on Linux** (Landlock + seccomp + network namespace with the same broker) so the feature works where most operators run | Same canary suite as macOS passes on Linux x86_64/ARM64; live answer through the broker |
| S4 | **Enterprise Copilot validation**: run the live test against a real GHE.com tenant and a GHES 3.22 instance | Evidence JSON for both, docs no longer say "not demonstrated" |
| S5 | **Screen-capture hygiene for `:ask`** (done in rc3): a revealed Secret value is hidden before any capture, and each capture is used once | Tests cover both; extend the same rule to any future view that shows a revealed value |
| S6 | **Windows file trust**: owner-only ACLs on config, saved selections, audit and exports; handle-based checks against swap races | Windows CI test asserts ACLs; S01 closed in REMAINING_WORK.md |
| S7 | **Audit sink**: optional JSON-lines audit to syslog/journald or a file with hash chaining, so companies can ship write actions to their SIEM | Every write family emits one record with intent, approval hash and outcome; tamper test detects a removed line |
| S8 | **Independent review**: commission an external audit of the write path, Copilot containment and terminal sanitizer; publish the report and fixes | Report linked from SECURITY.md |
| S9 | **Supply chain gates**: `cargo deny` (licenses, advisories, duplicate crates, banned sources) and `cargo vet` in CI; OpenSSF Scorecard | Scorecard ≥ 8; CI fails on a new advisory **Partly done**: `deny.toml` (advisories, Apache-2.0-compatible licenses, duplicate crates as warnings, crates.io only) passes locally; `supply-chain` CI job runs pinned cargo-deny 0.20.2; manual OpenSSF Scorecard workflow. Open: a first Scorecard score (needs a public repo and a dispatched run) and `cargo vet` (deferred, see RELEASE_PROCESS.md) |

## 3. Features

| # | Change | Done when |
|---|---|---|
| F1 | **Full interactive shell**: raw-mode passthrough so vim, top and less work inside `s` (k9s users expect it), with the same opt-in and audit | vim edits and saves a file in a BusyBox/Debian Pod; resize and Ctrl-] work **Done.** Shell, attach, node shell and debug sessions are a raw TTY passthrough like `kubectl exec -it`: every input byte and unfiltered output, SIGWINCH resizes, Ctrl-] (or `--session-escape KEY`, also detected in kitty/modifyOtherKeys encodings) disconnects. On return a reset sequence clears alternate screen, mouse, paste, keypad, cursor, scroll region, charset, palette and title before KBorg redraws (also from the panic hook). Same review, UID recheck, audit and operator-config gates. Live on Docker Desktop (`scripts/test-live-shell-ui.py`): BusyBox vi edits with arrow keys and saves, top draws and quits, `stty size` follows a resize, Ctrl-] mid-OSC with hostile modes returns to a clean screen |
| F2 | **Done.** **k9s plugins with variables** (`$NAMESPACE`, `$NAME`, `$CONTEXT`, `$COL-…`) passed as argv only, never through a shell, with the existing review. Delivered: `register-k9s`, per-argv expansion with injection refusals, shortCut bindings in table/containers/Helm views, `?` help and `:actions` entries, `RUN DANGEROUS NAME` with the exact argv in the review | `kborg extensions import-k9s` accepts the common community plugins (stern, dive, debug, logs) unchanged. stern, dive, log-full and helm-values import unchanged (fixture test); `bash -c` plugins (debug-container, watch-events, blame) stay flagged for review and point to `:debug`/`:events` |
| F3 | **Per-context configuration** like k9s (`clusters/CONTEXT/config.yaml`: namespace, view, theme, read-only) | Switching context switches theme and default view; a production context can be forced read-only **Done.** `contexts/NAME.yaml` (namespace, view, theme, read_only, strict_confirmations, header badge label/color), applied before every connect and re-checked against the context the gateway selected; restrictions only tighten; an untrusted or invalid file makes the context read-only with a `CTX CONFIG?` badge; live reload and `:ctx-config` editing. TUI tests cover the theme/view switch and forced read-only |
| F4 | **Copilot follow-ups**: multi-turn `:ask` within one review session, ask about selected log lines, and an "explain this error" action on warnings | Second question reuses the reviewed context with a fresh confirmation; works from logs and dashboard |
| F5 | **Cluster-scoped and prune apply** (Namespaces, ClusterRoles, CRDs) behind a separate flag with the same field-level review | Live apply/prune runs recorded in K9S_ACCEPTANCE.md |
| F6 | **Dashboard history**: optional local, bounded time series (7 days) so trend charts survive restarts | `G` shows yesterday's restarts after reopening |
| F7 | **Distribution**: Homebrew tap, `cargo binstall`, winget, Scoop, .deb/.rpm and a Nix flake, all from the attested artifacts (S1) | Install one-liners in README for each platform |

## 4. UI

| # | Change | Done when |
|---|---|---|
| U1 | **Done.** **Full Unicode**: grapheme-aware width (East Asian wide, emoji, combining marks) in tables, logs and YAML instead of `?`, keeping the sanitizer's control-character rules | Object names in Japanese and emoji render aligned in a PTY test; ASCII fallback still available. Delivered: `src/text.rs` (grapheme clusters, cell widths, fit/pad/wrap/pan that never split a cluster) used by tables, logs, YAML/describe, the dashboard, help and the pager; `ui.yaml` `text: auto\|unicode\|ascii`; review screens keep `\u{...}` escaping; screen tests with Japanese, emoji, ZWJ family, flags, combining accents and RTL names plus a live Docker Desktop PTY check |
| U2 | **k9s skin import**: `kborg themes import-k9s skin.yaml` converts colors to a KBorg theme | The 10 most popular k9s skins convert and pass the contrast check |
| U3 | **Real-terminal screenshot matrix** in CI: Ghostty, kitty, WezTerm, iTerm2, Terminal.app, Alacritty, Windows Terminal, tmux, screen, over SSH | Golden screenshots per terminal; a rendering regression fails CI |
| U4 | **Layout polish**: breadcrumbs (context › namespace › kind › object), sticky column headers on scroll, column auto-width with ellipsis in the middle for long names | Design review screenshots in docs/screenshots; no truncated identifiers in the default 120×32 view |
| U5 | **Live config reload**: watch theme, ui.yaml, keymap.yaml, views.yaml and apply on save with an error toast for invalid files. **Done** (0.6): `src/config_watch.rs` stats ui.yaml, appearance.json, the active custom theme file, keymap/hotkeys/aliases/views.yaml and the plugin registry every 500 ms on the loop tick and applies a file after 200 ms of stability (rename-over and truncate+write saves apply once). Each reload repeats the trust checks (no symlink, regular file, size bound, not group/world-writable, owner) and the strict startup loader; failures keep the last good config and show file, line/column and `Next:` on the status line. Key changes wait until any dialog or typed approval closes; views.yaml redraws the table; theme changes apply at once. Opt out with ui.yaml `reload: false` or `KBORG_CONFIG_RELOAD=off`; `:ui reload` unchanged. PTY check: `scripts/test-live-config-reload.py` | Editing ui.yaml updates the screen within 1 s without `:ui reload` |

## 5. UX

| # | Change | Done when |
|---|---|---|
| X1 | **Command palette** (Ctrl-P): fuzzy search over every action, command, resource and recent object, showing its key, so nothing has to be memorized | **Done**: Ctrl-P (remappable `palette`, global scope) opens over every browsing view with that view's actions and effective keys, `:` commands, resource kinds and aliases, recent contexts/namespaces and a 50-entry MRU of opened objects; Enter runs through the key/command path, OFF actions are refused with their reason; a test finds every action in its view by name, and typing `scale` ranks it first. The < 10 s usability test with new users is still to run |
| X2 | **First-run tour**: three skippable tips (`:` commands, `?` help, Ctrl-P palette) and a `kborg doctor` that checks kubeconfig, metrics-server, terminal colors and fonts | `kborg doctor` reports actionable fixes for each failure |
| X3 | **Actionable errors everywhere**: every error says what failed, why (RBAC verb/resource, network, TLS), and the next step. **Done** (0.5.0): one classifier (`src/errors.rs`) renders Kubernetes and transport failures as "what failed (HTTP code): why. Next: step" for the TUI status line, watches, logs, Helm, metrics, node operations and CLI stderr. 403s name verb, resource, group and namespace with the matching `kubectl auth can-i` command; TLS, DNS, refused and timeout errors name the server host; conflicts, admission webhooks/policies, Pod Security, quotas and validation errors each get their own step; admission/quota text is sanitized, redacted and bounded. Kubeconfig and connection errors carry a next step. The status bar keeps the next step visible on narrow terminals | Lint test: no error string without a next step; RBAC errors name the missing verb/resource |
| X4 | **Undo-friendly writes**: after scale/restart/edit, show a one-key "revert to previous" that prepares the inverse change for the same review | Scale 3→5, press `u`, review shows 5→3 |
| X5 | **Accessibility**: screen-reader friendly mode (linear output, no box drawing), high-contrast and color-blind-safe themes, reduced motion | VoiceOver and Orca read table rows in order; themes pass WCAG AAA for text |

## Order of work

1. **Now (0.5.0 final)**: P1, P5, X3. These are small, high value or close an existing gap.
2. **0.6**: P2, P4, S1, S2, S9, F1, F2, U1, X1, U5.
3. **0.7**: S3, S4, S6, S7, F3, F4, P3, P6, U2, U3, X2, X4.
4. **1.0**: S8 external audit, P7 soak, F5, F6, F7, U4, X5. A production pilot is the release gate.

Items in the same step are independent and can be built in parallel, one branch each.
