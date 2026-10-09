param([string]$Distribution='Ubuntu-26.04',[string]$FixtureDirectory)
$ErrorActionPreference='Stop'
$projectRoot=Split-Path -Parent $PSScriptRoot
# Paths are arguments, never interpolated shell source. The Linux script owns quoting.
$linuxRoot=(& wsl -d $Distribution -- wslpath -a $projectRoot).Trim()
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve project path in WSL.' }
$arguments=@($linuxRoot)
if ($FixtureDirectory) {
    $linuxFixture=(& wsl -d $Distribution -- wslpath -a ([IO.Path]::GetFullPath($FixtureDirectory))).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve fixture path in WSL.' }
    $arguments += $linuxFixture
}
& wsl -d $Distribution -- sh "$linuxRoot/scripts/test-wsl.sh" @arguments
if ($LASTEXITCODE -ne 0) { throw 'WSL validation failed.' }
