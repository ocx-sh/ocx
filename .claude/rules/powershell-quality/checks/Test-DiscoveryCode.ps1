#Requires -Version 5.1
<#
Test-DiscoveryCode.ps1 FILE...
SH-TEST-21: list statements in a Pester file that run at discovery and break or leak. Output is the violation, one line per
finding "file:line: discovery-only code at LEVEL: WHY". Exit 1 when any, 0 when none, so a missing or unparseable file fails closed.
  file level:       an assignment, a function definition, or a side-effect command (New, Set, Remove, Add, Install, Start, Stop, Invoke-*)
  Describe/Context: an assignment whose variable, or a function whose name, is used inside It, BeforeEach, AfterEach, BeforeAll or AfterAll
                    of the same file, or a side-effect command. Control flow, data used only by -ForEach or -TestCases, and
                    functions used only at discovery are not reported.
  InModuleScope blocks are transparent: the body is judged where the wrapper stands.
Run it on the files the CI step runs. ASCII only, 5.1 syntax.
Watched red on a plant and green on its twin on Pester 5.9.1 and 6.2.0 files under pwsh 7.4.7, 7.6.6 and Windows PowerShell 5.1.26100.9444 (measured 2026-10-11).
#>
param([Parameter(Mandatory = $true, ValueFromRemainingArguments = $true)][string[]]$Path)
$containers = 'Describe', 'Context', 'BeforeAll', 'AfterAll', 'BeforeDiscovery', 'BeforeEach', 'AfterEach', 'It', 'InModuleScope'
$runPhase = 'It', 'BeforeEach', 'AfterEach', 'BeforeAll', 'AfterAll'
$sideEffect = '^(New|Set|Remove|Add|Install|Start|Stop|Invoke)-'
$bad = 0
function Get-CommandName($st) {
  if ($st -is [Management.Automation.Language.PipelineAst] -and $st.PipelineElements[0] -is [Management.Automation.Language.CommandAst]) { return $st.PipelineElements[0].GetCommandName() }
  return $null
}
function Get-Block($cmd) {
  $sb = $cmd.CommandElements | Where-Object { $_ -is [Management.Automation.Language.ScriptBlockExpressionAst] } | Select-Object -Last 1
  if ($sb) { $sb.ScriptBlock.EndBlock.Statements }
}
function Test-Statements($stmts, $level, $file) {
  foreach ($st in $stmts) {
    if ($st -is [Management.Automation.Language.UsingStatementAst]) { continue }
    $name = Get-CommandName $st
    if ($name -and ($containers -contains $name)) {
      if ($name -in 'Describe', 'Context') { Test-Statements (Get-Block $st.PipelineElements[0]) 'body' $file }
      elseif ($name -eq 'InModuleScope') { Test-Statements (Get-Block $st.PipelineElements[0]) $level $file }
      continue
    }
    $why = $null
    if ($st -is [Management.Automation.Language.FunctionDefinitionAst]) {
      if ($level -eq 'file' -or ($script:calls -contains $st.Name)) { $why = "function $($st.Name)" }
    }
    elseif ($st -is [Management.Automation.Language.AssignmentStatementAst]) {
      $v = $st.Left.Find({ param($n) $n -is [Management.Automation.Language.VariableExpressionAst] }, $true)
      $vn = if ($v) { $v.VariablePath.UserPath -replace '^(script|global|local|private):', '' }
      if ($level -eq 'file' -or ($vn -and ($script:refs -contains $vn))) { $why = "assignment to `$$vn" }
    }
    elseif ($name -and $name -match $sideEffect) { $why = "side effect $name" }
    if ($why) {
      $script:bad++
      '{0}:{1}: discovery-only code at {2}: {3}' -f $file, $st.Extent.StartLineNumber, $level, $why
    }
  }
}
foreach ($f in $Path) {
  if (-not (Test-Path -LiteralPath $f -PathType Leaf)) { '{0}: not a file' -f $f; $bad++; continue }
  $t = $null; $e = $null
  $ast = [Management.Automation.Language.Parser]::ParseFile($f, [ref]$t, [ref]$e)
  if ($e.Count) { '{0}: parse error {1}' -f $f, $e[0].Message; $bad++; continue }
  $blocks = $ast.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] -and ($runPhase -contains $n.GetCommandName()) }, $true)
  $script:refs = @(); $script:calls = @()
  foreach ($b in $blocks) {
    $sb = $b.CommandElements | Where-Object { $_ -is [Management.Automation.Language.ScriptBlockExpressionAst] } | Select-Object -Last 1
    if (-not $sb) { continue }
    $assigned = @($sb.FindAll({ param($n) $n -is [Management.Automation.Language.AssignmentStatementAst] }, $true) | ForEach-Object { $x = $_.Left.Find({ param($m) $m -is [Management.Automation.Language.VariableExpressionAst] }, $true); if ($x) { $x.VariablePath.UserPath -replace '^(script|global|local|private):', '' } })
    foreach ($v in $sb.FindAll({ param($n) $n -is [Management.Automation.Language.VariableExpressionAst] }, $true)) {
      $vn = $v.VariablePath.UserPath -replace '^(script|global|local|private):', ''
      if ($assigned -notcontains $vn) { $script:refs += $vn }
    }
    foreach ($c in $sb.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] }, $true)) { $script:calls += $c.GetCommandName() }
  }
  Test-Statements $ast.EndBlock.Statements 'file' $f
}
if ($bad) { exit 1 }
exit 0
