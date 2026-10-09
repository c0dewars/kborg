# Development-only: downloads source/notice text. Nothing downloaded is executed.
$ErrorActionPreference='Stop'
$projectRoot=Split-Path -Parent $PSScriptRoot
$assetRoot=Join-Path $projectRoot 'assets\upstream'
New-Item -ItemType Directory -Path $assetRoot -Force | Out-Null
$sources=@(
    @{id='catppuccin';repo='catppuccin/palette';assets=@('palette.json','LICENSE')},
    @{id='dracula';repo='dracula/dracula-theme';assets=@('README.md','LICENSE')},
    @{id='nord';repo='nordtheme/nord';assets=@('src/nord.css','license')},
    @{id='gruvbox';repo='morhetz/gruvbox';assets=@('colors/gruvbox.vim','README.md')},
    @{id='tokyo-night';repo='folke/tokyonight.nvim';assets=@('lua/tokyonight/colors/init.lua','lua/tokyonight/colors/night.lua','lua/tokyonight/colors/storm.lua','lua/tokyonight/colors/moon.lua','lua/tokyonight/colors/day.lua','LICENSE')},
    @{id='solarized';repo='altercation/solarized';assets=@('README.md','LICENSE')}
)
$manifest=@()
foreach ($source in $sources) {
    $commit=(Invoke-RestMethod -Uri "https://api.github.com/repos/$($source.repo)/commits?per_page=1" -Headers @{'User-Agent'='K8Borg-build'})[0].sha
    if ($commit -notmatch '^[a-f0-9]{40}$') {throw 'Invalid upstream revision.'}
    $directory=Join-Path $assetRoot $source.id
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
    foreach ($asset in $source.assets) {
        $url="https://raw.githubusercontent.com/$($source.repo)/$commit/$asset"
        $target=Join-Path $directory ($asset.Replace('/','_'))
        Invoke-WebRequest -Uri $url -OutFile $target
        $manifest+=@{family=$source.id;repository=$source.repo;revision=$commit;sourceAsset=$asset;localAsset="assets/upstream/$($source.id)/$($asset.Replace('/','_'))";sha256=(Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()}
    }
}
$manifest | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $assetRoot 'manifest.json') -Encoding utf8
Write-Host 'Theme source assets pinned. Review notices and palette values before updating built-ins.'
