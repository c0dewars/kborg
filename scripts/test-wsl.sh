#!/bin/sh
set -eu
project=$1
fixture=${2:-}
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
command -v cargo >/dev/null || { echo 'Install official Rust with rustup first.' >&2; exit 1; }
command -v cc >/dev/null || { echo 'Install build-essential in this WSL distribution.' >&2; exit 1; }
export CARGO_TARGET_DIR="$HOME/.cache/kborg-target"
cd "$project"
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
if [ -n "$fixture" ]; then
    python3 - "$fixture" <<'PY'
import base64, hashlib, json, pathlib, re, sys, urllib.parse
def require(condition, message):
    if not condition:
        raise RuntimeError(message)
directory = pathlib.Path(sys.argv[1]).resolve(strict=True)
record = json.loads((directory / 'environment.json').read_text())
run_id = record.get('runId', '')
require(re.fullmatch(r'[a-f0-9]{12}', run_id), 'Invalid fixture identity')
require(record['namespace'] == 'kborg-tests-' + run_id, 'Invalid test namespace')
require(directory.name == run_id and directory.parent.name == '.local', 'Expected owned local fixture directory')
endpoint = urllib.parse.urlparse(record['server'])
require(endpoint.scheme == 'https' and endpoint.hostname in ('localhost', '127.0.0.1', 'kubernetes.docker.internal'), 'Expected local HTTPS cluster')
for identity in ('reader', 'operator', 'denied'):
    config = json.loads((directory / (identity + '.kubeconfig')).read_text())
    require(len(config['contexts']) == len(config['clusters']) == len(config['users']) == 1, 'Expected single-context fixture')
    require(config['contexts'][0]['context']['namespace'] == record['namespace'], 'Namespace mismatch')
    cluster = config['clusters'][0]['cluster']
    require(cluster['server'] == record['server'] and not cluster.get('insecure-skip-tls-verify'), 'TLS endpoint mismatch')
    require(hashlib.sha256(base64.b64decode(cluster['certificate-authority-data'])).hexdigest() == record['caSha256'], 'Fixture CA mismatch')
PY
    export KBORG_TEST_READER_CONFIG="$fixture/reader.kubeconfig"
    export KBORG_TEST_OPERATOR_CONFIG="$fixture/operator.kubeconfig"
    export KBORG_TEST_DENIED_CONFIG="$fixture/denied.kubeconfig"
    export KBORG_CONFIG_DIR="$HOME/.cache/kborg-wsl-acceptance"
    cargo test --locked --test docker_desktop -- --ignored --nocapture
    python3 scripts/Test-Performance.py --binary "$CARGO_TARGET_DIR/release/kborg" \
        --output test-results/performance-linux.json --fixture "$fixture/reader.kubeconfig"
else
    python3 scripts/Test-Performance.py --binary "$CARGO_TARGET_DIR/release/kborg" \
        --output test-results/performance-linux.json
fi
mkdir -p dist/linux-x86_64
cp "$CARGO_TARGET_DIR/release/kborg" dist/linux-x86_64/kborg
sha256sum dist/linux-x86_64/kborg > dist/linux-x86_64/SHA256SUMS
