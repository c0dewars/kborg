# Reviewed GitHub Copilot advice

KBorg can send an explicitly reviewed, minimized Kubernetes status bundle to GitHub Copilot. The response is advice: KBorg never executes suggested commands or applies suggested changes. The contained inference path currently supports **macOS ARM64 and the reviewed native Copilot CLI 1.0.91**, against **GitHub.com, GitHub Enterprise Cloud with data residency (`TENANT.ghe.com`) or GitHub Enterprise Server 3.22+ in offline provider mode**. Other platforms and unsupported runtime versions fail closed.

`:copilot` opens a local preview for the selected resource. A configured policy enables a separate transfer review; configuration alone never sends data. Check the preview and press `y` to send (strict mode: type the exact host/account confirmation). CLI `copilot diagnose RESOURCE NAME --copilot-policy ABS` requires an interactive terminal and the same explicit confirmation. Demo mode keeps `:copilot` local preview only (see `:ask` below). Cancel with Ctrl-C; cancellation cannot retract bytes already received by the provider.

## Start here: guided setup

Use `:ask` or `:copilot setup` inside KBorg. Missing configuration, an unsupported platform, or an operator prohibition produces a visible explanation. Press **S** in the setup panel to enter the interactive wizard, then return to the TUI. Choose GitHub.com or your company `TENANT.ghe.com`; if needed, the official GitHub CLI presents a one-time code and browser link. No copied token is requested. `:copilot status` shows the current readiness; CLI equivalents are:

```sh
kborg copilot status
kborg copilot setup
```

The wizard discovers `gh`, lets you select the GitHub host/account and offers browser login if needed. On supported macOS ARM64 it also discovers and validates the native Copilot runtime, then writes the pinned policy after review. On other platforms it completes account sign-in only and says that in-app inference remains unavailable. No token copying or manual JSON is required for that path. Setup makes no model request and sends no Kubernetes data. An entitled account, the supported runtime and working network are prerequisites.

**Windows, Linux and Intel macOS:** the contained inference runtime is not implemented for these platforms. KBorg explains this before offering account-only login; a successful login alone does not enable containment. Browsing Kubernetes does not depend on Copilot. Do not bypass the policy or copy cluster credentials into another AI tool to work around this limit.

The JSON examples below are advanced/manual configuration for administrators, not the normal onboarding path.

## Ask about the screen (`:ask`)

`:ask QUESTION` (or `:ask` alone, which prompts for the question) asks Copilot about what you are looking at. When you press `:`, KBorg renders the current view as plain text: the same frame you see, with box drawing turned into ASCII and the key-hint row dropped. That works for tables, describe/YAML, logs, metrics and the dashboard. The question (1–500 characters, one line) and up to 24 KiB of screen text form the payload.

- **Setup once:** use the guided setup above. Its `copilot-policy.json` is reused automatically, or an administrator may pass `--copilot-policy ABS`. `:ask` is available by default; `allow_copilot_screen: false` disables it. Missing setup opens a visible next-step screen and sends nothing.
- **Not minimized:** unlike `:copilot`, the screen text can include context, namespace and object names, statuses, events and log lines. Masked Secret values and other display redaction apply first. The review lists the exact bytes, and every line must be painted before approval.
- **Approval:** press `y` after the review (strict mode: type `SEND SCREEN TO HOST AS ACCOUNT`). It expires after five minutes and is bound to the policy, executable and credential identity, like `:copilot`.
- **Prompt injection:** the fixed instruction tells the model to treat screen text as data. A hostile log line can still try to steer the answer, so answers are advice only and are never executed.
- **Demo mode:** `kborg demo` screens are synthetic, so `:ask` can send them, which is a safe way to try the feature with your account.
- **Historical validation:** the [2026-10-23 acceptance report](TRUST_MIGRATION_ACCEPTANCE.md) records a GitHub.com backend test with a selected stored `gh` account and a synthetic public bundle. Its source-specific evidence does not validate the new release/next device-login wizard: live OAuth/account acceptance and enterprise tenant acceptance remain open.

The instruction and payload are written together to the CLI's stdin. They are not passed with `-p`, which would put the payload in the process argument list, and the pinned CLI ignores stdin when `-p` is present. The review's exact payload is that stdin text.

## Explicit policy and account

Use an absolute, nonsymlink JSON policy path with no group/world write permission. Unknown and duplicate JSON fields are rejected. Example using a deliberately selected existing `gh` account:

