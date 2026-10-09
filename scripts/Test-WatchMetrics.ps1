param([Parameter(Mandatory)][string]$ExpectedServer)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$projectRoot=Split-Path -Parent $PSScriptRoot
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
$kubectl=(Get-Command kubectl).Source
$cargo=(Get-Command cargo).Source
function Invoke-Kube([string[]]$Arguments) {
    $result=& $kubectl --context docker-desktop @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Fixture Kubernetes operation failed.' }
    return $result
}
$config=(Invoke-Kube @('config','view','--minify','--raw','-o','json')) | ConvertFrom-Json
$cluster=$config.clusters[0].cluster
$endpoint=[Uri]$cluster.server
if ($cluster.server -ne $ExpectedServer -or $endpoint.Scheme -ne 'https' -or $endpoint.Host -notin @('127.0.0.1','localhost','kubernetes.docker.internal') -or $endpoint.Port -lt 1024) { throw 'Exact local Docker Desktop endpoint mismatch.' }
if (-not $cluster.'certificate-authority-data') { throw 'Embedded verified CA required.' }
if ($cluster.PSObject.Properties.Name -contains 'insecure-skip-tls-verify' -and $cluster.'insecure-skip-tls-verify') { throw 'Insecure TLS prohibited.' }
$runId=[Guid]::NewGuid().ToString('N').Substring(0,12)
$namespace="k8borg-watch-$runId"
$localDir=Join-Path $projectRoot ".local\watch-$runId"
New-Item -ItemType Directory -Path $localDir -Force | Out-Null
if ($IsWindows) {
    $acl=Get-Acl -LiteralPath $localDir
    $acl.SetAccessRuleProtection($true,$false)
    $identity=[Security.Principal.WindowsIdentity]::GetCurrent().Name
    $acl.SetAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))
    Set-Acl -LiteralPath $localDir -AclObject $acl
}
$created=$false
try {
    @{apiVersion='v1';kind='Namespace';metadata=@{name=$namespace;labels=@{'k8borg.dev/run-id'=$runId;'pod-security.kubernetes.io/enforce'='restricted'}}} | ConvertTo-Json -Depth 10 | & $kubectl --context docker-desktop create -f -
    if ($LASTEXITCODE -ne 0) { throw 'Namespace creation failed.' }
    $created=$true
    $resources=@()
    foreach ($name in @('reader','denied')) { $resources+=@{apiVersion='v1';kind='ServiceAccount';metadata=@{name=$name;namespace=$namespace};automountServiceAccountToken=$false} }
    $resources+=@{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name='reader';namespace=$namespace};rules=@(@{apiGroups=@('');resources=@('configmaps');verbs=@('get','list','watch')})}
    $resources+=@{apiVersion='rbac.authorization.k8s.io/v1';kind='RoleBinding';metadata=@{name='reader';namespace=$namespace};subjects=@(@{kind='ServiceAccount';name='reader';namespace=$namespace});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='Role';name='reader'}}
    $resources+=@{apiVersion='v1';kind='ConfigMap';metadata=@{name='initial';namespace=$namespace};data=@{sample='K8BORG_WATCH_PAYLOAD_CANARY'}}
    @{apiVersion='v1';kind='List';items=$resources} | ConvertTo-Json -Depth 20 | & $kubectl --context docker-desktop apply -f -
    if ($LASTEXITCODE -ne 0) { throw 'Scoped fixture creation failed.' }
    foreach ($name in @('reader','denied')) {
        $token=(Invoke-Kube @('-n',$namespace,'create','token',$name,'--duration=10m')).Trim()
        $fixture=@{apiVersion='v1';kind='Config';'current-context'='k8borg-watch-test';clusters=@(@{name='local';cluster=@{server=$cluster.server;'certificate-authority-data'=$cluster.'certificate-authority-data'}});users=@(@{name=$name;user=@{token=$token}});contexts=@(@{name='k8borg-watch-test';context=@{cluster='local';user=$name;namespace=$namespace}})}
        $path=Join-Path $localDir "$name.kubeconfig"
        $fixture | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath $path -Encoding utf8
        [Environment]::SetEnvironmentVariable("K8BORG_WATCH_$($name.ToUpperInvariant())_CONFIG",$path,'Process')
        $token=$null; $fixture=$null
    }
    $env:K8BORG_WATCH_NAMESPACE=$namespace
    $env:K8BORG_WATCH_RUN_ID=$runId
    $env:K8BORG_WATCH_KUBECTL=$kubectl
    Push-Location $projectRoot
    try {
        & $cargo test --test watch_metrics_acceptance -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'Watch/metrics acceptance failed.' }
    } finally { Pop-Location }
    $report=@{status='passed';runId=$runId;namespace=$namespace;server=$cluster.server;timestamp=(Get-Date).ToUniversalTime().ToString('o');fixture='ConfigMaps only; scoped read/list/watch Role; no cluster components installed'}
    $reportDir=Join-Path $projectRoot 'test-results'
    New-Item -ItemType Directory -Path $reportDir -Force | Out-Null
    $report | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $reportDir "watch-$runId.json") -Encoding utf8
    Write-Host 'Docker Desktop watch/metrics acceptance PASSED.'
} finally {
    if ($created) {
        $owned=(Invoke-Kube @('get','namespace',$namespace,'-o','json')) | ConvertFrom-Json
        if ($owned.metadata.labels.'k8borg.dev/run-id' -ne $runId -or $namespace -ne "k8borg-watch-$runId") { throw 'Cleanup ownership mismatch.' }
        Invoke-Kube @('delete','namespace',$namespace,'--wait=false')
    }
    foreach ($name in @('reader','denied')) {
        $credentialPath=Join-Path $localDir "$name.kubeconfig"
        if (Test-Path -LiteralPath $credentialPath) { Remove-Item -LiteralPath $credentialPath }
        [Environment]::SetEnvironmentVariable("K8BORG_WATCH_$($name.ToUpperInvariant())_CONFIG",$null,'Process')
    }
    foreach ($name in @('K8BORG_WATCH_NAMESPACE','K8BORG_WATCH_RUN_ID','K8BORG_WATCH_KUBECTL')) { [Environment]::SetEnvironmentVariable($name,$null,'Process') }
    $config=$null
}
