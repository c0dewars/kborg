# Catppuccin Mocha feature tour

The README animation is recorded from the real KBorg binary in an isolated pseudo-terminal, using `--demo --theme catppuccin-mocha`. Cluster data is synthetic. The recording does not contact Kubernetes, run shell/scan/Helm mutations, reveal a Secret, or send a Copilot request. Operation screens show reviews or available actions, not proof of real-cluster execution.

The current main tour is 54 seconds (96 captured frames, 2.51 MiB), recorded on 2026-11-13 from the 0.5.0 macOS ARM64 release build; `tour.json` pins the binary's SHA-256. It has two parts: a feature walk on the small demo data set, then the same binary started with `KBORG_DEMO_SCALE=20000`, which adds 20,000 synthetic Pods. The startup tour (`connect.gif`) is 12 seconds and was recorded earlier from the [release/next review](RELEASE_NEXT_REVIEW.md) build.

## Coverage

| Feature family | Shown in the tour | Real acceptance remains separate |
|---|---|---|
| Browse | Pods with the k9s-style header, a failing row in red, the keys of the view (`?`) | CRD/provider and large-cluster compatibility |
| Inspect | YAML with `/` search and highlighted matches, logs, `:lint` | Real RBAC, log reconnects and data-source failures |
| Columns | Nodes with role, architecture, taints, version and CPU/memory use | Usage percentages need metrics-server on a real cluster |
| Review changes | Deployment scale review, cancelled | Apply/edit/delete, concurrency and admission tests |
| Secrets | Masked YAML only | Timed reveal and every confidential-output boundary |
| Multi-cluster | Synthetic dashboard | Separate cluster identities, failure isolation and sustained load |
| Discoverability and looks | Ctrl-P command palette, theme chooser | Accessibility and the real terminal matrix |
| Scale | 20,000 synthetic Pods: first screen, paging, filter as you type, sort | The numbers behind the captions come from the [performance gate](PERFORMANCE.md) on the same binary: first data 0.36 s, about 8 ms per key, filter 0.4 s. A live cluster adds network and API time |

Helm, RBAC subjects, relationships, charts, shells, forwards, scans and Copilot are not in this shorter tour; see the [README](../README.md) and the guides it links.

The separate `connect.gif` records the real startup confirmation with a synthetic local kubeconfig. It demonstrates No, search and selected-context review, then exits without connecting.

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
