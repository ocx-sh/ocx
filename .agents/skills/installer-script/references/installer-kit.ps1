<#
installer-kit.ps1 -Check iex|tamper -Path INSTALLER.ps1 [options]
Behavioural checks for an install.ps1. Runs on Windows PowerShell 5.1 and PowerShell 7 (measured on 5.1.26100.9444,
7.4.7 and 7.6.6, 2026-10-11). It needs no module. Exit: 0 green, 1 red, 2 bad invocation.

  iex      Delivers the file the way 'irm URL | iex' does, in a child host, and judges whether the host survived.
           -HostModel repl : the delivery and a marker are two statements fed on stdin, like a person at a prompt.
                             Red when the script ended the host (the marker never printed).
           -HostModel ci   : one encoded command plus the trailer a CI pwsh step appends (exit $LASTEXITCODE).
                             Red when a failing installer ends the step with status 0.
           Default: both. Both models need a FAILING run (the exit paths are the point): the default
           base URL is unreachable, or pass -Env 'A=1;B=2' to set variables the child inherits.
  tamper   Runs the file in this process with Invoke-WebRequest and Invoke-RestMethod replaced by stubs that serve a
           fake release: *.zip is the archive, *.sha256 and *.sum the sidecar. Judged by exit status and the install dir.
           A swapped archive, a blank digest and (with -PinEnv) a wrong pinned digest must each exit non-zero and install
           nothing, and the good payload must install. It cannot judge a script that downloads with WebClient or
           HttpClient (it reports 'no download seen' and exits 1).
           -ZipEntry the binary inside the zip (default toolx.exe)  -InstallDirEnv the variable naming the install dir
           (default TOOLX_INSTALL_DIR)  -PinEnv the variable holding a pinned digest (default none)  -Env as above.
           The user PATH is saved before the run and restored after it, because a compliant installer edits it.
#>
param(
    [Parameter(Mandatory)][ValidateSet('iex', 'tamper')][string]$Check,
    [Parameter(Mandatory)][string]$Path,
    [ValidateSet('repl', 'ci', 'both')][string]$HostModel = 'both',
    [string]$Env = '',
    [string]$ZipEntry = 'toolx.exe',
    [string]$InstallDirEnv = 'TOOLX_INSTALL_DIR',
    [string]$PinEnv = ''
)
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { [Console]::Error.WriteLine("installer-kit: no such file: $Path"); exit 2 }
$full = (Resolve-Path -LiteralPath $Path).Path
$bad = 0
function Out-Result([string]$Message) { Write-Host $Message }
function Import-EnvVar([string]$Pairs) {
    foreach ($kv in ($Pairs -split ';' | Where-Object { $_ })) {
        $i = $kv.IndexOf('=')
        Set-Item -LiteralPath ('env:' + $kv.Substring(0, $i)) -Value $kv.Substring($i + 1)
    }
}

function Test-IexHost([string]$Model) {
    $exe = (Get-Process -Id $PID).Path
    $deliver = "Get-Content -LiteralPath '$full' -Raw | Invoke-Expression"
    $marker = 'Write-Output (''HOST-SURVIVED lastexit='' + [string]$LASTEXITCODE)'
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $exe
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.UseShellExecute = $false
    if ($Model -eq 'ci') {
        $trailer = "`nif ((Test-Path -LiteralPath variable:\LASTEXITCODE)) { exit `$LASTEXITCODE }"
        $cmd = "`$ErrorActionPreference = 'Continue'`n$deliver`n$marker$trailer"
        $enc = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($cmd))
        $psi.Arguments = "-NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc"
        $p = [System.Diagnostics.Process]::Start($psi)
    } else {
        $psi.RedirectStandardInput = $true
        $psi.Arguments = '-NoProfile -ExecutionPolicy Bypass -Command -'
        $p = [System.Diagnostics.Process]::Start($psi)
        $p.StandardInput.WriteLine($deliver)
        $p.StandardInput.WriteLine($marker)
        $p.StandardInput.Close()
    }
    $soT = $p.StandardOutput.ReadToEndAsync()
    $seT = $p.StandardError.ReadToEndAsync()
    $p.WaitForExit()
    $so = $soT.Result
    $esc = [regex]'\x1b\[[0-9;?]*[A-Za-z]'
    $so = $esc.Replace($so, '')
    $lines = ($so -split "`r?`n" | Where-Object { $_ -and $_ -notmatch '^#< CLIXML' }) | Select-Object -First 3
    foreach ($l in $lines) { Out-Result "     out: $l" }
    if ($Model -eq 'ci') {
        if ($p.ExitCode -ne 0) { Out-Result "ok   [ci] the failing run ended the step loudly (step exit $($p.ExitCode))"; return 0 }
        Out-Result 'RED  [ci] step exit 0: the failure was swallowed or never happened (give -Env that makes the installer fail)'; return 1
    }
    if ($so -match 'HOST-SURVIVED') { Out-Result "ok   [repl] the host survived ($($Matches[0]), child exit $($p.ExitCode))"; return 0 }
    Out-Result "RED  [repl] the served script ended the host (child exit $($p.ExitCode), marker never printed)"; return 1
}

