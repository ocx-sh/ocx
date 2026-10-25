#Requires -Version 5.1
<#
Invoke-PesterGate.ps1 -Path DIR -RequiredVersion 6.2.0 [-MaxSkipped 0] [-ExpectedTotal N] [-ModuleBase DIR]
SH-TEST-19: the CI Pester step as a census gate. Exit 1 when anything failed OR the run proved nothing, 0 otherwise.
Red: failed tests, containers or blocks, zero tests discovered, zero passed, a total that differs from -ExpectedTotal,
skipped above -MaxSkipped, not-run, inconclusive. Run.Exit is on, so Invoke-Pester itself exits (code = failures)
when something failed and a vacuous or skip-only run returns here to be judged. -ExpectedTotal pins the test count of a
generated suite: a vanished data source shrinks the plan and the run together and only a pin catches it.
Never pass -AllowNullOrEmptyForEach in a gating run (SH-TEST-20). -ModuleBase DIR holds RequiredVersion\Pester.psd1.
Exit 0 green, 1 red, 2 the pinned Pester cannot be imported. ASCII only, 5.1 syntax.
Watched red and green on Pester 5.9.1 and 6.2.0 under pwsh 7.4.7, 7.6.6 and Windows PowerShell 5.1.26100.9444 (measured 2026-10-11).
#>
param(
    [Parameter(Mandatory = $true)][string]$Path,
    [Parameter(Mandatory = $true)][string]$RequiredVersion,
    [int]$MaxSkipped = 0,
    [int]$ExpectedTotal = -1,
    [string]$ModuleBase = ''
)
$ErrorActionPreference = 'Stop'
try {
    if ($ModuleBase) { Import-Module (Join-Path (Join-Path $ModuleBase $RequiredVersion) 'Pester.psd1') -Force }
    else { Import-Module Pester -RequiredVersion $RequiredVersion -Force }
} catch { [Console]::Error.WriteLine("pestergate: cannot import Pester $RequiredVersion"); exit 2 }
$c = New-PesterConfiguration
$c.Run.Path = $Path
$c.Run.PassThru = $true
$c.Run.Exit = $true
$c.Output.Verbosity = 'Minimal'
$r = Invoke-Pester -Configuration $c
$why = @()
if ($r.FailedCount -gt 0 -or $r.FailedBlocksCount -gt 0 -or $r.FailedContainersCount -gt 0) { $why += 'failed' }
if ($r.TotalCount -eq 0) { $why += 'zero tests discovered' } elseif ($r.PassedCount -eq 0) { $why += 'zero tests passed' }
if ($ExpectedTotal -ge 0 -and $r.TotalCount -ne $ExpectedTotal) { $why += ('total={0} but the pin says {1}' -f $r.TotalCount, $ExpectedTotal) }
if ($r.SkippedCount -gt $MaxSkipped) { $why += ('skipped={0} over budget {1}' -f $r.SkippedCount, $MaxSkipped) }
if ($r.NotRunCount -gt 0) { $why += ('notrun={0}' -f $r.NotRunCount) }
if ($r.InconclusiveCount -gt 0) { $why += ('inconclusive={0}' -f $r.InconclusiveCount) }
if ($why.Count -gt 0) { 'GATE RED: ' + ($why -join '; '); exit 1 }
'GATE GREEN: total={0} passed={1} skipped={2}' -f $r.TotalCount, $r.PassedCount, $r.SkippedCount
exit 0
