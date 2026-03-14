# install.ps1 — OCX installer for Windows
# https://ocx.sh
#
# Usage:
#   irm https://ocx.sh/install.ps1 | iex
#   $env:OCX_NO_MODIFY_PATH = '1'; irm https://ocx.sh/install.ps1 | iex
#   & { $Version = '0.5.0'; irm https://ocx.sh/install.ps1 | iex }
#
# Future enhancements (not yet implemented):
#   - Download retry with backoff
#   - GPG/cosign signature verification
#   - Custom install location flag
#   - --force / -y flag for non-interactive mode

#Requires -Version 5.1

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$GitHubRepo = 'ocx-sh/ocx'
$GitHubDownloadUrl = "https://github.com/$GitHubRepo/releases/download"
$GitHubApiUrl = "https://api.github.com/repos/$GitHubRepo/releases"

# --- Output helpers ---

function Say {
    param([string]$Message)
    Write-Host "ocx-install: $Message"
}

function Err {
    param([string]$Message)
    Write-Host "ocx-install: error: $Message" -ForegroundColor Red
    exit 1
}

function Warn {
    param([string]$Message)
    Write-Host "ocx-install: warning: $Message" -ForegroundColor Yellow
}

# --- Platform detection ---

function Detect-Architecture {
    # Try .NET RuntimeInformation first (PowerShell 7+ / .NET Framework 4.7.1+)
    try {
        $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        switch ($arch) {
            'X64'   { return 'x86_64-pc-windows-msvc' }
            'Arm64' { return 'aarch64-pc-windows-msvc' }
            'X86'   { Err '32-bit Windows is not supported. OCX requires a 64-bit system.' }
            'Arm'   { Err '32-bit ARM Windows is not supported. OCX requires a 64-bit system.' }
            default { Err "Unsupported architecture: $arch" }
        }
    }
    catch {
        # Fallback for older PowerShell / .NET versions
    }

    # Fallback: PROCESSOR_ARCHITECTURE environment variable
    $procArch = $env:PROCESSOR_ARCHITECTURE
    switch ($procArch) {
        'AMD64' { return 'x86_64-pc-windows-msvc' }
        'ARM64' { return 'aarch64-pc-windows-msvc' }
        'x86'   { Err '32-bit Windows is not supported. OCX requires a 64-bit system.' }
        default { Err "Unsupported architecture: $procArch" }
    }
}

# --- Download utilities ---

function Download-File {
    param(
        [string]$Url,
        [string]$Destination
    )

    $headers = @{}
    if ($env:GITHUB_TOKEN) {
        $headers['Authorization'] = "token $env:GITHUB_TOKEN"
    }

    # Use TLS 1.2+ (required by GitHub)
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13

    try {
        $ProgressPreference = 'SilentlyContinue'
        Invoke-WebRequest -Uri $Url -OutFile $Destination -Headers $headers -UseBasicParsing
    }
    catch {
        return $false
    }
    return $true
}

function Download-String {
    param([string]$Url)

    $headers = @{}
    if ($env:GITHUB_TOKEN) {
        $headers['Authorization'] = "token $env:GITHUB_TOKEN"
    }

    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13

    $ProgressPreference = 'SilentlyContinue'
    (Invoke-WebRequest -Uri $Url -Headers $headers -UseBasicParsing).Content
}

# --- Checksum verification ---

function Verify-Checksum {
    param(
        [string]$Dir,
        [string]$File
    )

    $checksumFile = Join-Path $Dir 'sha256.sum'
    $checksumContent = Get-Content $checksumFile -Raw

    # Find the expected hash for our file
    $expected = $null
    foreach ($line in $checksumContent.Split("`n")) {
        $line = $line.Trim()
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+(.+)$') {
            # Strip leading '*' (BSD-style binary mode indicator from cargo-dist)
            $matchedFile = $Matches[2].Trim().TrimStart('*')
            if ($matchedFile -eq $File) {
                $expected = $Matches[1].ToLower()
                break
            }
        }
    }

    if (-not $expected) {
        Err "Checksum for $File not found in sha256.sum"
    }

    $filePath = Join-Path $Dir $File
    $actual = (Get-FileHash -Path $filePath -Algorithm SHA256).Hash.ToLower()

    if ($expected -ne $actual) {
        Err "Checksum mismatch for $File`n  expected: $expected`n  got:      $actual"
    }

    Say 'Checksum verified.'
}

# --- Version resolution ---

