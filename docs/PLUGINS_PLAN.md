# Plugins and packs: plan (drafted 2026-11-16)

Goal: every k9s plugin works in KBorg unchanged, it is easy to write a new one, and people can find and install plugins from a store (`:plugins` / `kborg plugin install NAME`). This builds on the k9s model of "everything on by default, every change reviewed".

## Why `:nodeclaims` and most k9s plugins don't work today

Three things are missing. None of them is specific to Karpenter.

1. **Custom resources get Pod columns.** `:nodeclaims` opens on a Karpenter cluster, because discovery finds every CRD. The table, though, uses KBorg's generic columns (NAME, READY, STATUS, RESTARTS, AGE), and for a NodeClaim those are mostly empty. k9s shows the CRD's own `additionalPrinterColumns` (TYPE, CAPACITY, ZONE, NODE, READY). KBorg never reads them (`grep additionalPrinterColumns src` finds only the Helm CRD check). So any operator's resources (NodeClaims, NodePools, Argo Applications, Flux Kustomizations, Certificates, CNPG Clusters) look unimplemented, even though they are listed and watched. **Fixed in 0.8.0** (phase A below).
2. **The extension contract was built for a locked-down product.** `src/handoff.rs` and `src/extensions.rs` date from the opt-in security model. A plugin:
   - gets the object as JSON on stdin and a 15-second limit
   - has its output captured, never the terminal
   - has to be registered by hand with an absolute, SHA-pinned path
   - is refused or flagged when it is a `sh -c` script
   - needs `--allow-extensions --allow-tools`, which contradicts the product model (everything on).
3. **The k9s plugin schema has moved on.** `plugins::Meta` has `deny_unknown_fields` and no `override` or `inputs`. Up to 15 of the 135 community plugins (9 use `override`, 6 use `inputs`) fail to parse for that reason alone (argocd, flux, debug-container, pvc-resize, karpenter).

Counted on the 135 plugins in `derailed/k9s/plugins` (55 files, Apache-2.0):

| Shape | Plugins | Works in KBorg today |
|---|---:|---|
| `bash -c` / `sh -c` / PowerShell script | 107 | No: flagged, and values are limited to a tiny charset |
| Wants the terminal (`background: false`: `less`, `stern`, `dive`, `psql`, `watch`) | 118 | No: output is captured and cut at 15 s |
| Changes the cluster (patch, annotate, reconcile, promote, delete; keyword count, approximate) | ~49 | Only after typing `RUN DANGEROUS`, with no review of the change itself |
| Single program, no TTY, read-only | ~5 | Yes |

The plugin variables used are `$CONTEXT` (147 uses), `$NAME` (142), `$NAMESPACE` (134), `$RESOURCE_NAME`, `$POD`, `$KUBECONFIG`, `$COL-NAME`, `$INPUT_*` (from `inputs`), and a few environment variables (`$HOME`, `$XDG_CONFIG_HOME`), which KBorg refuses today.

## Design

### 1. A pack is the unit of extension

A **pack** is a folder that bundles config fragments, which KBorg layers over the user's own files:

```
karpenter/
  pack.yaml        # id, version, description, homepage, license, requires, crds
  plugins.yaml     # k9s plugin format, unchanged
  views.yaml       # k9s views format: columns for nodeclaims, nodepools
  aliases.yaml     # np -> nodepools, nc -> nodeclaims
  hotkeys.yaml
  relations.yaml   # KBorg: declarative jumps (nodeclaim -> node)
  actions.yaml     # KBorg: declarative reviewed changes (patch/annotate templates)
  skins/*.yaml     # optional themes
  README.md
```

Every file is optional, and a k9s `plugins.yaml` on its own is a valid pack. Fragments from different packs never overwrite each other or the user's files: the user's own file wins, then packs in install order, and every conflict shows in `?` and `kborg config check`, as keymap conflicts already do.

`pack.yaml`:

```yaml
id: karpenter
version: 1.2.0
description: NodePools and NodeClaims, with jumps between them and Nodes
license: Apache-2.0
source: https://github.com/derailed/k9s/tree/master/plugins/karpenter.yaml
requires: [kubectl]          # programs; checked on PATH and shown in the review
crds: [nodeclaims.karpenter.sh, nodepools.karpenter.sh]   # suggest the pack when these exist
```

### 2. Run k9s plugins as k9s does

