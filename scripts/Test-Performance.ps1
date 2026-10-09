param([int]$Samples=30,[string]$ReaderConfig)
$ErrorActionPreference='Stop'
if ($Samples -lt 5 -or $Samples -gt 100) { throw 'Samples must be5..100.' }
$projectRoot=Split-Path -Parent $PSScriptRoot
$binary=Join-Path $projectRoot 'target/release/k8borg.exe'
if (-not (Test-Path -LiteralPath $binary)) { throw 'Run cargo build --locked --release first.' }
$scratch=Join-Path $projectRoot ('.local/performance-'+[Guid]::NewGuid().ToString('N'))
[void](New-Item -ItemType Directory -Path $scratch)
$acl=Get-Acl -LiteralPath $scratch
$acl.SetAccessRuleProtection($true,$false)
$identity=[Security.Principal.WindowsIdentity]::GetCurrent().Name
$rule=[Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','ContainerInherit,ObjectInherit','None','Allow')
$acl.AddAccessRule($rule)
Set-Acl -LiteralPath $scratch -AclObject $acl
function Measure-Command {
    param([string[]]$Arguments)
    $process=[Diagnostics.Process]::new()
    $process.StartInfo.FileName=$binary
    $process.StartInfo.UseShellExecute=$false
    $process.StartInfo.CreateNoWindow=$true
    $process.StartInfo.RedirectStandardOutput=$true
    $process.StartInfo.RedirectStandardError=$true
    foreach ($argument in $Arguments) { $process.StartInfo.ArgumentList.Add($argument) }
    $clock=[Diagnostics.Stopwatch]::StartNew()
    $peak=0L
    try {
        [void]$process.Start()
        $stdout=$process.StandardOutput.ReadToEndAsync()
        $stderr=$process.StandardError.ReadToEndAsync()
        while (-not $process.HasExited) {
            $process.Refresh()
            try { $peak=[Math]::Max($peak,$process.PeakWorkingSet64) } catch { }
            if ($clock.Elapsed.TotalSeconds -gt 30) { $process.Kill(); throw 'Benchmark timed out.' }
            [Threading.Thread]::Sleep(1)
        }
        $process.WaitForExit()
        $clock.Stop()
        [void]$stdout.GetAwaiter().GetResult()
        [void]$stderr.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { throw 'Benchmark failed; refresh fixture credentials if supplied.' }
        return [pscustomobject]@{wall_ms=$clock.Elapsed.TotalMilliseconds; cpu_ms=$process.TotalProcessorTime.TotalMilliseconds; sampled_peak_working_set_bytes=$peak}
    } finally { $process.Dispose() }
}
function Summarize {
    param([object[]]$Results)
    $ordered=@($Results.wall_ms | Sort-Object)
    [pscustomobject]@{samples=$Results.Count; median_wall_ms=$ordered[[int][Math]::Floor($ordered.Count/2)]; p95_wall_ms=$ordered[[Math]::Min($ordered.Count-1,[int][Math]::Floor($ordered.Count*.95))]; max_sampled_working_set_bytes=($Results.sampled_peak_working_set_bytes | Measure-Object -Maximum).Maximum; total_cpu_ms=($Results.cpu_ms | Measure-Object -Sum).Sum}
}
try {
    $configuration=@{apiVersion='v1';kind='Config';'current-context'='context-000';clusters=@(@{name='synthetic';cluster=@{server='https://127.0.0.1:9'}});users=@(@{name='synthetic';user=@{token='SYNTHETIC-NOT-A-CREDENTIAL'}})}
    $context=@{cluster='synthetic';user='synthetic';namespace='default'}
    if ($ReaderConfig) {
        $configuration=Get-Content -LiteralPath $ReaderConfig -Raw | ConvertFrom-Json -AsHashtable
        $context=$configuration.contexts[0].context
    }
    $configuration.contexts=@(0..99 | ForEach-Object { @{name=('context-{0:d3}' -f $_);context=$context} })
    $configuration.'current-context'='context-000'
    $path=Join-Path $scratch 'hundred-contexts.kubeconfig'
    $configuration | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $path -Encoding utf8
    $demo=@(1..$Samples | ForEach-Object { Measure-Command @('--demo','--json','list','pods') })
    $contexts=@(1..$Samples | ForEach-Object { Measure-Command @('--kubeconfig',$path,'--json','contexts') })
    $report=[ordered]@{schema=1;utc=[DateTime]::UtcNow.ToString('o');platform='Windows';binary_bytes=(Get-Item -LiteralPath $binary).Length;binary_sha256=(Get-FileHash -LiteralPath $binary -Algorithm SHA256).Hash.ToLowerInvariant();method='Stopwatch wall time and process CPU;1ms peak-working-set sampling is a lower bound, not exact peak; warmed cache';scope='100 context aliases, not100 clusters';demo_cli=(Summarize $demo);hundred_context_listing=(Summarize $contexts)}
    if ($ReaderConfig) {
        $live=@(0..99 | ForEach-Object { Measure-Command @('--kubeconfig',$path,'--context',('context-{0:d3}' -f $_),'--json','list','pods') })
        $report.live_sequential_context_reads=Summarize $live
    }
    $output=Join-Path $projectRoot 'test-results/performance-windows.json'
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $output) -Force)
    $report | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $output -Encoding utf8
    $report | ConvertTo-Json -Depth 10
} finally {
    $path=Join-Path $scratch 'hundred-contexts.kubeconfig'
    if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path }
    Remove-Item -LiteralPath $scratch
}
