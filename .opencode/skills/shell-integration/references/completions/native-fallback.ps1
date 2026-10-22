# native-fallback.ps1 [-Mode twin|plant] : there is ONE -NativeFallback slot per session (PowerShell 7.6 and later). A user
# cover-all fallback is registered first where the parameter exists. Mode plant has the tool register -NativeFallback too,
# mode twin has it register `-Native -CommandName demotool` only. Red = the tool registration throws, or the user's fallback
# no longer serves a native command nobody registered (`uname`) afterwards.
# Run it on 7.4 (no parameter, user row reads n/a), 7.6 and Windows PowerShell 5.1.
param([ValidateSet('twin', 'plant')][string]$Mode = 'twin')
function Complete([string]$text) { @((TabExpansion2 -inputScript $text -cursorColumn $text.Length).CompletionMatches | ForEach-Object CompletionText) -join ',' }
$has = (Get-Command Register-ArgumentCompleter).Parameters.ContainsKey('NativeFallback')
if ($has) { Register-ArgumentCompleter -NativeFallback -ScriptBlock { param($w, $a, $c) [System.Management.Automation.CompletionResult]::new('userfb', 'userfb', 'ParameterValue', 'user fallback') } }
$err = ''
try {
    if ($Mode -eq 'plant') { Register-ArgumentCompleter -NativeFallback -ScriptBlock { param($w, $a, $c) [System.Management.Automation.CompletionResult]::new('toolfb', 'toolfb', 'ParameterValue', 'tool fallback') } }
    else { Register-ArgumentCompleter -Native -CommandName demotool -ScriptBlock { param($w, $a, $c) [System.Management.Automation.CompletionResult]::new('sub', 'sub', 'ParameterValue', 'tool') } }
} catch { $err = $_.Exception.Message.Split([char]10)[0].Trim() }
$user = if ($has) { Complete 'uname ' } else { 'n/a (no -NativeFallback on this version)' }
$tool = Complete 'demotool '
"mode=$Mode v=$($PSVersionTable.PSVersion) hasNativeFallback=$has toolThrew=[$err] userFallbackServes=[$user] toolCompletes=[$tool]"
if ($err -or ($has -and $user -ne 'userfb')) { exit 1 } else { exit 0 }
