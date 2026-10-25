#requires -Version 7.0
param([string]$Executable, [int]$TimeoutSeconds = 5)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!$Executable) { $Executable = Join-Path $root 'target/x86_64-pc-windows-msvc/release/kborg.exe' }
$Executable = (Resolve-Path -LiteralPath $Executable).Path
if ($TimeoutSeconds -lt 1 -or $TimeoutSeconds -gt 15) { throw 'Timeout must be between 1 and 15 seconds.' }
$run = [Guid]::NewGuid().ToString('N').Substring(0,12)
$fixtures = Join-Path $root ".local/adversarial-$run"
New-Item -ItemType Directory -Path $fixtures -Force | Out-Null
$records = [Collections.Generic.List[object]]::new()
$utf8 = [Text.UTF8Encoding]::new($false)
function Write-Fixture([string]$Name, [string]$Content) {
    $path = Join-Path $fixtures $Name
    [IO.File]::WriteAllText($path, $Content, $utf8)
    return $path
}
function Invoke-Case([string]$Name, [string[]]$Arguments, [bool]$Reject = $true, [string]$Expected = '') {
    $start = [Diagnostics.ProcessStartInfo]::new($Executable)
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardInput = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $start.WorkingDirectory = $fixtures
    $start.Environment['KBORG_CONFIG_DIR'] = Join-Path $fixtures 'config'
    $start.Environment['KBORG_NO_SPLASH'] = '1'
    $start.Environment['NO_COLOR'] = '1'
    $start.Environment.Remove('KUBECONFIG') | Out-Null
    foreach ($argument in $Arguments) { $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $start
    $watch = [Diagnostics.Stopwatch]::StartNew()
    if (!$process.Start()) { throw 'Could not start tested executable.' }
    $process.StandardInput.Close()
    $outTask = $process.StandardOutput.ReadToEndAsync()
    $errTask = $process.StandardError.ReadToEndAsync()
    $timedOut = !$process.WaitForExit($TimeoutSeconds * 1000)
    if ($timedOut) { $process.Kill($true); $process.WaitForExit() }
    $output = $outTask.GetAwaiter().GetResult() + $errTask.GetAwaiter().GetResult()
    $watch.Stop()
    $controls = $output -match '[\x1b\x80-\x9f\u061c\u200b-\u200f\u202a-\u202e\u2060-\u206f\ufeff]'
    $passed = !$timedOut -and !$controls -and (($Reject -and $process.ExitCode -eq 1) -or (!$Reject -and $process.ExitCode -eq 0))
    if ($Expected) { $passed = $passed -and $output.Contains($Expected) }
    $records.Add([ordered]@{ case = $Name; passed = $passed; expected_rejection = $Reject; exit_code = $process.ExitCode; elapsed_ms = $watch.ElapsedMilliseconds; timed_out = $timedOut; unsafe_output_controls = $controls; response_excerpt = $output.Substring(0, [Math]::Min(300, $output.Length)).Trim() })
    $process.Dispose()
}
function Test-Theme([string]$Name, [string]$Content, [bool]$Reject = $true) {
    $path = Write-Fixture "$Name.json" $Content
    Invoke-Case $Name @('theme','validate',$path) $Reject
}
$valid = '{"schema_version":1,"id":"adversarial-valid","name":"Synthetic fixture","base":"catppuccin-mocha","colors":{}}'
Test-Theme 'valid-theme-control' $valid $false
Test-Theme 'theme-malformed-json' '{"schema_version":'
Test-Theme 'theme-deep-json' ('{"schema_version":1,"id":"deep","name":"Deep","base":"catppuccin-mocha","colors":{"text":' + ('[' * 300) + '0' + (']' * 300) + '}}')
Test-Theme 'theme-duplicate-top-field' '{"schema_version":1,"schema_version":1,"id":"dupe","name":"Dupe","base":"catppuccin-mocha","colors":{}}'
Test-Theme 'theme-duplicate-color' '{"schema_version":1,"id":"dupe","name":"Dupe","base":"catppuccin-mocha","colors":{"text":"#CDD6F4","text":"#FFFFFF"}}'
Test-Theme 'theme-oversized' ($valid + (' ' * 65537))
Test-Theme 'theme-unknown-hook' ($valid.Substring(0,$valid.Length-1) + ',"on_load":"DO_NOT_EXECUTE"}')
Test-Theme 'theme-path-id' ($valid.Replace('adversarial-valid','../outside'))
Test-Theme 'theme-reserved-id' ($valid.Replace('adversarial-valid','catppuccin-mocha'))
Test-Theme 'theme-custom-base' ($valid.Replace('catppuccin-mocha','arbitrary-custom-parent'))
Test-Theme 'theme-invalid-color' ($valid.Replace('"colors":{}','"colors":{"text":"#ZZZZZZ"}'))
Test-Theme 'theme-unicode-color' ($valid.Replace('"colors":{}','"colors":{"text":"#ab💥"}'))
Test-Theme 'theme-low-contrast' ($valid.Replace('"colors":{}','"colors":{"text":"#1E1E2E"}'))
Test-Theme 'theme-escape-name' ($valid.Replace('Synthetic fixture','Name\u001b]52;c;CANARY\u0007'))
Test-Theme 'theme-bidi-name' ($valid.Replace('Synthetic fixture','Name\u202eCANARY'))
Test-Theme 'theme-long-name' ($valid.Replace('Synthetic fixture',('A' * 81)))
Test-Theme 'theme-unknown-control-role' ($valid.Replace('"colors":{}','"colors":{"role\u001b":"#FFFFFF"}'))
Test-Theme 'theme-invalid-schema' ($valid.Replace('"schema_version":1','"schema_version":2'))
Test-Theme 'theme-null' 'null'
$invalidUtf8 = Join-Path $fixtures 'theme-invalid-utf8.json'
[IO.File]::WriteAllBytes($invalidUtf8,[byte[]](0xff,0xfe,0xff))
Invoke-Case 'theme-invalid-utf8' @('theme','validate',$invalidUtf8)
Invoke-Case 'theme-control-id-output-sanitized' @('theme','use',("missing" + [char]27 + ']52;c;CANARY' + [char]7))
Invoke-Case 'theme-new-traversal-id' @('theme','new','../outside')
function Test-Kube([string]$Name, [string]$Content, [bool]$Reject = $true) {
    $path = Write-Fixture "$Name.kubeconfig" $Content
    Invoke-Case $Name @('--kubeconfig',$path,'contexts','--json') $Reject
}
Test-Kube 'kube-empty-valid-control' '{"apiVersion":"v1","kind":"Config","contexts":[]}' $false
Test-Kube 'kube-malformed' 'contexts: [oops'
Test-Kube 'kube-oversized' ('{"contexts":[]}' + (' ' * 1048577))
Test-Kube 'kube-deep' ('{"contexts":' + ('[' * 300) + '0' + (']' * 300) + '}')
Test-Kube 'kube-duplicate-context-names' '{"contexts":[{"name":"duplicate","context":{"cluster":"one"}},{"name":"duplicate","context":{"cluster":"two"}}]}'
Test-Kube 'kube-duplicate-cluster-names' '{"clusters":[{"name":"duplicate","cluster":{"server":"https://127.0.0.1:1"}},{"name":"duplicate","cluster":{"server":"https://127.0.0.1:2"}}]}'
Test-Kube 'kube-duplicate-user-names' '{"users":[{"name":"duplicate","user":{"token":"CANARY-A"}},{"name":"duplicate","user":{"token":"CANARY-B"}}]}'
Test-Kube 'kube-control-context-name' '{"contexts":[{"name":"context\u001b]52;c;CANARY\u0007","context":{"cluster":"synthetic"}}]}'
Test-Kube 'kube-bidi-context-name' '{"contexts":[{"name":"context\u202eCANARY","context":{"cluster":"synthetic"}}]}'
Test-Kube 'kube-long-context-name' ('{"contexts":[{"name":"' + ('A' * 254) + '","context":{"cluster":"synthetic"}}]}')
Test-Kube 'kube-offline-helper-not-executed' '{"current-context":"synthetic","contexts":[{"name":"synthetic","context":{"cluster":"synthetic","user":"synthetic"}}],"clusters":[{"name":"synthetic","cluster":{"server":"https://127.0.0.1:1"}}],"users":[{"name":"synthetic","user":{"exec":{"command":"SYNTHETIC_DO_NOT_EXECUTE","apiVersion":"client.authentication.k8s.io/v1"}}}]}' $false
foreach ($command in @(@('scale','api','2'),@('restart','api'),@('delete','deployments','api'))) {
    Invoke-Case "noninteractive-$($command[0])-refused" (@('--demo','--write') + $command) $true 'noninteractive writes are disabled'
}
Invoke-Case 'noninteractive-copilot-login-refused' @('copilot','login','--cli',$Executable,'--host','github.com') $true 'interactive terminal'
Invoke-Case 'copilot-untrusted-host-refused' @('copilot','login','--cli',$Executable,'--host','https://github.com@evil.example') $true 'Copilot host must'
Invoke-Case 'secret-read-disabled' @('--demo','list','secrets') $true 'excluded from generic views'
Invoke-Case 'object-name-traversal-refused' @('--demo','get','pods','../api') $true 'invalid object name'
Invoke-Case 'unknown-yes-option-refused' @('--demo','scale','api','2','--yes') $true 'unknown option'
$bundle = Join-Path $fixtures 'context.json'
Invoke-Case 'copilot-context-local-export-control' @('--demo','copilot','context','pods','api-7d4f9','--output',$bundle) $false 'No AI request was made'
Invoke-Case 'copilot-context-refuses-overwrite' @('--demo','copilot','context','pods','api-7d4f9','--output',$bundle) $true 'cannot create file'
$content = [IO.File]::ReadAllText($bundle)
$records.Add([ordered]@{ case = 'copilot-context-minimized-content'; passed = ($content -notmatch 'api-7d4f9|demo-node|"namespace"|"metadata"|"env"'); expected_rejection = $false; exit_code = $null; elapsed_ms = 0; timed_out = $false; unsafe_output_controls = $false; response_excerpt = 'Checked synthetic bundle for excluded names, node identity and raw workload fields.' })
$failures = @($records | Where-Object { !$_.passed })
$report = [ordered]@{ schema_version = 1; tested_at_utc = (Get-Date).ToUniversalTime().ToString('o'); executable_sha256 = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant(); run_id = $run; source_revision = ((& git -C $root rev-parse HEAD) | Out-String).Trim(); operating_system = [Environment]::OSVersion.VersionString; per_process_timeout_seconds = $TimeoutSeconds; case_count = $records.Count; failure_count = $failures.Count; scope = 'Bounded Windows release CLI black-box regressions; synthetic offline files; no model requests, login, cluster writes or network fixture connections'; cases = $records }
$evidence = Join-Path $root 'docs/evidence/adversarial.json'
New-Item -ItemType Directory -Path (Split-Path -Parent $evidence) -Force | Out-Null
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $evidence -Encoding utf8
Write-Host "$($records.Count) cases; $($failures.Count) failed. Evidence: $evidence"
if ($failures.Count) { $failures | ForEach-Object { [PSCustomObject]$_ } | Format-Table case,exit_code,timed_out,response_excerpt; exit 1 }

