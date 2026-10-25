#Requires -Version 5.1
<#
Invoke-PSGate.ps1 -Path DIR -Settings FILE.psd1 [-RequiredVersion 1.25.0] [-Module PATH]
The PSScriptAnalyzer half of the PowerShell gate (PS-GATE-01). Run sequentially, never under ForEach-Object -Parallel.
Exit 0 clean, 1 findings, 2 gate misconfigured or no result on this host. A misconfigured gate never exits 0 and never exits 1:
  - path or settings file missing, or no .ps1/.psm1/.psd1 file under the path (a vacuous gate is a green lie)
  - Severity is set in the settings and omits ParseError (an absent Severity reports it, and is valid)
  - a rule named in ExcludeRules, IncludeRules or Rules that PSScriptAnalyzer does not know (a typo disables nothing)
  - the analyzer itself failed (a RULE_ERROR on a broken manifest is not a finding)
  - NO-RESULT, never a finding: a record that is an environment limit of this host, as in gates-parse-gate.ps1:
    TypeNotFound (a type from an assembly or module not loaded here, such as System.Drawing; PSScriptAnalyzer reports it
    as an Information record), ModuleNotFoundDuringParse and ResourceNotDefined (a DSC resource). On a non-Windows host the DSC Configuration keyword also fails ("Unable to
    find DSC schema store") and every ParseError after it in that file is the same limit. They print as "environment:"
    lines and exit 2 when nothing else is wrong; a real finding still exits 1. Judge those files on Windows.
Prints "file:line: Severity Rule: message" per finding and a summary line. ASCII only, 5.1 syntax.
Watched red and green on pwsh 7.4.7 (PSScriptAnalyzer 1.25.0), pwsh 7.6.6 and Windows PowerShell 5.1.26100.9444 (measured 2026-10-11).
Environment classification watched on pwsh 7.6.6 on Linux: a DSC Configuration file and a file with a missing module or base type exit 2, a real syntax error still exits 1.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$Settings,
    [string]$RequiredVersion = '',
    [string]$Module = ''
)
$ErrorActionPreference = 'Stop'
function Stop-Gate([string]$Message) { [Console]::Error.WriteLine("psgate: $Message"); exit 2 }
try {
    if ($Module) { Import-Module $Module }
    elseif ($RequiredVersion) { Import-Module PSScriptAnalyzer -RequiredVersion $RequiredVersion }
    else { Import-Module PSScriptAnalyzer }
} catch { Stop-Gate "cannot load PSScriptAnalyzer: $(($_.Exception.Message -replace '\s+', ' '))" }
if (-not (Test-Path -LiteralPath $Path)) { Stop-Gate "path not found: $Path" }
if (-not (Test-Path -LiteralPath $Settings -PathType Leaf)) { Stop-Gate "settings not found: $Settings" }
$cfg = Import-PowerShellDataFile -LiteralPath $Settings
if ($cfg.ContainsKey('Severity') -and 'ParseError' -notin @($cfg.Severity)) {
    Stop-Gate 'Severity is set and omits ParseError (an absent Severity reports every severity, ParseError included)'
}
$known = (Get-ScriptAnalyzerRule).RuleName
$named = @($cfg.ExcludeRules) + @($cfg.IncludeRules) + @($cfg.Rules.Keys) | Where-Object { $_ -and $_ -notmatch '[*?]' }
$unknown = @($named | Where-Object { $_ -notin $known })
if ($unknown.Count -gt 0) { Stop-Gate "unknown rule name(s) in settings: $($unknown -join ', ')" }
$files = @(Get-ChildItem -LiteralPath $Path -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1', '.psd1' })
if ($files.Count -eq 0) { Stop-Gate "no .ps1/.psm1/.psd1 files under $Path" }
try { $findings = @(Invoke-ScriptAnalyzer -Path $Path -Recurse -Settings $Settings -ErrorAction Stop) }
catch { Stop-Gate "analyzer error (exit 2, not 1): $(($_.Exception.Message -replace '\s+', ' '))" }
$onWindows = ($PSVersionTable.PSEdition -eq 'Desktop') -or ($IsWindows -eq $true)
$envIds = 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined'
$dscFiles = @($findings | Where-Object { $_.Severity -eq 'ParseError' -and $_.Message -match 'DSC schema store' } | ForEach-Object { $_.ScriptPath })
$real = @()
$limits = @()
foreach ($f in $findings) {
    if ($f.RuleName -in $envIds -or ($f.Severity -eq 'ParseError' -and -not $onWindows -and $f.ScriptPath -in $dscFiles)) { $limits += $f }
    else { $real += $f }
}
foreach ($f in $real) { '{0}:{1}: {2} {3}: {4}' -f $f.ScriptPath, $f.Line, $f.Severity, $f.RuleName, ($f.Message -replace '\s+', ' ') }
foreach ($f in $limits) { '{0}:{1}: environment: {2}' -f $f.ScriptPath, $f.Line, ($f.Message -replace '\s+', ' ') }
'psgate: {0} files, {1} findings' -f $files.Count, $real.Count
if ($real.Count -gt 0) { exit 1 }
if ($limits.Count -gt 0) {
    $n = @($limits | ForEach-Object { $_.ScriptPath } | Sort-Object -Unique).Count
    Stop-Gate ('NO-RESULT: {0} records in {1} file(s) are environment limits of this host (a type, module or DSC resource not available here), not findings; judge those files on Windows' -f $limits.Count, $n)
}
exit 0
