# Catppuccin Mocha feature tour

The README animation uses the published Apple Silicon build of [KBorg 0.9.0](https://github.com/c0dewars/kborg/releases/tag/v0.9.0) in an isolated pseudo-terminal with Catppuccin Mocha. It opens on startup review for synthetic EKS contexts, exits before connecting, then switches to `--demo`. It ends with `KBORG_DEMO_SCALE=20000`, which adds 20,000 synthetic Pods. `tour.json` records the binary and GIF digests, frame count, duration, and captions.

## Coverage

| Feature family | Shown in the tour | Real acceptance remains separate |
|---|---|---|
| Startup | Context search, the highlighted selection, exact EKS identifiers, and the Nodes and all-namespaces default | Connection and login helpers for a real cluster |
| Browse | Pods with the k9s-style header, a failing row in red, the keys of the view (`?`) | CRD/provider and large-cluster compatibility |
| Inspect | YAML with `/` search, drag selection and copy controls in Logs, `:lint` | Real RBAC, log reconnects, clipboard delivery and data-source failures |
| Copilot | Setup panel showing GitHub account reuse and browser sign-in | Account authorization and a reviewed request |
| Columns | Nodes with role, architecture, taints, version and CPU/memory use | Usage percentages need metrics-server on a real cluster |
| Review changes | Deployment scale review, cancelled | Apply/edit/delete, concurrency and admission tests |
| Secrets | Masked YAML only | Timed reveal and every confidential-output boundary |
| Multi-cluster | Synthetic dashboard | Separate cluster identities, failure isolation and sustained load |
| Discoverability and looks | Ctrl-P command palette, theme chooser | Accessibility and the real terminal matrix |
| Scale | First render, paging, filtering as you type and sorting with 20,000 synthetic Pods | Network and API latency on a real cluster. See the [performance gate](PERFORMANCE.md). |

The separate `connect.gif` is a 12-second recording from the earlier [release/next review](RELEASE_NEXT_REVIEW.md) build. The README tour includes the 0.9.0 startup flow.

## Reproduce

Use a KBorg 0.9.0 or later binary for Linux or macOS and Python 3 with Pillow.

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
