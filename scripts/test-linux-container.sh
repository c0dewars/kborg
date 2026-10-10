#!/bin/sh
set -eu
# Run only inside the documented disposable validation container.
if [ "${KBORG_CONTAINER_VALIDATION:-}" != 1 ] || [ ! -f /.dockerenv ]; then
  printf '%s\n' 'Use the Docker command in docs/PRIORITIES_ACCEPTANCE.md.' >&2
  exit 2
fi
[ -f /source/Cargo.toml ] && [ -d /output ] && [ -d /build ]
finish() {
  task_exit=$?
  for task_log in /build/*.txt /build/*.json; do if [ -f "$task_log" ]; then cp "$task_log" /output/; fi; done
  printf '%s\n' "$task_exit" > /output/exit-code.txt
  if [ "$task_exit" -ne 0 ]; then
    for task_log in /build/tests.txt /build/clippy.txt /build/release.txt; do if [ -f "$task_log" ]; then tail -40 "$task_log"; fi; done
  fi
}
trap finish EXIT
DEBIAN_FRONTEND=noninteractive apt-get update -qq
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq procps python3 >/build/apt.txt
mkdir -p /build/work /build/cargo
if [ -d /cache/registry ]; then cp -R /cache/registry /build/cargo/; fi
export CARGO_HOME=/build/cargo
cp -R /source/src /source/tests /source/docs /source/assets /build/work/
cp /source/Cargo.toml /source/Cargo.lock /source/rust-toolchain.toml /build/work/
mkdir -p /build/work/scripts
cp /source/scripts/test-terminal.py /build/work/scripts/
cd /build/work
sha256sum /source/src/*.rs /source/tests/*.rs /source/Cargo.toml /source/Cargo.lock /source/rust-toolchain.toml /source/docs/CLI_HELP.txt /source/scripts/test-terminal.py /source/scripts/test-linux-container.sh > /build/source-hashes.txt
rustc --version
uname -m
cargo fmt --all -- --check > /build/fmt.txt 2>&1
cargo test --locked > /build/tests.txt 2>&1
cargo clippy --locked --all-targets -- -D warnings > /build/clippy.txt 2>&1
cargo build --release --locked > /build/release.txt 2>&1
./target/release/kborg --demo list pods --json > /build/demo.json
./target/release/kborg --demo --metrics-resources metrics pods --json > /build/metrics.json
python3 scripts/test-terminal.py --binary target/release/kborg --output /build/terminal-linux-arm64.json
sha256sum -c /build/source-hashes.txt > /build/source-recheck.txt
cp ./target/release/kborg /output/kborg-linux-arm64
sha256sum /output/kborg-linux-arm64
