# readback-pwsh.ps1 [-Mode bare|both|exe-only] : which spellings of a native command reach a completer registered with
# `Register-ArgumentCompleter -Native -CommandName`? A throwaway stub command `demotool` is created and put on PATH
# (on Windows a copy of a system .exe, elsewhere an executable shell script). Every spelling a user types is read back
# through TabExpansion2, the live engine. Exit 1 when any spelling does not complete to `sub`.
#   bare      registers 'demotool' only (the compliant form)
#   both      registers 'demotool' and 'demotool.exe'
#   exe-only  registers 'demotool.exe' only (the plant: the bare spellings miss)
# Run it on Windows PowerShell 5.1 and on 7.x. Off Windows only the first spelling applies.
param([ValidateSet('bare', 'both', 'exe-only')][string]$Mode = 'bare')
$ErrorActionPreference = 'Stop'
$win = $env:OS -eq 'Windows_NT'
$dir = Join-Path ([IO.Path]::GetTempPath()) ('rbp-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $dir | Out-Null
if ($win) {
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\whoami.exe') -Destination (Join-Path $dir 'demotool.exe')
} else {
    $stub = Join-Path $dir 'demotool'
    [IO.File]::WriteAllText($stub, "#!/bin/sh`n")
    chmod +x $stub
}
$env:PATH = $dir + [IO.Path]::PathSeparator + $env:PATH
function Complete([string]$text) { @((TabExpansion2 -inputScript $text -cursorColumn $text.Length).CompletionMatches | ForEach-Object CompletionText) -join ',' }
$sb = { param($w, $a, $c) [System.Management.Automation.CompletionResult]::new('sub', 'sub', 'ParameterValue', 'tool') }
if ($Mode -ne 'exe-only') { Register-ArgumentCompleter -Native -CommandName demotool -ScriptBlock $sb }
if ($Mode -in 'both', 'exe-only') { Register-ArgumentCompleter -Native -CommandName demotool.exe -ScriptBlock $sb }
$spellings = if ($win) { 'demotool ', 'DEMOTOOL ', 'demotool.exe ', '.\demotool ', '.\demotool.exe ', '& demotool ' } else { 'demotool ' }
$fail = 0
foreach ($t in $spellings) {
    $r = Complete $t
    '{0,-16} -> [{1}]' -f $t, $r
    if ($r -ne 'sub') { $fail = 1 }
}
"v=$($PSVersionTable.PSVersion) mode=$Mode Get-Command=$((Get-Command demotool).CommandType)"
Set-Location $HOME
Remove-Item -Recurse -Force -LiteralPath $dir
exit $fail
