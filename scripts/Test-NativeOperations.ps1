param([Parameter(Mandatory)][string]$ExpectedServer,[switch]$RunWsl)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$projectRoot=Split-Path -Parent $PSScriptRoot
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
$kubectl=(Get-Command kubectl).Source
$docker=(Get-Command docker).Source
function Invoke-Kube([string[]]$Arguments) {
    $result=& $kubectl --context docker-desktop @Arguments
    if ($LASTEXITCODE -ne 0) { throw 'Owned fixture kubectl request failed' }
    return $result
}
$config=Invoke-Kube @('config','view','--minify','--raw','-o','json') | ConvertFrom-Json
if ($config.contexts.Count -ne 1 -or $config.contexts[0].name -ne 'docker-desktop') {throw 'Expected selected docker-desktop context'}
$cluster=$config.clusters[0].cluster
$endpoint=[Uri]$cluster.server
if ($cluster.server -ne $ExpectedServer -or $endpoint.Scheme -ne 'https' -or $endpoint.Host -notin @('localhost','127.0.0.1','kubernetes.docker.internal') -or $endpoint.Port -lt 1024) {throw 'Exact local TLS endpoint mismatch'}
if (-not $cluster.'certificate-authority-data') {throw 'Missing CA'}
if ($cluster.PSObject.Properties.Name -contains 'insecure-skip-tls-verify' -and $cluster.'insecure-skip-tls-verify') {throw 'Insecure TLS forbidden'}
$caHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($cluster.'certificate-authority-data'))).ToLowerInvariant()
$runId=[Guid]::NewGuid().ToString('N').Substring(0,12)
$namespace="kborg-ops-$runId"
$localDir=Join-Path $projectRoot ".local\ops-$runId"
New-Item -ItemType Directory -Path $localDir | Out-Null
if ($IsWindows) {
    $acl=Get-Acl -LiteralPath $localDir
    $acl.SetAccessRuleProtection($true,$false)
    $identity=[Security.Principal.WindowsIdentity]::GetCurrent().Name
    $acl.SetAccessRule([Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))
    Set-Acl -LiteralPath $localDir -AclObject $acl
}
$created=$false
$passed=$false
try {
    & $docker pull busybox:1.37.0
    if ($LASTEXITCODE -ne 0) {throw 'Image pull failed'}
    $image=(& $docker image inspect --format '{{index .RepoDigests 0}}' busybox:1.37.0).Trim()
    if ($image -notmatch '@sha256:[a-f0-9]{64}$') {throw 'Missing pinned digest'}
    @{apiVersion='v1';kind='Namespace';metadata=@{name=$namespace;labels=@{'kborg.dev/run-id'=$runId;'pod-security.kubernetes.io/enforce'='restricted'}}} | ConvertTo-Json -Depth 20 | & $kubectl --context docker-desktop create -f -
    if ($LASTEXITCODE -ne 0) {throw 'Namespace creation failed'}
    $created=$true
    $resources=@()
    foreach ($name in @('reader','operator','fixture')) {
        $resources+=@{apiVersion='v1';kind='ServiceAccount';metadata=@{name=$name;namespace=$namespace};automountServiceAccountToken=$false}
        $rules=@(@{apiGroups=@('');resources=@('pods');verbs=@('get')})
        if ($name -eq 'fixture') {$rules=@(@{apiGroups=@('');resources=@('pods');verbs=@('get','create','delete')})}
        if ($name -eq 'operator') {$rules+=@{apiGroups=@('');resources=@('pods/exec','pods/attach','pods/portforward');verbs=@('get','create')}}
        $resources+=@{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name=$name;namespace=$namespace};rules=$rules}
        $resources+=@{apiVersion='rbac.authorization.k8s.io/v1';kind='RoleBinding';metadata=@{name=$name;namespace=$namespace};subjects=@(@{kind='ServiceAccount';name=$name;namespace=$namespace});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='Role';name=$name}}
    }
    $resources+=@{apiVersion='v1';kind='Pod';metadata=@{name='server';namespace=$namespace};spec=@{automountServiceAccountToken=$false;securityContext=@{runAsNonRoot=$true;runAsUser=1000;seccompProfile=@{type='RuntimeDefault'}};volumes=@(@{name='www';emptyDir=@{medium='Memory';sizeLimit='1Mi'}});containers=@(@{name='server';image=$image;command=@('sh','-c','printf KBORG_FORWARD_OK > /www/index.html; exec httpd -f -p 8080 -h /www');volumeMounts=@(@{name='www';mountPath='/www'});securityContext=@{allowPrivilegeEscalation=$false;readOnlyRootFilesystem=$true;capabilities=@{drop=@('ALL')}};resources=@{requests=@{cpu='10m';memory='8Mi'};limits=@{cpu='100m';memory='32Mi'}}})}}
    $resources[-1].spec.securityContext.fsGroup=1000
    @{apiVersion='v1';kind='List';items=$resources}|ConvertTo-Json -Depth 40 | & $kubectl --context docker-desktop apply -f -
    if ($LASTEXITCODE -ne 0) {throw 'Fixture creation failed'}
    Invoke-Kube @('-n',$namespace,'wait','--for=condition=Ready','pod/server','--timeout=120s')
    foreach ($name in @('reader','operator','fixture')) {
        $token=(Invoke-Kube @('-n',$namespace,'create','token',$name,'--duration=10m')).Trim()
        $cfg=@{apiVersion='v1';kind='Config';'current-context'='native-test';clusters=@(@{name='local';cluster=@{server=$cluster.server;'certificate-authority-data'=$cluster.'certificate-authority-data'}});users=@(@{name=$name;user=@{token=$token}});contexts=@(@{name='native-test';context=@{cluster='local';user=$name;namespace=$namespace}})}
        $path=Join-Path $localDir "$name.kubeconfig"
        $cfg|ConvertTo-Json -Depth 20|Set-Content -LiteralPath $path
        [Environment]::SetEnvironmentVariable("KBORG_OPS_$($name.ToUpperInvariant())_CONFIG",$path,'Process')
        $token=$null;$cfg=$null
    }
    $env:KBORG_CONFIG_DIR=Join-Path $localDir 'app-config'
    Push-Location $projectRoot
    try { & cargo test --locked --test native_operations -- --ignored --nocapture; if ($LASTEXITCODE -ne 0) {throw 'Native operation tests failed'} } finally {Pop-Location}
    if ($RunWsl) {
        $linuxProject='/mnt/c/Users/Garcia/Desktop/projects/k8borg'
        $linuxLocal="$linuxProject/.local/ops-$runId"
        $command="cd $linuxProject && export PATH=/home/garcia/.cargo/bin:/usr/local/bin:/usr/bin:/bin && export CARGO_TARGET_DIR=/home/garcia/.cache/k8borg-target && export KBORG_OPS_OPERATOR_CONFIG=$linuxLocal/operator.kubeconfig KBORG_OPS_READER_CONFIG=$linuxLocal/reader.kubeconfig KBORG_OPS_FIXTURE_CONFIG=$linuxLocal/fixture.kubeconfig KBORG_CONFIG_DIR=/home/garcia/.cache/kborg-native-$runId && cargo test --locked --test native_operations -- --ignored --nocapture"
        & wsl.exe -d Ubuntu-26.04 -- bash -lc $command
        if ($LASTEXITCODE -ne 0) {throw 'WSL native operations acceptance failed'}
    }
    $passed=$true
} finally {
    if ($created) {
        $owned=Invoke-Kube @('get','namespace',$namespace,'-o','json')|ConvertFrom-Json
        if ($owned.metadata.labels.'kborg.dev/run-id' -ne $runId) {throw 'Cleanup blocked by ownership mismatch'}
        Invoke-Kube @('delete','namespace',$namespace,'--wait=true','--timeout=90s')
    }
    foreach ($name in @('reader','operator','fixture')) {
        $path=Join-Path $localDir "$name.kubeconfig"
        if (Test-Path -LiteralPath $path) {Remove-Item -LiteralPath $path}
        [Environment]::SetEnvironmentVariable("KBORG_OPS_$($name.ToUpperInvariant())_CONFIG",$null,'Process')
    }
    $env:KBORG_CONFIG_DIR=$null
    $config=$null
    $report=@{passed=$passed;wsl=[bool]$RunWsl;runId=$runId;namespace=$namespace;server=$ExpectedServer;caSha256=$caHash;cleanup=$true;timestamp=(Get-Date).ToUniversalTime().ToString('o')}
    New-Item -ItemType Directory -Force (Join-Path $projectRoot 'test-results')|Out-Null
    $report|ConvertTo-Json|Set-Content (Join-Path $projectRoot "test-results\native-$runId.json")
}


