# Clusters, connecting and safety

## Connecting

KBorg reads kubeconfig exactly as kubectl does: `$KUBECONFIG` (several files joined with `:`, or `;` on Windows), else `~/.kube/config`, or `--kubeconfig FILE`. Exec helpers (`aws`, `gcloud`, `kubelogin`), auth providers, impersonation, `proxy-url` and `insecure-skip-tls-verify` work.

- **The connection prompt:** at start KBorg names the context it is about to connect to and asks Yes or No. Nothing connects and no credential helper runs before Yes, and No offers the other contexts.
- **Choosing:** `kborg --context NAME` starts on a context and `-n NAMESPACE` on a namespace (`all` for every namespace). In the TUI, `:ctx` picks a context, `:ctx NAME` switches to one, and `:pod @NAME` opens a resource on another context.
- **Listing:** `kborg contexts` lists the contexts without connecting, each with its KBorg settings.
- **Kubeconfig stays untouched:** KBorg never writes to it, and never changes the current context in it.

## Per-cluster settings: contexts/NAME.yaml

<!-- FILE: contexts/production.yaml -->
```yaml
version: 1
namespace: payments          # namespace on entering (all = every namespace)
view: deployments            # resource shown when switching to this context
theme: flexoki-dark          # theme while this context is selected
read_only: true              # no changes in this context
strict_confirmations: true   # a typed phrase for every change instead of y/Enter
label: PROD                  # header badge, up to 16 characters
color: danger                # a palette role or #RRGGBB
```

- **Opening it:** `:ctx-config` in the TUI opens the current context's file in `$VISUAL` or `$EDITOR` (with a commented template when none exists) and applies it on save. Outside KBorg, a saved change applies within a second.
- **File name:** the kubeconfig context name, with every character other than letters, digits, `-` and `_` written as `%XX`. For example `arn:aws:eks:eu-west-1:123:cluster/prod` becomes `arn%3Aaws%3Aeks%3Aeu-west-1%3A123%3Acluster%2Fprod.yaml`. Prefer `:ctx-config`, which always picks the right name.
- **Only tightens:** `read_only` and `strict_confirmations` also apply to one-shot CLI commands against that context. Nothing in the file can turn on what `--read-only` or an operator config turned off.
- **Trust:** the file must be a regular file that group and others cannot write. A file that fails validation or that check makes its context read-only and shows a red `CTX CONFIG?` badge until it is fixed. `kborg config check` finds the problem.

A common setup:
- **Production:** `label: PROD`, `color: danger`, `strict_confirmations: true`, and possibly `read_only: true`.
- **Staging:** `label: STAGING`, `color: warning`.
- **Development:** a lighter theme, so it never looks like production.

## How changes are protected

Every change shows a review first (see using-the-tui.md). On top of that:
- **`--read-only`** browses with every change turned off.
- **Typed confirmations:** `strict_confirmations: true` in `contexts/NAME.yaml` or the operator config requires a typed phrase for each change.
- **Dry runs:** supported writes are dry-run on the server and locked to the reviewed UID and resourceVersion. If the object changed in the meantime, nothing is written.
- **Audit:** every write leaves a local intent record, and `kborg audit pending` lists outcomes KBorg could not confirm. KBorg never retries a write by itself.

## Operator config: restrictions for a team

A strict JSON file passed with `kborg --config FILE`. It can only take capabilities away, never add them:
- **Write one:** `kborg config init FILE` writes every field with its default.
- **Check one:** `kborg config validate FILE`.

<!-- FILE: kborg-team.json -->
```json
{
  "schema_version": 1,
  "profile": "operator",
  "strict_confirmations": true,
  "allow_exec": false,
  "allow_attach": false,
  "allow_node_shell": false,
  "allow_secret_reveal": false,
  "allow_secret_edit": false,
  "allow_force_kill": false,
  "allow_system_namespaces": false
}
```

The fields:
- **Profile:** `"profile": "read-only"` turns every change off.
- **Capabilities:** each `allow_*` turns one off. These are `logs`, `helm`, `helm_write`, `helm_hooks`, `helm_crds`, `exec`, `attach`, `forward`, `file_copy`, `tools`, `extensions`, `auth_helper`, `secret_reveal`, `secret_edit`, `force_kill`, `system_namespaces`, `cluster_scoped`, `yaml_edit`, `node_maintenance`, `external_editor`, `apply`, `node_shell`, `copilot_screen`, `debug` and `scan`.
- **Defaults:** `context`, `namespace`, `theme`, `no_splash` and `poll`.
- **Audit:** `audit_file`, `audit_file_max_bytes`, `audit_syslog` and `audit_required` (a hash-chained audit log that `kborg audit verify FILE` checks).

A refusal names what switched the feature off. A team usually ships the file with a wrapper script or a shell alias, `alias kborg='kborg --config /etc/kborg/team.json'`.
