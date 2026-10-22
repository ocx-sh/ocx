<#
PS-ERR-09: run a script in a child host and report RED when a StrictMode undefined-variable error raised inside a
`finally` block replaces the script's own failure. The match is the English message, so run it on an en-US UI language.
This EXECUTES the script. Run it in a throwaway container or with a throwaway HOME and no credentials in the environment.

Usage: pwsh -NoProfile -File finally-masks.ps1 -Path FILE.ps1 [-EnvVar 'NAME=value;NAME2=value']
  -EnvVar sets variables in the child first, to drive the script down its failing path.
Exit: 0 clean, 1 red, 2 bad invocation.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11).
#>
param([string]$Path = '', [string]$EnvVar = '')
if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    [Console]::Error.WriteLine('finally-masks: -Path must name an existing script file')
    exit 2
}
foreach ($kv in ($EnvVar -split ';' | Where-Object { $_ })) {
    $i = $kv.IndexOf('=')
    if ($i -lt 1) { [Console]::Error.WriteLine("finally-masks: bad -EnvVar entry: $kv"); exit 2 }
    [Environment]::SetEnvironmentVariable($kv.Substring(0, $i), $kv.Substring($i + 1))
}
$exe = (Get-Process -Id $PID).Path
$script = (Resolve-Path -LiteralPath $Path).Path
$out = & $exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $script 2>&1 | Out-String
$m = [regex]::Match($out, '(?m)^.*(cannot be retrieved because it has not been set|VariableIsUndefined).*$')
if ($m.Success) {
    "RED: a finally block failed on an undefined variable and masked the real failure: $($m.Value.Trim())"
    exit 1
}
exit 0
