$ErrorActionPreference='Stop'
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
Push-Location (Split-Path -Parent $PSScriptRoot)
try {
    cargo fmt --check
    if ($LASTEXITCODE -ne 0) {throw 'Formatting failed.'}
    cargo clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) {throw 'Clippy failed.'}
    cargo test --locked
    if ($LASTEXITCODE -ne 0) {throw 'Tests failed.'}
    cargo build --locked --release
    if ($LASTEXITCODE -ne 0) {throw 'Release build failed.'}
} finally {Pop-Location}