- **Terminal handoff.** `background: false` suspends the TUI and gives the process the terminal, the same path the reviewed shell (`s`) already uses, then restores the table. `background: true` runs detached and reports its exit status on the status line. The 15-second limit and output capture apply only to the existing `:ext` read-only views.
- **Scripts allowed, without injection.** k9s substitutes `$NAME` into the script text, so a crafted object name becomes shell code. KBorg instead passes every variable as an environment variable and leaves `$NAME` in a `sh -c` script for the shell to expand. The script behaves the same, but a value is never parsed as code. Arguments of a non-shell `command` keep the current single-argv expansion. `$COL-X` (not a valid shell name) is substituted only when the value is in the safe charset.
- **Full schema:** `override`, `inputs` (a small form, typed `string`/`bool`/`int`, giving `$INPUT_NAME`), `dangerous`, `confirm`, `overwriteOutput`. Unknown fields warn instead of failing the parse.
- **The plugin's environment** is `PATH`, `HOME`, `XDG_*`, `TERM`, `LANG`, `KUBECONFIG` and the plugin variables. `$CONTEXT` and `$KUBECONFIG` always name the connection KBorg is on, so `kubectl --context $CONTEXT` hits the same cluster.
- **Confirmation follows the product model.** The first run of a plugin, and the first run after its definition or pack version changes, shows a review: pack, command, exact argv or script, programs it calls, and whether it is marked dangerous. y/Enter approves and is remembered for that content hash. `confirm: true` and `dangerous: true` review on every run. Strict mode (`strict_confirmations`) keeps typed phrases.
- **Read-only stays read-only.** A KBorg plugin cannot be checked for writes (it is a script that calls kubectl), so `--read-only`, `KBORG_READ_ONLY=1` and a read-only context config turn plugins off. Declarative actions (section 3) are refused like any other write. Operator config: `allow_plugins: false`, `allow_plugin_packs: [ids]`.
- **No flags.** Plugins are on by default. `--allow-extensions` / `--allow-tools` stay accepted and do nothing.
- **Where they are found:** `~/.config/kborg/plugins.yaml`, `plugins/*.yaml`, installed packs, and the k9s locations (`$XDG_CONFIG_HOME/k9s/plugins.yaml`, `$XDG_DATA_HOME/k9s/plugins/`), which are read so a k9s user's plugins just appear. A k9s plugin carries a "from k9s" badge and still gets its first-run review.

### 3. Declarative relations and actions (KBorg-native, cross-platform)

About half the community plugins do something KBorg already does safely: list related objects, jump to them, or patch/annotate an object. These become data in a pack, not bash:

```yaml
# relations.yaml: Enter/shortcut jumps, rendered as KBorg views (no less, no kubectl)
relations:
  - from: nodeclaims.karpenter.sh
    key: n
    description: Node backing this NodeClaim
    to: nodes
    name: .status.nodeName
  - from: nodepools.karpenter.sh
    key: Shift-N
    description: Nodes of this NodePool
    to: nodes
    selector: karpenter.sh/nodepool={{.metadata.name}}
```

```yaml
# actions.yaml: reviewed changes through KBorg's normal pipeline
actions:
  - id: argocd-refresh
    scopes: [applications.argoproj.io]
    key: r
    description: Refresh from git
    patch:
      type: merge
      body: {metadata: {annotations: {argocd.argoproj.io/refresh: normal}}}
  - id: flux-reconcile
    scopes: [kustomizations.kustomize.toolkit.fluxcd.io, helmreleases.helm.toolkit.fluxcd.io]
    key: r
    description: Reconcile now
    patch:
      type: merge
      body: {metadata: {annotations: {reconcile.fluxcd.io/requestedAt: "{{now}}"}}}
  - id: keda-pause
    toggle: {path: .metadata.annotations["autoscaling.keda.sh/paused"], on: "true", off: null}
```

Actions get everything a built-in change has: server dry-run, the diff in the review, UID/resourceVersion lock, audit, `z` undo for toggles, read-only refusal, and Windows support with no bash. The template language is limited to fields of the selected object, `{{now}}` and inputs. It has no loops and no shell.

The ported packs use relations and actions where they can and keep the original exec plugin as an alternative (`argocd-sync` via the `argocd` CLI, for example).

### 4. The store

- **Index:** a GitHub repo `c0dewars/kborg-plugins` with `packs/<id>/` and a generated `index.json` (id, version, description, crds, requires, sha256 of the pack archive, commit). The index is signed in CI with cosign keyless, and KBorg verifies it with the sigstore code it already has for Helm (`helm_sigstore.rs`).
- **CLI:**
  - `kborg plugin search [TEXT]` / `list` / `info ID`
  - `install ID[@VERSION] | GIT_URL[@REF] | PATH`
  - `update [ID]` (shows a diff of what changes, then y/Enter) / `remove ID`
  - `new ID` (scaffold) / `lint PATH` / `test PATH --demo`
