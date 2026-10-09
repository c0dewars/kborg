param([ValidateSet('1.34','1.35')][string[]]$Versions=@('1.34','1.35'))
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$projectRoot=Split-Path -Parent $PSScriptRoot
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
$docker=(Get-Command docker -ErrorAction Stop).Source
$kubectl=(Get-Command kubectl -ErrorAction Stop).Source
$kind=Join-Path $projectRoot '.local/tools/kind.exe'
$kindSha='4b22adaa135368c5a465d56bbd8e520cbea87272a06ca00b6078e7b81515c9fc'
if (-not (Test-Path -LiteralPath $kind)) {
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $kind) -Force)
    Invoke-WebRequest -Uri 'https://github.com/kubernetes-sigs/kind/releases/download/v0.33.0/kind-windows-amd64' -OutFile $kind
}
if ((Get-FileHash -LiteralPath $kind -Algorithm SHA256).Hash.ToLowerInvariant() -ne $kindSha) { throw 'Official pinned kind checksum mismatch.' }
$daemon=(& $docker info --format '{{json .}}') | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $daemon.MemTotal -lt 10GB -or $daemon.NCPU -lt 4) { throw 'Need at least10GiB and4CPUs allocated to the existing Docker daemon.' }
$images=@{
    '1.34'='kindest/node:v1.34.11@sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d'
    '1.35'='kindest/node:v1.35.8@sha256:07b2536e30b803ed61d1677a79df6115f798ce64c80f9e22f6ed45afd09323c0'
}
foreach ($minor in $Versions) {
    $patchVersion=($images[$minor].Split(':')[1]).Split('@')[0]
    $kubectl=Join-Path $projectRoot ".local/tools/kubectl-$patchVersion.exe"
    $kubectlChecksum="$kubectl.sha256"
    if (-not (Test-Path -LiteralPath $kubectl)) {
        Invoke-WebRequest -Uri "https://dl.k8s.io/release/$patchVersion/bin/windows/amd64/kubectl.exe" -OutFile $kubectl
        Invoke-WebRequest -Uri "https://dl.k8s.io/release/$patchVersion/bin/windows/amd64/kubectl.exe.sha256" -OutFile $kubectlChecksum
    }
    $kubectlSha=(Get-Content -LiteralPath $kubectlChecksum -Raw).Trim().ToLowerInvariant()
    if ($kubectlSha -notmatch '^[a-f0-9]{64}$' -or (Get-FileHash -LiteralPath $kubectl -Algorithm SHA256).Hash.ToLowerInvariant() -ne $kubectlSha) { throw 'Official version-matched kubectl checksum mismatch.' }
    $runId=[Guid]::NewGuid().ToString('N').Substring(0,12)
    $name="k8borg-compat-$runId"
    $directory=Join-Path $projectRoot ".local/$name"
    [void](New-Item -ItemType Directory -Path $directory)
    $acl=Get-Acl -LiteralPath $directory
    $acl.SetAccessRuleProtection($true,$false)
    $rule=[Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.WindowsIdentity]::GetCurrent().Name,'FullControl','ContainerInherit,ObjectInherit','None','Allow')
    $acl.SetAccessRule($rule)
    Set-Acl -LiteralPath $directory -AclObject $acl
    $admin=Join-Path $directory 'admin.kubeconfig'
    $server=$null
    $ca=$null
    try {
        & $kind create cluster --name $name --image $images[$minor] --kubeconfig $admin --wait 120s
        if ($LASTEXITCODE -ne 0) { throw 'Owned kind cluster provisioning failed.' }
        & $docker update --memory 4g --memory-swap 4g --cpus 4 "$name-control-plane"
        if ($LASTEXITCODE -ne 0) { throw 'Cannot enforce compatibility cluster resource cap.' }
        $configuration=(& $kubectl --kubeconfig $admin --context "kind-$name" config view --minify --raw -o json) | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect owned kubeconfig.' }
        $server=$configuration.clusters[0].cluster.server
        $endpoint=[Uri]$server
        if ($endpoint.Scheme -ne 'https' -or $endpoint.Host -notin @('127.0.0.1','localhost') -or $endpoint.Port -lt 1024) { throw 'Expected loopback TLS endpoint.' }
        $ca=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($configuration.clusters[0].cluster.'certificate-authority-data'))).ToLowerInvariant()
        $actual=(& $kubectl --kubeconfig $admin --context "kind-$name" version -o json) | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or $actual.serverVersion.gitVersion -notlike "v$minor.*") { throw 'Server minor version mismatch.' }
        & "$PSScriptRoot/Test-KindFixture.ps1" -AdminConfig $admin -ExpectedContext "kind-$name" -ExpectedServer $server -KubectlPath $kubectl
        if (-not $?) { throw 'Compatibility acceptance failed.' }
        $evidence=[ordered]@{schema=1;status='passed';kubernetesVersion=$actual.serverVersion.gitVersion;nodeImage=$images[$minor];kindVersion='v0.33.0';kindBinarySha256=$kindSha;utc=[DateTime]::UtcNow.ToString('o');suite='cargo test --locked;tests/docker_desktop.rs isolated_cluster_acceptance;CLI doctor;CLI JSON pods;DiagnosticProbe CRD resources/list/get/local minimized Copilot export';diagnosticCrdChecksPassed=$true;clientBinarySha256=(Get-FileHash -LiteralPath (Join-Path $projectRoot 'target/debug/k8borg.exe') -Algorithm SHA256).Hash.ToLowerInvariant();kubectlVersion=$actual.clientVersion.gitVersion;clientPlatform='Windows native Rust';isolatedCluster=$true;maxConcurrentCompatibilityClusters=1;nodeMemoryLimitBytes=4GB;nodeCpuLimit=4;cleanupVerified=$false}
    } finally {
        $current=(& $docker info --format '{{json .}}') | ConvertFrom-Json
        if ($LASTEXITCODE -ne 0 -or $current.ID -ne $daemon.ID) { throw 'Cleanup blocked: Docker daemon identity changed.' }
        $owned=(& $docker ps -a --filter "label=io.x-k8s.kind.cluster=$name" --format '{{.Names}}')
        if ($owned) {
            if (@($owned).Count -ne 1 -or $owned -ne "$name-control-plane") { throw 'Cleanup blocked: unexpected cluster node ownership.' }
            if ($server -and $ca) {
                $check=(& $kubectl --kubeconfig $admin --context "kind-$name" config view --minify --raw -o json) | ConvertFrom-Json
                $checkCa=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($check.clusters[0].cluster.'certificate-authority-data'))).ToLowerInvariant()
                if ($LASTEXITCODE -ne 0 -or $check.clusters[0].cluster.server -ne $server -or $checkCa -ne $ca) { throw 'Cleanup blocked: endpoint or CA changed.' }
            }
            & $kind delete cluster --name $name --kubeconfig $admin
            if ($LASTEXITCODE -ne 0) { throw 'Owned kind cluster deletion failed.' }
            $remaining=(& $docker ps -a --filter "label=io.x-k8s.kind.cluster=$name" --format '{{.Names}}')
            if ($remaining) { throw 'Cleanup left owned cluster nodes running.' }
        }
        if (Test-Path -LiteralPath $admin) { Remove-Item -LiteralPath $admin }
    }
    $evidence.cleanupVerified=$true
    $outputDirectory=Join-Path $projectRoot 'docs/evidence'
    [void](New-Item -ItemType Directory -Path $outputDirectory -Force)
    $evidence | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $outputDirectory "compat-kubernetes-$minor.json") -Encoding utf8
    $evidence | ConvertTo-Json -Depth 10
}

