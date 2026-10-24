#Requires -Version 5.1
<#
compat-modules.ps1 [PATH...]
PS-COMPAT-08: import every module the repo ships on the host that runs this script, with Import-Module -Force.
Run it once under powershell.exe (5.1) and once under pwsh (7.6). A directory that holds a .psd1 with a ModuleVersion
imports the manifest, any other .psm1 imports directly. Importing runs the module's top-level code, so run it in CI,
not on a machine that holds secrets. Test-ModuleManifest is not used: it passes a .psm1 containing ?? that 5.1 rejects.
No PATH lists every .psd1 and .psm1 git knows. Prints "file: import-FAIL message", ASCII only, 5.1 syntax.
Exit 0 every module imported, 1 at least one failed, 78 no git work tree, 79 no module.
Ran on Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6 on 2026-10-11, no module needed.
#>
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Path)
$ErrorActionPreference = 'Stop'
if ($Path) {
    $files = @($Path | ForEach-Object { (Resolve-Path -LiteralPath $_).ProviderPath })
} else {
    $inside = & git rev-parse --is-inside-work-tree
    if ($LASTEXITCODE -ne 0 -or $inside -ne 'true') { [Console]::Error.WriteLine('compat-modules: not inside a git work tree'); exit 78 }
    $top = (& git rev-parse --show-toplevel)
    $files = @(& git -c core.quotepath=off ls-files --cached --others --exclude-standard -- '*.psd1' '*.psm1' | ForEach-Object { Join-Path $top $_ })
}
$manifests = @($files | Where-Object { $_ -like '*.psd1' -and (Select-String -LiteralPath $_ -SimpleMatch -Pattern 'ModuleVersion' -Quiet) })
$dirs = @($manifests | ForEach-Object { Split-Path -Parent $_ })
$targets = @($manifests) + @($files | Where-Object { $_ -like '*.psm1' -and ($dirs -notcontains (Split-Path -Parent $_)) })
if (-not $targets) { [Console]::Error.WriteLine('compat-modules: no module'); exit 79 }
$bad = 0
foreach ($t in $targets) {
    try {
        $m = Import-Module -Name $t -Force -PassThru -ErrorAction Stop
        Remove-Module -ModuleInfo $m -Force -ErrorAction SilentlyContinue
    } catch {
        ('{0}: import-FAIL {1}' -f $t, ($_.Exception.Message -replace '\s+', ' '))
        $bad++
    }
}
if ($bad) { exit 1 }
exit 0
