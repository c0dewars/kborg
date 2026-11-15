# AI assistants and KBorg: security and privacy

AI assistants (GitHub Copilot, Claude, Codex, Gemini and others) can use KBorg in two ways:
- **With a shell:** they run KBorg's commands themselves to inspect and debug a cluster.
- **In a chat:** they ask the person to run the commands and paste the output.

The [KBorg skill](../skills/kborg/SKILL.md) tells them how. This page records:
- what reaches an assistant
- what KBorg masks
- how changes stay in the person's hands
- how this was checked (2026-11-15, the source after 0.6.0)

## Changes stay with the person

**Rule for the assistant.** The skill's first ground rule, repeated in its investigation guide:
1. **Describe:** before any change to a live cluster, the assistant says exactly what changes, where, the risk and how to undo it.
2. **Ask:** it gets a clear yes for that one change.
3. **Hand over:** it gives the person the command or keys to run themselves.

It never runs a change, never types into a KBorg prompt and never works around a review. KBorg's own review is then a second confirmation by the person.

**Enforced by KBorg, whatever the model does:**

| Situation | What KBorg does |
|---|---|
| A change started without a terminal (an agent's shell) | Refused before anything is sent: `noninteractive writes are disabled` and similar, for every write command |
| A change with `--read-only` (the skill puts it on every command) | Refused before any review or prompt, so there is nothing to answer |
| `KBORG_READ_ONLY=1` in the environment | Same as `--read-only` for every command and the TUI, and no flag (not even `--write`) turns changes back on |
| A context with `read_only: true` in `contexts/NAME.yaml`, or an operator config with `profile: read-only` | Changes to that cluster are off, for people and assistants alike |

The safest setup is to start the assistant from a shell where `KBORG_READ_ONLY=1` is set, so every command it runs inherits it:

```sh
KBORG_READ_ONLY=1 code .          # VS Code with Copilot agent mode
KBORG_READ_ONLY=1 claude          # Claude Code
KBORG_READ_ONLY=1 codex           # OpenAI Codex CLI
KBORG_READ_ONLY=1 gemini          # Gemini CLI
KBORG_READ_ONLY=1 copilot         # GitHub Copilot CLI
```

Tools that set environment variables for their agent in a settings file can set it there instead. For example, Claude Code takes `"env": {"KBORG_READ_ONLY": "1"}` in its `settings.json`. `kborg doctor` then shows `mode: read only (KBORG_READ_ONLY)`.

**Validation.** `.local/e2e/write-guard-audit.py`, run on a throwaway k3s 1.35 cluster, covers 21 write commands: scale, restart, set-image, set-resources, rollback, delete, kill, cordon, drain, uncordon, exec, attach, forward, cp-to, cp-from, apply, yaml-edit, node-shell, debug, trigger, and helm uninstall. Each ran three ways:
1. **No terminal:** every command was refused.
2. **A real pseudo-terminal with `--read-only`:** every command was refused before showing a prompt.
3. **A real pseudo-terminal with `KBORG_READ_ONLY=1` and `--write`:** every command was refused before showing a prompt.

In cases 2 and 3 the harness answered `y` to any prompt that appeared, and none did. Helm was also checked with a stand-in tool. The Deployment, Pods, Node and ConfigMaps were compared before and after: nothing changed. Result: PASS.

**Residual risk.** An assistant driving a real terminal without `--read-only` and without `KBORG_READ_ONLY` could reach a review and type `y`. Only the skill's rule stops that, which is why setting `KBORG_READ_ONLY=1` for the assistant is the recommended setup, and `read_only: true` for production contexts.

## What reaches an assistant

Everything an assistant reads through KBorg goes to its AI provider. KBorg's read commands print the same masked projection that `kborg get` has always printed (Copilot inside KBorg receives the same). The audit ran `.local/e2e/privacy-audit.sh`, 56 read commands in text and `--json`, on a cluster seeded with marked fake secrets:
- an Opaque Secret
- registry credentials
- a real signed ServiceAccount token
- `last-applied-configuration` copies of applied Secrets and ConfigMaps
- credential-named ConfigMap keys, and a token inside JSON stored in a ConfigMap
- credential-named annotations on a Deployment, its Pod template and a ConfigMap
- `DB_PASSWORD` and `AWS_SECRET_ACCESS_KEY` env values
- a password passed as a container argument
- a line in the application's log

It also searched every output for slices of the real ServiceAccount token and of the kubeconfig's client certificate and key.

| Data | In assistant output |
|---|---|
| Secrets (any type, ServiceAccount tokens, registry auth) | Never: `list`, `get` and `describe` refuse Secrets |
| Kubeconfig credentials | Never; `doctor` shows the server address and the kind of auth only |
| ConfigMap `data`/`binaryData`, every `env` value, `last-applied-configuration` | `[REDACTED]` |
| Values under credential-like names (`password`, `token`, `api_key`, `secret`, `credential`, `access_key`, `private_key`, ...), annotations included | `[REDACTED]` |
| Credentials in container `command`/`args` (`--password=V`, `--token V`, `NAME_PASSWORD=V`) | `[REDACTED]` (fixed in this audit) |
| Terminal control sequences | Removed |
| Application logs | Shown as written; free-form text cannot be told apart from secrets |
| Event messages, conditions, names, labels, images, IPs, hostnames, context names | Shown; they identify systems but hold no credentials |

Final run: no fake secret and no slice of a real credential appeared in any read output. The only marked string left was the plain text `CANARY-LOG-LINE`. It is part of the container's shell command (`echo CANARY-LOG-LINE`) and is printed to the application's log, as expected.

## What the audit fixed

1. **Credential arguments.** A container's `command` and `args` were printed unmasked, so `--password=…` reached `list --json`, `get`, `describe` and `watch` for Pods, Deployments and ReplicaSets. KBorg now masks the value of `--NAME=VALUE` and `NAME=VALUE` words whose name is credential-like, and the word after a credential flag such as `--token`. This applies on screen and in every output. In the YAML editor such an argument becomes read-only and is preserved.
2. **Credential-named values on screen.** The screen and editor projection hid only exact keys (`password`, `token`) and names containing `secret` or `credential`. So `db_password` in a ConfigMap, or an `owner-token` annotation, appeared in describe, YAML, the editor and `kborg yaml-template`. Any text value under a credential-like name is now masked there too, as 0.6.0's release notes described. Settings such as `automountServiceAccountToken: true` (not text) stay visible.
3. **`kborg yaml-template` under `--read-only`.** It prints the editable YAML, ConfigMap data included, for a following `yaml-edit`. As the first step of an edit it is now refused in read-only sessions, which includes every command an assistant runs.

## Other protections

- **Prompt injection.** Cluster text (logs, events, annotations) can contain instructions aimed at the assistant. The skill tells the assistant to treat all of it as data. Even if an assistant is misled, its commands are read-only, and any change still needs the person's approval.
- **Scope.**
  - **Cluster:** the skill makes the assistant confirm the context, name it on every command, and ask before reading a production cluster the person did not name.
  - **Output:** it keeps raw output out of answers and bounds logs with `--tail`.
- **Credentials.** The skill forbids asking for a kubeconfig, a token or a Secret value. If one turns up in a log, the assistant points to it and suggests rotation instead of repeating it.
- **The skill files themselves.** `kborg skill install` writes only into a folder that is empty or holds an earlier KBorg skill. It checks paths for symlinks, writes atomically, and never runs anything.

## Reproducing

Both scripts live in `.local/e2e/` (git-ignored) and need a throwaway cluster:
1. **Privacy:** `privacy-audit.sh KUBECONFIG OUTDIR`, after applying the canary fixture `canary.yaml` from `.local/ai-privacy-audit-1115/`.
2. **Changes:** `write-guard-audit.py KUBECONFIG`.

The outputs of this run are in `.local/ai-privacy-audit-1115/`. Unit tests cover the new masking (`safety` credential arguments and names), the `KBORG_READ_ONLY` parser, and every example in the skill.
