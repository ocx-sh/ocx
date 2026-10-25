# dir-guard.ps1 [-Guard] : a directory-change hook body must run only for a FileSystem location: Set-Location Env:\ (or HKCU:\, Cert:\) fires the same hook.
# 7.x: a LocationChangedAction delegate; 5.1: a prompt wrapper with a $PWD compare. The body counts $global:acted and records the provider name.
# Violation (exit 1) = the body acted on a non-FileSystem location. -Guard adds `$PWD.Provider.Name -eq 'FileSystem'` to the body.
param([switch]$Guard)
$ErrorActionPreference = 'Stop'
$global:acted = New-Object System.Collections.Generic.List[string]
$body = { if ($Guard -and $PWD.Provider.Name -ne 'FileSystem') { return }; $global:acted.Add($PWD.Provider.Name) }
$ic = $ExecutionContext.SessionState.InvokeCommand
$has = $null -ne ($ic | Get-Member -Name LocationChangedAction)
if ($has) { $ic.LocationChangedAction = [Delegate]::Combine($ic.LocationChangedAction, [EventHandler[System.Management.Automation.LocationChangedEventArgs]] { & $body }.GetNewClosure()) }
else { $global:__last = "$PWD"; $global:__body = $body; function global:prompt { if ("$PWD" -ne $global:__last) { $global:__last = "$PWD"; & $global:__body }; 'P> ' } }
$d = Join-Path ([IO.Path]::GetTempPath()) ('dirguard-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory -Force -Path $d
foreach ($step in { Set-Location $d }, { Set-Location Env:\ }, { Set-Location $d }) { & $step; if (-not $has) { $null = & $function:prompt } }
Set-Location $HOME
Remove-Item -Recurse -Force -LiteralPath $d
$bad = @($global:acted | Where-Object { $_ -ne 'FileSystem' })
"v=$($PSVersionTable.PSVersion) form=$(if ($has) { 'LocationChangedAction' } else { 'prompt-wrap' }) guard=$($Guard.IsPresent) acted=[$($global:acted -join ',')]"
if ($bad.Count) { "FAIL: the hook body ran for provider(s): $($bad -join ',')"; exit 1 } else { 'OK'; exit 0 }
