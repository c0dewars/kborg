# KBorg local storage behavior

Updated 2026-10-10 for the 0.3 development iteration.

Appearance persistence now writes a unique sibling staging file with create-new semantics, syncs that file and replaces `appearance.json` with one same-directory `std::fs::rename`. The old destination is never deleted before the rename. Failed staging or rename leaves the previous destination in place. Leftover staging files are not loaded as preferences and do not collide with the former fixed process-ID staging name. On Unix the parent directory is synced after commit; an error there explicitly reports that replacement committed but durability is uncertain.

Rust documents that `std::fs::rename` replaces an existing destination file and uses the native platform operation, including Windows. The former comment saying Windows rename cannot replace a file was incorrect. See [Rust std::fs::rename](https://doc.rust-lang.org/std/fs/fn.rename.html). Same-directory replacement removes the application's delete/rename gap; universal power-loss guarantees across filesystems, antivirus/share modes and network storage are not claimed.

New directory components are created with mode 0700 on Unix; new files use 0600. Existing directories are not chmod'ed automatically because they may be shared user configuration roots. Paths continue to reject symlinks and Windows reparse points. All writes use checked paths; these path-based checks still have a check/use race against another process with authority to replace the directory. They are not a sandbox against the same user.

Windows file and directory creation inherits the destination's ACL. Rust std does not provide an ACL policy/verifier, and this implementation neither invokes `icacls` nor adds unsafe FFI. Managed Windows deployments must provision and validate a private user configuration directory. A reviewed native ACL wrapper and handle-relative path operations remain production hardening gates where hostile local writers are in scope.

`themes::replace(path)` validates the entire input before mutation and only accepts a custom theme identity. It preserves the installed bytes as `<id>.json.bak` using create-new semantics, then atomically replaces the installed JSON. An existing backup causes a refusal; the user must archive it explicitly before another replacement. Failure after creating the backup leaves the old theme recoverable. Replacement does not automatically change the selected theme. Backups are not enumerated as installed themes.

`themes::create_from_current(id)` copies the currently resolved RGB roles into a new validated custom theme and refuses overwrites or reserved identifiers. A host-dependent terminal palette cannot be exported as fixed RGB. These helpers create data-only files and do not execute subprocesses or contact a cluster.

Windows validation: 75 combined library tests passed after these changes, including repeated atomic replacement, preserved prior preference across staging failure, leftover staging isolation, custom-theme backup preservation, refusal to overwrite a backup and refusal to replace built-in themes. A Unix-only mode test verifies 0700 directories and 0600 files when run on Unix; its presence is not a claim that it ran on Windows. No real user appearance settings were modified by these tests.

Independent source review of the new configuration, pending-audit and context-overview modules found no immediate write-policy bypass in their reviewed paths: configured read-only rejects `--write`, privileged operations also check writable policy, overview explicitly resets writable/log access and retains only bounded summaries, and incomplete audit outcomes remain unknown. These observations do not certify external audit integrity, OS isolation, real identity coverage or all concurrency behavior. Configuration is explicitly selected local data, not organizational enforcement against its owner.

WSL2 Ubuntu-26.04 validation also passed all six safety tests, including actual Unix0700/0600 assertions, using the native Linux Rust build. Combined Windows Clippy passed with warnings denied after the storage change.
