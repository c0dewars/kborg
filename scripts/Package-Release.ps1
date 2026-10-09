#requires -Version 7.0
param(
    [Parameter(Mandatory)][ValidateSet('x86_64-pc-windows-msvc','aarch64-pc-windows-msvc','x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu','x86_64-apple-darwin','aarch64-apple-darwin')][string]$Target,
    [Parameter(Mandatory)][ValidatePattern('^[a-f0-9]{40}$')][string]$SourceCommit
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
Push-Location $projectRoot
try {
    $metadataText = & cargo metadata --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    $package = $metadata.packages | Where-Object name -eq 'k8borg'
    if (@($package).Count -ne 1) { throw 'Cannot determine project version' }
    $label = "k8borg-$($package.version)-$Target"
    $stage = Join-Path $projectRoot "dist/staging/$label"
    $packages = Join-Path $projectRoot 'dist/packages'
    if (Test-Path -LiteralPath $stage) { throw 'Release stage exists; use a clean checkout' }
    New-Item -ItemType Directory -Path $stage,$packages -Force | Out-Null
    $binaryName = if ($Target -like '*windows*') { 'k8borg.exe' } else { 'k8borg' }
    $binary = Join-Path $projectRoot "target/$Target/release/$binaryName"
    if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Target binary is missing' }
    & $binary --version
    if ($LASTEXITCODE -ne 0) { throw 'Version smoke test failed' }
    $demoText = & $binary --demo list pods --json
    if ($LASTEXITCODE -ne 0) { throw 'Offline JSON smoke test failed' }
    $demo = ($demoText -join "`n") | ConvertFrom-Json
    if (@($demo.items).Count -lt 1) { throw 'Demo returned no objects' }
    $themes = & $binary theme list
    if ($LASTEXITCODE -ne 0 -or !($themes -match 'catppuccin-mocha')) { throw 'Theme inventory smoke test failed' }
    Copy-Item -LiteralPath $binary -Destination (Join-Path $stage $binaryName)
    foreach ($file in @('README.md','LICENSE','THIRD_PARTY_NOTICES.md','SECURITY.md','docs/RELEASE_NOTES.md')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot $file) -Destination $stage
    }
    $notices = Join-Path $stage 'theme-notices'
    New-Item -ItemType Directory -Path $notices | Out-Null
    Copy-Item -Path (Join-Path $projectRoot 'assets/upstream/*') -Destination $notices -Recurse
    $inventory = $metadata.packages | Sort-Object name,version | Select-Object name,version,license,source
    ConvertTo-Json -InputObject @($inventory) -Depth 6 | Set-Content -LiteralPath (Join-Path $stage 'DEPENDENCIES.json') -Encoding utf8NoBOM
    $build = [ordered]@{schema=1;version=$package.version;target=$Target;sourceCommit=$SourceCommit;rust=(& rustc --version);builtAtUtc=[DateTime]::UtcNow.ToString('o');binarySha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant();tests='native unit tests in CI; offline CLI smoke';signed=$false;productionCertified=$false}
    $build | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $stage 'BUILD.json') -Encoding utf8NoBOM
    if ($Target -like '*windows*') {
        $archive = Join-Path $packages "$label.zip"
        if (Test-Path -LiteralPath $archive) { throw 'Archive exists' }
        Compress-Archive -LiteralPath $stage -DestinationPath $archive
    } else {
        $archive = Join-Path $packages "$label.tar.gz"
        if (Test-Path -LiteralPath $archive) { throw 'Archive exists' }
        & tar -czf $archive -C (Split-Path $stage -Parent) $label
        if ($LASTEXITCODE -ne 0) { throw 'Archive creation failed' }
    }
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$checksum  $(Split-Path $archive -Leaf)" | Set-Content -LiteralPath "$archive.sha256" -Encoding utf8NoBOM
    Copy-Item -LiteralPath (Join-Path $stage 'BUILD.json') -Destination (Join-Path $packages "$label.build.json")
    Write-Output "Packaged $archive (unsigned)"
} finally { Pop-Location }
