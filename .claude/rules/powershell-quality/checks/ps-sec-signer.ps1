#Requires -Version 5.1
<#
ps-sec-signer.ps1 -Path FILE.ps1 -Signer 'O=Vendor Inc.' [-Function Test-Signature]
PS-SEC-01, the Authenticode alternative: test an installer's own signature check against mocked Get-AuthenticodeSignature
results. The script is parsed, never run: only the named function is extracted and defined in this session, and a function
named Get-AuthenticodeSignature shadows the cmdlet. -Signer is the exact subject component the installer pins.
The function takes the file path as its first argument and returns $true to accept, $false or throws to refuse.
Cases (the file name is only a key into the mock):
  good           Status Valid, subject carries the pin              must be accepted
  lookalike      Status Valid, the pin with "Not " after the "="   must be refused (an unanchored regex accepts it)
  suffix         Status Valid, the pin followed by "Evil"          must be refused (an unanchored end accepts it)
  tampered       Status HashMismatch, subject carries the pin      must be refused (a check that skips Status accepts it)
  untrusted      Status NotTrusted, subject carries the pin        must be refused
  unsigned       Status NotSigned, no certificate                  must be refused
On Windows it also asks the real cmdlet about cmd.exe and an unsigned temp file and requires both refused.
Exit 0 every case behaves, 1 a case that must be refused was accepted (or the good case refused), 2 bad invocation.
ASCII only, 5.1 syntax.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11).
#>
param([string]$Path = '', [string]$Signer = '', [string]$Function = 'Test-Signature')
if (-not $Path -or -not $Signer -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
    [Console]::Error.WriteLine("ps-sec-signer: -Path must name an existing script and -Signer a pin such as 'O=Vendor Inc.'")
    exit 2
}
$tok = $null; $err = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $Path).Path, [ref]$tok, [ref]$err)
$err = @($err | Where-Object { $_.ErrorId -notin 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined' })   # a missing dependency leaves a usable tree
if ($err.Count) { [Console]::Error.WriteLine('ps-sec-signer: parse error: ' + $err[0].Message); exit 2 }
$def = $ast.Find({ param($a) $a -is [System.Management.Automation.Language.FunctionDefinitionAst] -and $a.Name -eq $Function }, $true)
if (-not $def) { [Console]::Error.WriteLine("ps-sec-signer: no function named $Function in $Path"); exit 2 }

$mock = @{}
function Build-Sig($status, $subject) {
    $cert = $null
    if ($subject) { $cert = New-Object psobject -Property @{ Subject = $subject } }
    New-Object psobject -Property @{ Status = $status; SignerCertificate = $cert }
}
function Get-AuthenticodeSignature {
    [Diagnostics.CodeAnalysis.SuppressMessageAttribute('PSAvoidOverwritingBuiltInCmdlets', '', Justification = 'the mock shadows the cmdlet on purpose')]
    param([Parameter(Position = 0)][string]$FilePath, [string]$LiteralPath)
    $k = if ($FilePath) { $FilePath } else { $LiteralPath }
    if ($mock.ContainsKey($k)) { return $mock[$k] }
    Microsoft.PowerShell.Security\Get-AuthenticodeSignature -FilePath $k
}
. ([scriptblock]::Create($def.Extent.Text))

$eq = $Signer.IndexOf('=')
$notSigner = if ($eq -ge 0) { $Signer.Substring(0, $eq + 1) + 'Not ' + $Signer.Substring($eq + 1) } else { 'Not ' + $Signer }
$mock['good'] = Build-Sig 'Valid' "CN=Publisher, $Signer, L=Town, C=US"
$mock['lookalike'] = Build-Sig 'Valid' "CN=Evil, $notSigner, C=US"
$mock['suffix'] = Build-Sig 'Valid' "CN=Evil, ${Signer}Evil, C=US"
$mock['tampered'] = Build-Sig 'HashMismatch' "CN=Publisher, $Signer, C=US"
$mock['untrusted'] = Build-Sig 'NotTrusted' "CN=Publisher, $Signer, C=US"
$mock['unsigned'] = Build-Sig 'NotSigned' $null

function Test-Case($key) {
    try { return [bool](& $Function $key) } catch { return $false }
}
$bad = 0
foreach ($c in @(@('good', $true), @('lookalike', $false), @('suffix', $false), @('tampered', $false), @('untrusted', $false), @('unsigned', $false))) {
    $got = Test-Case $c[0]
    $verdict = 'ok'
    if ($got -ne $c[1]) { $verdict = 'RED'; $bad++ }
    '{0,-10} want-accept={1,-5} got-accept={2,-5} {3}' -f $c[0], $c[1], $got, $verdict
}
if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) 'ps-sec-signer-unsigned.txt'
    Set-Content -LiteralPath $tmp -Value 'x'
    foreach ($real in @((Join-Path $env:SystemRoot 'System32\cmd.exe'), $tmp)) {
        $got = Test-Case $real
        $verdict = 'ok'
        if ($got) { $verdict = 'RED'; $bad++ }
        '{0,-10} real file {1} accepted={2} {3}' -f 'real', (Split-Path -Leaf $real), $got, $verdict
    }
    Remove-Item -LiteralPath $tmp
}
if ($bad) { "RED ($bad)"; exit 1 }
'GREEN'; exit 0