function Get-LatestVersion {
    try {
        $releaseInfo = Download-String "$GitHubApiUrl/latest"
    }
    catch {
        if (-not $env:GITHUB_TOKEN) {
            Err "Failed to fetch latest release from GitHub.`nThis may be a rate-limit issue. Try setting GITHUB_TOKEN:`n  `$env:GITHUB_TOKEN = 'ghp_...'`n  irm https://ocx.sh/install.ps1 | iex"
        }
        else {
            Err 'Failed to fetch latest release from GitHub — check your internet connection and token.'
        }
    }

    # Parse tag_name from JSON
    if ($releaseInfo -match '"tag_name"\s*:\s*"([^"]+)"') {
        $tag = $Matches[1]
        # Strip leading 'v'
        return $tag -replace '^v', ''
    }

    Err 'Could not determine latest version from GitHub.'
}

# --- Environment file creation ---

function Create-EnvFile {
    param([string]$OcxHome)

    $envFile = Join-Path $OcxHome 'env.ps1'

    $envContent = @'
# OCX shell environment — generated by install.ps1
# Sourced by your PowerShell profile to add OCX to PATH and enable completions.
# Manual changes will be overwritten on reinstall.
$_OcxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { Join-Path $env:USERPROFILE '.ocx' }
$_OcxBin = Join-Path $_OcxHome 'installs\ocx.sh\ocx\current\bin'
if ($env:PATH -notlike "*$_OcxBin*") {
    $env:PATH = "$_OcxBin;$env:PATH"
}
$_OcxExe = Join-Path $_OcxBin 'ocx.exe'
if (Test-Path $_OcxExe) {
    try { & $_OcxExe --offline shell profile load --shell powershell 2>$null | Invoke-Expression } catch {}
    try { & $_OcxExe --offline shell completion --shell powershell 2>$null | Invoke-Expression } catch {}
}
Remove-Variable _OcxHome, _OcxBin, _OcxExe -ErrorAction SilentlyContinue
'@

    Set-Content -Path $envFile -Value $envContent -Encoding UTF8
}

# --- Profile modification ---

