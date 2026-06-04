# install.ps1 - OCX installer for Windows
# https://ocx.sh
#
# This is a thin bootstrap: it detects the architecture, downloads the
# published release archive, verifies its checksum, and then hands off to the
# downloaded binary's `ocx self setup`. `ocx self setup` owns everything that
# touches the machine - the self-install into the package store, the per-shell
# env shims under $OcxHome, and the managed PowerShell-profile activation
# block. Run `ocx self setup --help` for the full setup contract.
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

# Support Windows PowerShell 5.1+ (the default on Windows 10/11). Zip extraction
# routes through Expand-ZipSafely, which validates every entry against zip-slip
# before writing - so we don't depend on Expand-Archive's PS 7.4 hardening.
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

# --- Archive extraction ---

# Extract a .zip with zip-slip protection on PowerShell 5.1+. Expand-Archive
# only validates entry paths from PS 7.4 onwards, so we use the .NET API
# directly and reject any entry that escapes the destination directory. We
# stay on [System.IO.*] APIs (not PowerShell cmdlets) to avoid parameter-set
# binding errors under Set-StrictMode in Windows PowerShell 5.1.
function Expand-ZipSafely {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Destination
    )

    Add-Type -AssemblyName System.IO.Compression.FileSystem | Out-Null

    [System.IO.Directory]::CreateDirectory($Destination) | Out-Null
    $destRoot = [System.IO.Path]::GetFullPath($Destination).TrimEnd('\', '/')
    $sep = [System.IO.Path]::DirectorySeparatorChar

    $zip = [System.IO.Compression.ZipFile]::OpenRead($Path)
    try {
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName
            $rel = $name -replace '/', '\'

            # Reject absolute paths, drive letters, and parent-traversal segments.
            $segments = $rel.Split('\')
            if ($rel.StartsWith('\') -or $rel -match '^[A-Za-z]:' -or
                ($segments -contains '..')) {
                throw "Archive contains unsafe entry: $name"
            }

            $target = [System.IO.Path]::GetFullPath(
                [System.IO.Path]::Combine($destRoot, $rel))
            if ($target -ne $destRoot -and
                -not $target.StartsWith($destRoot + $sep,
                    [System.StringComparison]::OrdinalIgnoreCase)) {
                throw "Archive entry escapes destination: $name"
            }

            # Directory entries (zip spec uses trailing '/').
            if ($name.EndsWith('/') -or $name.EndsWith('\')) {
                [System.IO.Directory]::CreateDirectory($target) | Out-Null
                continue
            }

            $parent = [System.IO.Path]::GetDirectoryName($target)
            if ($parent) {
                [System.IO.Directory]::CreateDirectory($parent) | Out-Null
            }

            $in = $entry.Open()
            try {
                $out = [System.IO.File]::Create($target)
                try {
                    $in.CopyTo($out)
                }
                finally { $out.Dispose() }
            }
            finally { $in.Dispose() }
        }
    }
    finally {
        $zip.Dispose()
    }
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
            Err 'Failed to fetch latest release from GitHub - check your internet connection and token.'
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

# --- OCX_HOME validation ---

# Defence-in-depth: $OcxHome reaches the env.ps1 shim and the PowerShell
# profile activation block written by `ocx self setup`, embedded inside
# double-quoted strings. Reject a path that is not absolute, contains '..'
# components, or carries characters that could break out of that quoting
# context (CWE-22 / CWE-78). Mirrors install.sh guards.
function Assert-SafeOcxHome {
    param([string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        Err 'OCX_HOME must not be empty'
    }
    if (-not [System.IO.Path]::IsPathRooted($Path)) {
        Err "OCX_HOME must be an absolute path: $Path"
    }
    if ($Path -match '\.\.[\\/]' -or $Path -match '[\\/]\.\.$' -or $Path -eq '..') {
        Err "OCX_HOME must not contain '..' components: $Path"
    }
    # `"`, backtick and `$` would break the double-quoted embedding; `;` and
    # newlines would inject statements into env.ps1 / the profile. `[`, `]`,
    # `(`, `)` can interfere with PowerShell expression / index / sub-expression
    # evaluation when the path is re-interpolated. U+2028 (line separator) and
    # U+2029 (paragraph separator) are tokenized as line breaks by the
    # PowerShell parser in some hosts - treat them as injection vectors
    # (CWE-94 / CWE-78 defence-in-depth).
    if ($Path -match '["`$;\r\n\[\]()]') {
        Err "OCX_HOME contains characters unsafe for shell embedding: $Path"
    }
}

# --- Hand off to `ocx self setup` ---

# Run the downloaded binary's `ocx self setup`, which owns the package-store
# self-install, the per-shell env shims under $OcxHome, and the managed
# PowerShell-profile activation block. The installer never writes those files
# itself - `ocx self setup` is the single source of truth (see its --help).
#
# Global flags (e.g. --offline) precede the subcommand; subcommand flags (e.g.
# --no-modify-path) follow it - clap parses them at different levels.
function Invoke-SelfSetup {
    param(
        [string]$Bin,
        [string[]]$PreArgs = @(),
        [string[]]$PostArgs = @()
    )

    Say 'Running ocx self setup...'
    $argList = @($PreArgs) + @('self', 'setup') + @($PostArgs)
    & $Bin @argList
    if ($LASTEXITCODE -ne 0) {
        Err "'ocx self setup' failed - see the output above for details"
    }
}

# --- Test-mode install (CI / local dev) ---

# Install a pre-built ocx.exe as the candidate, bypassing download + registry
# bootstrap. Driven by $env:__OCX_TESTING_INSTALL_BINARY (double-underscore =
# test-only, never a supported public knob). After the candidate is placed,
# `ocx self setup --offline` writes the env shims against it (offline because
# no registry is reachable and the candidate is present).
function Install-LocalTestBinary {
    param([string]$Source, [string]$OcxHome)
    if (-not (Test-Path $Source -PathType Leaf)) {
        Err "__OCX_TESTING_INSTALL_BINARY does not point to a file: $Source"
    }
    $candDir = Join-Path $OcxHome 'symlinks\ocx.sh\ocx\cli\current\content\bin'
    Say 'Test mode: installing local binary as the candidate (no download, no bootstrap).'
    New-Item -ItemType Directory -Path $candDir -Force | Out-Null
    Copy-Item -Path $Source -Destination (Join-Path $candDir 'ocx.exe') -Force
    Say "Installed to $candDir\ocx.exe"
}

# --- Main ---

function Main {
    # Runtime PS version check - belt-and-suspenders alongside the #Requires
    # directive above. `irm ... | iex` evaluates content as a string and
    # bypasses parser-level #Requires (which only fires when executing a .ps1
    # from disk). 5.1 is the minimum because Expand-ZipSafely uses
    # System.IO.Compression.ZipFile, which ships in .NET 4.5+.
    if ($PSVersionTable.PSVersion -lt [Version]'5.1') {
        Write-Host 'ocx-install: error: PowerShell 5.1+ required.' -ForegroundColor Red
        Write-Host 'Upgrade: https://aka.ms/install-powershell'
        exit 1
    }

    # Read parameters from caller scope (for piped execution: & { $Version = '0.5.0'; irm ... | iex })
    $requestedVersion = if (Get-Variable -Name 'Version' -Scope 1 -ErrorAction SilentlyContinue) {
        (Get-Variable -Name 'Version' -Scope 1).Value
    } else { '' }

    $noModifyPath = $env:OCX_NO_MODIFY_PATH
    $skipProfile = $false
    if ($noModifyPath -match '^(1|true|yes|on)$') {
        $skipProfile = $true
    }

    # $env:USERPROFILE is null on Linux/macOS pwsh; fall back to $HOME so the
    # default OCX_HOME resolves to an absolute path on every platform (matches
    # the env.ps1 shim) instead of a relative '.ocx' that Assert-SafeOcxHome rejects.
    $_ocxBase = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    $ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } else { Join-Path $_ocxBase '.ocx' }
    Assert-SafeOcxHome -Path $ocxHome

    # Subcommand flags handed to `ocx self setup` (after the subcommand).
    $postArgs = @()
    if ($skipProfile) { $postArgs += '--no-modify-path' }

    # Test-mode hatch: install a locally-built ocx.exe as the candidate and skip
    # the download + registry bootstrap (and the network version probe) entirely.
    # `ocx self setup --offline` then writes the env shims against that candidate.
    # $env:__OCX_TESTING_INSTALL_BINARY is test-only (double-underscore prefix).
    if ($env:__OCX_TESTING_INSTALL_BINARY) {
        Install-LocalTestBinary -Source $env:__OCX_TESTING_INSTALL_BINARY -OcxHome $ocxHome
        $candBin = Join-Path $ocxHome 'symlinks\ocx.sh\ocx\cli\current\content\bin\ocx.exe'
        Invoke-SelfSetup -Bin $candBin -PreArgs @('--offline') -PostArgs $postArgs
        if ($env:GITHUB_PATH) {
            Export-GitHubPath -OcxHome $ocxHome
        }
        return
    }

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

        # Extract archive (zip-slip safe on PS 5.1+; see Expand-ZipSafely).
        $extractDir = Join-Path $tmpDir 'extracted'
        try {
            Expand-ZipSafely -Path (Join-Path $tmpDir $archive) -Destination $extractDir
        }
        catch {
            Err "Failed to extract $archive - $($_.Exception.Message)"
        }

        # Locate binary - cargo-dist puts it in a target-named subdirectory
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
            Warn 'Binary failed to execute - it may be blocked by antivirus or execution policy.'
        }

        # PATH shadowing: warn if a different ocx.exe already exists on PATH.
        # Use OrdinalIgnoreCase (CWE-178 defence - incorrect case handling):
        # Windows file paths are case-insensitive at the OS layer, but the
        # default `String.StartsWith` is culture-sensitive (e.g. in Turkish
        # locale 'i' and 'I' don't match), which could miss the shadow check
        # and silently let an unrelated `ocx.exe` win on PATH.
        #
        # Anchor the prefix to a trailing path separator so a sibling directory
        # named '.ocx-evil\' or '.ocxbackup\' cannot pose as an in-tree binary
        # and suppress the warning. Without the trailing '\', StartsWith would
        # accept any directory that lexically begins with $ocxHome.
        $existingOcx = Get-Command ocx -ErrorAction SilentlyContinue
        $ocxHomePrefix = $ocxHome.TrimEnd('\') + '\'
        if ($existingOcx -and -not $existingOcx.Source.StartsWith($ocxHomePrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            Warn "An existing ocx was found at $($existingOcx.Source)"
            Warn 'The new install may be shadowed - check your PATH order.'
        }

        # Hand off to `ocx self setup`: it self-installs the latest published
        # ocx into the package store, writes the env.ps1 shim under $ocxHome,
        # and (unless --no-modify-path) adds the managed activation block to the
        # PowerShell profile. The downloaded archive binary drives its own
        # bootstrap.
        Invoke-SelfSetup -Bin $bin -PostArgs $postArgs

        if ($env:GITHUB_PATH) {
            Export-GitHubPath -OcxHome $ocxHome
        }
    }
    finally {
        # Cleanup temp directory
        Remove-Item -Recurse -Force $tmpDir -ErrorAction SilentlyContinue
    }
}

# Export the OCX bin directory to $GITHUB_PATH for GitHub Actions (Decision D2:
# the CI sink stays in the installer, not `ocx self setup`).
function Export-GitHubPath {
    param([string]$OcxHome)
    $ghBinPath = Join-Path $OcxHome 'symlinks\ocx.sh\ocx\cli\current\content\bin'
    try {
        Add-Content -Path $env:GITHUB_PATH -Value $ghBinPath
    }
    catch {
        Warn 'Failed to write to $GITHUB_PATH.'
    }
}

# Run Main only when executed (irm|iex, & ./install.ps1, iex (irm ...)) - skip
# when dot-sourced (. ./install.ps1) so tests can call helper functions
# directly. InvocationName is '.' only for dot-source; all execution forms give
# the script name or empty string, never a literal dot.
if ($MyInvocation.InvocationName -ne '.') { Main }