function Write-Zip([string]$Dest, [string]$Content) {
    Add-Type -AssemblyName System.IO.Compression
    $fs = [IO.File]::Open($Dest, 'Create')
    try {
        $za = New-Object IO.Compression.ZipArchive($fs, [IO.Compression.ZipArchiveMode]::Create)
        $e = $za.CreateEntry($ZipEntry)
        $sw = New-Object IO.StreamWriter($e.Open())
        $sw.Write($Content)
        $sw.Dispose()
        $za.Dispose()
    } finally { $fs.Dispose() }
}

function Test-Tamper {
    $root = Join-Path ([IO.Path]::GetTempPath()) ('ikit-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $root | Out-Null
    $good = Join-Path $root 'good.zip'
    $evil = Join-Path $root 'evil.zip'
    Write-Zip $good 'GOOD-BINARY'
    Write-Zip $evil 'EVIL-BINARY'
    $goodHash = (Get-FileHash -LiteralPath $good -Algorithm SHA256).Hash
    $global:IkitState = @{ Served = $good; Side = $goodHash; Hits = 0 }
    function Get-SideText([string]$Name) {
        if ($global:IkitState.Side) { return "$($global:IkitState.Side)  $Name`n" }
        return ''
    }
    function Invoke-WebRequest {
        param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing, [Parameter(ValueFromRemainingArguments)]$Rest)
        $global:IkitState.Hits++
        $name = ($Uri -split '/')[-1]
        if ($name -match '\.zip$') { Copy-Item -LiteralPath $global:IkitState.Served -Destination $OutFile -Force }
        elseif ($name -match '\.(sha256|sha256sum|sum)$') { [IO.File]::WriteAllText($OutFile, (Get-SideText $name)) }
        else { throw "404 $Uri" }
    }
    function Invoke-RestMethod {
        param([string]$Uri, [string]$OutFile, [switch]$UseBasicParsing, [Parameter(ValueFromRemainingArguments)]$Rest)
        $global:IkitState.Hits++
        $name = ($Uri -split '/')[-1]
        if ($name -match '\.(sha256|sha256sum|sum)$') { return (Get-SideText $name) }
        throw "404 $Uri"
    }
    $scenarios = @(
        @{ Label = 'A swapped archive, sidecar as published'; Zip = $evil; Side = $goodHash; Pin = ''; Want = 'refuse' },
        @{ Label = 'B blank digest'; Zip = $good; Side = ''; Pin = ''; Want = 'refuse' },
        @{ Label = 'D control, good archive'; Zip = $good; Side = $goodHash; Pin = ''; Want = 'install' }
    )
    if ($PinEnv) {
        $scenarios += @{ Label = 'C wrong pinned digest'; Zip = $good; Side = $goodHash; Pin = ('0' * 64); Want = 'refuse' }
        $scenarios += @{ Label = 'E control, right pinned digest'; Zip = $good; Side = $goodHash; Pin = $goodHash; Want = 'install' }
    }
    $red = 0
    $saved = @{}
    $userPath = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) { [Environment]::GetEnvironmentVariable('Path', 'User') } else { $null }
    foreach ($n in @($InstallDirEnv, $PinEnv)) { if ($n) { $saved[$n] = [Environment]::GetEnvironmentVariable($n) } }
    try {
        foreach ($s in ($scenarios | Sort-Object { $_.Label })) {
            $dir = Join-Path $root ('dest-' + $s.Label.Substring(0, 1))
            Set-Item -LiteralPath "env:$InstallDirEnv" -Value $dir
            if ($PinEnv) { if ($s.Pin) { Set-Item -LiteralPath "env:$PinEnv" -Value $s.Pin } else { Remove-Item -LiteralPath "env:$PinEnv" -ErrorAction SilentlyContinue } }
            Import-EnvVar $Env
            $global:IkitState.Served = $s.Zip
            $global:IkitState.Side = $s.Side
            $global:IkitState.Hits = 0
            $global:LASTEXITCODE = 0
            $err = ''
            try { & $full 2>&1 | Out-Null } catch { $err = $_.Exception.Message }
            $code = $global:LASTEXITCODE
            $installed = Test-Path -LiteralPath (Join-Path $dir $ZipEntry)
            if ($global:IkitState.Hits -eq 0) { Out-Result "RED  $($s.Label): no download seen (the script does not use Invoke-WebRequest or Invoke-RestMethod)"; $red++; continue }
            $refused = ($code -ne 0 -or $err) -and -not $installed
            if (($s.Want -eq 'refuse' -and $refused) -or ($s.Want -eq 'install' -and $code -eq 0 -and -not $err -and $installed)) {
                Out-Result "ok   $($s.Label): exit $code, installed=$installed"
            } else {
                Out-Result "RED  $($s.Label): exit $code, installed=$installed $err"
                $red++
            }
        }
    } finally {
        if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) { [Environment]::SetEnvironmentVariable('Path', $userPath, 'User') }
        foreach ($n in $saved.Keys) { if ($null -eq $saved[$n]) { Remove-Item -LiteralPath "env:$n" -ErrorAction SilentlyContinue } else { Set-Item -LiteralPath "env:$n" -Value $saved[$n] } }
        Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Variable -Name IkitState -Scope Global -ErrorAction SilentlyContinue
    }
    return $red
}

if ($Check -eq 'iex') {
    Import-EnvVar $Env
    $models = if ($HostModel -eq 'both') { @('repl', 'ci') } else { @($HostModel) }
    foreach ($m in $models) { $bad += Test-IexHost $m }
} else {
    $bad += Test-Tamper
}
if ($bad) { Out-Result "RED ($bad)"; exit 1 }
Out-Result 'GREEN'
exit 0
