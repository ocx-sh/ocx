# hook-case.ps1 IMPL [-Twice] [-Undo]   or   hook-case.ps1 -ImplFile PATH [-Twice]
# IMPL = plant-clobber | plant-wrap-unguarded | plant-wrap-leaky | twin (the built-in reference forms).
# -ImplFile dot-sources your own emitted prompt-hook file instead, once (twice with -Twice). Your file must
# add one to $global:__xFired every time its hook runs and must not write to the output stream.
# Simulates the host calling `prompt` three times. Run it on 7.4, 7.6 and Windows PowerShell 5.1.
# Exit 1 = violation, 0 = compliant. A hook counts as fired through $global:__xFired, never through the output stream:
# the host uses the prompt function's output as the prompt text, so a hook that writes there corrupts the prompt (plant-wrap-leaky).
param([string]$Impl = '', [string]$ImplFile = '', [switch]$Twice, [switch]$Undo)
$ErrorActionPreference = 'Stop'
$global:__xFired = 0; $global:__xUser = 0
function global:prompt { $global:__xUser++; 'USERP> ' }      # the user's own prompt, defined first (profile order)

function Install-PlantClobber {
  # PLANT: redefine the prompt outright
  function global:prompt { $global:__xFired++; 'OURS> ' }
}
function Install-PlantWrapUnguarded {
  # PLANT: save-and-wrap with no guard; a second registration saves our own wrapper and recurses
  # (depth-capped at 40 so the fixture terminates; uncapped it recurses until the host dies)
  $global:__xPrev = $function:prompt
  $global:__xDepth = 0
  function global:prompt { $global:__xFired++; $global:__xDepth++; if ($global:__xDepth -gt 40) { throw 'prompt recursion' }; & $global:__xPrev; $global:__xDepth-- }
}
function Install-PlantWrapLeaky {
  # PLANT: guarded and counted, but writes a status line to the output stream, which becomes part of the prompt text
  if ("$($function:prompt)" -notmatch '__xLeakMarker') {
    $global:__xPrevPrompt = $function:prompt
    function global:prompt { $global:__xFired++; 'hook ran'; $null = '__xLeakMarker'; & $global:__xPrevPrompt }
  }
}
function Install-Twin {
  # TWIN: probe the LIVE prompt body for the marker (a clobber that kept a variable is re-wrapped), wrap once
  if ("$($function:prompt)" -notmatch '__xHookMarker') {
    $global:__xPrevPrompt = $function:prompt
    function global:prompt {
      $__xLast = $global:LASTEXITCODE                     # first statements: capture $LASTEXITCODE (and $? where the hook needs it)
      $global:__xFired++; $null = '__xHookMarker'
      $__xOut = if ($global:__xPrevPrompt) { & $global:__xPrevPrompt } else { "PS $($PWD.Path)> " }
      $global:LASTEXITCODE = $__xLast
      $__xOut
    }
  }
}
if ($ImplFile) {
  . $ImplFile                    # dot-sourced at script scope, as a profile is
  if ($Twice) { . $ImplFile }
} else {
  $install = switch ($Impl) {
    'plant-clobber' { 'Install-PlantClobber' } 'plant-wrap-unguarded' { 'Install-PlantWrapUnguarded' }
    'plant-wrap-leaky' { 'Install-PlantWrapLeaky' } 'twin' { 'Install-Twin' } default { throw "unknown impl $Impl" } }
  & $install
  if ($Twice) { & $install }     # the rc file sourced a second time
}
if ($Undo) { $function:prompt = $global:__xPrevPrompt; Remove-Variable -Scope Global __xPrevPrompt }   # twin undo: restore what we saved
$why = @(); $texts = @()
try { 1..3 | ForEach-Object { $r = @(& $function:prompt); $texts += ,$r } } catch { $why += 'ERROR:' + $_.Exception.Message }
foreach ($r in $texts) { if ($r.Count -ne 1) { $why += "prompt returned $($r.Count) objects, want 1: [$($r -join '|')]"; break } }
$wantOurs = if ($Undo) { 0 } else { 3 }
if ($global:__xUser -ne 3) { $why += "user prompt ran $($global:__xUser)x (want 3)" }
if ($global:__xFired -ne $wantOurs) { $why += "hook ran $($global:__xFired)x (want $wantOurs)" }
"impl=$Impl$ImplFile twice=$($Twice.IsPresent) undo=$($Undo.IsPresent)  user=$($global:__xUser) ours=$($global:__xFired)"
if ($why.Count -eq 0) { 'OK'; exit 0 } else { $why | ForEach-Object { "FAIL: $_" }; exit 1 }
