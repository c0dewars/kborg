#requires -Version 7.0
param([Parameter(Mandatory)][string]$Archive,[Parameter(Mandatory)][ValidatePattern('^[a-fA-F0-9]{64}$')][string]$ExpectedSha256)
$ErrorActionPreference='Stop'
if (!(Test-Path -LiteralPath $Archive -PathType Leaf)) {throw 'Archive missing'}
$actual=(Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash
if ($actual -ne $ExpectedSha256) {throw 'Package digest mismatch; do not execute or install this archive'}
Write-Output 'Archive SHA-256 matches the supplied trusted digest. This is integrity verification, not publisher authentication.'
