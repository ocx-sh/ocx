#Requires -Version 5.1
<#
compat-ast.ps1 [-AnyHost] [-Notes] [PATH...]
PS-COMPAT-02 parse, 03 unbraced null-conditional, 04 platform variables, 05 cmdlets, parameters and members
that 5.1 lacks, 06 Invoke-WebRequest without -UseBasicParsing. Run it under powershell.exe (Windows PowerShell 5.1)
so the parser and the .NET Framework are the target's own. No PATH lists every .ps1 and .psm1 git knows.
Prints "file:line MUST RULE-ID message". -Notes also prints the SHOULD lines, which never change the exit status.
ASCII only, 5.1 syntax.
Exit 0 clean, 1 a MUST finding, 69 not Windows PowerShell 5.1 (pass -AnyHost to
run the host-independent rules on another edition), 78 no git work tree, 79 no file. Written for 5.1 and 7.x.
Ran on Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6 on 2026-10-11, no module needed.
#>
param(
    [switch]$AnyHost,
    [switch]$Notes,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$Path
)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -ne 5 -and -not $AnyHost) {
    [Console]::Error.WriteLine('compat-ast: PowerShell ' + $PSVersionTable.PSVersion + ' cannot prove 5.1 compatibility, run under powershell.exe or pass -AnyHost')
    exit 69
}
$files = @()
if ($Path) {
    foreach ($p in $Path) {
        if (Test-Path -LiteralPath $p -PathType Container) {
            $files += Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1' } | ForEach-Object { $_.FullName }
        } else { $files += (Resolve-Path -LiteralPath $p).ProviderPath }
    }
} else {
    $inside = & git rev-parse --is-inside-work-tree
    if ($LASTEXITCODE -ne 0 -or $inside -ne 'true') { [Console]::Error.WriteLine('compat-ast: not inside a git work tree'); exit 78 }
    $top = (& git rev-parse --show-toplevel)
    $files = & git -c core.quotepath=off ls-files --cached --others --exclude-standard -- '*.ps1' '*.psm1' | ForEach-Object { Join-Path $top $_ }
}
if (-not $files) { [Console]::Error.WriteLine('compat-ast: no file'); exit 79 }

