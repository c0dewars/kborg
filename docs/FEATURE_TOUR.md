# Catppuccin Mocha feature tour

The README animation is recorded from the real KBorg binary in an isolated pseudo-terminal, using `--demo --theme catppuccin-mocha`. Cluster data is synthetic. The recording does not contact Kubernetes, run shell/scan/Helm mutations, reveal a Secret, or send a Copilot request. Operation screens show reviews or available actions, not proof of real-cluster execution.

The current main tour is 75 seconds (141 captured frames, 2.86 MiB); the startup tour is 12 seconds (6 frames, 0.09 MiB). Both manifests pin the same Linux binary used for the [release/next review](RELEASE_NEXT_REVIEW.md).

## Coverage

| Feature family | Shown in the tour | Real acceptance remains separate |
|---|---|---|
| Browse and search | Pods, workload commands, filter, errors-only, sorting | CRD/provider and large-cluster compatibility |
| Inspect and diagnose | Describe, logs, lint, relationship tree, RBAC subjects | Real RBAC, log reconnects and data-source failures |
| Metrics | Pod CPU/memory charts and dashboard trends | Metrics-server availability and live quantity validation |
| Review changes | Deployment scale preview, cancelled | Apply/edit/delete, concurrency and admission tests |
| Secrets | Masked YAML only | Timed reveal and every confidential-output boundary |
| Multi-cluster | Synthetic dashboard and trend chart | Separate cluster identities, failure isolation and sustained load |
| Helm | Synthetic release browser | Native tool trust, chart verification and real lifecycle checks |
| Node operations | Node view and its contextual action/help surface | Cordon/drain/node shell must run only on an owned test cluster |
| Shell, forward, copy, debug, scans, extensions | Discoverability through command palette/help | Actual sessions/tools are not faked inside the offline GIF |
| Copilot | Visible setup/platform guidance | Supported runtime, entitlement, reviewed transfer and real provider response |
| Customization | Mocha theme chooser, contextual help, keymap | Accessibility and real terminal matrix |

The separate `connect.gif` records the real startup confirmation with a synthetic local kubeconfig. It demonstrates No, search and selected-context review, then exits without connecting. It is separate from the offline cluster-data tour. See [Operations](OPERATIONS.md) for the `orbit-use`/existing-kubeconfig flow and [Testing](TESTING.md) for its negative-path checks.

## Reproduce

Use a freshly built Linux/macOS binary of the source being documented and Python 3 with Pillow. The application itself has no Python dependency.

```sh
cargo build --release --locked
python3 scripts/make-demo-gif.py --tour --binary target/release/kborg \
  --out docs/screenshots/tour.gif --frames /tmp/kborg-tour-frames
python3 scripts/make-demo-gif.py --connection-tour --binary target/release/kborg \
  --out docs/screenshots/connect.gif --frames /tmp/kborg-connect-frames
```

The recorder uses temporary HOME/config/state directories, answers terminal queries using Mocha's colors and restores/terminates its child process. Each expected scene must appear or recording fails. On Linux it uses DejaVu Sans Mono or Liberation Mono; macOS uses Menlo. The caption bar labels demo frames. `tour.json` records the exact binary and GIF SHA-256, frame count, duration and scene captions. This is recording provenance, not a signed release attestation.

Do not pass `--copilot-policy` when reproducing the offline public tour: that optional flag performs a separately authorized real provider request with the synthetic screen. Never record a production cluster, kubeconfig, token, real account/login screen or revealed Secret for public documentation.

The animation demonstrates feature families; an exhaustive real execution of every CLI variant would be misleading and unreadable in one GIF. Use the [feature inventory](IMPLEMENTATION_STATUS.md), [compatibility map](K9S_COMPATIBILITY.md) and source-specific [acceptance register](REMAINING_WORK.md) for complete workflow coverage.
