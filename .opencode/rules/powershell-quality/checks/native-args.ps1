# native-args.ps1 -- print what a native child receives for the five argument hazards.
# Run it under every engine the script targets (Windows PowerShell 5.1, pwsh 7.x) and compare the rows.
# A probe, not a gate: it always exits 0. To see 7.x Legacy, set $PSNativeCommandArgumentPassing = "Legacy" in the calling session first.
# Engines run: Windows PowerShell 5.1.26100.9444, pwsh 7.4.7, pwsh 7.6.6 (2026-10-11).
if ($args.Count -gt 0 -and $args[0] -eq 'child') {
    'argc={0} {1}' -f ($args.Count - 1), (($args | Select-Object -Skip 1 | ForEach-Object { "[$_]" }) -join ' ')
    return
}
$exe = (Get-Process -Id $PID).Path
$mode = if (Test-Path variable:PSNativeCommandArgumentPassing) { $PSNativeCommandArgumentPassing } else { 'absent (Legacy)' }
"engine=$($PSVersionTable.PSVersion) mode=$mode"
function Show([string]$Name, [scriptblock]$Call) {
    $out = & $Call 2>&1 | ForEach-Object { "$_" }
    '{0,-20} {1}' -f $Name, ($out -join ' ')
}
$child = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath, 'child')
Show 'empty-middle' { & $exe @child 'a' '' 'b' }
Show 'embedded-dquote' { & $exe @child 'say "hi"' }
Show 'dquote-and-space' { & $exe @child 'say "hi there"' }
Show 'trailing-backslash' { & $exe @child 'C:\dir with space\' }
Show 'splatted-empty' { $a = @('x', '', 'y'); & $exe @child @a }
