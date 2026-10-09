# Watches and Metrics UX Verification

Verified 2026-10-09 on Windows using the actual release executable copied to `.local/ux-watchmetrics/k8borg.exe` before launching. The tested binary was 9,972,224 bytes and retained the pre-version-bump 0.1 label. Runtime source files were not modified by this tester.

## Actual execution

Demo CLI pod metrics JSON returned explicitly labeled deterministic sample data: CPU `125m`, `125000000` nanocores, memory `64Mi` / `67108864` bytes, timestamp `2026-01-01T00:00:00Z`, window `30s`. Node text output returned `750` millicores, `1073741824` bytes, the same sample timestamp and window. This is synthetic evidence, not a live Metrics API success.

Windows PTY rendered at 80 columns by 24 rows. Demo `m` opened the metrics detail modal with complete CPU, memory, timestamp and measurement window. The long explanatory line wrapped. Escape restored the two cached pod rows. Demo `w` kept POLL and explained that demo uses synthetic polling. Demo quit returned exit code 0 and restored cursor, alternate screen and mouse modes.

Live checks explicitly used the existing trusted kubeconfig and `--context docker-desktop`, with read-only mode and no context changes or cluster writes. Pod watch in kube-system populated eight pod rows and displayed LIVE. Bounded stream endings visibly changed to WATCH/STALE with `watch stream ended; reconnecting`, then recovered LIVE; the test observed recovery rather than hiding the interruption.

Docker Desktop did not serve metrics.k8s.io/v1beta1. Metrics CLI exited 1 with an explicit API-unavailable diagnostic, and no fake zero samples or JSON success result. TUI `m` opened METRICS unavailable and stated that resource browsing remains available. Core pod rows remained retained; the watch independently recovered LIVE while the error modal stayed open. This establishes separation of metrics errors from the resource cache.

Live `w` changed to POLL, cleared pending old data, refilled the same eight rows, then another `w` started WATCH synchronization and recovered LIVE. Explicit `--poll` launch stayed POLL and reported five-second refreshes, with zero pods in default. Quit returned exit code 0. Quit during an active LIVE watch also returned exit code 0 with terminal restoration.

Changing namespace from kube-system to default immediately cleared the old eight pod rows; the default watch settled LIVE with an empty result. Rapid resource changes using Tab followed by `jjj` reached Events through Deployments and Services, synchronized the new scope and showed no old pod rows. This was terminal UX evidence, not fault-injected late-reply coverage.

## Finding delivered to integration owner

The metrics success modal wraps its body, but the unavailable-error body was clipped at 80 columns after `no metrics serve`. The primary API-unavailable meaning and browsing-available line remained readable, but the final explanation could not be inspected with vertical scrolling. Integration owner corrected this issue. A copied rebuilt executable reporting K8Borg 0.2.0 was retested in the same 80-column Windows PTY: the unavailable message wrapped onto a second line and displayed the complete explanation, including `er is installed automatically`. The browsing-available line remained readable. Finding closed. Escape and quit restored terminal modes and returned exit code 0; no PTY was left running.

## Evidence limits

No fixture namespace or workload was created by this tester. Live watch ADD/MODIFY/DELETE and RBAC tests belong to the separate acceptance harness. No live successful metrics server was available. Cached filtering was inspected in code but not instrumented to prove zero network calls; that claim remains unverified by this UX run. No physical Linux or macOS terminal was tested. Terminal resize/minimum-size behavior and long-name metrics wrapping were not exercised here. PTY ANSI output is the execution evidence; no desktop screenshots were captured.

