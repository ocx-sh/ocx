#Requires -Version 5.1
# test-windows-activation.ps1 — Windows shell-activation gate harness.
# Runnable under Windows PowerShell 5.1 and PowerShell 7.
#
# Usage (CI):
#   powershell -NoProfile -File .github\scripts\test-windows-activation.ps1
#   pwsh       -NoProfile -File .github\scripts\test-windows-activation.ps1
#
# Usage (local):
#   .\.github\scripts\test-windows-activation.ps1 -OcxBin .\target\x86_64-pc-windows-msvc\release\ocx.exe

param(
    # Path to the freshly built release ocx.exe.
    [string]$OcxBin = '',
    # Path to the installer script (dot-sourced to obtain Create-EnvFile).
    [string]$InstallPs1 = ''
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host "Running Windows activation gate on PowerShell $($PSVersionTable.PSVersion)"

# ---------------------------------------------------------------------------
# Resolve repo root (this script lives at .github/scripts/, two levels up).
# ---------------------------------------------------------------------------
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..' ))

if (-not $OcxBin) {
    $OcxBin = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\ocx.exe'
}
if (-not $InstallPs1) {
    $InstallPs1 = Join-Path $repoRoot 'website\src\public\install.ps1'
}

if (-not (Test-Path $OcxBin -PathType Leaf)) {
    throw "ocx.exe not found at: $OcxBin`nBuild first: cargo build --release --locked -p ocx_cli --target x86_64-pc-windows-msvc"
}
if (-not (Test-Path $InstallPs1 -PathType Leaf)) {
    throw "install.ps1 not found at: $InstallPs1"
}

$ocxBin = [System.IO.Path]::GetFullPath($OcxBin)
Write-Host "  ocx.exe : $ocxBin"
Write-Host "  install : $InstallPs1"

# ---------------------------------------------------------------------------
# Create a fresh temp OCX_HOME with the expected package layout.
# ---------------------------------------------------------------------------
$tmpBase = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$env:OCX_HOME = Join-Path $tmpBase "ocx-activation-test-$([System.Guid]::NewGuid().ToString('N').Substring(0, 8))"

$contentBinDir = Join-Path $env:OCX_HOME 'symlinks\ocx.sh\ocx\cli\current\content\bin'
[System.IO.Directory]::CreateDirectory($contentBinDir) | Out-Null
Copy-Item -Path $ocxBin -Destination (Join-Path $contentBinDir 'ocx.exe')
Write-Host "  OCX_HOME: $env:OCX_HOME"

# ---------------------------------------------------------------------------
# Generate the real env.ps1 by dot-sourcing install.ps1 and calling
# Create-EnvFile.  The Main-invocation guard in install.ps1 prevents the
# installer from running when dot-sourced.
# ---------------------------------------------------------------------------
. $InstallPs1
Create-EnvFile -OcxHome $env:OCX_HOME

$envPs1 = Join-Path $env:OCX_HOME 'env.ps1'
if (-not (Test-Path $envPs1 -PathType Leaf)) {
    throw "env.ps1 was not written by Create-EnvFile at: $envPs1"
}
Write-Host "  env.ps1 written: $envPs1"

# ---------------------------------------------------------------------------
# Gap D — null-bind guard:
# env.ps1 must be dot-sourceable without throwing
# "Cannot bind argument to parameter 'Command' because it is null".
# The binary is present, so the `self activate` path runs end-to-end.
# After sourcing: OCX_ACTIVATED must be set and content\bin must be on PATH.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '[Gap D] env.ps1 null-bind guard + PATH + activation ...'
Remove-Item Env:_OCX_ENV_LOADED -ErrorAction SilentlyContinue

try {
    . $envPs1
} catch {
    throw "Gap D FAIL: env.ps1 threw on dot-source: $($_.Exception.Message)"
}

# env.ps1 captures `self activate` output and Invoke-Expressions it.
# The `self activate` stream sets $env:OCX_ACTIVATED = '1'.
if ($env:OCX_ACTIVATED -ne '1') {
    throw "Gap D FAIL: `$env:OCX_ACTIVATED not set to '1' after dot-sourcing env.ps1 (got: '$env:OCX_ACTIVATED')"
}

# content\bin directory must now be on PATH (added by the activation stream).
if ($env:PATH -notlike "*$contentBinDir*") {
    throw "Gap D FAIL: content\bin dir not found on `$env:PATH after activation"
}
Write-Host '[Gap D] PASS — env.ps1 dot-sourced cleanly; OCX_ACTIVATED=1; content\bin on PATH'

# ---------------------------------------------------------------------------
# Second bug — activation stream must be non-empty AND using-namespace-free.
# `using namespace` belongs only inside the completion file, never in the
# activation stream itself.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '[Second bug] Activation stream using-namespace-free ...'

$_stream = (& $ocxBin self activate --shell=powershell --no-completion 2>$null) | Out-String

if (-not $_stream -or $_stream.Trim() -eq '') {
    throw "Second bug FAIL: activation stream is empty — PATH prepend line must always be emitted"
}
if ($_stream -match '\busing\s+namespace\b') {
    throw "Second bug FAIL: activation stream contains 'using namespace' — must be using-free; got: $_stream"
}
Write-Host '[Second bug] PASS — stream non-empty and using-namespace-free'

# ---------------------------------------------------------------------------
# Gap E — --completion path (RELEASE binary required; debug panics on clap
# debug-assert).  Asserts: completion file written + dot-sources without throw.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '[Gap E] Completion file written and dot-sourceable ...'

# Run --completion; capture output (we don't need it here, just trigger the write).
$null = (& $ocxBin self activate --shell=powershell --completion 2>$null) | Out-String

$completionFile = Join-Path $env:OCX_HOME 'state\completions\ocx.ps1'
if (-not (Test-Path $completionFile -PathType Leaf)) {
    throw "Gap E FAIL: completion file not written at: $completionFile"
}

try {
    . $completionFile
} catch {
    throw "Gap E FAIL: completion file threw on dot-source: $($_.Exception.Message)"
}
Write-Host "[Gap E] PASS — completion file exists and dot-sources cleanly: $completionFile"

# ---------------------------------------------------------------------------
# Gap A — empty OCX_HOME: --global env must exit 0 and emit no output when
# no global toolchain is configured (no packages installed).
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host '[Gap A] --global env with empty OCX_HOME ...'

$savedOcxHome = $env:OCX_HOME
$emptyHome = Join-Path $tmpBase "ocx-empty-home-$([System.Guid]::NewGuid().ToString('N').Substring(0, 8))"
[System.IO.Directory]::CreateDirectory($emptyHome) | Out-Null
$env:OCX_HOME = $emptyHome

$_globalEnvOut = (& $ocxBin --global env --shell=powershell 2>$null) | Out-String
$_exitCode = $LASTEXITCODE

$env:OCX_HOME = $savedOcxHome

if ($_exitCode -ne 0) {
    throw "Gap A FAIL: --global env exited $($_exitCode) (expected 0) with empty OCX_HOME"
}
if ($_globalEnvOut -and $_globalEnvOut.Trim() -ne '') {
    throw "Gap A FAIL: --global env emitted non-empty output with empty OCX_HOME: $_globalEnvOut"
}
Write-Host '[Gap A] PASS — --global env exits 0 with empty output on empty OCX_HOME'

# ---------------------------------------------------------------------------
# Done.
# ---------------------------------------------------------------------------
Write-Host ''
Write-Host "ALL ACTIVATION CHECKS PASSED (PowerShell $($PSVersionTable.PSVersion))"
