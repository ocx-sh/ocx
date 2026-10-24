#Requires -Version 5.1
<#
ps-sec-ast.ps1 [-Verifier REGEX] [PATH...]
AST checks for the PS-SEC rules. Run it on the installer and bootstrap scripts (the iex-delivered ones for the exit rules).
No PATH lists every .ps1 and .psm1 git knows, a PATH is a file or a directory. Prints "file:line TOKEN (rule)" per finding.
  UNVERIFIED-PATH         PS-SEC-01  Expand-Archive, Start-Process, ExtractToDirectory or ExtractToFile with no verifying call
                                     (Get-FileHash, Get-AuthenticodeSignature, Test-Signature, Get-Verified*, ComputeHash, or a
                                     name matching -Verifier) in a statement that precedes it on its own path. Other branches and
                                     function bodies do not count. A Start-Process of a local file is a known false positive.
  NO-LITERALPATH          PS-SEC-05  Get-FileHash or Expand-Archive without -LiteralPath (positional paths included)
  EXIT-IN-FUNCTION        PS-SEC-09  exit inside a function body
  BREAK-OUTSIDE-LOOP      PS-SEC-09  break or continue outside a loop or switch (the "leave the script" idiom)
  EXIT-TOPLEVEL-UNGUARDED PS-SEC-09  a top-level exit in a file that never names $PSCommandPath
  INVOKE-EXPRESSION-TOKEN PS-SEC-10  a command named Invoke-Expression or iex (comments and help blocks are not code),
                                     skipped in a file that carries a SuppressMessage for PSAvoidUsingInvokeExpression
Run it under the host the script targets, the parser is that host's own. A parse error is reported and counts as a finding.
Exit 0 clean, 1 a finding, 78 no git work tree, 79 no file. ASCII only, 5.1 syntax.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11).
#>
[CmdletBinding(PositionalBinding = $false)]
param(
    [string]$Verifier = '',
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Path
)
$ErrorActionPreference = 'Stop'
$files = @()
if ($Path) {
    foreach ($p in $Path) {
        if (Test-Path -LiteralPath $p -PathType Container) {
            $files += Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1' } | ForEach-Object { $_.FullName }
        } else { $files += (Resolve-Path -LiteralPath $p).ProviderPath }
    }
} else {
    $inside = & git rev-parse --is-inside-work-tree
    if ($LASTEXITCODE -ne 0 -or $inside -ne 'true') { [Console]::Error.WriteLine('ps-sec-ast: not inside a git work tree'); exit 78 }
    $top = (& git rev-parse --show-toplevel)
    $files = & git -c core.quotepath=off ls-files --cached --others --exclude-standard -- '*.ps1' '*.psm1' | ForEach-Object { Join-Path $top $_ }
}
if (-not $files) { [Console]::Error.WriteLine('ps-sec-ast: no file'); exit 79 }

$verify = '^(Get-FileHash|Get-AuthenticodeSignature|Test-Signature|Get-Verified\w*|Get-FileIntegrity|Test-NpmSignature)$'
$n = 0

function Test-InFunction($node) {
    $p = $node.Parent
    while ($p) { if ($p -is [System.Management.Automation.Language.FunctionDefinitionAst]) { return $true }; $p = $p.Parent }
    return $false
}
function Test-VerifyNode($node, $extra) {
    $found = $node.FindAll({ param($a) $a -is [System.Management.Automation.Language.CommandAst] -or $a -is [System.Management.Automation.Language.InvokeMemberExpressionAst] }, $true)
    foreach ($c in $found) {
        $p = $c.Parent; $inFn = $false
        while ($p -and $p -ne $node.Parent) { if ($p -is [System.Management.Automation.Language.FunctionDefinitionAst]) { $inFn = $true; break }; $p = $p.Parent }
        if ($inFn) { continue }
        if ($c -is [System.Management.Automation.Language.CommandAst]) {
            $name = $c.GetCommandName()
            if ($name -and ($name -match $verify -or ($extra -and $name -match $extra))) { return $true }
        } elseif ("$($c.Member)" -eq 'ComputeHash') { return $true }
    }
    return $false
}

