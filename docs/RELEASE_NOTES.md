# KBorg 0.3.0 — renamed client and operational workflows

K8Borg is now **KBorg**, executable `kborg`, private repository `c0dewars/kborg`. Existing pre-rename settings directories and `K8BORG_CONFIG_DIR` / `K8BORG_NO_SPLASH` remain compatible. Built-in theme identifiers remain stable. The updated logo is `assets/kborg-logo.png`.

New workflows: redacted YAML inspection; container selection and finite live log following; explicitly selected multi-context pod summaries; reviewed deployment container-image edits with GitOps guidance; native bounded exec/output attach and loopback forwarding behind separate capabilities; strict JSON read-only/operator configuration; shell completions; pending-audit reporting; recoverable custom-theme replacement and copying current colors. TUI adds visible actions, YAML, follow, image-edit and overview workflows.

Security changes: discovery now limits bytes/requests before decoding the complete catalog; image edits use UID/resourceVersion, server dry-run, spec/metadata admission checks and value-bound approval; exec preview rejects invisible/control arguments; native operations have finite lifetimes and protected audit intent; settings use staged same-directory replacement with restrictive Unix creation. Overview has four independent connections and a 30-second global budget.

Release packages include CycloneDX SBOM, dependency inventory, checksums and unsigned source/build records. These are not signed provenance or publisher authentication.

This remains a local-testing prerelease. Exec is noninteractive (no stdin/TTY), attach is output-only, and pod subresource APIs have a residual name-recreation race. AI requests, generic executable extensions and cloud authentication helpers remain disabled. Windows ACL/handle-race hardening, validated Copilot isolation/account acceptance, publisher signing/notarization, broader real-terminal acceptance and a production pilot remain release gates. `docs/REMAINING_WORK.md` is the authoritative register; historical 0.1/0.2 reports describe their original artifacts.
