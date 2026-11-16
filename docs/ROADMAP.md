# KBorg roadmap

Reviewed 2026-11-16 against [KBorg 0.8.3](https://github.com/c0dewars/kborg/releases/tag/v0.8.3), merged main, and the local Copilot chat work. The [feature and acceptance register](REMAINING_WORK.md) records supported workflows and their evidence. The priorities below describe proposed work and how to verify it.

## Where KBorg stands

- **Released in 0.8.3 or earlier.** Core Kubernetes and Helm workflows, reviewed cluster-scoped apply, reversible changes through `z`, CRD printer columns, paging beyond 10,000 objects, discovery caching, dashboard history, live config reload, the first-run tour, and `kborg doctor` are available. The accessibility mode, color-blind-safe themes, and k9s skin import are implemented. Real terminal, screen-reader, and downloaded-skin checks are still open.
- **Merged for the next release.** Drag to highlight text in Logs and press `c` to copy it. The six-platform CI passed on the merged tree. Native clipboard behavior still needs checks in real desktop sessions. [Log clipboard PR](https://github.com/c0dewars/kborg/pull/47)
- **In local development.** The Copilot chat shows the captured source, wraps and searches answers, and keeps follow-ups in one conversation. Logs highlights can scope a question to the selected text. EKS headers show the readable cluster name and `:identity` opens the full identifiers. Offline tests cover resizing, returning to the originating view, and reviewing each follow-up. Real Ghostty and live Copilot checks are open for this UI.
- **Distribution.** The 0.8.3 release has Sigstore signatures, checksums, GitHub build attestations, archives for six native targets, and `.deb` and `.rpm` packages. The Homebrew tap builds from the tagged source. Apple Developer ID signing and notarization have a workflow path and still need first-run acceptance. [Release process](RELEASE_PROCESS.md)

## Next release priorities

| Priority | Work | Done when |
| --- | --- | --- |
| **Readable connection identity** | Keep the active name and scope easy to scan at narrow and wide sizes. Use `:identity` for exact values. | Long EKS names remain recognizable and region/account details stay accessible while opening views and reviewing changes. |
| **Faster startup** | Open a standard cluster session in Nodes with all namespaces. Make context search and the focused row easy to see before connection. | A pseudoterminal test verifies the launch scope, explicit overrides, selection highlight, and the approval before credentials run. |
| **Copilot onboarding** | Offer the existing GitHub account first and present browser sign-in when needed. | Local fixtures cover account selection, cancellation, and saving the connection. Live acceptance covers browser sign-in and the first reviewed question with a real Copilot account. |
| **Core terminal workflows** | Test the review, Logs selection and clipboard, shell return, and Copilot chat on real terminals. Start with Ghostty on macOS, a Linux terminal through tmux/SSH, and Windows Terminal. Include wrapped Unicode, narrow resizing, and split or modified mouse reports. | The same short acceptance script is recorded against the release commit. The intended text reaches the clipboard, the correct view returns after a shell or chat, and mouse input never synthesizes an approval. |

For 0.9.0, prioritize a readable connection header, a clear log-selection workflow, and a chat that returns to its source. Validate those flows in real terminals and run the existing performance gate on the candidate.

## Follow-on capabilities

| Work | Done when |
| --- | --- |
| **Favorite contexts and environment labels** | Operators can pin their most-used contexts and recognize production in the startup picker. Connection review still shows the exact context and API host. |
| **k9s plugin compatibility (revisit later in November)** | Extend the current importer/runner to cover `override`, `inputs`, and terminal-interactive plugins. Decide trust, read-only behavior, discovery locations, and how the existing extension runner joins packs before expanding automatic discovery or execution. | Representative k9s plugins import and run from a disposable fixture, including input and terminal plugins. Tests show exact invocation, return to a clean TUI, read-only handling, and resistance to shell injection from Kubernetes names. [Plugin phases and decisions](PLUGINS_PLAN.md#phases) |
| **Declarative packs and a plugin store** | A local pack can add a relationship view and a reviewed action with conflict reporting. A later signed index supports install, update, and removal. A tampered index or pack is refused, and updates show what will change. [Plugin plan](PLUGINS_PLAN.md) |
| **Reviewed apply with prune** | The review lists the exact objects to delete, the selection scope, and exclusions. A disposable-cluster test verifies identity checks, RBAC and admission denial, and reporting of partial failure. |
| **Broader distribution** | Add native installers or package-manager channels based on demand. A clean-machine installation verifies the released artifact's digest and provenance. Check Apple Developer ID signing and notarization with real release credentials when available. |
| **Copilot platform coverage** | Treat Linux containment and enterprise-provider support as separate work. Linux must pass isolation canaries and produce a live advisory answer on x86_64 and ARM64. Enterprise validation requires authorized GHE.com and GHES fixtures. |

## Production acceptance

| Work | Done when |
| --- | --- |
| **Accessibility and terminal coverage** | VoiceOver and Orca read the selected table row, current context, and review in order. Expand terminal tests across common fonts, SSH, multiplexers, and color modes after the focused release pass. Verify a representative set of real k9s skins and the keyboard-only path. [Terminal evidence](MAC_V1_TERMINALS.md) |
| **Windows file trust** | Tests verify owner-only ACLs on configuration, saved selections, audit files, and exports and exercise handle-based path replacement protection. |
| **Scale and recovery** | Run a 24-hour soak with less than 5% RSS growth, repeat reconnect and slow-link checks, and time discovery on a 300-CRD cluster. Track p95/p99 key-to-frame latency in the existing manual performance gate. Use profiling to decide whether frame diffing is needed. [Performance](PERFORMANCE.md) |
| **Supply chain and operational evidence** | Record longer fuzz campaigns, the first OpenSSF Scorecard run, and source-specific platform and authorization checks. Run an organizational pilot covering least-privilege use, interrupted writes, audit review, upgrades, and recovery. [Acceptance register](REMAINING_WORK.md#security-and-operational-acceptance) |
