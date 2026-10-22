# env-empty.ps1 : can this PowerShell edition carry an empty environment value? Exit 1 when
# `$env:X = ''` removes the variable (7.4 and Windows PowerShell 5.1), 0 when it keeps it (7.5 and later).
# An emitter whose range includes such an edition refuses an empty value.
$env:XE = ''
"set via `$env:XE='' -> Test-Path env:XE = $(Test-Path env:XE)   PSVersion=$($PSVersionTable.PSVersion)"
Remove-Item env:XE -ErrorAction SilentlyContinue
[Environment]::SetEnvironmentVariable('XE', '', 'Process')
"set via [Environment]::SetEnvironmentVariable('XE','','Process') -> Test-Path env:XE = $(Test-Path env:XE)"
Remove-Item env:XE -ErrorAction SilentlyContinue
Set-Item -Path env:XE -Value ''
"set via Set-Item env:XE '' -> Test-Path env:XE = $(Test-Path env:XE)"
Remove-Item env:XE -ErrorAction SilentlyContinue
New-Item -Path env:XE -Value '' -Force | Out-Null
"set via New-Item env:XE -Value '' -> Test-Path env:XE = $(Test-Path env:XE)"
Remove-Item env:XE -ErrorAction SilentlyContinue
${env:XE} = 'x'; ${env:XE} = ''
"set 'x' then '' -> Test-Path env:XE = $(Test-Path env:XE)"
# does a child process see it?
$env:XE = ''
$child = if ($PSVersionTable.PSEdition -eq 'Core' -and $IsLinux) { & /usr/bin/env | Select-String -SimpleMatch 'XE=' } else { cmd /c set XE 2>&1 }
"child sees: [$child]"
$env:XE = ''
$kept = Test-Path env:XE
Remove-Item env:XE -ErrorAction SilentlyContinue
if ($kept) { exit 0 } else { exit 1 }