function Modify-Profile {
    param([string]$OcxHome)

    $profilePath = $PROFILE.CurrentUserCurrentHost

    # Build the source line
    if ($OcxHome -eq (Join-Path $env:USERPROFILE '.ocx')) {
        $sourceLine = '. "$env:USERPROFILE\.ocx\env.ps1"'
    }
    else {
        $sourceLine = ". `"$OcxHome\env.ps1`""
    }

    # Create profile directory and file if they don't exist
    $profileDir = Split-Path $profilePath -Parent
    if ($profileDir -and -not (Test-Path $profileDir)) {
        New-Item -ItemType Directory -Path $profileDir -Force | Out-Null
    }

    if (-not (Test-Path $profilePath)) {
        New-Item -ItemType File -Path $profilePath -Force | Out-Null
    }

    # Idempotent: skip if already present
    $profileContent = Get-Content $profilePath -Raw -ErrorAction SilentlyContinue
    if ($profileContent -and $profileContent.Contains('.ocx\env.ps1')) {
        Say "PowerShell profile already configured ($profilePath)."
        return
    }

    # Append source line
    Add-Content -Path $profilePath -Value "`n# OCX`n$sourceLine"
    Say "Added OCX to $profilePath"
}

# --- Success message ---

function Print-Success {
    param(
        [string]$InstalledVersion,
        [string]$OldVersion = ''
    )

    $ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { Join-Path $env:USERPROFILE '.ocx' }

    Write-Host ''
    if ($OldVersion -and $OldVersion -ne $InstalledVersion) {
        Write-Host "  ocx upgraded: $OldVersion -> $InstalledVersion" -ForegroundColor Green
    }
    else {
        Write-Host "  ocx $InstalledVersion installed successfully!" -ForegroundColor Green
    }

    Write-Host @"

  To get started, restart your shell or run:

    . "$ocxHome\env.ps1"

  Then verify with:

    ocx info

  To uninstall, remove the OCX home directory:

    Remove-Item -Recurse -Force "$ocxHome"

"@
}

# --- Main ---

function Main {
    # Read parameters from caller scope (for piped execution: & { $Version = '0.5.0'; irm ... | iex })
    $requestedVersion = if (Get-Variable -Name 'Version' -Scope 1 -ErrorAction SilentlyContinue) {
        (Get-Variable -Name 'Version' -Scope 1).Value
    } else { '' }

    $noModifyPath = $env:OCX_NO_MODIFY_PATH
    $skipProfile = $false
    if ($noModifyPath -match '^(1|true|yes)$') {
        $skipProfile = $true
    }

    $ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { Join-Path $env:USERPROFILE '.ocx' }

    # Detect architecture
    $target = Detect-Architecture
    Say "Detected platform: $target"

    # Resolve version
    if (-not $requestedVersion) {
        Say 'Fetching latest version...'
        $requestedVersion = Get-LatestVersion
    }

    # Validate version format
    if ($requestedVersion -notmatch '^\d+\.\d+\.\d+') {
        Err "Invalid version format: $requestedVersion (expected semver like 1.2.3)"
    }

    # Detect existing installation for upgrade messaging
    $oldVersion = ''
    $existingBin = Join-Path $ocxHome 'installs\ocx.sh\ocx\current\bin\ocx.exe'
    if (Test-Path $existingBin) {
        try { $oldVersion = & $existingBin version 2>$null } catch {}
    }

    Say "Installing ocx v$requestedVersion..."

    # Create temporary directory
    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "ocx-install-$([System.Guid]::NewGuid().ToString('N').Substring(0,8))"
    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

    try {
        # Download archive and checksums
        $archive = "ocx-$target.zip"
        $tag = "v$requestedVersion"
        $archiveUrl = "$GitHubDownloadUrl/$tag/$archive"
        $checksumUrl = "$GitHubDownloadUrl/$tag/sha256.sum"

        Say "Downloading $archive..."
        $downloaded = Download-File -Url $archiveUrl -Destination (Join-Path $tmpDir $archive)
        if (-not $downloaded) {
            Err "Failed to download $archiveUrl`nEnsure v$requestedVersion is a valid release with a binary for $target.`nAvailable releases: https://github.com/$GitHubRepo/releases"
        }

        $checksumDownloaded = Download-File -Url $checksumUrl -Destination (Join-Path $tmpDir 'sha256.sum')
        if (-not $checksumDownloaded) {
            Err "Failed to download checksums from $checksumUrl"
        }

        # Verify checksum
        Verify-Checksum -Dir $tmpDir -File $archive

        # Extract archive
        $extractDir = Join-Path $tmpDir 'extracted'
        try {
            Expand-Archive -Path (Join-Path $tmpDir $archive) -DestinationPath $extractDir -Force
        }
        catch {
            Err "Failed to extract $archive — $($_.Exception.Message)"
        }

        # Locate binary — cargo-dist puts it in a target-named subdirectory
        $bin = $null
        $candidatePaths = @(
            (Join-Path $extractDir "ocx-$target\ocx.exe"),
            (Join-Path $extractDir 'ocx.exe')
        )
        foreach ($candidate in $candidatePaths) {
            if (Test-Path $candidate) {
                $bin = $candidate
                break
            }
        }

        if (-not $bin) {
            Err 'Could not find ocx.exe binary in archive.'
        }

        # Smoke-test the binary before installing
        try {
            $null = & $bin version 2>$null
        }
        catch {
            Warn 'Binary failed to execute — it may be blocked by antivirus or execution policy.'
        }

        # PATH shadowing: warn if a different ocx.exe already exists on PATH
        $existingOcx = Get-Command ocx -ErrorAction SilentlyContinue
        if ($existingOcx -and -not $existingOcx.Source.StartsWith($ocxHome)) {
            Warn "An existing ocx was found at $($existingOcx.Source)"
            Warn 'The new install may be shadowed — check your PATH order.'
        }

        # Bootstrap: OCX installs itself into its own package store
        Say 'Bootstrapping OCX into its own package store...'
        & $bin --remote install --select "ocx.sh/ocx:$requestedVersion"
        if ($LASTEXITCODE -ne 0) {
            Err "Bootstrap failed: 'ocx --remote install --select ocx.sh/ocx:$requestedVersion'`nEnsure ocx v$requestedVersion is published to the ocx.sh registry."
        }
        $installDir = Join-Path $ocxHome 'installs\ocx.sh\ocx\current\bin'
        Say "Installed to $installDir\ocx.exe"

        # Create environment file
        if (-not (Test-Path $ocxHome)) {
            New-Item -ItemType Directory -Path $ocxHome -Force | Out-Null
        }
        Create-EnvFile -OcxHome $ocxHome

        # Modify PowerShell profile
        if ($skipProfile) {
            Say 'Skipping profile modification (OCX_NO_MODIFY_PATH).'
        }
        else {
            try {
                Modify-Profile -OcxHome $ocxHome
            }
            catch {
                Warn "Failed to modify PowerShell profile: $($_.Exception.Message)"
                Warn 'You can manually add OCX to your profile by running:'
                Warn "  Add-Content `$PROFILE '`. `"$ocxHome\env.ps1`"'"
            }
        }

        # Export GitHub Actions path if in CI
        if ($env:GITHUB_PATH) {
            $ghBinPath = Join-Path $ocxHome 'installs\ocx.sh\ocx\current\bin'
            try {
                Add-Content -Path $env:GITHUB_PATH -Value $ghBinPath
            }
            catch {
                Warn 'Failed to write to $GITHUB_PATH.'
            }
        }

        Print-Success -InstalledVersion $requestedVersion -OldVersion $oldVersion
    }
    finally {
        # Cleanup temp directory
        Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    }
}

Main
