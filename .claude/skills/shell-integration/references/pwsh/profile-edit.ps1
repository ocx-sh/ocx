# profile-edit.ps1 [-Method add-content|append-all-text|out-file|twin] : append a managed block to a PowerShell profile.
# Four copies of a user profile (ASCII LF, ASCII CRLF, UTF-8 BOM + CRLF with U+00E9, UTF-16LE BOM + CRLF with U+00E9: what `>` and Out-File write on 5.1) live in a temp directory
# (never $PROFILE). The editor appends the managed block TWICE, then the copy is dot-sourced in this host. Violation = the original line or the block no longer loads, the sentinel
# U+00E9 is wrong, there are not exactly 1 block, or the original bytes are no longer a prefix of the file. Exit 1 on any violation; output lines starting FAIL name it.
param([ValidateSet('add-content','append-all-text','out-file','twin')][string]$Method = 'twin')
$ErrorActionPreference = 'Stop'
$BEGIN = '# >>> demo managed block >>>'; $END = '# <<< demo managed block <<<'
$block = "`$global:BLK_OK = 1"
$dir = Join-Path ([IO.Path]::GetTempPath()) ('profedit-' + [guid]::NewGuid().ToString('N')); $null = New-Item -ItemType Directory -Path $dir
$E = [char]0xE9
$orig = "`$global:ORIG_OK = 1", "`$global:ORIG_E = '$E'"
function Write-Variant($name, $eol, $enc, $bom) {
  $t = ($orig -join $eol) + $eol
  $b = $enc.GetBytes($t); if ($bom) { $b = $enc.GetPreamble() + $b }
  [IO.File]::WriteAllBytes((Join-Path $dir $name), $b)
}
$u8 = New-Object Text.UTF8Encoding($false); $u8b = New-Object Text.UTF8Encoding($true); $u16 = New-Object Text.UnicodeEncoding($false, $true)
$ascii = @("`$global:ORIG_OK = 1", "`$global:ORIG_E = [char]0xE9")
[IO.File]::WriteAllBytes((Join-Path $dir 'ascii-lf.ps1'), $u8.GetBytes(($ascii -join "`n") + "`n"))
[IO.File]::WriteAllBytes((Join-Path $dir 'ascii-crlf.ps1'), $u8.GetBytes(($ascii -join "`r`n") + "`r`n"))
Write-Variant 'utf8bom-crlf.ps1' "`r`n" $u8b $true
Write-Variant 'utf16le-crlf.ps1' "`r`n" $u16 $true
function Edit-Twin([string]$Path) {
  $bytes = [IO.File]::ReadAllBytes($Path)
  if ($bytes.Length -ge 2 -and $bytes[0] -eq 0xFF -and $bytes[1] -eq 0xFE) { $enc = New-Object Text.UnicodeEncoding($false, $true); $off = 2; $pre = [byte[]](0xFF, 0xFE) }
  elseif ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) { $enc = New-Object Text.UTF8Encoding($true); $off = 3; $pre = [byte[]](0xEF, 0xBB, 0xBF) }
  else { $enc = New-Object Text.UTF8Encoding($false); $off = 0; $pre = [byte[]]@() }
  $text = $enc.GetString($bytes, $off, $bytes.Length - $off)
  $eol = if ($text.Contains("`r`n")) { "`r`n" } else { "`n" }
  $rx = '(?s)' + [regex]::Escape($BEGIN) + '\r?\n.*?' + [regex]::Escape($END) + '(\r?\n|$)'
  $text = [regex]::Replace($text, $rx, '')
  if ($text.Length -gt 0 -and -not $text.EndsWith("`n")) { $text += $eol }
  $text += $BEGIN + $eol + $block + $eol + $END + $eol
  [IO.File]::WriteAllBytes($Path, $pre + $enc.GetBytes($text))
}
function Edit-Naive([string]$Path) {
  $text = $BEGIN + "`r`n" + $block + "`r`n" + $END + "`r`n"
  switch ($Method) {
    'add-content'     { Add-Content -Path $Path -Value $text -NoNewline }
    'append-all-text' { [IO.File]::AppendAllText($Path, $text) }
    'out-file'        { $text | Out-File -FilePath $Path -Append -NoNewline }
  }
}
$bad = 0
foreach ($f in Get-ChildItem $dir -Filter *.ps1 | Sort-Object Name) {
  $before = [IO.File]::ReadAllBytes($f.FullName)
  $times = if ($Method -eq 'twin') { 1, 2 } else { 1 }   # a naive append doubles by construction; applied once it isolates the encoding effect
  foreach ($i in $times) { if ($Method -eq 'twin') { Edit-Twin $f.FullName } else { Edit-Naive $f.FullName } }
  $after = [IO.File]::ReadAllBytes($f.FullName)
  $prefix = $true; for ($i = 0; $i -lt $before.Length; $i++) { if ($after[$i] -ne $before[$i]) { $prefix = $false; break } }
  # load it the way the host loads a profile: dot-source in a child scope, then read the variables
  $global:ORIG_OK = $null; $global:BLK_OK = $null; $global:ORIG_E = $null; $err = ''
  try { . $f.FullName } catch { $err = $_.Exception.Message.Split("`n")[0] }
  # block count from the decoded text (UTF-16 aware)
  $dec = if ($after.Length -ge 2 -and $after[0] -eq 0xFF -and $after[1] -eq 0xFE) { [Text.Encoding]::Unicode.GetString($after) } else { [Text.Encoding]::UTF8.GetString($after) }
  $nblk = ([regex]::Matches($dec, [regex]::Escape($BEGIN))).Count
  $why = @()
  if ($err) { $why += "load error: $err" }
  if ($global:ORIG_OK -ne 1) { $why += 'original line lost' }
  if ($global:BLK_OK -ne 1) { $why += 'block not loaded' }
  if ($global:ORIG_E -ne [string]$E) { $why += ('sentinel U+00E9 became ' + (($global:ORIG_E -as [string]).ToCharArray() | ForEach-Object { '{0:X4}' -f [int]$_ }) -join ',') }
  if ($nblk -ne 1) { $why += "$nblk blocks (want 1)" }
  if (-not $prefix) { $why += 'original bytes are not a prefix of the result' }
  '{0,-18} {1,-16} v={2} {3}' -f $f.Name, $Method, $PSVersionTable.PSVersion, $(if ($why.Count) { 'FAIL: ' + ($why -join '; ') } else { 'OK' })
  if ($why.Count) { $bad = 1 }
}
Remove-Item -Recurse -Force -LiteralPath $dir
exit $bad
