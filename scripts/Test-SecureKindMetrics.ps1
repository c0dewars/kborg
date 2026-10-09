param()
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest
$root=Split-Path -Parent $PSScriptRoot
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
$docker=(Get-Command docker).Source
$kind=Join-Path $root '.local/tools/kind.exe'
$kubectl=Join-Path $root '.local/tools/kubectl-v1.34.11.exe'
$binary=Join-Path $root 'target/debug/kborg.exe'
$kubectlSha=(Get-Content -LiteralPath ($kubectl+'.sha256') -Raw).Trim().ToLowerInvariant()
if ((Get-FileHash $kubectl -Algorithm SHA256).Hash.ToLowerInvariant() -ne $kubectlSha -or $kubectlSha -notmatch '^[a-f0-9]{64}$') {throw 'Version-matched kubectl checksum mismatch'}
if ((Get-FileHash $kind -Algorithm SHA256).Hash.ToLowerInvariant() -ne '4b22adaa135368c5a465d56bbd8e520cbea87272a06ca00b6078e7b81515c9fc') {throw 'kind checksum mismatch'}
$nodeImage='kindest/node:v1.34.11@sha256:44e222ee2132dab25ff87301682f89eb82c7880ea3a1bf543bfe9708fd08d67d'
$metricsImage='registry.k8s.io/metrics-server/metrics-server:v0.8.1@sha256:b2d2efaf5ac3b366ed0f839d2412a2c4279d4fc2a2a733f12c52133faed36c41'
$busyImage='busybox@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e'
$daemon=(& $docker info --format '{{json .}}')|ConvertFrom-Json
if ($daemon.MemTotal -lt 10GB -or $daemon.NCPU -lt 4) {throw 'Insufficient Docker VM capacity'}
$run=[Guid]::NewGuid().ToString('N').Substring(0,12)
$name="kborg-metrics-$run"
$node="$name-control-plane"
$context="kind-$name"
$dir=Join-Path $root ".local/$name"
New-Item -ItemType Directory -Path $dir | Out-Null
$acl=Get-Acl -LiteralPath $dir
$acl.SetAccessRuleProtection($true,$false)
$acl.SetAccessRule([Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.WindowsIdentity]::GetCurrent().Name,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))
Set-Acl -LiteralPath $dir -AclObject $acl
$admin=Join-Path $dir 'admin.kubeconfig'
$reader=Join-Path $dir 'reader.kubeconfig'
$watchProcess=$null
$kindProcess=$null
$report=$null
function K([string[]]$Arguments) {
    $value=& $kubectl --kubeconfig $admin --context $context @Arguments
    if ($LASTEXITCODE -ne 0) {throw 'Owned cluster kubectl operation failed'}
    return $value
}
function D([string[]]$Arguments) {
    $value=& $docker @Arguments
    if ($LASTEXITCODE -ne 0) {throw 'Owned cluster Docker operation failed'}
    return $value
}
function Apply($object) {
    $object|ConvertTo-Json -Depth 45|& $kubectl --kubeconfig $admin --context $context apply -f -
    if ($LASTEXITCODE -ne 0) {throw 'Owned cluster object apply failed'}
}
try {
    $kindConfig=Join-Path $dir 'kind.yaml'
    @('kind: Cluster','apiVersion: kind.x-k8s.io/v1alpha4','nodes:','- role: control-plane','kubeadmConfigPatches:','- |','  kind: KubeletConfiguration','  serverTLSBootstrap: true')|Set-Content -LiteralPath $kindConfig
    $kindProcess=Start-Process -FilePath $kind -ArgumentList @('create','cluster','--name',$name,'--image',$nodeImage,'--kubeconfig',('"'+$admin+'"'),'--config',('"'+$kindConfig+'"'),'--wait','120s') -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $dir 'kind-create.stdout') -RedirectStandardError (Join-Path $dir 'kind-create.stderr')
    $capDeadline=[DateTime]::UtcNow.AddSeconds(90)
    $capped=$false
    do {
        $createdNodes=@(& $docker ps -a --filter "label=io.x-k8s.kind.cluster=$name" --format '{{.Names}}')
        if ($createdNodes.Count -eq 1 -and $createdNodes[0] -eq $node) {D @('update','--memory','4g','--memory-swap','4g','--cpus','4',$node);$capped=$true;break}
        if ($kindProcess.HasExited) {throw 'kind creation exited before owned resource-capped node appeared'}
        Start-Sleep -Seconds 1
    } while ([DateTime]::UtcNow -lt $capDeadline)
    if (-not $capped) {throw 'Owned node resource cap deadline exceeded'}
    $createDeadline=[DateTime]::UtcNow.AddSeconds(180)
    while (-not $kindProcess.HasExited -and [DateTime]::UtcNow -lt $createDeadline) {Start-Sleep -Seconds 1}
    if (-not $kindProcess.HasExited) {throw 'Owned kind creation deadline exceeded'}
    if ($kindProcess.ExitCode -ne 0) {throw 'Owned kind provisioning failed'}
    $kindProcess=$null
    D @('update','--memory','4g','--memory-swap','4g','--cpus','4',$node)
    $configuration=(K @('config','view','--minify','--raw','-o','json'))|ConvertFrom-Json
    $cluster=$configuration.clusters[0].cluster
    if (([Uri]$cluster.server).Host -notin @('127.0.0.1','localhost') -or ([Uri]$cluster.server).Scheme -ne 'https') {throw 'Loopback TLS endpoint required'}
    $ca=$cluster.'certificate-authority-data'
    $nodes=(K @('get','nodes','-o','json'))|ConvertFrom-Json
    if ($nodes.items.Count -ne 1 -or $nodes.items[0].metadata.name -ne $node -or $nodes.items[0].status.nodeInfo.kubeletVersion -ne 'v1.34.11') {throw 'Owned node identity/version mismatch'}
    $ip=($nodes.items[0].status.addresses|Where-Object type -eq 'InternalIP').address
    $deadline=[DateTime]::UtcNow.AddSeconds(60)
    $csr=$null
    do {
        $csrs=(K @('get','csr','-o','json'))|ConvertFrom-Json
        $matching=@($csrs.items|Where-Object {$_.spec.signerName -eq 'kubernetes.io/kubelet-serving' -and $_.spec.username -eq "system:node:$node"})
        if ($matching.Count -ge 1) {$csr=$matching | Sort-Object { $_.metadata.creationTimestamp } -Descending | Select-Object -First 1;break}
        Start-Sleep -Seconds 2
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $csr) {throw 'Owned kubelet serving CSR missing'}
    if (@($csr.spec.usages|Where-Object {$_ -notin @('digital signature','key encipherment','server auth')}).Count -ne 0) {throw 'Unexpected CSR usages'}
    $csrPath=Join-Path $dir 'kubelet.csr'
    [IO.File]::WriteAllBytes($csrPath,[Convert]::FromBase64String($csr.spec.request))
    D @('cp',$csrPath,"${node}:/var/lib/kborg-kubelet.csr")
    $csrText=(D @('exec',$node,'openssl','req','-in','/var/lib/kborg-kubelet.csr','-noout','-text')) -join "`n"
    $csrText | Set-Content -LiteralPath (Join-Path $dir 'csr-public-inspection.txt')
    if ($csrText -notmatch ('CN\s*=\s*'+[Regex]::Escape("system:node:$node")) -or $csrText -notmatch [Regex]::Escape("DNS:$node") -or $csrText -notmatch [Regex]::Escape("IP Address:$ip")) {throw 'Kubelet CSR subject/SAN mismatch'}
    $sanLine=[Regex]::Match($csrText,'X509v3 Subject Alternative Name:\s*\r?\n\s*([^\r\n]+)').Groups[1].Value.Trim()
    $sans=@($sanLine -split ',\s*' | Sort-Object)
    $expectedSans=@("DNS:$node","IP Address:$ip")|Sort-Object
    if (($sans -join '|') -ne ($expectedSans -join '|')) {throw 'Unexpected additional kubelet serving CSR SANs'}
    K @('certificate','approve',$csr.metadata.name)
    D @('exec',$node,'openssl','req','-new','-newkey','rsa:2048','-nodes','-keyout','/var/lib/kborg-metrics.key','-out','/var/lib/kborg-metrics.csr','-subj','/CN=metrics-server.kube-system.svc')
    $ext=Join-Path $dir 'metrics-ext.cnf'
    "subjectAltName=DNS:metrics-server.kube-system.svc,DNS:metrics-server.kube-system.svc.cluster.local`nextendedKeyUsage=serverAuth`nkeyUsage=digitalSignature,keyEncipherment"|Set-Content -LiteralPath $ext
    D @('cp',$ext,"${node}:/var/lib/kborg-metrics-ext.cnf")
    D @('exec',$node,'openssl','x509','-req','-in','/var/lib/kborg-metrics.csr','-CA','/etc/kubernetes/pki/ca.crt','-CAkey','/etc/kubernetes/pki/ca.key','-CAcreateserial','-days','1','-extfile','/var/lib/kborg-metrics-ext.cnf','-out','/var/lib/kborg-metrics.crt')
    D @('cp',"${node}:/var/lib/kborg-metrics.crt",(Join-Path $dir 'metrics.crt'))
    D @('cp',"${node}:/var/lib/kborg-metrics.key",(Join-Path $dir 'metrics.key'))
    [IO.File]::WriteAllBytes((Join-Path $dir 'ca.crt'),[Convert]::FromBase64String($ca))
    K @('-n','kube-system','create','secret','tls','kborg-metrics-tls',"--cert=$(Join-Path $dir 'metrics.crt')","--key=$(Join-Path $dir 'metrics.key')")
    K @('-n','kube-system','create','configmap','kborg-kubelet-ca',"--from-file=ca.crt=$(Join-Path $dir 'ca.crt')")
    $source=Join-Path $dir 'components-v0.8.1.yaml'
    Invoke-WebRequest 'https://github.com/kubernetes-sigs/metrics-server/releases/download/v0.8.1/components.yaml' -OutFile $source
    $manifest=Get-Content -LiteralPath $source -Raw
    if (-not $manifest.Contains('  insecureSkipTLSVerify: true')) {throw 'Pinned manifest TLS anchor changed'}
    $manifest=$manifest.Replace('  insecureSkipTLSVerify: true',"  insecureSkipTLSVerify: false`n  caBundle: $ca").Replace('registry.k8s.io/metrics-server/metrics-server:v0.8.1',$metricsImage)
    $manifest=$manifest.Replace('        - --secure-port=10250',"        - --secure-port=10250`n        - --tls-cert-file=/certs/tls.crt`n        - --tls-private-key-file=/certs/tls.key`n        - --kubelet-certificate-authority=/kubelet-ca/ca.crt")
    $manifest=$manifest.Replace('      nodeSelector:',"        - mountPath: /certs`n          name: serving-cert`n          readOnly: true`n        - mountPath: /kubelet-ca`n          name: kubelet-ca`n          readOnly: true`n      nodeSelector:")
    $manifest=$manifest.Replace('      - emptyDir: {}',"      - name: serving-cert`n        secret:`n          secretName: kborg-metrics-tls`n      - name: kubelet-ca`n        configMap:`n          name: kborg-kubelet-ca`n      - emptyDir: {}")
    if ($manifest.Contains('--kubelet-insecure-tls') -or $manifest.Contains('insecureSkipTLSVerify: true')) {throw 'Insecure metrics transport prohibited'}
    $secure=Join-Path $dir 'metrics-secure.yaml'
    $manifest|Set-Content -LiteralPath $secure
    D @('pull',$metricsImage)
    K @('apply','-f',$secure)
    K @('-n','kube-system','rollout','status','deployment/metrics-server','--timeout=150s')
    K @('wait','--for=condition=Available','apiservice/v1beta1.metrics.k8s.io','--timeout=90s')
    $namespace="kborg-metrics-fixture-$run"
    Apply @{apiVersion='v1';kind='Namespace';metadata=@{name=$namespace;labels=@{'kborg.dev/run-id'=$run;'pod-security.kubernetes.io/enforce'='restricted'}}}
    Apply @{apiVersion='v1';kind='ServiceAccount';metadata=@{name='reader';namespace=$namespace};automountServiceAccountToken=$false}
    Apply @{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name='reader';namespace=$namespace};rules=@(@{apiGroups=@('');resources=@('configmaps');verbs=@('get','list','watch')},@{apiGroups=@('metrics.k8s.io');resources=@('pods');verbs=@('get','list')})}
    Apply @{apiVersion='rbac.authorization.k8s.io/v1';kind='RoleBinding';metadata=@{name='reader';namespace=$namespace};subjects=@(@{kind='ServiceAccount';name='reader';namespace=$namespace});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='Role';name='reader'}}
    Apply @{apiVersion='rbac.authorization.k8s.io/v1';kind='ClusterRole';metadata=@{name="kborg-metrics-reader-$run"};rules=@(@{apiGroups=@('metrics.k8s.io');resources=@('nodes');verbs=@('get','list')})}
    Apply @{apiVersion='rbac.authorization.k8s.io/v1';kind='ClusterRoleBinding';metadata=@{name="kborg-metrics-reader-$run"};subjects=@(@{kind='ServiceAccount';name='reader';namespace=$namespace});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='ClusterRole';name="kborg-metrics-reader-$run"}}
    Apply @{apiVersion='v1';kind='ConfigMap';metadata=@{name='watch';namespace=$namespace;labels=@{revision='initial'}};data=@{sample='KBORG_METRICS_PAYLOAD_CANARY'}}
    $containers=@()
    foreach ($container in @('first','second')) {$containers+=@{name=$container;image=$busyImage;command=@('sh','-c','while true; do sleep 1; done');securityContext=@{allowPrivilegeEscalation=$false;readOnlyRootFilesystem=$true;capabilities=@{drop=@('ALL')}};resources=@{requests=@{cpu='10m';memory='8Mi'};limits=@{cpu='100m';memory='32Mi'}}}}
    Apply @{apiVersion='v1';kind='Pod';metadata=@{name='sample';namespace=$namespace};spec=@{automountServiceAccountToken=$false;securityContext=@{runAsNonRoot=$true;runAsUser=1000;seccompProfile=@{type='RuntimeDefault'}};containers=$containers}}
    K @('-n',$namespace,'wait','--for=condition=Ready','pod/sample','--timeout=120s')
    $token=(K @('-n',$namespace,'create','token','reader','--duration=10m')).Trim()
    @{apiVersion='v1';kind='Config';'current-context'='metrics-reader';clusters=@(@{name='local';cluster=@{server=$cluster.server;'certificate-authority-data'=$ca}});users=@(@{name='reader';user=@{token=$token}});contexts=@(@{name='metrics-reader';context=@{cluster='local';user='reader';namespace=$namespace}})}|ConvertTo-Json -Depth 20|Set-Content -LiteralPath $reader
    $token=$null
    $deadline=[DateTime]::UtcNow.AddSeconds(90)
    do {
        $podText=& $binary --kubeconfig $reader --context metrics-reader metrics pods --json
        if ($LASTEXITCODE -ne 0) {throw 'Application pod metrics failed'}
        $pods=($podText -join "`n")|ConvertFrom-Json
        if (@($pods.items).Count -eq 2) {break}
        Start-Sleep -Seconds 5
    } while ([DateTime]::UtcNow -lt $deadline)
    if (@($pods.items).Count -ne 2 -or @($pods.items|Where-Object {$_.name -ne 'sample' -or $_.namespace -ne $namespace}).Count -ne 0) {throw 'Expected both container metrics rows'}
    $nodeText=& $binary --kubeconfig $reader --context metrics-reader metrics nodes --json
    if ($LASTEXITCODE -ne 0) {throw 'Application node metrics failed'}
    $nodeMetrics=($nodeText -join "`n")|ConvertFrom-Json
    if (@($nodeMetrics.items).Count -ne 1 -or $nodeMetrics.items[0].name -ne $node) {throw 'Expected owned node metrics'}
    foreach ($row in @($pods.items)+@($nodeMetrics.items)) {if ($row.cpu_nanocores -lt 0 -or $row.memory_bytes -le 0 -or -not $row.timestamp -or -not $row.window) {throw 'Invalid normalized measurement'}}
    $podText | Set-Content -LiteralPath (Join-Path $dir 'pod-metrics.json')
    $nodeText | Set-Content -LiteralPath (Join-Path $dir 'node-metrics.json')
    $stdout=Join-Path $dir 'watch.jsonl';$stderr=Join-Path $dir 'watch.stderr'
    $watchProcess=Start-Process -FilePath $binary -ArgumentList @('--kubeconfig',('"'+$reader+'"'),'--context','metrics-reader','watch','configmaps','--json') -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    Start-Sleep -Seconds 2
    K @('-n',$namespace,'label','configmap/watch','revision=live','--overwrite')
    Start-Sleep -Seconds 2
    $watchProcess.Kill();$watchProcess.WaitForExit();$watchProcess=$null