foreach ($file in $files) {
    $tok = $null; $err = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($file, [ref]$tok, [ref]$err)
    $err = @($err | Where-Object { $_.ErrorId -notin 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined' })   # a missing dependency leaves a usable tree
    if ($err.Count) { '{0}: PARSE-ERROR {1}' -f $file, $err[0].Message; $n++; continue }
    $text = [IO.File]::ReadAllText($file)
    $cmds = $ast.FindAll({ param($a) $a -is [System.Management.Automation.Language.CommandAst] }, $true)

    # PS-SEC-10
    if ($text -notmatch 'SuppressMessage.*PSAvoidUsingInvokeExpression') {
        foreach ($c in $cmds) {
            if ($c.GetCommandName() -in 'Invoke-Expression', 'iex') { '{0}:{1} INVOKE-EXPRESSION-TOKEN (PS-SEC-10)' -f $file, $c.Extent.StartLineNumber; $n++ }
        }
    }
    # PS-SEC-05
    foreach ($c in $cmds) {
        $cn = $c.GetCommandName()
        if ($cn -in 'Get-FileHash', 'Expand-Archive') {
            $has = @($c.CommandElements | Where-Object { $_ -is [System.Management.Automation.Language.CommandParameterAst] -and $_.ParameterName.Length -ge 4 -and 'LiteralPath'.StartsWith($_.ParameterName, [StringComparison]::OrdinalIgnoreCase) }).Count
            if (-not $has) { '{0}:{1} NO-LITERALPATH {2} (PS-SEC-05)' -f $file, $c.Extent.StartLineNumber, $cn; $n++ }
        }
    }
    # PS-SEC-09
    $loops = { param($a) $a -is [System.Management.Automation.Language.LoopStatementAst] -or $a -is [System.Management.Automation.Language.SwitchStatementAst] }
    foreach ($b in $ast.FindAll({ param($a) $a -is [System.Management.Automation.Language.BreakStatementAst] -or $a -is [System.Management.Automation.Language.ContinueStatementAst] }, $true)) {
        $p = $b.Parent; $inLoop = $false
        while ($p) { if (& $loops $p) { $inLoop = $true; break }; $p = $p.Parent }
        if (-not $inLoop) { '{0}:{1} BREAK-OUTSIDE-LOOP (PS-SEC-09)' -f $file, $b.Extent.StartLineNumber; $n++ }
    }
    foreach ($x in $ast.FindAll({ param($a) $a -is [System.Management.Automation.Language.ExitStatementAst] }, $true)) {
        if (Test-InFunction $x) { '{0}:{1} EXIT-IN-FUNCTION (PS-SEC-09)' -f $file, $x.Extent.StartLineNumber; $n++ }
        elseif ($text -notmatch 'PSCommandPath') { '{0}:{1} EXIT-TOPLEVEL-UNGUARDED (PS-SEC-09)' -f $file, $x.Extent.StartLineNumber; $n++ }
    }
    # PS-SEC-01: every path that reaches an extract or run call needs a verifying statement before it
    $targets = $ast.FindAll({ param($a)
        ($a -is [System.Management.Automation.Language.CommandAst] -and ($a.GetCommandName() -in 'Expand-Archive', 'Start-Process')) -or
        ($a -is [System.Management.Automation.Language.InvokeMemberExpressionAst] -and "$($a.Member)" -in 'ExtractToDirectory', 'ExtractToFile') }, $true)
    foreach ($c in $targets) {
        $label = if ($c -is [System.Management.Automation.Language.CommandAst]) { $c.GetCommandName() } else { "$($c.Member)" }
        $ok = $false; $child = $c; $par = $c.Parent
        while ($par -and -not $ok) {
            $stmts = $null
            if ($par -is [System.Management.Automation.Language.StatementBlockAst] -or $par -is [System.Management.Automation.Language.NamedBlockAst]) { $stmts = $par.Statements }
            if ($stmts) {
                foreach ($s in $stmts) {
                    if ($s.Extent.EndOffset -le $child.Extent.StartOffset -and $s -isnot [System.Management.Automation.Language.FunctionDefinitionAst] -and (Test-VerifyNode $s $Verifier)) { $ok = $true; break }
                }
            }
            if ($par -is [System.Management.Automation.Language.FunctionDefinitionAst]) { break }
            $child = $par; $par = $par.Parent
        }
        if (-not $ok) { '{0}:{1} UNVERIFIED-PATH {2} (PS-SEC-01)' -f $file, $c.Extent.StartLineNumber, $label; $n++ }
    }
}
if ($n) { "RED ($n)"; exit 1 }
'GREEN'; exit 0