```json
{
  "schema_version": 1,
  "executable": "/absolute/trusted/copilot-native",
  "expected_sha256": "64-lowercase-hex-characters-from-a-trusted-source",
  "host": "github.com",
  "gh_cli": {
    "executable": "/absolute/trusted/native/gh",
    "expected_sha256": "64-lowercase-hex-characters-from-a-trusted-source",
    "account": "your-selected-login",
    "config_dir": "/absolute/user/.config/gh"
  },
  "model": "auto",
  "seconds": 90
}
```

## GitHub Enterprise deployments

The policy `host` selects one deployment. Each deployment has its own exact broker allowlist; there is no wildcard and no fallback to another deployment.

| Deployment | Policy | Broker authorities (TLS, port 443) | Runtime selection |
|---|---|---|---|
| GitHub.com | `"host": "github.com"` | `api.github.com`, `api.githubcopilot.com`, `api.individual/business/enterprise.githubcopilot.com`, `copilot-proxy.githubusercontent.com` | `GH_HOST=github.com` |
| GHE.com (data residency) | `"host": "TENANT.ghe.com"` (deployment inferred, or `"deployment": "ghe.com"`) | `TENANT.ghe.com`, `api.TENANT.ghe.com`, `copilot-api.TENANT.ghe.com` | `GH_HOST` and `COPILOT_GH_HOST` set to the tenant |
| GitHub Enterprise Server | `"host": "github.example.internal", "deployment": "server"` | only that host | `COPILOT_OFFLINE=true`, `COPILOT_PROVIDER_GHES_HOST`, `COPILOT_PROVIDER_GHES_TOKEN` |

- A GHE.com tenant must be a single label (`acme.ghe.com`). The pinned 1.0.91 runtime derives `api.` and `copilot-api.` endpoints from the selected host; both are approved exactly, nothing else under `ghe.com` is.
- GHES uses GitHub's documented offline provider mode (GHES 3.22+, technical preview at the time of writing). The instance forwards the bundle to the model provider its administrator configured; KBorg cannot see or verify that provider, and the transfer review says so. A GHES host must be named explicitly with `"deployment": "server"` and cannot be a GitHub-owned domain, an IP literal or a name with a port.
- GHES-only options: `"allow_private_network": true` lets the broker reach RFC 1918, CGNAT (100.64/10) and IPv6 ULA addresses for that one host. Loopback, link-local (including 169.254.169.254 metadata), multicast and unspecified addresses remain blocked. `"tls_ca_file": "/absolute/ca.pem"` pins an internal CA bundle; it must be a non-writable, single-link file containing only PEM certificates, and is copied into the private sandbox state for the run.
- Credentials are host-bound: `gh auth token --hostname HOST --user LOGIN` for `gh_cli`, and a `credential_file` whose `host` equals the policy host. Classic `ghp_` tokens are still refused.
- Validation status: the policy, allowlists, private-range gating and environment are unit-tested, and the token-free macOS sandbox canary passes against the pinned runtime. No GHE.com tenant or GHES 3.22 instance was available, so live enterprise inference has **not** been demonstrated; run the existing live test against your own tenant before relying on it.

Example GHES policy fragment:

```json
{
  "host": "github.example.internal",
  "deployment": "server",
  "allow_private_network": true,
  "tls_ca_file": "/Library/KBorg/corp-root-ca.pem"
}
```

Both digest values must actually contain 64 lowercase hexadecimal characters. Select native Mach-O executables directly, not npm JavaScript loaders or symlinks. The supplied pins are trust decisions: a computed matching hash and successful native signature check do not independently establish publisher identity.

Exactly one credential source is required:

- `gh_cli` deliberately reuses the selected stored GitHub CLI account. KBorg invokes the pinned private snapshot with `gh auth token --hostname HOST --user LOGIN`, without switching accounts. Ambient token variables are removed. Only this separate credential lookup receives the user HOME and explicit GitHub configuration directory, allowing the selected stored Keychain credential. Its output remains in memory. The Copilot child receives neither directory nor the `gh` executable. Existing `gh` authority can be broader than Copilot; KBorg cannot independently inspect all grants. A stored OAuth, user-to-server or fine-grained token is required; classic PAT fallback is refused.
- Alternatively use `credential_file` instead of `gh_cli`. This must be an absolute, nonsymlink, single-link file with mode 0600 containing strict JSON: `{"schema_version":1,"host":"HOST","account":"LOGIN","token":"github_pat_…"}` with `HOST` equal to the policy host. Select a fine-grained PAT limited to Copilot Requests. Never commit this file. KBorg cannot independently verify every grant from token syntax.

