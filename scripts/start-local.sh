#!/bin/sh
# Native macOS/Linux launcher. Pass --demo for an offline session.
set -eu
task_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
task_binary="$task_root/target/release/kborg"
if [ ! -x "$task_binary" ]; then
    printf '%s\n' 'Build KBorg first: cargo build --release --locked' >&2
    exit 1
fi
exec "$task_binary" "$@"
