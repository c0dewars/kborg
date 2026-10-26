#requires -Version 7.0
param(
    [Parameter(Mandatory)][ValidateSet('x86_64-pc-windows-msvc','aarch64-pc-windows-msvc','x86_64-unknown-linux-gnu','aarch64-unknown-linux-gnu','x86_64-apple-darwin','aarch64-apple-darwin')][string]$Target,
    [Parameter(Mandatory)][ValidatePattern('^[a-f0-9]{40}$')][string]$SourceCommit,
    # Optional macOS Developer ID identity (certificate SHA-1 from `security
    # find-identity`). Signs the fresh binary with the hardened runtime before
    # the smoke checks, so the packaged binary is exactly the signed one.
    # Notarization is a separate step (.github/workflows/release.yml).
    [ValidatePattern('^[A-F0-9]{40}$')][string]$CodesignIdentity
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$buildDirectory = $null
function Assert-CleanSource {
    $actualCommit = (& git rev-parse --verify HEAD | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $actualCommit -cne $SourceCommit) { throw 'SourceCommit must equal the actual checked-out HEAD' }
    $status = @(& git status --porcelain=v1 --untracked-files=all)
    if ($LASTEXITCODE -ne 0 -or $status.Count) { throw 'Release packaging requires a clean checkout, including untracked files' }
    # Ignored files are absent from git status but recursive packaging would copy
    # them. Require every packaged input to be tracked, plain and unlinked.
    $tracked = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    & git -c core.quotepath=false ls-files | ForEach-Object { [void]$tracked.Add($_) }
    if ($LASTEXITCODE -ne 0) { throw 'Cannot read tracked source inventory' }
    $inputs = @('README.md','LICENSE','THIRD_PARTY_NOTICES.md','SECURITY.md','docs','assets/kborg-logo.png','assets/LOGO.md','assets/upstream','assets/dependency-licenses')
    foreach ($inputPath in $inputs) {
        $root = Get-Item -LiteralPath (Join-Path $projectRoot $inputPath) -Force
        $items = @($root)
        if ($root.PSIsContainer) { $items += @(Get-ChildItem -LiteralPath $root.FullName -Recurse -Force) }
        foreach ($item in $items) {
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Linked package inputs are forbidden' }
            if (!$item.PSIsContainer) {
                $relative = [IO.Path]::GetRelativePath($projectRoot,$item.FullName).Replace('\','/')
                if ($relative -match '[\x00-\x1f\x7f]' -or !$tracked.Contains($relative)) { throw 'Every package input must be a tracked plain file, including ignored inputs' }
            }
        }
    }
}
Push-Location $projectRoot
try {
    Assert-CleanSource
    $rustDetails = @(& rustc -vV)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect Rust compiler' }
    $rustHost = @($rustDetails | Where-Object { $_ -like 'host: *' })
    if ($rustHost.Count -ne 1 -or $rustHost[0].Substring(6) -cne $Target) { throw 'Packaging requires the native Rust host target' }
    $nativeArchitecture = [Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString().ToLowerInvariant()
    $expectedArchitecture = if ($Target.StartsWith('aarch64-')) { 'arm64' } else { 'x64' }
    if ($nativeArchitecture -cne $expectedArchitecture) { throw 'PowerShell process architecture must match the native target' }
    foreach ($name in @('RUSTFLAGS','CARGO_ENCODED_RUSTFLAGS','RUSTC','RUSTC_WRAPPER','RUSTC_WORKSPACE_WRAPPER','CARGO_BUILD_RUSTC','CARGO_BUILD_RUSTC_WRAPPER','CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER','CARGO_BUILD_RUSTFLAGS')) {
        if ([Environment]::GetEnvironmentVariable($name)) { throw "Release packaging refuses compiler override environment: $name" }
    }
    $metadataText = & cargo metadata --locked --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Dependency metadata failed' }
    $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    $package = $metadata.packages | Where-Object name -eq 'kborg'
    if (@($package).Count -ne 1) { throw 'Cannot determine project version' }
    $label = "kborg-$($package.version)-$Target"
    $stage = Join-Path $projectRoot "dist/staging/$label"
    $packages = Join-Path $projectRoot 'dist/packages'
    if (Test-Path -LiteralPath $stage) { throw 'Release stage exists; use a clean checkout' }
    New-Item -ItemType Directory -Path $stage,$packages -Force | Out-Null
    $binaryName = if ($Target -like '*windows*') { 'kborg.exe' } else { 'kborg' }
    # Never attribute an arbitrary old target binary to this checkout. A unique
    # empty target directory forces a fresh build with the inspected compiler.
    $buildDirectory = Join-Path $projectRoot ("dist/build/" + [Guid]::NewGuid().ToString('N'))
    if (Test-Path -LiteralPath $buildDirectory) { throw 'Fresh build directory unexpectedly exists' }
    $buildStarted = [DateTime]::UtcNow.ToString('o')
    & cargo build --release --locked --target $Target --target-dir $buildDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Fresh native release build failed' }
    $buildFinished = [DateTime]::UtcNow.ToString('o')
    Assert-CleanSource
    $binary = Join-Path $buildDirectory "$Target/release/$binaryName"
    if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw 'Fresh target binary is missing' }
    $codesigned = $false
    if ($CodesignIdentity) {
        if (!$IsMacOS) { throw 'CodesignIdentity is only valid on macOS' }
        & codesign --force --options runtime --timestamp --sign $CodesignIdentity $binary
        if ($LASTEXITCODE -ne 0) { throw 'codesign failed' }
        & codesign --verify --strict --verbose=2 $binary
        if ($LASTEXITCODE -ne 0) { throw 'codesign verification failed' }
        $codesigned = $true
    }
    $binarySha256 = (Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant()
    $versionText = @(& $binary --version)
    if ($LASTEXITCODE -ne 0 -or $versionText.Count -ne 1 -or $versionText[0] -cne "KBorg $($package.version)") { throw 'Binary version must exactly match Cargo package version' }
    $demoText = & $binary --demo list pods --json
    if ($LASTEXITCODE -ne 0) { throw 'Offline JSON smoke test failed' }
    $demo = ($demoText -join "`n") | ConvertFrom-Json
    if (@($demo.items).Count -lt 1) { throw 'Demo returned no objects' }
    $themes = & $binary theme list
    if ($LASTEXITCODE -ne 0 -or !($themes -match 'catppuccin-mocha')) { throw 'Theme inventory smoke test failed' }
    $metricsText = & $binary --demo metrics pods --json
    if ($LASTEXITCODE -ne 0) { throw 'Offline metrics smoke failed' }
    $metrics = ($metricsText -join "`n") | ConvertFrom-Json
    if (@($metrics.items).Count -lt 1 -or $metrics.source -ne 'demo (deterministic sample)' -or $metrics.items[0].cpu_nanocores -ne 125000000) { throw 'Metrics normalization smoke failed' }
    foreach ($shell in @('bash','zsh','fish','powershell')) {
        $completion = & $binary completions $shell
        if ($LASTEXITCODE -ne 0 -or !($completion -match 'kborg')) { throw "Completion smoke failed: $shell" }
    }
    $smokeConfig = Join-Path $stage 'smoke-config.json'
    & $binary config init $smokeConfig
    if ($LASTEXITCODE -ne 0) { throw 'Default configuration creation smoke failed' }
    & $binary config validate $smokeConfig
    if ($LASTEXITCODE -ne 0) { throw 'Default configuration validation smoke failed' }
    $profile = Get-Content -LiteralPath $smokeConfig -Raw | ConvertFrom-Json
    if ($profile.schema_version -ne 1 -or $profile.profile -cne 'operator' -or $profile.strict_confirmations -cne $false) { throw 'Default configuration does not match the operator profile contract' }
    $capabilities = @($profile.PSObject.Properties | Where-Object Name -Like 'allow_*')
    if ($capabilities.Count -eq 0 -or @($capabilities | Where-Object { $_.Value -cne $true }).Count) { throw 'Default operator capabilities must be enabled' }
    # Check explicit policy constraints as well as the new operator default.
    # --demo keeps these checks offline; --write must not override the profile.
    $profile.profile = 'read-only'
    $profile.strict_confirmations = $true
    $profile | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $smokeConfig -Encoding utf8NoBOM
    & $binary config validate $smokeConfig
    if ($LASTEXITCODE -ne 0) { throw 'Read-only configuration validation smoke failed' }
    $readOnlyDoctor = @(& $binary --demo --config $smokeConfig --write doctor)
    if ($LASTEXITCODE -ne 0 -or !($readOnlyDoctor -ccontains 'mode: read only, strict typed confirmations')) { throw 'Configured read-only mode and strict confirmations must override CLI write mode' }
    Remove-Item -LiteralPath $smokeConfig
    Copy-Item -LiteralPath $binary -Destination (Join-Path $stage $binaryName)
    if ((Get-FileHash -LiteralPath (Join-Path $stage $binaryName) -Algorithm SHA256).Hash.ToLowerInvariant() -cne $binarySha256) { throw 'Built binary changed during packaging smoke checks' }
    foreach ($file in @('README.md','LICENSE','THIRD_PARTY_NOTICES.md','SECURITY.md','docs/RELEASE_NOTES.md')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot $file) -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $projectRoot 'docs') -Destination $stage -Recurse
    $brandAssets = Join-Path $stage 'assets'
    New-Item -ItemType Directory -Path $brandAssets | Out-Null
    foreach ($file in @('kborg-logo.png','LOGO.md')) {
        Copy-Item -LiteralPath (Join-Path $projectRoot "assets/$file") -Destination $brandAssets
    }
    $notices = Join-Path $stage 'theme-notices'
    New-Item -ItemType Directory -Path $notices | Out-Null
    Copy-Item -Path (Join-Path $projectRoot 'assets/upstream/*') -Destination $notices -Recurse
    $inventory = $metadata.packages | Sort-Object name,version | Select-Object name,version,license,source
    ConvertTo-Json -InputObject @($inventory) -Depth 6 | Set-Content -LiteralPath (Join-Path $stage 'DEPENDENCIES.json') -Encoding utf8NoBOM
    # Cargo metadata supplies paths to fetched package sources. Copy only bounded,
    # regular top-level notice files; never evaluate source contents or scripts.
    $licenseRoot = Join-Path $stage 'dependency-licenses'
    New-Item -ItemType Directory -Path $licenseRoot | Out-Null
    $licenseCoverage = @()
    $licenseBytes = 0L
    foreach ($dependency in $metadata.packages | Sort-Object name,version) {
        if ($dependency.name -notmatch '^[a-zA-Z0-9_-]+$' -or $dependency.version -notmatch '^[a-zA-Z0-9.+_-]+$') { throw 'Unsafe dependency notice directory name' }
        $manifestRoot = [IO.Path]::GetFullPath((Split-Path $dependency.manifest_path -Parent))
        $noticeFiles = @(Get-ChildItem -LiteralPath $manifestRoot -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE|AUTHORS|COPYRIGHT)([._-].*)?$' })
        if ($dependency.license_file) {
            $declared = [IO.Path]::GetFullPath((Join-Path $manifestRoot $dependency.license_file))
            $prefix = $manifestRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
            if (!$declared.StartsWith($prefix,[StringComparison]::Ordinal)) { throw 'Declared license file escapes its package directory' }
            $noticeFiles += Get-Item -LiteralPath $declared
        }
        $noticeSource = 'published-package-source'
        if (!$noticeFiles.Count) {
            $fallback = Join-Path $projectRoot "assets/dependency-licenses/$($dependency.name)-$($dependency.version)"
            $provenancePath = Join-Path $fallback 'PROVENANCE.json'
            if (Test-Path -LiteralPath $provenancePath -PathType Leaf) {
                $provenanceItem = Get-Item -LiteralPath $provenancePath
                if ($provenanceItem.Length -gt 64KB -or ($provenanceItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Invalid vendored license provenance file' }
                $noticeProvenance = Get-Content -LiteralPath $provenancePath -Raw | ConvertFrom-Json
                if ($noticeProvenance.name -ne $dependency.name -or $noticeProvenance.version -ne $dependency.version -or $noticeProvenance.sourceCommit -notmatch '^[a-f0-9]{40}$') { throw 'Vendored license identity mismatch' }
                foreach ($record in $noticeProvenance.files) {
                    if ($record.file -notmatch '^(LICENSE|LICENCE|COPYING|NOTICE)([._-][a-zA-Z0-9._-]+)?$' -or $record.sha256 -notmatch '^[a-f0-9]{64}$') { throw 'Invalid vendored license record' }
                    $noticePath = Join-Path $fallback $record.file
                    if ((Get-FileHash -LiteralPath $noticePath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $record.sha256) { throw 'Vendored license checksum mismatch' }
                    $noticeFiles += Get-Item -LiteralPath $noticePath
                }
                $noticeFiles += $provenanceItem
                $noticeSource = 'vendored-upstream-notices-with-provenance'
                $manifestRoot = [IO.Path]::GetFullPath($fallback)
            }
        }
        $noticeFiles = @($noticeFiles | Sort-Object FullName -Unique)
        if ($noticeFiles.Count -gt 32) { throw 'Dependency notice file count exceeds bound' }
        $destination = Join-Path $licenseRoot "$($dependency.name)-$($dependency.version)"
        $copied = @()
        foreach ($notice in $noticeFiles) {
            if ($notice.PSIsContainer -or ($notice.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $notice.Length -gt 1MB) { throw 'Dependency notice must be a bounded regular file' }
            $ancestor = $notice.Directory
            while ($ancestor -and $ancestor.FullName.StartsWith($manifestRoot,[StringComparison]::Ordinal)) {
                if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Dependency notice has a linked parent' }
                $ancestor = $ancestor.Parent
            }
            $bytes = [IO.File]::ReadAllBytes($notice.FullName)
            $licenseBytes += $bytes.Length
            if ($bytes.Length -gt 1MB -or $licenseBytes -gt 32MB) { throw 'Dependency notice byte budget exceeded' }
            New-Item -ItemType Directory -Path $destination -Force | Out-Null
            $output = Join-Path $destination $notice.Name
            if (Test-Path -LiteralPath $output) { throw 'Dependency notice filename collision' }
            [IO.File]::WriteAllBytes($output,$bytes)
            $copied += @{file=$notice.Name;sha256=(Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash.ToLowerInvariant()}
        }
        $licenseCoverage += @{name=$dependency.name;version=$dependency.version;declaredLicense=$dependency.license;status=$(if($copied.Count){'notice-files-included'}else{'manual-review-required'});source=$noticeSource;files=$copied}
    }
    $missingNotices = @($licenseCoverage | Where-Object status -eq 'manual-review-required')
    @{schema=1;complete=($missingNotices.Count -eq 0);verification='Notice file presence and hashes; not a legal compliance certification. See vendored provenance limitations.';scope='All packages in Cargo metadata, including target-specific and development dependencies';totalBytes=$licenseBytes;packages=$licenseCoverage} | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $licenseRoot 'COVERAGE.json') -Encoding utf8NoBOM
    if ($missingNotices.Count) { Write-Warning "$($missingNotices.Count) package sources omit standalone notice files; review dependency-licenses/COVERAGE.json before production distribution. License expressions alone are not complete attribution evidence." }
    $referenceMap = @{}
    foreach ($entry in $metadata.packages) { $referenceMap[$entry.id] = "pkg:cargo/$($entry.name)@$($entry.version)" }
    $components = @($metadata.packages | Where-Object id -ne $package.id | Sort-Object name,version | ForEach-Object {
        $component = [ordered]@{type='library';'bom-ref'=$referenceMap[$_.id];name=$_.name;version=$_.version;purl="pkg:cargo/$($_.name)@$($_.version)"}
        if ($_.license) { $component.licenses = @(@{expression=($_.license -replace '^MIT/Apache-2.0$','MIT OR Apache-2.0')}) }
        $component
    })
    $edges = @($metadata.resolve.nodes | ForEach-Object { @{ref=$referenceMap[$_.id];dependsOn=@($_.dependencies | ForEach-Object { $referenceMap[$_] })} })
    $references = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    if (!$references.Add($referenceMap[$package.id])) { throw 'Application SBOM reference missing' }
    foreach ($component in $components) { if (!$references.Add($component['bom-ref'])) { throw 'Duplicate SBOM component reference' } }
    $edgeReferences = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($edge in $edges) {
        if (!$references.Contains($edge.ref) -or !$edgeReferences.Add($edge.ref)) { throw 'Unknown or duplicate SBOM dependency reference' }
        foreach ($child in $edge.dependsOn) { if (!$references.Contains($child)) { throw 'Unresolved SBOM dependency reference' } }
    }
    $sbom = [ordered]@{bomFormat='CycloneDX';specVersion='1.6';version=1;metadata=@{timestamp=[DateTime]::UtcNow.ToString('o');component=@{type='application';name='kborg';version=$package.version;'bom-ref'=$referenceMap[$package.id]}};components=$components;dependencies=$edges}
    $sbom | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $stage 'SBOM.cdx.json') -Encoding utf8NoBOM
    $build = [ordered]@{schema=2;version=$package.version;target=$Target;sourceCommit=$SourceCommit;sourceTree=((& git rev-parse 'HEAD^{tree}' | Out-String).Trim());cleanCheckoutVerified=$true;freshNativeBuild=$true;rust=$rustDetails;buildStartedAtUtc=$buildStarted;buildFinishedAtUtc=$buildFinished;packagedAtUtc=[DateTime]::UtcNow.ToString('o');binarySha256=$binarySha256;tests=@{packagingSmoke=@('exact-version','demo-pods-json','theme-inventory','demo-metrics','four-shell-completions','default-operator-config','explicit-read-only-strict-config');nativeUnitTests='not run by this packager; consult source-specific acceptance evidence'};signed=$codesigned;signature=$(if($codesigned){'macOS Developer ID codesign, hardened runtime; notarization and Sigstore signatures are added by the release workflow'}else{'none'});productionCertified=$false}
    $build | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $stage 'BUILD.json') -Encoding utf8NoBOM
    Assert-CleanSource
    if ($Target -like '*windows*') {
        $archive = Join-Path $packages "$label.zip"
        if (Test-Path -LiteralPath $archive) { throw 'Archive exists' }
        Compress-Archive -LiteralPath $stage -DestinationPath $archive
    } else {
        $archive = Join-Path $packages "$label.tar.gz"
        if (Test-Path -LiteralPath $archive) { throw 'Archive exists' }
        if ($IsMacOS) {
            # AppleDouble records otherwise copy builder-specific extended
            # attributes into the archive, including a second top-level entry.
            & tar --no-mac-metadata --no-xattrs --no-acls --no-fflags -czf $archive -C (Split-Path $stage -Parent) $label
        } else {
            & tar -czf $archive -C (Split-Path $stage -Parent) $label
        }
        if ($LASTEXITCODE -ne 0) { throw 'Archive creation failed' }
    }
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$checksum  $(Split-Path $archive -Leaf)" | Set-Content -LiteralPath "$archive.sha256" -Encoding utf8NoBOM
    Copy-Item -LiteralPath (Join-Path $stage 'BUILD.json') -Destination (Join-Path $packages "$label.build.json")
    Copy-Item -LiteralPath (Join-Path $stage 'SBOM.cdx.json') -Destination (Join-Path $packages "$label.sbom.cdx.json")
    $provenance = [ordered]@{schemaVersion=2;signed=$false;artifact=(Split-Path $archive -Leaf);sha256=$checksum;sourceRepository='https://github.com/c0dewars/kborg';sourceCommit=$SourceCommit;target=$Target;cargoLockSha256=(Get-FileHash -LiteralPath (Join-Path $projectRoot 'Cargo.lock') -Algorithm SHA256).Hash.ToLowerInvariant();builder='Package-Release.ps1 fresh native build and local smoke checks';verification='Unsigned local record with clean HEAD checks before/after build and packaging; not authenticated provenance, bit-for-bit reproducibility or publisher verification'}
    $provenance | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $packages "$label.provenance.json") -Encoding utf8NoBOM
    Write-Output "Packaged $archive (unsigned)"
} finally {
    if ($buildDirectory -and (Test-Path -LiteralPath $buildDirectory)) {
        Remove-Item -LiteralPath $buildDirectory -Recurse -Force
    }
    Pop-Location
}
