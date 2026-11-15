# Install, upgrade and troubleshoot

## Install

| Platform | Command |
|---|---|
| macOS, Linux (Homebrew) | `brew install c0dewars/tap/kborg` |
| Debian, Ubuntu | download the `.deb` from the latest GitHub release, then `sudo apt install ./kborg_VERSION_amd64.deb` (or `arm64`) |
| Fedora, RHEL | `sudo dnf install ./kborg-VERSION-1.x86_64.rpm` (or `aarch64`) |
| Windows | download the ZIP for the CPU, extract it, run `kborg.exe` in Windows Terminal or PowerShell |
| Any, from source | `cargo install --locked --git https://github.com/c0dewars/kborg --tag vVERSION` |

Releases are at https://github.com/c0dewars/kborg/releases. Every file is signed, and the README explains `gh attestation verify` and `cosign verify-blob`.

## Upgrade

`brew upgrade kborg`, or install the new package over the old one. After upgrading:
- **Check the version:** `kborg --version`. If it still shows the old one, an older copy earlier in PATH is shadowing it. `which -a kborg` lists every copy; remove the stale one.
- **Refresh the assistant skill:** run `kborg skill install` again.
- **Read the release notes:** they are on the release page and in `docs/RELEASE_NOTES.md`. New default keys never take a key the person bound in `keymap.yaml`.

## First run

- **No cluster needed:** `kborg demo` opens sample data.
- **With a cluster:** `kborg` (or `kborg --context NAME`) names the context and asks before connecting.
- **Learning it:** `?` lists the keys of the current view, `:keymap` lists every key, and Ctrl-P opens the palette. The first start also asks for a theme.

## When something is wrong

Start with these two; both explain each problem and the next step:
- `kborg doctor`: terminal, tools (helm, scanners, Copilot), kubeconfig, authentication and cluster access.
- `kborg config check`: every config file, with the file and line of each error.

KBorg's errors end with `Next:`, the step to take.

| Symptom | Likely cause and fix |
|---|---|
| A config change has no effect | The file is invalid, so KBorg kept the last good settings (the status line says so). Run `kborg config check`. Live reload can also be off: `reload: false` in `ui.yaml`, or `KBORG_CONFIG_RELOAD=off` |
| A key does nothing | It is remapped or belongs to another view: `kborg keymap --json`. Hotkeys work only in the resource table |
| `CTX CONFIG?` badge, context read-only | `contexts/NAME.yaml` is invalid or writable by others: `kborg config check`, then `chmod go-w FILE` |
| `READ ONLY` | `--read-only`, the context's `read_only`, or an operator config is in effect; the refusal message names which |
| `STALE` that stays | The cluster is unreachable; KBorg reconnects by itself. `kborg doctor` checks access |
| `TRUNCATED` | More than 10,000 objects; narrow it with a namespace (`:pod NAMESPACE`) or a label selector |
| Garbled boxes or `?` instead of letters | The terminal is not UTF-8. Set `LANG=en_US.UTF-8`, or use `text: ascii` and `borders: ascii` in `ui.yaml` |
| Dull or wrong colors | The terminal lacks truecolor. Set `COLORTERM=truecolor` if it supports it, or pick a theme that suits 256 colors; `theme: terminal` uses the terminal's palette |
| Exec login helper fails (aws, gcloud, kubelogin) | It must be on the PATH of the shell that starts KBorg. Run `kborg doctor` with the same `--context` |
| 403 errors | Missing RBAC. The message names the verb and resource and the `kubectl auth can-i` command to confirm it |
| Helm install says it needs a static token | The `helm` binary is given the context's credentials, so Helm installs need a token or client-certificate context |
| Copilot answers missing | `kborg copilot status`; answers inside KBorg work on Apple Silicon Macs only |

## Resetting

- **One file:** delete it, and KBorg uses its built-in defaults. `kborg theme reset` restores the default theme.
- **Everything:** move the config folder away. Saved selections (`selections/v1.json`), the last dashboard (`last-dashboard.json`) and custom themes live there too.
- **Never stored there:** cluster data and credentials.
