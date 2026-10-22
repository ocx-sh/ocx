#Requires -Version 5.1
<#
ps-sec-iex-leak.ps1 -Path FILE.ps1 [-EnvVar 'NAME=value;NAME2=value']
PS-SEC-08(e): deliver a script with "Get-Content -Raw | Invoke-Expression" in a CHILD host of the same engine, then report what
it left in the caller's session: $ErrorActionPreference, StrictMode (an undefined variable throws), new functions, new variables.
This EXECUTES the script. Run it in a throwaway container or with a throwaway HOME and no credentials in the environment.
  -EnvVar sets variables in the child first, to drive the script down its failing path (iex leaves the session changed on both).
Exit 0 the session is unchanged, 1 the script changed it or ended the host (a top-level exit under iex closes the caller's
session, so the probe never reports back), 2 bad invocation or the probe never started.
ASCII only, 5.1 syntax.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11).
#>
param([string]$Path = '', [string]$EnvVar = '')
if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    [Console]::Error.WriteLine('ps-sec-iex-leak: -Path must name an existing script file')
    exit 2
}
foreach ($kv in ($EnvVar -split ';' | Where-Object { $_ })) {
    $i = $kv.IndexOf('=')
    if ($i -lt 1) { [Console]::Error.WriteLine("ps-sec-iex-leak: bad -EnvVar entry: $kv"); exit 2 }
    [Environment]::SetEnvironmentVariable($kv.Substring(0, $i), $kv.Substring($i + 1))
}
$full = (Resolve-Path -LiteralPath $Path).Path
$exe = (Get-Process -Id $PID).Path
$probe = @"
Write-Output 'PROBE-START'
`$ErrorActionPreference = 'Continue'
`$fn0 = @(Get-ChildItem function: | Where-Object { -not `$_.ModuleName } | ForEach-Object Name); `$v0 = @(Get-Variable | ForEach-Object Name)
try { Get-Content -LiteralPath '$full' -Raw | Invoke-Expression 2>&1 | Out-Null } catch { }
`$fn1 = @(Get-ChildItem function: | Where-Object { -not `$_.ModuleName } | ForEach-Object Name | Where-Object { `$_ -notin `$fn0 })
`$v1 = @(Get-Variable | ForEach-Object Name | Where-Object { `$_ -notin `$v0 -and `$_ -notin 'fn0','v0','fn1','_','PSItem','switch' })
`$strict = 'off'; try { `$null = `$undefinedProbeVar123 } catch { `$strict = 'ON' }
Write-Output ('LEAK EAP=' + `$ErrorActionPreference + ' strict=' + `$strict + ' functions=' + (`$fn1 -join ',') + ' variables=' + (`$v1 -join ','))
"@
$enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($probe))
$o = & $exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc 2>&1 | Out-String
$m = [regex]::Match($o, 'LEAK EAP=[^\r\n]*')
if (-not $m.Success -and $o -match 'PROBE-START') {
    'RED the script ended the child host (a top-level exit under iex closes the caller session)'
    exit 1
}
if (-not $m.Success) {
    'NO-RESULT (the probe printed nothing): ' + $o.Substring(0, [Math]::Min(300, $o.Length))
    exit 2
}
$m.Value
if ($m.Value -match 'EAP=Continue strict=off functions= variables=$') { 'GREEN the session is unchanged'; exit 0 }
'RED the script changed the caller session'; exit 1
