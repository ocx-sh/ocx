# checks/ps-enc-writers.ps1: PS-ENC-03 and PS-ENC-07, the AST scan for file writers and locale-dependent encodings in 5.1-bound PowerShell.
# Usage: pwsh -NoProfile -File ps-enc-writers.ps1 PATH...   (PATH is a file or a directory searched for *.ps1 and *.psm1; also runs under powershell.exe 5.1)
# Output: "FILE:LINE CODE message", one per finding. Exit: 0 clean, 1 a finding, 79 no files in scope.
#   ENC03  >, >>, Out-File, Tee-Object, Set-Content, Add-Content or Export-Csv without -Encoding ascii, -Encoding utf8NoBOM, utf8BOM or ansi,
#          or [Text.Encoding]::UTF8 handed to WriteAllText, AppendAllText or WriteAllLines (the BOM-carrying static)
#   ENC07  -Encoding Default or OEM, and [Text.Encoding]::Default, which pick the code page by machine; a same-line `# tell-ok: reason` waives it
# Accepted form (PS-ENC-10): Out-File -Encoding utf8 -Append onto $env:GITHUB_ENV, GITHUB_PATH, GITHUB_OUTPUT or GITHUB_STATE.
# Written in 5.1 syntax so one file runs on both engines. A file this engine cannot parse is reported as PARSE and skipped.
# Ceiling: a splatted Encoding key and an abbreviation shorter than -En are not seen. Run it on the oldest engine the code must support.
# Engines: Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6; no module needed. Watched red and green (measured 2026-10-11).
param([Parameter(ValueFromRemainingArguments)][string[]]$Path)
$ErrorActionPreference = 'Stop'
$files = @(foreach ($p in $Path) {
    if (Test-Path -LiteralPath $p -PathType Container) { Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1' } }
    elseif (Test-Path -LiteralPath $p -PathType Leaf) { Get-Item -LiteralPath $p }
})
if ($files.Count -eq 0) { [Console]::Error.WriteLine('ps-enc-writers: no files in scope'); exit 79 }
$out = New-Object System.Collections.Generic.List[string]
$Ast = [System.Management.Automation.Language.Parser]
function Add-Finding($f, $node, $code, $msg) {
    $n = $node.Extent.StartLineNumber
    if ($code -eq 'ENC07' -and $script:lines[$n - 1] -match 'tell-ok:\s*\S') { return }
    $out.Add(('{0}:{1} {2} {3}' -f $f.Name, $n, $code, $msg))
}
function Test-Prefix($name, $full, $min) { ($name.Length -ge $min) -and $full.StartsWith($name, [StringComparison]::OrdinalIgnoreCase) }
function Get-Text($node) { if ($node) { $node.Extent.Text.Trim('"', "'") } else { '' } }
foreach ($f in $files) {
    $tokens = $null; $errors = $null
    $script:lines = [IO.File]::ReadAllLines($f.FullName)
    $tree = $Ast::ParseFile($f.FullName, [ref]$tokens, [ref]$errors)
    $syntax = @($errors | Where-Object { $_.ErrorId -notin 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined' })   # a missing dependency leaves a usable tree
    if ($syntax.Count) { $out.Add(('{0}:{1} PARSE {2}' -f $f.Name, $syntax[0].Extent.StartLineNumber, ($syntax[0].Message -replace '\s+', ' '))); continue }
    foreach ($c in $tree.FindAll({ param($n) $n -is [System.Management.Automation.Language.CommandAst] }, $true)) {
        $name = $c.GetCommandName()
        if (-not $name) { continue }
        $enc = ''; $append = $false; $elements = @($c.CommandElements | Select-Object -Skip 1)
        for ($i = 0; $i -lt $elements.Count; $i++) {
            $el = $elements[$i]
            if ($el -isnot [System.Management.Automation.Language.CommandParameterAst]) { continue }
            $val = $el.Argument
            if (-not $val -and ($i + 1) -lt $elements.Count -and $elements[$i + 1] -isnot [System.Management.Automation.Language.CommandParameterAst]) { $val = $elements[$i + 1] }
            if (Test-Prefix $el.ParameterName 'Encoding' 2) { $enc = Get-Text $val }
            if (Test-Prefix $el.ParameterName 'Append' 2) { $append = $true }
        }
        if ($enc -match '^(utf8NoBOM|utf8BOM|ansi)$') { Add-Finding $f $c 'ENC03' ('-Encoding ' + $enc + ' is rejected by the 5.1 ValidateSet') }
        if ($enc -match '^(default|oem)$') { Add-Finding $f $c 'ENC07' ('-Encoding ' + $enc + ' writes different bytes by engine and machine') }
        if ($name -in 'Out-File', 'Set-Content', 'Add-Content', 'Export-Csv', 'Tee-Object') {
            $target = ($c.CommandElements | ForEach-Object { $_.Extent.Text }) -join ' '
            $runnerFile = ($name -eq 'Out-File' -and $enc -ieq 'utf8' -and $append -and $target -match '[$]env:GITHUB_(ENV|PATH|OUTPUT|STATE)\b')
            if ($enc -ine 'ascii' -and -not $runnerFile) { Add-Finding $f $c 'ENC03' ($name + ' without -Encoding ascii: the bytes differ by edition (UTF-16LE, ANSI, UTF-8 with BOM)') }
        }
    }
    foreach ($r in $tree.FindAll({ param($n) $n -is [System.Management.Automation.Language.FileRedirectionAst] }, $true)) {
        if ($r.Location.Extent.Text -ne '$null') { Add-Finding $f $r 'ENC03' ('redirection ' + $r.Extent.Text + ' writes UTF-16LE with BOM on 5.1') }
    }
    foreach ($m in $tree.FindAll({ param($n) $n -is [System.Management.Automation.Language.MemberExpressionAst] -and $n.Static }, $true)) {
        $type = $m.Expression.Extent.Text -replace '\s', ''
        if ($type -notmatch '^\[(System\.)?Text\.Encoding\]$') { continue }
        if ($m.Member.Extent.Text -ieq 'Default') { Add-Finding $f $m 'ENC07' '[Text.Encoding]::Default is cp1252 on 5.1 and UTF-8 on 7.x' }
        if ($m.Member.Extent.Text -ieq 'UTF8' -and $m.Parent -is [System.Management.Automation.Language.InvokeMemberExpressionAst]) {
            $call = $m.Parent.Member.Extent.Text
            if ($call -in 'WriteAllText', 'AppendAllText', 'WriteAllLines') { Add-Finding $f $m 'ENC03' ('[Text.Encoding]::UTF8 carries a BOM into ' + $call) }
        }
    }
}
$out | Sort-Object
if ($out.Count -gt 0) { exit 1 }
exit 0
