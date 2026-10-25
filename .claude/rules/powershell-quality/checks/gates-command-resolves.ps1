#Requires -Version 5.1
<#
gates-command-resolves.ps1 -Path PATH [PATH...] [-CheckParameters] [-ParametersOnly] [-Allow NAME,NAME] [-DefinedRoot DIR] [-Strict]
PS-GATE-06: flag Verb-Noun command names, and with -CheckParameters the -Parameter names, that do not exist on THIS host.
Advisory, never a required status check. Run it on the kind's target host: Windows PowerShell 5.1 for installer and module kinds.
A Linux pwsh cannot resolve Get-CimInstance or Get-NetAdapter, so a Linux run is a different, wrong check.
Exit 0 clean, 1 findings, 2 misconfigured (path missing, no .ps1/.psm1 file, or a wildcard in -Allow).
-ParametersOnly reports only unknown parameters (the stronger signal, implies -CheckParameters).
-Allow takes exact command names from a reviewed, repo-owned list. A wildcard entry is refused: -Allow '*-Az*' hides Get-AzVMFooBarBaz.
Suppressions, each counted on the summary line and never silent:
  not-verb-noun   a name without a hyphen (git, tar), with a path separator, or a lowercase non-verb (code-insiders): natives, not cmdlets
  defined         a function defined in the file, or in any file under -DefinedRoot (dot-sourced libraries)
  module-missing  the file imports a module this host lacks (Import-Module, using module, #Requires -Modules): unresolved names are unknown, not hallucinated (-Strict reports them)
  pester          parameters of Pester commands (DynamicParam, and the host's Pester is not the repo's)
  allowed         names in -Allow
  guarded         names the file probes with Get-Command before use (optional module commands)
  requires7       a file with #Requires -Version 6 to 9, skipped on Windows PowerShell (declared 7-only)
  version-guarded commands inside an if whose condition tests $PSVersionTable, $IsWindows, $IsLinux, $IsMacOS, $IsCoreCLR or $PSEdition
                  (this also hides a hallucination inside such a gate: PS-COMPAT-05's smoke run covers it)
  dynamic         & $name, . $path, splatted parameters: no static name to check
Blind spots: .NET members, parameter values (Get-Content -Encoding utf8NoBOM), dynamic command names.
Watched red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (measured 2026-10-11). ASCII only, 5.1 syntax.
#>
param(
    [Parameter(Mandatory = $true)][string[]]$Path,
    [string]$DefinedRoot = '',
    [switch]$CheckParameters,
    [switch]$ParametersOnly,
    [switch]$Strict,
    [string[]]$Allow = @()
)
$ErrorActionPreference = 'Stop'
$Allow = @($Allow | ForEach-Object { $_ -split ',' } | Where-Object { $_ })   # -File passes 'a,b' as one string
function Stop-Tool([string]$m) { [Console]::Error.WriteLine("resolver: $m"); exit 2 }
if ($ParametersOnly) { $CheckParameters = $true }
if (@($Allow | Where-Object { $_ -match '[*?\[]' }).Count -gt 0) { Stop-Tool 'wildcard in -Allow: list exact command names' }
$files = @()
foreach ($p in $Path) {
    if (-not (Test-Path -LiteralPath $p)) { Stop-Tool "path not found: $p" }
    $it = Get-Item -LiteralPath $p
    if ($it.PSIsContainer) { $files += @(Get-ChildItem -LiteralPath $p -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1' } | ForEach-Object { $_.FullName }) } else { $files += $it.FullName }
}
if ($files.Count -eq 0) { Stop-Tool 'no .ps1/.psm1 files (a vacuous check is a green lie)' }
$PARSER = [System.Management.Automation.Language.Parser]
$isFn = { param($n) $n -is [System.Management.Automation.Language.FunctionDefinitionAst] }
$isCmd = { param($n) $n -is [System.Management.Automation.Language.CommandAst] }
$global_defined = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
if ($DefinedRoot) {
    if (-not (Test-Path -LiteralPath $DefinedRoot -PathType Container)) { Stop-Tool "DefinedRoot not a directory: $DefinedRoot" }
    foreach ($f in (Get-ChildItem -LiteralPath $DefinedRoot -Recurse -File | Where-Object { $_.Extension -in '.ps1', '.psm1' })) {
        $t = $null; $e = $null
        try { $a = $PARSER::ParseFile($f.FullName, [ref]$t, [ref]$e) } catch { continue }
        foreach ($fd in $a.FindAll($isFn, $true)) { [void]$global_defined.Add(($fd.Name -replace '^(global|script|local|private):', '')) }
    }
}
$cache = @{}
$script:approved = @(Get-Verb | ForEach-Object { $_.Verb.ToLowerInvariant() })
function Get-Info([string]$n) {
    if ($cache.ContainsKey($n)) { return $cache[$n] }
    $c = Get-Command -Name $n -ErrorAction SilentlyContinue | Select-Object -First 1
    $r = $null
    if ($c) {
        $real = $c
        if ($c.CommandType -eq 'Alias' -and $c.ResolvedCommand) { $real = $c.ResolvedCommand }
        $pn = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
        if ($real.Parameters) { foreach ($k in $real.Parameters.Keys) { [void]$pn.Add($k); foreach ($al in $real.Parameters[$k].Aliases) { [void]$pn.Add($al) } } }
        if ($c.CommandType -eq 'Cmdlet') {   # rev3: provider dynamic parameters (Set-ItemProperty -Type is registry-only)
            foreach ($root in 'C:\', 'HKLM:\', 'Cert:\', 'Function:\', 'Alias:\') {   # Set-Item -Options is a Function: and Alias: parameter
                $dc = $null; try { $dc = Get-Command -Name $n -ArgumentList $root -ErrorAction SilentlyContinue | Select-Object -First 1 } catch { [Console]::Error.WriteLine('rev3-dyn-skip ' + $n + ' ' + $root) }
                if ($dc -and $dc.Parameters) { foreach ($k in $dc.Parameters.Keys) { [void]$pn.Add($k); foreach ($al in $dc.Parameters[$k].Aliases) { [void]$pn.Add($al) } } }
            }
        }
        $r = [pscustomobject]@{ Type = [string]$c.CommandType; Params = $pn; HasParams = [bool]$real.Parameters; Module = [string]$real.ModuleName }
    }
    $cache[$n] = $r
    return $r
}
$bad = 0; $sup = @{ 'vguard' = 0; 'not-verb-noun' = 0; 'defined' = 0; 'module-missing' = 0; 'dynamic' = 0; 'pester' = 0; 'allowed' = 0; 'guarded' = 0; 'requires7' = 0 }
foreach ($f in $files) {
    $t = $null; $e = $null
    $ast = $PARSER::ParseFile($f, [ref]$t, [ref]$e)
    if ($PSVersionTable.PSEdition -eq 'Desktop' -and $ast.ScriptRequirements -and $ast.ScriptRequirements.RequiredPSVersion -and $ast.ScriptRequirements.RequiredPSVersion.Major -ge 6) { $sup['requires7']++; continue }   # declared 7-only: out of the 5.1 scope
    $e = @($e | Where-Object { $_.ErrorId -notin 'TypeNotFound', 'ModuleNotFoundDuringParse', 'ResourceNotDefined' })   # a missing dependency leaves a usable tree
    if (@($e).Count -gt 0) { '{0}:{1}: parse error (not resolved): {2}' -f $f, $e[0].Extent.StartLineNumber, ($e[0].Message -replace '\s+', ' '); $bad++; continue }
    $defined = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($fd in $ast.FindAll($isFn, $true)) { [void]$defined.Add(($fd.Name -replace '^(global|script|local|private):', '')) }
    # modules the file asks for and this host lacks
    $missingMod = $false
    foreach ($im in $ast.FindAll($isCmd, $true)) {
        if ($im.GetCommandName() -in @('Import-Module', 'ipmo')) {
            foreach ($el in ($im.CommandElements | Select-Object -Skip 1)) {
                if ($el -is [System.Management.Automation.Language.StringConstantExpressionAst] -and -not (Get-Module -ListAvailable -Name $el.Value -ErrorAction SilentlyContinue) -and -not (Test-Path -LiteralPath $el.Value)) { $missingMod = $true }
            }
        }
    }
    if ($ast.ScriptRequirements -and $ast.ScriptRequirements.RequiredModules) { foreach ($m in $ast.ScriptRequirements.RequiredModules) { if (-not (Get-Module -ListAvailable -Name $m.Name -ErrorAction SilentlyContinue)) { $missingMod = $true } } }
    foreach ($u in $ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.UsingStatementAst] -and $n.UsingStatementKind -eq 'Module' }, $true)) { $missingMod = $true }
    # names the file probes for before use (if (Get-Command X ...)): optional commands, not hallucinations
    $guarded = New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach ($g in $ast.FindAll($isCmd, $true)) {
        if ($g.GetCommandName() -in @('Get-Command', 'gcm')) {
            foreach ($el in ($g.CommandElements | Select-Object -Skip 1)) { if ($el -is [System.Management.Automation.Language.StringConstantExpressionAst]) { [void]$guarded.Add($el.Value) } }
        }
    }
    foreach ($c in $ast.FindAll($isCmd, $true)) {
        $name = $c.GetCommandName()
        $line = $c.Extent.StartLineNumber
        if (-not $name) { $sup['dynamic']++; continue }
        $vg = $false; $anc = $c.Parent
        while ($anc -and -not $vg) {
            if ($anc -is [System.Management.Automation.Language.IfStatementAst]) { foreach ($cl in $anc.Clauses) { if ($cl.Item1.Extent.Text -match '\$PSVersionTable|\$IsWindows|\$IsLinux|\$IsMacOS|\$IsCoreCLR|\$PSEdition' -and $c.Extent.StartOffset -ge $cl.Item2.Extent.StartOffset -and $c.Extent.EndOffset -le $cl.Item2.Extent.EndOffset) { $vg = $true } } }
            $anc = $anc.Parent
        }
        if ($vg) { $sup['vguard']++; continue }
        if ($name -notmatch '^[A-Za-z]+-[A-Za-z0-9_.]+$') { $sup['not-verb-noun']++; continue }
        if ($name -cmatch '^[a-z]+-[a-z0-9_.]+$' -and $script:approved -notcontains ($name -split '-')[0]) { $sup['not-verb-noun']++; continue }   # rev3: code-insiders, git-lfs
        if ($defined.Contains($name) -or $global_defined.Contains($name)) { $sup['defined']++; continue }
        if ($guarded.Contains($name)) { $sup['guarded'] = 1 + [int]$sup['guarded']; continue }
        if ($Allow -contains $name) { $sup['allowed'] = 1 + [int]$sup['allowed']; continue }
        $r = Get-Info $name
        if (-not $r) {
            if ($missingMod -and -not $Strict) { $sup['module-missing']++; continue }
            if (-not $ParametersOnly) { '{0}:{1}: unresolved command {2}' -f $f, $line, $name; $bad++ }
            continue
        }
        if (-not $CheckParameters -or $r.Type -eq 'Application' -or -not $r.HasParams) { continue }
        if ($r.Module -eq 'Pester') { $sup['pester'] = 1 + [int]$sup['pester']; continue }   # Pester commands take DynamicParam and the host's Pester is not the repo's
        $splat = $false
        foreach ($el in $c.CommandElements) { if ($el -is [System.Management.Automation.Language.VariableExpressionAst] -and $el.Splatted) { $splat = $true } }
        if ($splat) { $sup['dynamic']++ }
        foreach ($el in $c.CommandElements) {
            if ($el -isnot [System.Management.Automation.Language.CommandParameterAst]) { continue }
            $pname = $el.ParameterName
            $ok = $r.Params.Contains($pname)
            if (-not $ok) { foreach ($k in $r.Params) { if ($k.StartsWith($pname, [StringComparison]::OrdinalIgnoreCase)) { $ok = $true; break } } }
            if (-not $ok) { '{0}:{1}: unknown parameter -{2} on {3}' -f $f, $line, $pname, $name; $bad++ }
        }
    }
}
[Console]::Error.WriteLine(('resolver: {0} files, {1} findings; suppressed not-verb-noun={2} defined={3} module-missing={4} dynamic={5} pester-params-skipped={6} allowed={7} guarded={8} version-guarded={9} requires7={10}' -f $files.Count, $bad, $sup['not-verb-noun'], $sup['defined'], $sup['module-missing'], $sup['dynamic'], $sup['pester'], $sup['allowed'], $sup['guarded'], $sup['vguard'], $sup['requires7']))
if ($bad -gt 0) { exit 1 }
