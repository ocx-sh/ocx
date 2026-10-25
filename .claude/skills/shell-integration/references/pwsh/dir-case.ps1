# dir-case.ps1 IMPL [-Twice] [-NoUser] : directory-change hook forms for PowerShell. IMPL = plant-assign | plant-append | twin | twin-prompt.
# On Windows PowerShell 5.1 (no LocationChangedAction) only twin-prompt applies. Run it on 7.4, 7.6 and 5.1.
# A user handler is installed first (profile order); IMPL is applied (twice with -Twice); then the session does
#   Set-Location d1 ; Set-Location d2 ; Push-Location d3 ; Pop-Location     (4 directory changes, d1..d3 fresh temp dirs)
# and, for the prompt-based twin, the host calls `prompt` after each. Counters: $global:__xUser (user handler), $global:__xFired (ours).
# Exit 1 = violation (user handler count != 4, ours != 4, a throw), 0 = compliant.
param([Parameter(Mandatory)][string]$Impl, [switch]$Twice, [switch]$NoUser)
$ErrorActionPreference = 'Stop'
$global:__xFired = 0; $global:__xUser = 0
$ic = $ExecutionContext.SessionState.InvokeCommand
$has = $null -ne ($ic | Get-Member -Name LocationChangedAction)
function global:prompt { 'USERP> ' }
if (-not $NoUser -and $has) { $ic.LocationChangedAction = [EventHandler[System.Management.Automation.LocationChangedEventArgs]] { $global:__xUser++ } }
function Install {
  switch ($Impl) {
    'plant-assign' { $ic.LocationChangedAction = [EventHandler[System.Management.Automation.LocationChangedEventArgs]] { $global:__xFired++ } }
    'plant-append' { $ic.LocationChangedAction += [EventHandler[System.Management.Automation.LocationChangedEventArgs]] { $global:__xFired++ } }
    'twin' {
      if ($global:__xDirHandler) { $ic.LocationChangedAction = [Delegate]::Remove($ic.LocationChangedAction, $global:__xDirHandler) }
      $global:__xDirHandler = [EventHandler[System.Management.Automation.LocationChangedEventArgs]] { $global:__xFired++ }
      $ic.LocationChangedAction = [Delegate]::Combine($ic.LocationChangedAction, $global:__xDirHandler)
    }
    'twin-prompt' {
      # works where LocationChangedAction does not exist (Windows PowerShell 5.1): wrap prompt once, compare $PWD
      if ("$($function:prompt)" -notmatch '__xDirMarker') {
        $global:__xLastPwd = "$PWD"; $global:__xPrevPrompt = $function:prompt
        function global:prompt { $__xl = $global:LASTEXITCODE; $null = '__xDirMarker'; if ("$PWD" -ne $global:__xLastPwd) { $global:__xLastPwd = "$PWD"; $global:__xFired++ }; $__xo = & $global:__xPrevPrompt; $global:LASTEXITCODE = $__xl; $__xo }
      }
    }
  }
}
$why = @()
if ($Impl -ne 'twin-prompt' -and -not $has) { "impl=$Impl v=$($PSVersionTable.PSVersion) LocationChangedAction=absent"; 'FAIL: no LocationChangedAction on this version'; exit 1 }
try {
  Install; if ($Twice) { Install }
  $root = Join-Path ([IO.Path]::GetTempPath()) ('dircase-' + [guid]::NewGuid().ToString('N')); $d = 1..3 | ForEach-Object { (New-Item -ItemType Directory -Path (Join-Path $root "d$_")).FullName }
  foreach ($step in { Set-Location $d[0] }, { Set-Location $d[1] }, { Push-Location $d[2] }, { Pop-Location }) { & $step; if ($Impl -eq 'twin-prompt') { $null = & $function:prompt } }
} catch { $why += 'ERROR:' + $_.Exception.Message }
$nUser = $global:__xUser; $nOurs = $global:__xFired
Set-Location $HOME
if ($root -and (Test-Path -LiteralPath $root)) { Remove-Item -Recurse -Force -LiteralPath $root }
if (-not $NoUser -and $has -and $nUser -ne 4) { $why += "user handler ran ${nUser}x (want 4)" }
if ($nOurs -ne 4) { $why += "ours ran ${nOurs}x (want 4)" }
"impl=$Impl twice=$($Twice.IsPresent) v=$($PSVersionTable.PSVersion) hasLocationChangedAction=$has user=$nUser ours=$nOurs"
if ($why.Count -eq 0) { 'OK'; exit 0 } else { $why | ForEach-Object { "FAIL: $_" }; exit 1 }
