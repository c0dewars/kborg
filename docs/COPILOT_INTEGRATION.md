# Reviewed GitHub Copilot advice

KBorg can send an explicitly reviewed, minimized Kubernetes status bundle to GitHub Copilot. The response is advice: KBorg never executes suggested commands or applies suggested changes. The contained inference path currently supports **macOS ARM64, GitHub.com and the reviewed native Copilot CLI 1.0.91**. Other platforms and unsupported runtime versions fail closed.

`:copilot` opens a local preview for the selected resource. A configured policy enables a separate transfer review; configuration alone never sends data. Read every preview line and type the exact host/account confirmation. CLI `copilot diagnose RESOURCE NAME --copilot-policy ABS` requires an interactive terminal and the same explicit confirmation. Demo mode remains local preview only. Cancel with Ctrl-C; cancellation cannot retract bytes already received by the provider.

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

Both digest values must actually contain 64 lowercase hexadecimal characters. Select native Mach-O executables directly, not npm JavaScript loaders or symlinks. The supplied pins are trust decisions: a computed matching hash and successful native signature check do not independently establish publisher identity.

Exactly one credential source is required:

- `gh_cli` deliberately reuses the selected stored GitHub CLI account. KBorg invokes the pinned private snapshot with `gh auth token --hostname github.com --user LOGIN`, without switching accounts. Ambient token variables are removed. Only this separate credential lookup receives the user HOME and explicit GitHub configuration directory, allowing the selected stored Keychain credential. Its output remains in memory. The Copilot child receives neither directory nor the `gh` executable. Existing `gh` authority can be broader than Copilot; KBorg cannot independently inspect all grants. A stored OAuth, user-to-server or fine-grained token is required; classic PAT fallback is refused.
- Alternatively use `credential_file` instead of `gh_cli`. This must be an absolute, nonsymlink, single-link file with mode 0600 containing strict JSON: `{"schema_version":1,"host":"github.com","account":"LOGIN","token":"github_pat_…"}`. Select a fine-grained PAT limited to Copilot Requests. Never commit this file. KBorg cannot independently verify every grant from token syntax.

`--copilot-policy` enables setup only. No account is implicitly selected and no environment-token fallback occurs. Policy, executable and credential identities are rechecked; a changed account/token after review requires a new preparation. The exact approval expires after five minutes.

## What is shared

The bundle uses an allowlist of resource/status words and bounded numeric counters. Resource names, namespaces, context identity, UIDs, manifests, environment values, images, messages, events and logs are omitted. Unknown strings become `Other`. Numeric counters and workload state can still be confidential. The exact JSON is shown before sharing.

The instruction asks for likely causes, missing evidence and safe read-only investigation steps. This limits the task but does not make a model response trustworthy. Review advice independently.

## Whole-process containment

Every preparation and execution establishes fresh positive and negative checks against the actual generated macOS policy before any credential-bearing Copilot subprocess starts:

- Private empty HOME/config/current directory; environment cleared. The native is pinned and copied to a separate private executable snapshot. Its version is checked without a credential.
- macOS SBPL denies outside reads/writes, child executables/fork and direct networking. Standard system libraries and the pinned native's private, versioned extracted addon cache remain readable/mappable. The process can write only its temporary state area. An explicitly permitted broker loopback port is the sole outbound socket destination.
- A random per-session authenticated CONNECT broker accepts only exact approved TLS authorities on port 443. It independently checks the ClientHello SNI, rejects encrypted ClientHello, resolves only public addresses, rejects reserved/private/transition ranges and bounds connections, DNS work, bytes and lifetime. Native TLS still validates the server certificate end to end. The broker cannot inspect encrypted HTTP paths or request bodies.
- Approved provider authorities are `api.github.com`, `api.githubcopilot.com`, `api.individual.githubcopilot.com`, `api.business.githubcopilot.com`, `api.enterprise.githubcopilot.com` and `copilot-proxy.githubusercontent.com`. These services receive the approved request and the selected credential as required by the trusted native runtime. Host containment does not reduce a reused token's server-side grants.
- Tool/MCP/custom-instruction/auto-login/remote switches are defense in depth. **They are not the sandbox.** The OS policy and broker remain mandatory. No insecure retry, alternate account, direct-network fallback or TLS bypass exists.
- Supervision bounds the inference subprocess wall time (1–120 seconds), CPU time, open descriptors, generated files, captured output and network bytes. Stdin/stdout/stderr are supervised concurrently. Cancellation, early errors and dropped inference futures kill and reap the process group and close broker connections. Temporary state is removed. Token-free startup checks and explicit credential lookup have separate short deadlines. There is no claim of a hard resident-memory cap.

Apple's sandbox-exec/SBPL interface is private/deprecated and can change. Fresh canaries establish specific observed denials, not a formal proof or independent security audit. The chosen native executable, its embedded code/addons, the selected credential source, the OS and provider remain trusted. GitHub service policies, model availability and account entitlements also apply.

The old `copilot preflight` command remains metadata-only diagnostic evidence. It neither reads credentials nor enables a transfer; use policy-based preparation for supported contained inference.

## Validation and tooling

`cargo test advice:: --lib` covers strict policy/source parsing, credential identity binding, authenticated proxy rejection and bounded subprocess cleanup. The ignored `actual_native_policy_canaries` test uses a verified local official runtime fixture, without credentials. `cargo test --test copilot_live -- --ignored` is a separate explicitly authorized synthetic public-bundle test; it never contacts Kubernetes. Sanitized evidence retains hashes, selected account metadata, canary outcomes and network counts, never token contents or the answer.

During development the preinstalled npm native failed strict signature verification. It was not repaired or re-signed. A fresh official 1.0.91 platform artifact was downloaded, checked against npm's published SHA-512 integrity and passed strict macOS signature verification before testing. Local fixture paths/pins are operator setup, not portable defaults.

Primary references: [GitHub Copilot SDK](https://github.com/github/copilot-sdk), [SDK authentication](https://github.com/github/copilot-sdk/blob/main/docs/auth/authenticate.md), [Copilot CLI reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference), [programmatic CLI](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference), [explicit gh token selection](https://cli.github.com/manual/gh_auth_token). The official SDK launches the native CLI; avoiding an SDK dependency does not remove that native trust requirement.
