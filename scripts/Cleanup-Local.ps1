param([Parameter(Mandatory)][string]$RunId)
$ErrorActionPreference='Stop'
if ($RunId -notmatch '^[a-f0-9]{12}$') {throw 'RunId must be 12 lowercase hex characters.'}
$projectRoot=Split-Path -Parent $PSScriptRoot
$localRoot=[IO.Path]::GetFullPath((Join-Path $projectRoot '.local'))
$localDir=[IO.Path]::GetFullPath((Join-Path $localRoot $RunId))
if (-not $localDir.StartsWith($localRoot+[IO.Path]::DirectorySeparatorChar,[StringComparison]::OrdinalIgnoreCase)) {throw 'Path outside local workspace.'}
$environmentPath=Join-Path $localDir 'environment.json'
if (-not (Test-Path -LiteralPath $environmentPath)) {throw 'No successful test environment record; inspect failed runs separately.'}
$environment=Get-Content -LiteralPath $environmentPath -Raw | ConvertFrom-Json
if ($environment.runId -ne $RunId -or ($environment.namespace -ne "kborg-tests-$RunId" -and $environment.namespace -ne "k8borg-tests-$RunId")) {throw 'Environment identity mismatch.'}
$env:Path=[Environment]::GetEnvironmentVariable('Path','User')+';'+[Environment]::GetEnvironmentVariable('Path','Machine')
$kubectl=(Get-Command kubectl -ErrorAction Stop).Source
$server=& $kubectl --context docker-desktop config view --minify -o 'jsonpath={.clusters[0].cluster.server}'
if ($LASTEXITCODE -ne 0 -or $server -ne $environment.server) {throw 'Cluster endpoint changed; refusing cleanup.'}
$ca=& $kubectl --context docker-desktop config view --minify --raw -o 'jsonpath={.clusters[0].cluster.certificate-authority-data}'
if ($LASTEXITCODE -ne 0) {throw 'Cannot verify cluster CA.'}
$caHash=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($ca))).ToLowerInvariant()
if ($environment.PSObject.Properties.Name -notcontains 'caSha256' -or $caHash -ne $environment.caSha256) {throw 'Cluster CA changed or unrecorded; refusing cleanup.'}
$ownedJson=& $kubectl --context docker-desktop get namespace $environment.namespace -o json
if ($LASTEXITCODE -ne 0) {throw 'Cannot verify namespace ownership.'}
$owned=$ownedJson | ConvertFrom-Json
$ownerKey=if ($environment.namespace -eq "k8borg-tests-$RunId") {'k8borg.dev/run-id'} else {'kborg.dev/run-id'}
if ($owned.metadata.labels.$ownerKey -ne $RunId) {throw 'Namespace ownership mismatch.'}
& $kubectl --context docker-desktop delete namespace $environment.namespace --wait=false
if ($LASTEXITCODE -ne 0) {throw 'Namespace deletion failed.'}
foreach ($name in @('reader','operator','denied')) {
    $credentialPath=Join-Path $localDir "$name.kubeconfig"
    if (Test-Path -LiteralPath $credentialPath) {Remove-Item -LiteralPath $credentialPath}
}
Write-Host 'Owned fixture namespace deletion requested; local credential files removed. Audit records retained.'
