# Dashboard health evidence

Dashboard cards show read-only evidence from up to four explicitly selected contexts. Each context has its own connection and Pod/Event watches. All contexts use the explicitly selected namespace, or their kubeconfig namespace if no override was provided. All namespaces must be selected explicitly with `*`.

The CLI exposes the same structured observations:

```sh
kborg --namespace development --json overview docker-desktop local-test
kborg --namespace development --follow --json overview docker-desktop local-test
```

## What the numbers mean

| Field | Meaning |
| --- | --- |
| Ready | Running Pods with an explicit `Ready=True` condition, ready statuses for their regular containers, and no unhealthy current container/init state. Readiness gates participate in the Pod condition. |
| Unready | Running Pods without complete ready evidence, including missing Ready conditions or container statuses. |
| Pending, failed, completed, terminating, unknown | Separate current Pod lifecycle categories. Completed Pods do not count as unhealthy; terminating Pods do. |
| Restarts | Sum of the current restart counters reported for regular, init and ephemeral containers. This is not a rate or a durable history. |
| OOM Pods | Pods with `OOMKilled` in current or last reported container termination state. A recovered Pod can retain this evidence; it is not a cumulative OOM event count. |
| Warnings, last 15m | Warning Event records linked to the exact namespace, name and UID of a currently observed Pod, with a verified observation timestamp within 15 minutes. Repeated event occurrences remain one record. |

Warning evidence uses the latest valid `series.lastObservedTime`, `lastTimestamp`, `eventTime` or `firstTimestamp`. An old Pod's warning cannot attach to its replacement. Old events and unrelated UIDs are excluded. Missing timestamps or timestamps more than 60 seconds in the future make the linked evidence incomplete. Up to five recent reason/Pod pairs appear in each summary, newest first; the count includes all qualifying records within the bounded source. Event message bodies are not projected into dashboard cards or JSON health results.

## Freshness and failures

Pod and Event evidence have independent states: connecting, live, stale, denied, unavailable or truncated. A denied Event watch preserves valid Pod counts and reports an unknown warning count. A stale or denied Pod watch clears the counts and health projection immediately. Unknown is never represented as a healthy zero. A complete empty list is a known zero.

`observed_unix_seconds` is the last successful Pod source observation, including successful unchanged watch renewal. A timer or Event update does not refresh it. The dashboard recomputes the warning time window every five seconds while retaining the actual source evidence state. Read-only snapshots describe the time they were collected and are not a durable history.

Each watch retains at most 1,000 objects / 8 MiB. Pod and Event sources have separate bounded caches. Four contexts therefore use at most eight source watches; every source remains subject to the existing request, response-size, list-page and reconnect limits. Closing the dashboard cancels every source. Dashboard connections explicitly strip write, exec, attach, log, forwarding and Helm mutation capabilities.

The snapshot CLI accepts up to 100 explicit contexts, processes at most four contexts concurrently and has a 30-second overall budget. Each context has a 20-second connection/read budget; a failed Event read does not discard a successful Pod observation. Large scopes should be narrowed when evidence is incomplete.

## Validation

Focused tests cover readiness gates and missing conditions, lifecycle counts, current/last OOM deduplication, UID and namespace attribution, long DNS Pod names, event recency and ordering, denied/incomplete evidence, stale-value clearing, capability stripping and cancellation of independent overview jobs.

`scripts/test-dashboard-health-live.py` creates owned restricted namespaces on Docker Desktop and the disposable `kborg-dashboard` minikube profile. It supplies the app with short-lived read-only service accounts, exercises Ready/unready/pending Pods, linked and deliberately unrelated Warning Events, Event-only and Pod-only permission denials, and a live readiness recovery on one cluster while peers retain independent state. Its optional screenshot mode captures the real dashboard and keymap. Cleanup checks each namespace UID and run label before deleting it.
