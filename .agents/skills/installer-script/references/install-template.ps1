<#
toolx installer for Windows PowerShell 5.1 and PowerShell 7. Template: rename toolx and TOOLX_, set $BaseUrl.

    irm https://downloads.example.invalid/toolx/install.ps1 | iex

Flags go through environment variables, never parameters (a piped script cannot bind them):
  TOOLX_INSTALL_DIR (default: local application data\toolx\bin), TOOLX_INSTALL_VERSION,
  TOOLX_INSTALL_BASE_URL (https only), TOOLX_INSTALL_TARGET (skip detection),
  TOOLX_INSTALL_SHA256 (pinned digest, overrides the sidecar), TOOLX_NO_MODIFY_PATH.
Integrity: SHA-256 from a same-origin sidecar, which detects corruption and not a hostile origin.
  TOOLX_INSTALL_SHA256 pins the digest and makes it a trust root.
Exit: 0 ok; 1 generic; 2 usage or missing tool; 3 network; 4 checksum mismatch or unverifiable;
  5 extract; 6 setup; 7 unsupported platform. Under a pipe to the interpreter the status is
  $LASTEXITCODE and the host stays alive.
ASCII only: this file is served as text and read by Windows PowerShell 5.1 without a BOM.
#>
#Requires -Version 5.1
if ($PSVersionTable.PSVersion.Major -lt 5) {
    [Console]::Error.WriteLine('toolx-install: error: PowerShell 5.1 or newer is required')
    $Global:LASTEXITCODE = 2
    return
}
& {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest
    $IsPiped = [string]::IsNullOrEmpty($PSCommandPath)
    $App = 'toolx'
    $Version = if ($env:TOOLX_INSTALL_VERSION) { $env:TOOLX_INSTALL_VERSION } else { '1.2.3' }
    $BaseUrl = if ($env:TOOLX_INSTALL_BASE_URL) { $env:TOOLX_INSTALL_BASE_URL } else { 'https://downloads.example.invalid/toolx' }
    $rc = 0

    function Say([string]$Message) { [Console]::Error.WriteLine("$App-install: $Message") }
    function Fail([string]$Message, [int]$Code) {
        $ex = New-Object System.Exception($Message)
        $ex.Data['rc'] = $Code
        throw $ex
    }
    function Assert-SecureUrl([string]$Url) {
        if ($Url -notmatch '^https://') { Fail "refusing non-https download URL: $Url" 3 }
    }
    function Get-Target {
        if ($env:TOOLX_INSTALL_TARGET) { return $env:TOOLX_INSTALL_TARGET }
        $onWindows = ($PSVersionTable.PSVersion.Major -lt 6) -or $IsWindows
        if (-not $onWindows) { Fail 'unsupported operating system: this installer targets Windows' 7 }
        $arch = $env:PROCESSOR_ARCHITEW6432
        if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
        switch ($arch) {
            'AMD64' { return 'x86_64-pc-windows-msvc' }
            'ARM64' { return 'aarch64-pc-windows-msvc' }
            default { Fail "unsupported architecture: $arch (supported: AMD64, ARM64)" 7 }
        }
    }
    function Get-File([string]$Url, [string]$Path) {
        Assert-SecureUrl $Url
        for ($i = 1; $i -le 3; $i++) {
            try {
                Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Path
                return
            } catch {
                if ($i -eq 3) { Fail "download failed: $Url ($($_.Exception.Message))" 3 }
                Say "download failed (attempt $i of 3), retrying"
                Start-Sleep -Seconds 1
            }
        }
    }
    function Confirm-Digest([string]$Path, [string]$Asset, [string]$Dir) {
        if ($env:TOOLX_INSTALL_SHA256) {
            $want = $env:TOOLX_INSTALL_SHA256
            Say 'integrity: pinned digest from the environment'
        } else {
            $side = Join-Path $Dir "$Asset.sha256"
            Get-File -Url "$BaseUrl/$Asset.sha256" -Path $side
            $want = ([IO.File]::ReadAllText($side).Trim() -split '\s+')[0]
            Say 'integrity: same-origin sidecar (detects corruption, not a hostile origin)'
        }
        if ([string]::IsNullOrWhiteSpace($want)) { Fail "empty expected digest for $Asset" 4 }
        $got = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
        if ($got -ne $want.Trim()) { Fail "checksum mismatch for ${Asset}: expected $want, got $got" 4 }
        Say 'checksum verified'
    }

    $work = $null
    $oldProto = $null
    $touchedProto = $false
    try {
        $target = Get-Target
        if ($PSVersionTable.PSEdition -eq 'Desktop') {
            $oldProto = [Net.ServicePointManager]::SecurityProtocol
            if ([int]$oldProto -ne 0) {
                $p = 3072
                if ([enum]::GetNames([Net.SecurityProtocolType]) -contains 'Tls13') { $p = $p -bor 12288 }
                [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]$p
                $touchedProto = $true
            }
        }
        $work = Join-Path ([IO.Path]::GetTempPath()) ("$App-" + [Guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $work | Out-Null
        $asset = "$App-$Version-$target.zip"
        $zip = Join-Path $work $asset
        Get-File -Url "$BaseUrl/$asset" -Path $zip
        Confirm-Digest -Path $zip -Asset $asset -Dir $work

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $out = Join-Path $work 'x'
        try { [IO.Compression.ZipFile]::ExtractToDirectory($zip, $out) } catch { Fail "extract failed: $asset ($($_.Exception.Message))" 5 }
        $exe = Join-Path $out "$App.exe"
        if (-not (Test-Path -LiteralPath $exe)) { Fail "archive has no $App.exe" 5 }

        $dir = if ($env:TOOLX_INSTALL_DIR) { $env:TOOLX_INSTALL_DIR } else { Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) "$App\bin" }
        try {
            New-Item -ItemType Directory -Force -Path $dir | Out-Null
            Copy-Item -LiteralPath $exe -Destination (Join-Path $dir "$App.exe") -Force
        } catch { Fail "cannot install into ${dir}: $($_.Exception.Message)" 6 }
        Say "installed $App $Version to $dir"

        $onWindows = ($PSVersionTable.PSVersion.Major -lt 6) -or $IsWindows
        if ($onWindows -and -not $env:TOOLX_NO_MODIFY_PATH) {
            $user = [Environment]::GetEnvironmentVariable('Path', 'User')
            if (-not $user) { $user = '' }
            if (($user -split ';') -notcontains $dir) {
                [Environment]::SetEnvironmentVariable('Path', ($user.TrimEnd(';') + ';' + $dir).TrimStart(';'), 'User')
                Say "added $dir to your user PATH (set TOOLX_NO_MODIFY_PATH to skip)"
            }
        }
    } catch {
        $code = $_.Exception.Data['rc']
        $rc = if ($code) { [int]$code } else { 1 }
        [Console]::Error.WriteLine("$App-install: error: $($_.Exception.Message)")
    } finally {
        if ($touchedProto) { [Net.ServicePointManager]::SecurityProtocol = $oldProto }
        if ($work -and (Test-Path -LiteralPath $work)) { Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue }
    }
    if ($rc -ne 0) {
        if ($IsPiped) { $Global:LASTEXITCODE = $rc; return }
        exit $rc
    }
}