$kindProcess=$null
    $watchText=Get-Content -LiteralPath $stdout -Raw
    if ($watchText.Contains('KBORG_METRICS_PAYLOAD_CANARY')) {throw 'Watch payload leak'}
    $records=@(Get-Content -LiteralPath $stdout|Where-Object {$_}|ForEach-Object {$_|ConvertFrom-Json})
    if (@($records|Where-Object {@($_.items|Where-Object {$_.metadata.name -eq 'watch' -and $_.metadata.labels.revision -eq 'live'}).Count -gt 0}).Count -eq 0) {throw 'Real minimum-version watch update missing'}
    $api=(K @('get','apiservice','v1beta1.metrics.k8s.io','-o','json'))|ConvertFrom-Json
    if (($api.spec.PSObject.Properties.Name -contains 'insecureSkipTLSVerify' -and $api.spec.insecureSkipTLSVerify) -or $api.spec.caBundle -ne $ca) {throw 'Aggregation TLS verification lost'}
    $report=@{status='passed';runId=$run;kubernetesVersion='v1.34.11';kindNodeImage=$nodeImage;metricsServerImage=$metricsImage;kubeletTLSVerified=$true;aggregationTLSVerified=$true;podContainerRows=@($pods.items).Count;nodeRows=@($nodeMetrics.items).Count;realWatchUpdate=$true;sourceBinarySha256=(Get-FileHash $binary -Algorithm SHA256).Hash.ToLowerInvariant();timestamp=[DateTime]::UtcNow.ToString('o');nodeMemoryLimitBytes=4GB;nodeCpuLimit=4;cleanupVerified=$false}
} finally {
    if ($kindProcess -and -not $kindProcess.HasExited) {$kindProcess.Kill();$kindProcess.WaitForExit()}
    if ($watchProcess -and -not $watchProcess.HasExited) {$watchProcess.Kill();$watchProcess.WaitForExit()}
    $current=(& $docker info --format '{{json .}}')|ConvertFrom-Json
    if ($current.ID -ne $daemon.ID) {throw 'Cleanup blocked: daemon changed'}
    $owned=@(& $docker ps -a --filter "label=io.x-k8s.kind.cluster=$name" --format '{{.Names}}')
    if ($owned.Count -gt 0) {
        if ($owned.Count -ne 1 -or $owned[0] -ne $node) {throw 'Cleanup blocked: node ownership mismatch'}
        & $kind delete cluster --name $name --kubeconfig $admin
        if ($LASTEXITCODE -ne 0) {throw 'Owned cluster cleanup failed'}
        if (& $docker ps -a --filter "label=io.x-k8s.kind.cluster=$name" --format '{{.Names}}') {throw 'Owned node remains'}
    }
    foreach ($leaf in @('admin.kubeconfig','reader.kubeconfig','metrics.key')) {$target=Join-Path $dir $leaf;if (Test-Path -LiteralPath $target) {Remove-Item -LiteralPath $target}}
}
if ($report) {$report.cleanupVerified=$true;$report|ConvertTo-Json -Depth 10|Set-Content -LiteralPath (Join-Path $root "test-results/metrics-kind-$run.json");Write-Host 'Secure kind metrics+watch acceptance PASSED.'}












