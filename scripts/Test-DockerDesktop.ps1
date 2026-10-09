param([Parameter(Mandatory)][string]$ExpectedServer,[switch]$KeepFixtures)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$projectRoot = Split-Path -Parent $PSScriptRoot
$env:Path = [Environment]::GetEnvironmentVariable('Path','User') + ';' + [Environment]::GetEnvironmentVariable('Path','Machine')
$kubectl = (Get-Command kubectl -ErrorAction Stop).Source
$docker = (Get-Command docker -ErrorAction Stop).Source
$cargo = (Get-Command cargo -ErrorAction Stop).Source
function Invoke-Kube {
    param([string[]]$Arguments)
    $result = & $kubectl --context docker-desktop @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Local kubectl operation failed.' }
    return $result
}
$config = (Invoke-Kube @('config','view','--minify','--raw','-o','json')) | ConvertFrom-Json
if ($config.contexts.Count -ne 1 -or $config.contexts[0].name -ne 'docker-desktop') { throw 'Expected only the selected docker-desktop context.' }
$cluster = $config.clusters[0].cluster
$caHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($cluster.'certificate-authority-data'))).ToLowerInvariant()
$endpoint = [Uri]$cluster.server
if ($cluster.server -ne $ExpectedServer -or $endpoint.Scheme -ne 'https' -or $endpoint.Host -notin @('kubernetes.docker.internal','127.0.0.1','localhost') -or $endpoint.Port -lt 1024) { throw 'Docker Desktop endpoint does not match the exact approved local endpoint.' }
if (-not $cluster.'certificate-authority-data') { throw 'Expected embedded Docker Desktop CA.' }
if ($cluster.PSObject.Properties.Name -contains 'insecure-skip-tls-verify' -and $cluster.'insecure-skip-tls-verify') { throw 'Insecure TLS forbidden.' }
$nodes = (Invoke-Kube @('get','nodes','-o','json')) | ConvertFrom-Json
if ($nodes.items.Count -eq 0) { throw 'No local cluster nodes.' }
$runId = [Guid]::NewGuid().ToString('N').Substring(0,12)
$namespace = "kborg-tests-$runId"
$localDir = Join-Path $projectRoot ".local\$runId"
New-Item -ItemType Directory -Path $localDir -Force | Out-Null
if ($IsWindows) {
    $acl = Get-Acl -LiteralPath $localDir
    $acl.SetAccessRuleProtection($true,$false)
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','ContainerInherit,ObjectInherit','None','Allow')
    $acl.SetAccessRule($rule)
    Set-Acl -LiteralPath $localDir -AclObject $acl
}
$created = $false
$passed = $false
try {
    & $docker pull busybox:1.37.0
    if ($LASTEXITCODE -ne 0) { throw 'Fixture image pull failed.' }
    $image = (& $docker image inspect --format '{{index .RepoDigests 0}}' busybox:1.37.0).Trim()
    if ($LASTEXITCODE -ne 0 -or $image -notmatch '@sha256:[a-f0-9]{64}$') { throw 'Cannot resolve pinned fixture image digest.' }
    $nsObject = @{apiVersion='v1';kind='Namespace';metadata=@{name=$namespace;labels=@{'kborg.dev/run-id'=$runId;'pod-security.kubernetes.io/enforce'='restricted'}}}
    $nsObject | ConvertTo-Json -Depth 20 | & $kubectl --context docker-desktop create -f -
    if ($LASTEXITCODE -ne 0) { throw 'Cannot create isolated namespace.' }
    $created = $true
    $resources = @()
    foreach ($name in @('reader','operator','denied')) {
        $resources += @{apiVersion='v1';kind='ServiceAccount';metadata=@{name=$name;namespace=$namespace};automountServiceAccountToken=$false}
    }
    $resources += @{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name='reader';namespace=$namespace};rules=@(
        @{apiGroups=@('');resources=@('pods','services','events','configmaps');verbs=@('get','list','watch')},
        @{apiGroups=@('');resources=@('pods/log');verbs=@('get')},
        @{apiGroups=@('apps');resources=@('deployments');verbs=@('get','list','watch')})}
    $resources += @{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name='operator';namespace=$namespace};rules=@(
        @{apiGroups=@('');resources=@('pods','services','events','configmaps');verbs=@('get','list','watch','delete')},
        @{apiGroups=@('');resources=@('pods/log');verbs=@('get')},
        @{apiGroups=@('apps');resources=@('deployments');verbs=@('get','list','watch','patch','delete')},
        @{apiGroups=@('apps');resources=@('deployments/scale');verbs=@('get','patch')})}
    foreach ($name in @('reader','operator')) {
        $resources += @{apiVersion='rbac.authorization.k8s.io/v1';kind='RoleBinding';metadata=@{name=$name;namespace=$namespace};subjects=@(@{kind='ServiceAccount';name=$name;namespace=$namespace});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='Role';name=$name}}
    }
    $resources += @{apiVersion='apps/v1';kind='Deployment';metadata=@{name='sample';namespace=$namespace};spec=@{replicas=1;selector=@{matchLabels=@{app='kborg-sample'}};template=@{metadata=@{labels=@{app='kborg-sample'}};spec=@{automountServiceAccountToken=$false;securityContext=@{runAsNonRoot=$true;runAsUser=1000;seccompProfile=@{type='RuntimeDefault'}};containers=@(@{name='sample';image=$image;command=@('sh','-c','echo "KBorg fixture"; while true; do echo "INFO local test heartbeat"; sleep 10; done');securityContext=@{allowPrivilegeEscalation=$false;readOnlyRootFilesystem=$true;capabilities=@{drop=@('ALL')}};resources=@{requests=@{cpu='10m';memory='8Mi'};limits=@{cpu='100m';memory='32Mi'}}})}}}}
    $resources += @{apiVersion='v1';kind='ConfigMap';metadata=@{name='canary';namespace=$namespace};data=@{sample='KBORG_TEST_CANARY'}}
    $resources += @{apiVersion='v1';kind='Secret';metadata=@{name='fake-canary';namespace=$namespace};stringData=@{sample='KBORG_TEST_CANARY'}}
    $resources += @{apiVersion='v1';kind='Service';metadata=@{name='delete-me';namespace=$namespace};spec=@{selector=@{app='kborg-sample'};ports=@(@{port=8080;targetPort=8080})}}
    @{apiVersion='v1';kind='List';items=$resources} | ConvertTo-Json -Depth 40 | & $kubectl --context docker-desktop apply -f -
    if ($LASTEXITCODE -ne 0) { throw 'Fixture provisioning failed.' }
    Invoke-Kube @('-n',$namespace,'rollout','status','deployment/sample','--timeout=120s')
    foreach ($name in @('reader','operator','denied')) {
        $token = (Invoke-Kube @('-n',$namespace,'create','token',$name,'--duration=1h')).Trim()
        $testConfig = @{apiVersion='v1';kind='Config';'current-context'='kborg-test';clusters=@(@{name='local';cluster=@{server=$cluster.server;'certificate-authority-data'=$cluster.'certificate-authority-data'}});users=@(@{name=$name;user=@{token=$token}});contexts=@(@{name='kborg-test';context=@{cluster='local';user=$name;namespace=$namespace}})}
        $path = Join-Path $localDir "$name.kubeconfig"
        $testConfig | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $path -Encoding utf8
        [Environment]::SetEnvironmentVariable("KBORG_TEST_$($name.ToUpperInvariant())_CONFIG",$path,'Process')
        $token = $null; $testConfig = $null
    }
    $env:KBORG_CONFIG_DIR = Join-Path $localDir 'app-config'
    Push-Location $projectRoot
    try {
        & $cargo test --test docker_desktop -- --ignored --nocapture
        if ($LASTEXITCODE -ne 0) { throw 'Docker Desktop acceptance tests failed.' }
        & $cargo run -- --kubeconfig (Join-Path $localDir 'reader.kubeconfig') doctor
        if ($LASTEXITCODE -ne 0) { throw 'CLI doctor failed.' }
        & $cargo run -- --kubeconfig (Join-Path $localDir 'reader.kubeconfig') list pods --json
        if ($LASTEXITCODE -ne 0) { throw 'CLI list failed.' }
    } finally { Pop-Location }
    $report = @{status='passed';runId=$runId;namespace=$namespace;image=$image;server=$cluster.server;caSha256=$caHash;kubernetesVersion=($nodes.items[0].status.nodeInfo.kubeletVersion);timestamp=(Get-Date).ToUniversalTime().ToString('o');keptFixtures=[bool]$KeepFixtures}
    $reportDir = Join-Path $projectRoot 'test-results'; New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
    $report | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $reportDir "$runId.json") -Encoding utf8
    $report | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $localDir 'environment.json') -Encoding utf8
    Write-Host 'Local Kubernetes acceptance PASSED.'
    $passed = $true
    if ($KeepFixtures) { Write-Host "Launch reader TUI: cargo run -- --kubeconfig `"$(Join-Path $localDir 'reader.kubeconfig')`" --allow-logs";Write-Host "Fixtures: $namespace (tokens expire in one hour)." }
} finally {
    if ($created -and (-not $KeepFixtures -or -not $passed)) {
        $owned = (Invoke-Kube @('get','namespace',$namespace,'-o','json')) | ConvertFrom-Json
        if ($owned.metadata.labels.'kborg.dev/run-id' -ne $runId) { throw 'Cleanup blocked: namespace ownership mismatch.' }
        Invoke-Kube @('delete','namespace',$namespace,'--wait=false')
        foreach ($name in @('reader','operator','denied')) {
            $credentialPath = Join-Path $localDir "$name.kubeconfig"
            if (Test-Path -LiteralPath $credentialPath) { Remove-Item -LiteralPath $credentialPath }
        }
    }
    foreach ($name in @('reader','operator','denied')) { [Environment]::SetEnvironmentVariable("KBORG_TEST_$($name.ToUpperInvariant())_CONFIG",$null,'Process') }
    $env:KBORG_CONFIG_DIR = $null
    $config = $null
}
