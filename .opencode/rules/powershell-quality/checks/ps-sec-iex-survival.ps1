#Requires -Version 5.1
<#
ps-sec-iex-survival.ps1 -Path FILE.ps1 [-Mode iex|scriptblock|call] [-HostModel repl|sequence|ci] [-EnvVar 'NAME=value;NAME2=value']
PS-SEC-09: deliver a script the way "irm URL | iex" does, in a child host of the same engine, and report whether the HOST survived.
This EXECUTES the script. Run it in a throwaway container or with a throwaway HOME and no credentials in the environment.
  -Mode iex          Get-Content -Raw FILE | Invoke-Expression (what irm | iex does: one string)
        scriptblock  & ([scriptblock]::Create((Get-Content -Raw FILE)))
        call         & FILE (download then run: a child scope, exit is safe)
  -HostModel repl      the delivery and a marker are two statements fed on stdin, a person pasting the one-liner at a prompt
             sequence  both statements in one -EncodedCommand (a CI step, or "irm | iex; next" on one line)
             ci        like sequence plus the trailer the GitHub Actions pwsh wrapper appends (exit $LASTEXITCODE),
                       and GREEN also requires a non-zero STEP exit code, so a failure that is swallowed is RED
  -EnvVar sets variables in the child first, to drive the script down its failing path (a bad version, an unreachable URL).
Run both "-HostModel repl" and "-HostModel ci" with a script forced down its failure path: both must exit 0.
Exit 0 GREEN, 1 RED (the host was terminated, or in ci mode the failure never reached the step status), 2 bad invocation.
ASCII only, 5.1 syntax. Both output streams are read asynchronously, a script that floods stderr cannot deadlock the check.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11).
#>
param(
    [string]$Path = '',
    [ValidateSet('iex', 'scriptblock', 'call')][string]$Mode = 'iex',
    [ValidateSet('repl', 'sequence', 'ci')][string]$HostModel = 'repl',
    [string]$EnvVar = ''
)
$ErrorActionPreference = 'Stop'
if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    [Console]::Error.WriteLine('ps-sec-iex-survival: -Path must name an existing script file')
    exit 2
}
foreach ($kv in ($EnvVar -split ';' | Where-Object { $_ })) {
    $i = $kv.IndexOf('=')
    if ($i -lt 1) { [Console]::Error.WriteLine("ps-sec-iex-survival: bad -EnvVar entry: $kv"); exit 2 }
    [Environment]::SetEnvironmentVariable($kv.Substring(0, $i), $kv.Substring($i + 1))
}
$full = (Resolve-Path -LiteralPath $Path).Path
$exe = (Get-Process -Id $PID).Path
$deliver = switch ($Mode) {
    'iex'         { "Get-Content -LiteralPath '$full' -Raw | Invoke-Expression" }
    'scriptblock' { "& ([scriptblock]::Create((Get-Content -LiteralPath '$full' -Raw)))" }
    'call'        { "& '$full'" }
}
$marker = "Write-Output ('HOST-SURVIVED lastexit=' + [string]`$LASTEXITCODE)"
$psi = New-Object System.Diagnostics.ProcessStartInfo
$psi.FileName = $exe
$psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true; $psi.UseShellExecute = $false
if ($HostModel -in 'sequence', 'ci') {
    $trailer = if ($HostModel -eq 'ci') { "`nif ((Test-Path -LiteralPath variable:\LASTEXITCODE)) { exit `$LASTEXITCODE }" } else { '' }
    $cmd = "`$ErrorActionPreference = 'Continue'`n$deliver`n$marker$trailer"
    $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
    $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc"
    $p = [System.Diagnostics.Process]::Start($psi)
} else {
    $psi.RedirectStandardInput = $true
    $psi.Arguments = '-NoProfile -ExecutionPolicy Bypass -Command -'
    $p = [System.Diagnostics.Process]::Start($psi)
    $p.StandardInput.WriteLine($deliver)
    $p.StandardInput.WriteLine($marker)
    $p.StandardInput.Close()
}
$soT = $p.StandardOutput.ReadToEndAsync(); $seT = $p.StandardError.ReadToEndAsync()
$p.WaitForExit()
$so = $soT.Result; $se = $seT.Result
$esc = [regex]'\x1b\[[0-9;?]*[A-Za-z]'
$so = $esc.Replace($so, ''); $se = $esc.Replace($se, '')
foreach ($l in ($so -split "`r?`n" | Where-Object { $_ })) { "  out: $l" }
foreach ($l in ($se -split "`r?`n" | Where-Object { $_ -and $_ -notmatch '^#< CLIXML' -and $_ -notmatch '^<Objs' })) { "  err: $l" }
if ($HostModel -eq 'ci') {
    if ($p.ExitCode -ne 0) { "GREEN [$Mode/ci] step failed loudly (step exit $($p.ExitCode))"; exit 0 }
    "RED [$Mode/ci] failure was swallowed or never reached (step exit 0, host marker printed: $($so -match 'HOST-SURVIVED'))"; exit 1
}
if ($so -match 'HOST-SURVIVED') { "GREEN [$Mode/$HostModel] host survived ($($Matches[0]), child exit $($p.ExitCode))"; exit 0 }
"RED [$Mode/$HostModel] host terminated by the script (child exit $($p.ExitCode), host marker never printed)"; exit 1