- **TUI:**
  - `:plugins` has installed / available / suggested tabs, with Enter = info and `i` = install (the same review).
  - **Suggested** lists packs whose `crds` exist on the connected cluster: on a Karpenter cluster `:plugins` offers `karpenter` first, and the status line mentions it once.
- **Files:** packs go in `~/.config/kborg/packs/<id>/`, pinned in `packs.lock` (id, version, source, commit, sha256). Installs from git or a path are labeled "unverified source" in the review.
- **Offline / company use:** `KBORG_PLUGIN_INDEX=URL|PATH` points at a mirror, and operator config can allow-list pack ids or turn the store off.
- **Repo contents:**
  - **Ported packs:** all 55 k9s plugin files, with Apache-2.0 attribution in each `pack.yaml` and a NOTICE.
  - **KBorg-native packs:** karpenter, argocd, flux, cert-manager, cnpg, keda, external-secrets, crossplane, argo-rollouts, kubevirt.
  - **CI:** every pack is linted and run against the demo cluster.

### 5. Making packs easy to write

- `kborg plugin new my-pack` scaffolds a pack with one plugin, one relation and one action, plus a README.
- `kborg plugin lint` checks schema, keys (conflicts against the real keymap), scopes against discovery and template paths, the same way `kborg config check` checks config.
- `kborg plugin test PATH --demo` runs the relations and actions (dry-run) against the demo data.
- Live reload: editing a pack's files applies on save, as other config does.
- Docs: `docs/PLUGINS.md` (authoring guide with the full schema) and a skill reference `skills/kborg/references/plugins.md`, so AI assistants can write packs. Its examples are parsed by tests, like the other references.

## Phases

| Phase | Scope | Done when |
|---|---|---|
| **A. CRD columns** (**Done, 0.8.0**) | Read `additionalPrinterColumns` at discovery (one CRD list, cached with the discovery cache) and use them as the default view of a custom resource, as k9s does. `views.yaml` still wins. | `:nodeclaims` shows TYPE, CAPACITY, ZONE, NODE, READY, AGE on a Karpenter fixture; Argo/Flux/cert-manager fixtures show their columns |
| **B. k9s plugin runtime** | Full schema (`override`, `inputs`, lenient fields), terminal handoff, env-var scripts, k9s plugin locations, first-run review keyed by content hash, read-only off-switch, no flags | All 135 community plugins parse; karpenter, stern, dive, log-less, argocd and flux run unchanged on a throwaway cluster; an injection fixture (object named `$(touch /tmp/x)`, `;rm`) never executes |
| **C. Packs** | Pack folder format, layered fragments with conflict reporting, `relations.yaml`, `actions.yaml` through the change pipeline, `kborg plugin new/lint/test`, `:plugins` installed tab | Karpenter pack: `n` on a NodeClaim opens its Node with no kubectl; the argocd refresh action shows a dry-run diff and undoes |
| **D. Store** | `c0dewars/kborg-plugins` repo, signed index, install/update/remove/search, `packs.lock`, suggested packs from CRDs, mirror env var, operator allow-list | `kborg plugin install karpenter` from a clean config; a tampered index is refused; an update shows the diff |
| **E. Port** | All 55 k9s files as packs; 10 native packs with relations/actions; docs and skill reference | CI lints and demo-tests every pack |

Releases: **A** ships in 0.8.0 (read lazily, one CRD GET per resource per connection, rather than with the discovery cache). **B + C** are 0.9.0. **D + E** are 0.10.0.

## Decisions to confirm

1. **Trust on first use with y/Enter**, instead of a SHA-pinned registration. This is the product model; strict mode keeps typed phrases.
2. **Plugins are off under read-only.** A script's writes cannot be detected, so the safe answer is to not run it.
3. **Read k9s's plugin folders automatically** (with the first-run review), or only on `kborg plugin import-k9s`. Recommended: read them automatically, labeled "from k9s".
4. **Where the store lives:** a separate `c0dewars/kborg-plugins` repo (recommended: packs release on their own cadence) or `packs/` in this repo.
5. **The existing `:ext` / `kborg extensions`:** keep it as the capture-only "view" kind and fold it into packs (`kind: view`), with no breaking change.
