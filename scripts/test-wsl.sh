#!/bin/sh
set -eu
project=$1
fixture=${2:-}
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
command -v cargo >/dev/null || { echo 'Install official Rust with rustup first.' >&2; exit 1; }
command -v cc >/dev/null || { echo 'Install build-essential in this WSL distribution.' >&2; exit 1; }
export CARGO_TARGET_DIR="$HOME/.cache/k8borg-target"
cd "$project"
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
if [ -n "$fixture" ]; then
    export K8BORG_TEST_READER_CONFIG="$fixture/reader.kubeconfig"
    export K8BORG_TEST_OPERATOR_CONFIG="$fixture/operator.kubeconfig"
    export K8BORG_TEST_DENIED_CONFIG="$fixture/denied.kubeconfig"
    export K8BORG_CONFIG_DIR="$HOME/.cache/k8borg-wsl-acceptance"
    cargo test --locked --test docker_desktop -- --ignored --nocapture
    python3 scripts/Test-Performance.py --binary "$CARGO_TARGET_DIR/release/k8borg" \
        --output test-results/performance-linux.json --fixture "$fixture/reader.kubeconfig"
else
    python3 scripts/Test-Performance.py --binary "$CARGO_TARGET_DIR/release/k8borg" \
        --output test-results/performance-linux.json
fi
mkdir -p dist/linux-x86_64
cp "$CARGO_TARGET_DIR/release/k8borg" dist/linux-x86_64/k8borg
sha256sum dist/linux-x86_64/k8borg > dist/linux-x86_64/SHA256SUMS
