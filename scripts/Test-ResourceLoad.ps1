$ErrorActionPreference='Stop'
$kubectl='C:\Users\Garcia\AppData\Local\Programs\DockerDesktop\resources\bin\kubectl.exe'
$env:PATH=[Environment]::GetEnvironmentVariable('Path','Machine')+';'+[Environment]::GetEnvironmentVariable('Path','User')
function K([string[]]$a){$result=& $kubectl --context docker-desktop @a; if($LASTEXITCODE -ne 0){throw 'kubectl operation failed'};return $result}
function Verify { $v=(K @('config','view','--minify','--raw','-o','json'))|ConvertFrom-Json; $c=$v.clusters[0].cluster;if($c.'insecure-skip-tls-verify'){throw 'insecure TLS forbidden'}; if($c.server -ne 'https://127.0.0.1:64170'){throw 'endpoint mismatch'};$h=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData([Convert]::FromBase64String($c.'certificate-authority-data')));if($script:caHash -and $script:caHash -ne $h){throw 'CA changed'};$script:caHash=$h;return $c }
$cluster=Verify
$run=[Guid]::NewGuid().ToString('N').Substring(0,12);$ns='k8borg-load-'+$run;$created=$false
$project=Split-Path -Parent $PSScriptRoot
$testDir=Join-Path $project (".local\load-$run");New-Item -ItemType Directory -Path $testDir -Force|Out-Null
$dirAcl=Get-Acl -LiteralPath $testDir;$dirAcl.SetAccessRuleProtection($true,$false);$owner=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name;$dirAcl.SetAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new($owner,'FullControl','ContainerInherit,ObjectInherit','None','Allow'));Set-Acl -LiteralPath $testDir -AclObject $dirAcl
$credential=Join-Path $testDir 'load-reader.json';$manifest=Join-Path $testDir 'load-resources.json'
try {
 @{apiVersion='v1';kind='Namespace';metadata=@{name=$ns;labels=@{'k8borg.dev/run-id'=$run}}}|ConvertTo-Json -Depth 10|& $kubectl --context docker-desktop create -f -
 if($LASTEXITCODE -ne 0){throw 'namespace create failed'};$created=$true
 $rbac=@(@{apiVersion='v1';kind='ServiceAccount';metadata=@{name='load-reader';namespace=$ns};automountServiceAccountToken=$false},@{apiVersion='rbac.authorization.k8s.io/v1';kind='Role';metadata=@{name='load-reader';namespace=$ns};rules=@(@{apiGroups=@('');resources=@('configmaps');verbs=@('get','list')})},@{apiVersion='rbac.authorization.k8s.io/v1';kind='RoleBinding';metadata=@{name='load-reader';namespace=$ns};subjects=@(@{kind='ServiceAccount';name='load-reader';namespace=$ns});roleRef=@{apiGroup='rbac.authorization.k8s.io';kind='Role';name='load-reader'}})
 @{apiVersion='v1';kind='List';items=$rbac}|ConvertTo-Json -Depth 15|& $kubectl --context docker-desktop create -f -
 if($LASTEXITCODE -ne 0){throw 'RBAC failed'}
 $payload='K8BORG_LOAD_DATA_CANARY'+('x'*(4096-23))
 for($batch=0;$batch -lt 11;$batch++){
  $items=@();for($i=$batch*100;$i -lt [Math]::Min(($batch+1)*100,1001);$i++){ $items+=@{apiVersion='v1';kind='ConfigMap';metadata=@{name=('load-{0:d4}' -f $i);namespace=$ns;labels=@{'k8borg.dev/run-id'=$run;app='synthetic-load'}};data=@{payload=$payload}} }
  @{apiVersion='v1';kind='List';items=$items}|ConvertTo-Json -Depth 15 -Compress|Set-Content -LiteralPath $manifest -Encoding utf8
  & $kubectl --context docker-desktop create -f $manifest | Out-Null
  if($LASTEXITCODE -ne 0){throw 'fixture creation failed'}
  Write-Host "Created bounded batch $batch"
 }
 $token=(K @('-n',$ns,'create','token','load-reader','--duration=10m')).Trim()
 @{apiVersion='v1';kind='Config';'current-context'='load';clusters=@(@{name='local';cluster=@{server=$cluster.server;'certificate-authority-data'=$cluster.'certificate-authority-data'}});users=@(@{name='load-reader';user=@{token=$token}});contexts=@(@{name='load';context=@{cluster='local';user='load-reader';namespace=$ns}})}|ConvertTo-Json -Depth 15|Set-Content -LiteralPath $credential -Encoding utf8
 $acl=Get-Acl -LiteralPath $credential;$acl.SetAccessRuleProtection($true,$false);$identity=[System.Security.Principal.WindowsIdentity]::GetCurrent().Name;$acl.SetAccessRule([System.Security.AccessControl.FileSystemAccessRule]::new($identity,'FullControl','Allow'));Set-Acl -LiteralPath $credential -AclObject $acl
 $token=$null
 $linuxScript=((& wsl.exe -d Ubuntu-26.04 -- wslpath -u (Join-Path $PSScriptRoot 'Test-ResourceLoad.py')).Trim())
 $linuxCredential=((& wsl.exe -d Ubuntu-26.04 -- wslpath -u $credential).Trim())
 $linuxOutput=((& wsl.exe -d Ubuntu-26.04 -- wslpath -u (Join-Path $project 'docs\evidence\load-test.json')).Trim())
 & wsl.exe -d Ubuntu-26.04 -- python3 $linuxScript $linuxCredential $linuxOutput
 if($LASTEXITCODE -ne 0){throw 'load measurements failed'}
} finally {
 try {
  if($created){$null=Verify;$owned=(K @('get','namespace',$ns,'-o','json'))|ConvertFrom-Json;if($owned.metadata.labels.'k8borg.dev/run-id' -ne $run){throw 'ownership mismatch'};K @('delete','namespace',$ns,'--wait=true','--timeout=60s')}
 } finally {
  foreach($file in @($credential,$manifest)){if(Test-Path -LiteralPath $file){Remove-Item -LiteralPath $file}}
 }
}
