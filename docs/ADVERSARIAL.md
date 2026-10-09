# Bounded adversarial CLI acceptance

Executed on Windows on 2026-10-09 at 21:51 UTC against the optimized native Windows release corresponding to source revision `4e20eb2b1a6c56826665b1f8131f899ab6ff5c36`. Executable SHA-256: `5449f83eb19757c9a048b126594719cf2d6c82565a8ca7388b4b9d8382330b53`.

**Result: all 44 black-box cases passed.** No tested process timed out or emitted the terminal/control characters checked by the harness. The longest observed individual process duration was 28 ms; this timing is diagnostic, not a performance benchmark.

Reproduce with PowerShell 7:

```powershell
.\scripts\Test-Adversarial.ps1
# To test another locally built release:
.\scripts\Test-Adversarial.ps1 -Executable C:\absolute\path\k8borg.exe
```

The harness uses native process argument arrays, redirected stdin/stdout/stderr, a five-second deadline per process, an isolated `.local/adversarial-<run>` configuration/fixture directory and synthetic canary data. It performs no actual OAuth login, model request, Kubernetes mutation or network fixture connection. The minimized Copilot context is exported locally from offline demo data. Fixtures are retained under ignored `.local/` for reproducibility; they contain no real credentials.

Coverage:

- Theme validation: malformed JSON, nesting depth 300, duplicate top-level fields and duplicate color roles, files beyond 64 KiB, invalid UTF-8, unknown hooks/schema/roles, traversal/reserved IDs, custom inheritance, invalid hexadecimal and Unicode color strings, low contrast, oversized names, escape and bidi controls. A valid theme provides a positive control.
- Offline kubeconfig parsing: malformed/deep input, files beyond 1 MiB, duplicate context/cluster/user names, control/bidi/oversized context names. Valid empty contexts and an offline context with a synthetic authentication helper provide positive controls; offline context enumeration does not run the helper.
- Process/CLI boundaries: redirected noninteractive scale/restart/delete and Copilot login are refused; untrusted Copilot host, Secret read, traversal object name and unsupported `--yes` are rejected.
- Copilot data boundary: offline minimized export succeeds, overwrite is refused, and the produced bundle excludes synthetic object/node identity and raw metadata/workload fields.
- All process responses are checked for ESC, C1 and selected bidi/zero-width control characters, including sanitized unknown theme-ID error output.

Machine-readable results, exact cases, exit codes, response excerpts and durations: [adversarial.json](evidence/adversarial.json).

This is a finite regression corpus, not coverage-guided fuzzing, a hostile-server exercise or a security certification. It does not test live admission behavior, authenticated enterprise login, network denial-of-service, terminal recovery under crashes, filesystem replacement races, provider process isolation or platform-wide runtime compatibility. These remain separate production release gates recorded in [SECURITY_REVIEW.md](SECURITY_REVIEW.md).
