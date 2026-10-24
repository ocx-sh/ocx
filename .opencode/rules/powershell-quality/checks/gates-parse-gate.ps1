#Requires -Version 5.1
<#
gates-parse-gate.ps1 PATH [PATH...]
PS-GATE-04: parse every .ps1, .psm1 and .psd1 under the paths with the parser of the host that runs this script.
Run it under powershell.exe (Windows PowerShell 5.1) to gate 5.1-bound files: a pwsh 7 parse is a false green.
Exit 0 every file parses, 1 a syntax error, 2 the gate cannot judge (no file, a path that is missing, or only
environment errors: TypeNotFound, ModuleNotFoundDuringParse and ResourceNotDefined (a DSC resource) need the dependency installed on this host).
Skipped, and counted on the summary line: a file with a "#Requires -Version 6" to 9 line (7-only by declaration),
any file under a directory named templates or ending .template (un-rendered placeholders), and nothing else.
A .psd1 inside a culture-named directory (en, de-DE, zh) is Import-LocalizedData data: it is decoded as UTF-8,
as that loader does, not as the 5.1 ANSI default. ASCII only, 5.1 syntax.
Watched red and green on Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6 (measured 2026-10-11).
#>
param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Path)
$ErrorActionPreference = 'Stop'
$parser = [System.Management.Automation.Language.Parser]
$env_ids = 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined'
$files = @()
foreach ($p in $Path) {
    if (-not (Test-Path -LiteralPath $p)) { [Console]::Error.WriteLine("parse-gate: path not found: $p"); exit 2 }
    if (Test-Path -LiteralPath $p -PathType Container) {
        $files += @(Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1', '.psd1' })
    } else { $files += Get-Item -LiteralPath $p }
}
$syntax = 0; $environment = 0; $skipped = 0; $parsed = 0
foreach ($f in $files) {
    $segments = @($f.DirectoryName -split '[\\/]')
    if (@($segments | Where-Object { $_ -eq 'templates' -or $_ -like '*.template' }).Count -gt 0) { $skipped++; continue }
    if ([System.IO.File]::ReadAllText($f.FullName) -match '(?im)^#requires\s+-version\s+[6-9]') { $skipped++; continue }
    $tokens = $null; $errors = $null
    if ($f.Extension -eq '.psd1' -and $f.Directory.Name -match '^[a-z]{2,3}(-[A-Za-z]{2,4})?$') {
        $text = [System.IO.File]::ReadAllText($f.FullName, (New-Object System.Text.UTF8Encoding $false))
        [void]$parser::ParseInput($text, $f.FullName, [ref]$tokens, [ref]$errors)
    } else {
        [void]$parser::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    }
    $parsed++
    foreach ($e in $errors) {
        $kind = 'syntax'
        if ($env_ids -contains $e.ErrorId) { $kind = 'environment'; $environment++ } else { $syntax++ }
        '{0}:{1}:{2}: {3}: {4} [{5}]' -f $f.FullName, $e.Extent.StartLineNumber, $e.Extent.StartColumnNumber, $kind, ($e.Message -replace '\s+', ' '), $e.ErrorId
    }
}
'parse-gate: {0} parsed, {1} skipped, {2} syntax errors, {3} environment errors, PowerShell {4}' -f $parsed, $skipped, $syntax, $environment, $PSVersionTable.PSVersion
if ($syntax -gt 0) { exit 1 }
if ($parsed -eq 0 -or $environment -gt 0) { exit 2 }
exit 0
