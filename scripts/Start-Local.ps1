param([switch]$Write,[switch]$Demo,[string]$Theme)
$ErrorActionPreference='Stop'
$projectRoot=Split-Path -Parent $PSScriptRoot
$binary=Join-Path $projectRoot 'target\x86_64-pc-windows-msvc\release\kborg.exe'
if (-not (Test-Path -LiteralPath $binary)) { $binary=Join-Path $projectRoot 'target\release\kborg.exe' }
if (-not (Test-Path -LiteralPath $binary)) { $binary=Join-Path $projectRoot 'target\debug\kborg.exe' }
if (-not (Test-Path -LiteralPath $binary)) { throw 'Build first: cargo build --release' }
$arguments=@()
if ($Demo) { $arguments+=@('demo') }
else {
    $environments=@(Get-ChildItem -Path (Join-Path $projectRoot '.local\*\environment.json') -File | Sort-Object LastWriteTime -Descending)
    if ($environments.Count -eq 0) {throw 'Run Test-DockerDesktop.ps1 -ExpectedServer <verified endpoint> -KeepFixtures first.'}
    $localDir=$environments[0].DirectoryName
    $configName=if ($Write) {'operator.kubeconfig'} else {'reader.kubeconfig'}
    $configPath=Join-Path $localDir $configName
    if (-not (Test-Path -LiteralPath $configPath)) {throw 'Test credentials were cleaned up. Provision fresh fixtures.'}
    $environment=Get-Content -LiteralPath $environments[0].FullName -Raw | ConvertFrom-Json
    $contextName=if ($environment.namespace -like 'k8borg-tests-*') {'k8borg-test'} else {'kborg-test'}
    $arguments+=@('--kubeconfig',$configPath,'--context',$contextName,'--allow-logs')
    $env:KBORG_CONFIG_DIR=Join-Path $localDir 'app-config'
}
if ($Write) {$arguments+='--write'}
if ($Theme) {$arguments+=@('--theme',$Theme)}
try { & $binary @arguments } finally { $env:KBORG_CONFIG_DIR=$null }
if ($LASTEXITCODE -ne 0) {throw 'KBorg failed; inspect the displayed error.'}