# Commands and parameters 5.1 lacks (illustrative: the command resolver of PS-GATE-06 is the full check).
$sevenOnlyCommand = @('Test-Json', 'Join-String', 'Get-Error', 'ConvertTo-CliXml', 'ConvertFrom-CliXml')
$sevenOnlyParam = @{
    'ConvertFrom-Json' = @('AsHashtable', 'NoEnumerate', 'Depth')
    'Get-Content' = @('AsByteStream')
    'Set-Content' = @('AsByteStream')
    'ForEach-Object' = @('Parallel', 'ThrottleLimit')
    'Join-Path' = @('AdditionalChildPath')
    'Invoke-WebRequest' = @('MaximumRetryCount', 'RetryIntervalSec', 'SkipHttpErrorCheck', 'SkipCertificateCheck', 'Authentication', 'Token')
    'Invoke-RestMethod' = @('MaximumRetryCount', 'RetryIntervalSec', 'SkipHttpErrorCheck', 'SkipCertificateCheck', 'Authentication', 'Token', 'ResponseHeadersVariable', 'StatusCodeVariable')
    'ConvertTo-Json' = @('AsArray', 'EnumsAsStrings')
    'ConvertTo-Csv' = @('UseQuotes', 'QuoteFields')
    'Export-Csv' = @('UseQuotes', 'QuoteFields')
    'ConvertFrom-SecureString' = @('AsPlainText')
}
$showNotes = [bool]$Notes
$must = 0
$out = New-Object System.Collections.Generic.List[string]
function Add-Finding($file, $node, $sev, $rule, $msg) {
    if ($sev -ne 'MUST' -and -not $script:showNotes) { return }
    $out.Add(('{0}:{1} {2} {3} {4}' -f $file, $node.Extent.StartLineNumber, $sev, $rule, $msg))
    if ($sev -eq 'MUST') { $script:must++ }
}
function Test-Prefix($name, $full, $min) { ($name.Length -ge $min) -and $full.StartsWith($name, [StringComparison]::OrdinalIgnoreCase) }
function Get-Guarded($v) {
    # a platform variable on the right of -or / -and whose left side tests the edition, the version or that the variable exists (Test-Path variable:IsWindows, Get-Variable) first is never evaluated on 5.1
    $p = $v.Parent
    while ($p) {
        if ($p -is [System.Management.Automation.Language.BinaryExpressionAst] -and ($p.Operator -eq 'Or' -or $p.Operator -eq 'And') `
                -and $p.Left.Extent.Text -match 'PSEdition|PSVersion|OSVersion|Get-Variable|Variable:' `
                -and $p.Right.Extent.StartOffset -le $v.Extent.StartOffset -and $p.Right.Extent.EndOffset -ge $v.Extent.EndOffset) { return $true }
        $p = $p.Parent
    }
    return $false
}
foreach ($f in $files) {
    $tokens = $null; $errs = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($f, [ref]$tokens, [ref]$errs)
    # TypeNotFound, ModuleNotFoundDuringParse and ResourceNotDefined (a DSC resource module) need the dependency installed: environment, not syntax
    $syntax = @($errs | Where-Object { $_.ErrorId -notin 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined' })
    if ($syntax.Count) {
        foreach ($e in $syntax) { $out.Add(('{0}:{1} MUST PS-COMPAT-02 parse error: {2} [{3}]' -f $f, $e.Extent.StartLineNumber, ($e.Message -replace '\s+', ' '), $e.ErrorId)); $must++ }
        continue
    }
    $text = [IO.File]::ReadAllText($f)
    $strict = $text -match 'Set-StrictMode'
    $defined = $false
    foreach ($as in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.AssignmentStatementAst] }, $true)) {
        if ($as.Left -is [System.Management.Automation.Language.VariableExpressionAst] -and $as.Left.VariablePath.UserPath -match '^Is(Windows|Linux|MacOS|CoreCLR)$') { $defined = $true }
    }
    foreach ($v in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.VariableExpressionAst] }, $true)) {
        $name = $v.VariablePath.UserPath
        if ($name.Length -gt 1 -and $name.EndsWith('?') -and -not $v.Extent.Text.StartsWith('${')) {
            Add-Finding $f $v 'MUST' 'PS-COMPAT-03' ('variable named "' + $name + '": write ${name}?.m in 7.6-only code, an if test in 5.1-bound code')
        }
        if ($name -match '^Is(Windows|Linux|MacOS|CoreCLR)$' -and -not $defined -and -not (Get-Guarded $v)) {
            if ($name -eq 'IsWindows' -or $strict) {
                Add-Finding $f $v 'MUST' 'PS-COMPAT-04' ('$' + $name + ' is undefined on 5.1 (silently false, or an error under Set-StrictMode): use [Environment]::OSVersion.Platform -eq Win32NT')
            } else {
                Add-Finding $f $v 'SHOULD' 'PS-COMPAT-04' ('$' + $name + ' is undefined on 5.1 (falsey outside Set-StrictMode): prefer [Environment]::OSVersion.Platform')
            }
        }
    }
    foreach ($m in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.MemberExpressionAst] }, $true)) {
        if ($m.Expression.Extent.Text -eq '$PSVersionTable' -and $m.Member.Extent.Text -eq 'Platform' -and $strict -and -not (Get-Guarded $m)) {
            Add-Finding $f $m 'MUST' 'PS-COMPAT-04' '$PSVersionTable.Platform is absent on 5.1 and throws under Set-StrictMode'
        }
        if ($m.Static -and $m.Expression -is [System.Management.Automation.Language.TypeExpressionAst] -and $m.Member -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
            $ty = $m.Expression.TypeName.GetReflectionType()
            if ($ty -and $m.Member.Value -ine 'new') {
                $found = $ty.GetMember($m.Member.Value, [Reflection.BindingFlags]'Static,Public,FlattenHierarchy,IgnoreCase')
                if (-not $found -or @($found).Count -eq 0) {
                    Add-Finding $f $m 'MUST' 'PS-COMPAT-05' ('[' + $m.Expression.TypeName.FullName + ']::' + $m.Member.Value + ' has no static member on this host')
                }
            }
        }
        if ($m -is [System.Management.Automation.Language.InvokeMemberExpressionAst] -and -not $m.Static -and $m.Member.Extent.Text -eq 'Split' -and $m.Arguments.Count -eq 1 `
                -and $m.Arguments[0] -is [System.Management.Automation.Language.StringConstantExpressionAst] -and $m.Arguments[0].Value.Length -gt 1) {
            Add-Finding $f $m 'MUST' 'PS-COMPAT-05' '.Split() with a multi-character string splits on each character on 5.1 and on the whole string on 7.x: use -split'
        }
    }
    foreach ($c in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.CommandAst] }, $true)) {
        $name = $c.GetCommandName()
        if (-not $name) { continue }
        $els = @($c.CommandElements | Select-Object -Skip 1)
        $params = @{}; $positional = 0; $splat = $false
        for ($i = 0; $i -lt $els.Count; $i++) {
            $el = $els[$i]
            if ($el -is [System.Management.Automation.Language.CommandParameterAst]) {
                $params[$el.ParameterName] = $el
                if (-not $el.Argument -and ($i + 1) -lt $els.Count -and $els[$i + 1] -isnot [System.Management.Automation.Language.CommandParameterAst]) { $i++ }
            }
            elseif ($el -is [System.Management.Automation.Language.VariableExpressionAst] -and $el.Splatted) { $splat = $true }
            else { $positional++ }
        }
        if ($name -eq 'Join-Path') {
            $named = @($params.Keys | Where-Object { (Test-Prefix $_ 'Path' 1) -or (Test-Prefix $_ 'ChildPath' 2) }).Count
            if (($positional + $named) -gt 2) { Add-Finding $f $c 'MUST' 'PS-COMPAT-05' 'Join-Path with more than two path parts needs 6.0+: nest Join-Path' }
        }
        if ($sevenOnlyCommand -contains $name) {
            Add-Finding $f $c 'MUST' 'PS-COMPAT-05' ($name + ' does not exist on 5.1')
        }
        foreach ($cmd in $sevenOnlyParam.Keys) {
            if ($cmd -ine $name) { continue }
            foreach ($k in @($params.Keys)) {
                foreach ($seven in $sevenOnlyParam[$cmd]) {
                    if ((Test-Prefix $k $seven 2) -or ($k -ieq $seven)) { Add-Finding $f $c 'MUST' 'PS-COMPAT-05' ($name + ' -' + $seven + ' does not exist on 5.1') }
                }
            }
        }
        if ($name -in 'Invoke-WebRequest', 'iwr', 'curl', 'wget') {
            $ubp = $splat -and ($text -match 'UseBasicParsing')
            foreach ($k in @($params.Keys)) { if (Test-Prefix $k 'UseBasicParsing' 4) { $ubp = $true } }
            if ($text -match 'PSDefaultParameterValues\[.Invoke-WebRequest:UseBasicParsing.\]') { $ubp = $true }
            if (-not $ubp) { Add-Finding $f $c 'MUST' 'PS-COMPAT-06' ($name + ' without -UseBasicParsing prompts, then fails under -NonInteractive on 5.1 after the 2025-12-09 update') }
        }
    }
}
$out | Sort-Object
if ($must) { exit 1 }
exit 0
