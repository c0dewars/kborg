# Saved selections

Saved selections are explicit bookmarks for a resource view or a dashboard. They let you return to a working scope without changing the current context in your kubeconfig. Saving, reopening and deleting are separate user actions; opening KBorg does not automatically restore a saved view.

Press `b` in a resource table, or `s` on the dashboard, to save the current selection with a distinct name. Press `B` to open saved selections, use arrows or `j`/`k` to choose one, and press Enter to reopen it. In that picker, `d` or Delete asks you to type the exact name before removing a record. Esc cancels a save or deletion. The `?` keymap also lists these controls.

Reopening a resource selection restores its context, namespace, canonical resource kind and server-side label selector. If it includes a selected workload, KBorg focuses that workload only when the currently loaded object still has the saved namespace, name and UID. A deleted or recreated workload does not inherit the selection. A bounded or changed listing may not contain the saved workload; the resource view remains usable without that focus.

A dashboard selection stores one to four distinct context names and a shared namespace scope. It does not store dashboard results, object contents, logs or metrics. An all-namespace scope is represented as `*`. Cluster-scoped resource selections also use `*`.

## Reopening and permissions

Every explicit live reopen validates the saved structure and checks that its contexts still exist in the active kubeconfig. Resource kinds and namespace scope are checked against discovery from the newly selected context. Missing contexts, unavailable resource kinds and invalid preferences produce an error. Offline dashboards may use named synthetic contexts. A live selection cannot be reopened in demo mode, or a demo selection in a live session.

A selection contains no kubeconfig paths, credentials, authentication-helper approvals, mutation permissions, write confirmations or AI consent. The current session's explicit options still govern all capabilities. Reopening only navigates and performs the normal reads for that view. It never executes a saved operation or restores an approval.

Context names are references to the active kubeconfig, not a saved server identity. If you deliberately repoint a context in that kubeconfig, reopening uses its current configuration. Check the context and server displayed by KBorg before operating on a cluster.

## Local storage

Selections use strict schema version 1 JSON at `selections/v1.json` beneath the KBorg configuration directory. On macOS the default directory is `~/Library/Application Support/KBorg`; `KBORG_CONFIG_DIR` can select a private absolute directory. Existing legacy KBorg configuration-directory compatibility follows the application's normal settings rules.

- At most 16 selections, each with a name of 1–64 plain characters and at most 128 UTF-8 bytes.
- At most four distinct contexts per dashboard. Names and selectors have their normal bounded validation.
- At most 64 KiB for the complete file. Unknown fields, duplicate names, unsupported schema versions and invalid records are rejected.
- Reading an absent store creates no directory or file. Only explicit save or deletion changes preferences.
- New storage uses directory mode `0700` and file mode `0600` on Unix. Existing nonprivate files or directories and symlink paths are rejected.
- Updates use a local exclusive lock and an atomic sibling-file replacement. Another process updating the file produces a retry message rather than losing its changes.
- Saving never replaces an existing name. Choose a different name or explicitly delete the old record first. Invalid or corrupt files are retained for inspection rather than overwritten.

These are private preferences, but context names, namespaces, workload names and labels can still reveal environment details. Do not put credentials or other secrets into selection names or label selectors.

## Validation coverage

Focused tests cover private storage and explicit deletion, reading without side effects, duplicate-name refusal, strict schema and capability-field rejection, record and text bounds, stale kubeconfig contexts, live/demo isolation, discovery and namespace scope, exact workload UID matching, concurrent mutation, corrupt-file retention, and symlink/nonprivate-file rejection. The product acceptance report records end-to-end saved-selection tests against the final UI build.