`--copilot-policy` enables setup only. No account is implicitly selected and no environment-token fallback occurs. Policy, executable and credential identities are rechecked; a changed account/token after review requires a new preparation. The exact approval expires after five minutes.

## What is shared

The bundle uses an allowlist of resource/status words and bounded numeric counters. Resource names, namespaces, context identity, UIDs, manifests, environment values, images, messages, events and logs are omitted. Unknown strings become `Other`. Numeric counters and workload state can still be confidential. The exact JSON is shown before sharing.

The instruction asks for likely causes, missing evidence and safe read-only investigation steps. This limits the task but does not make a model response trustworthy. Review advice independently.

## Whole-process containment

Every preparation and execution establishes fresh positive and negative checks against the actual generated macOS policy before any credential-bearing Copilot subprocess starts:

- Private empty HOME/config/current directory; environment cleared. The native is pinned and copied to a separate private executable snapshot. Its version is checked without a credential.
- macOS SBPL denies outside reads/writes, child executables/fork and direct networking. Standard system libraries and the pinned native's private, versioned extracted addon cache remain readable/mappable. The process can write only its temporary state area. An explicitly permitted broker loopback port is the sole outbound socket destination.
- A random per-session authenticated CONNECT broker accepts only exact approved TLS authorities on port 443. It independently checks the ClientHello SNI, rejects encrypted ClientHello, resolves only public addresses (plus enterprise-private ranges for an explicitly opted-in GHES host), rejects reserved/private/transition ranges and bounds connections, DNS work, bytes and lifetime. Native TLS still validates the server certificate end to end. The broker cannot inspect encrypted HTTP paths or request bodies.
- Approved provider authorities depend on the deployment (see the table above). For GitHub.com they are `api.github.com`, `api.githubcopilot.com`, `api.individual.githubcopilot.com`, `api.business.githubcopilot.com`, `api.enterprise.githubcopilot.com` and `copilot-proxy.githubusercontent.com`. These services receive the approved request and the selected credential as required by the trusted native runtime. Host containment does not reduce a reused token's server-side grants.
- Tool/MCP/custom-instruction/auto-login/remote switches are defense in depth. **They are not the sandbox.** The OS policy and broker remain mandatory. No insecure retry, alternate account, direct-network fallback or TLS bypass exists.
- Supervision bounds the inference subprocess wall time (1–120 seconds), CPU time, open descriptors, generated files, captured output and network bytes. Stdin/stdout/stderr are supervised concurrently. Cancellation, early errors and dropped inference futures kill and reap the process group and close broker connections. Temporary state is removed. Token-free startup checks and explicit credential lookup have separate short deadlines. There is no claim of a hard resident-memory cap.

Apple's sandbox-exec/SBPL interface is private/deprecated and can change. Fresh canaries establish specific observed denials, not a formal proof or independent security audit. The chosen native executable, its embedded code/addons, the selected credential source, the OS and provider remain trusted. GitHub service policies, model availability and account entitlements also apply.

The old `copilot preflight` command remains metadata-only diagnostic evidence. It neither reads credentials nor enables a transfer; use policy-based preparation for supported contained inference.

## Validation and tooling

`cargo test advice:: --lib` covers strict policy/source parsing, credential identity binding, authenticated proxy rejection and bounded subprocess cleanup. The ignored `actual_native_policy_canaries` test uses a verified local official runtime fixture, without credentials. `cargo test --test copilot_live -- --ignored` is a separate explicitly authorized synthetic public-bundle test; it never contacts Kubernetes. Sanitized evidence retains hashes, selected account metadata, canary outcomes and network counts, never token contents or the answer.

During development the preinstalled npm native failed strict signature verification. It was not repaired or re-signed. A fresh official 1.0.91 platform artifact was downloaded, checked against npm's published SHA-512 integrity and passed strict macOS signature verification before testing. Local fixture paths/pins are operator setup, not portable defaults.

Primary references: [GitHub Copilot SDK](https://github.com/github/copilot-sdk), [SDK authentication](https://github.com/github/copilot-sdk/blob/main/docs/auth/authenticate.md), [Copilot CLI reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), [programmatic CLI](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference), [explicit gh token selection](https://cli.github.com/manual/gh_auth_token). The official SDK launches the native CLI; avoiding an SDK dependency does not remove that native trust requirement.
