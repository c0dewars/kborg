param([Parameter(Mandatory)][string]$AdminConfig,[Parameter(Mandatory)][string]$ExpectedContext,[Parameter(Mandatory)][string]$ExpectedServer,[Parameter(Mandatory)][string]$Namespace,[Parameter(Mandatory)][string]$ReaderConfig,[Parameter(Mandatory)][string]$OutputDirectory,[Parameter(Mandatory)][string]$KubectlPath)
$ErrorActionPreference='Stop'
if ($ExpectedContext -notmatch '^kind-kborg-compat-[a-f0-9]{12}$' -or $Namespace -notmatch '^kborg-tests-[a-f0-9]{12}$') { throw 'Expected owned compatibility scope.' }
$projectRoot=Split-Path -Parent $PSScriptRoot
$approvedAdmin=Join-Path $projectRoot ('.local\'+$ExpectedContext.Substring(5)+'\admin.kubeconfig')
if ([IO.Path]::GetFullPath($AdminConfig) -ne $approvedAdmin) { throw 'Unexpected admin kubeconfig.' }
$configuration=(& $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext config view --minify --raw -o json) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $configuration.clusters[0].cluster.server -ne $ExpectedServer) { throw 'Compatibility endpoint changed.' }
$owned=(& $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext get namespace $Namespace -o json) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0 -or $owned.metadata.labels.'kborg.dev/run-id' -ne $Namespace.Substring(13)) { throw 'Fixture namespace ownership mismatch.' }
$schema=@{type='object';properties=@{
    spec=@{type='object';properties=@{signal=@{type='string'};token=@{type='string'}}}
    status=@{type='object';properties=@{phase=@{type='string'};conditions=@{type='array';items=@{type='object';properties=@{type=@{type='string'};status=@{type='string'};reason=@{type='string'};message=@{type='string'}}}}}}
}}
$crd=@{apiVersion='apiextensions.k8s.io/v1';kind='CustomResourceDefinition';metadata=@{name='diagnosticprobes.kborg.dev'};spec=@{group='kborg.dev';scope='Namespaced';names=@{plural='diagnosticprobes';singular='diagnosticprobe';kind='DiagnosticProbe'};versions=@(@{name='v1';served=$true;storage=$true;schema=@{openAPIV3Schema=$schema}})}}
$crd | ConvertTo-Json -Depth 30 | & $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext create -f -
if ($LASTEXITCODE -ne 0) { throw 'Diagnostic CRD provisioning failed.' }
& $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext wait --for=condition=Established customresourcedefinition/diagnosticprobes.kborg.dev --timeout=30s
if ($LASTEXITCODE -ne 0) { throw 'Diagnostic CRD establishment timed out.' }
$role=(& $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext -n $Namespace get role reader -o json) | ConvertFrom-Json -AsHashtable
if ($LASTEXITCODE -ne 0) { throw 'Cannot read fixture role.' }
$role.rules+=@{apiGroups=@('kborg.dev');resources=@('diagnosticprobes');verbs=@('get','list')}
$role | ConvertTo-Json -Depth 30 | & $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext replace -f -
if ($LASTEXITCODE -ne 0) { throw 'Cannot grant restricted diagnostic read permission.' }
$probe=@{apiVersion='kborg.dev/v1';kind='DiagnosticProbe';metadata=@{name='synthetic-probe';namespace=$Namespace};spec=@{signal='KBORG_PRIVATE_SPEC_CANARY';token='KBORG_PRIVATE_TOKEN_CANARY'};status=@{phase='Pending';conditions=@(@{type='Ready';status='False';reason='SyntheticWait';message='KBORG_PRIVATE_MESSAGE_CANARY'})}}
$probe | ConvertTo-Json -Depth 30 | & $KubectlPath --kubeconfig $AdminConfig --context $ExpectedContext create -f -
if ($LASTEXITCODE -ne 0) { throw 'Diagnostic object provisioning failed.' }
$binary=Join-Path $projectRoot 'target/debug/kborg.exe'
$resources=& $binary --kubeconfig $ReaderConfig --json resources
if ($LASTEXITCODE -ne 0) { throw 'Resource discovery failed.' }
$discovered=($resources -join "`n") | ConvertFrom-Json
if (-not @($discovered | Where-Object { $_.plural -eq 'diagnosticprobes' -and $_.group -eq 'kborg.dev' })) { throw 'Discovery omitted the diagnostic CRD.' }
$list=& $binary --kubeconfig $ReaderConfig --json list diagnosticprobes.kborg.dev
if ($LASTEXITCODE -ne 0) { throw 'Fully qualified diagnostic list failed.' }
$listed=($list -join "`n") | ConvertFrom-Json
if ($listed.items.Count -ne 1 -or $listed.items[0].kind -ne 'DiagnosticProbe') { throw 'Diagnostic list result mismatch.' }
$get=& $binary --kubeconfig $ReaderConfig get diagnosticprobes.kborg.dev synthetic-probe
if ($LASTEXITCODE -ne 0) { throw 'Fully qualified diagnostic get failed.' }
$text=$get -join "`n"
$object=$text | ConvertFrom-Json
if ($object.status.phase -ne 'Pending' -or $text.Contains('KBORG_PRIVATE_TOKEN_CANARY')) { throw 'Diagnostic get status/redaction mismatch.' }
$export=Join-Path $OutputDirectory 'diagnostic-context.json'
& $binary --kubeconfig $ReaderConfig copilot context diagnosticprobes.kborg.dev synthetic-probe --output $export
if ($LASTEXITCODE -ne 0) { throw 'Local minimized Copilot context export failed.' }
$minimized=Get-Content -LiteralPath $export -Raw
foreach ($forbidden in @('KBORG_PRIVATE','synthetic-probe',$Namespace,'"spec"','"token"','"message"')) {
    if ($minimized.Contains($forbidden)) { throw 'Diagnostic context retained a forbidden field or canary.' }
}
$value=$minimized | ConvertFrom-Json
if ($value.diagnostics.phase -ne 'Pending') { throw 'Diagnostic context omitted its status signal.' }
Remove-Item -LiteralPath $export
Write-Host 'Diagnostic CRD discovery/list/get and local minimized Copilot context export PASSED; no AI request was made.'
