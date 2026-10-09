# Compatibility matrix against the original PaperBack executables.
#
#   powershell -File tools\interop\matrix.ps1 -OriginalExe C:\path\PaperBak.exe -Label v110 `
#       -WorkDir C:\temp\pbwork -Paperback target\release\paperback-rs.exe
#
# For every case the original writes bitmaps that this tool must read, and this tool writes
# bitmaps that the original must read. Encrypted pages written by this tool use the 1.10
# scheme, so for a 1.00 executable those "ours -> original" cases are expected to fail.
param(
  [Parameter(Mandatory)][string]$OriginalExe,
  [Parameter(Mandatory)][string]$Label,
  [Parameter(Mandatory)][string]$WorkDir,
  [Parameter(Mandatory)][string]$Paperback,
  [string]$Only = '',
  [switch]$Legacy   # the executable is PaperBack 1.00: encrypted pages written here are expected to fail
)
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$run = Join-Path $WorkDir $Label
$out = Join-Path $run 'out'
New-Item -ItemType Directory -Force $out | Out-Null
Copy-Item $OriginalExe (Join-Path $run 'PaperBak.exe') -Force
$exe = Join-Path $run 'PaperBak.exe'
$pb = (Resolve-Path $Paperback).Path

$text = Join-Path $WorkDir 'sample.txt'
if (-not (Test-Path $text)) { ("Lorem ipsum paper test line`r`n" * 400) | Set-Content $text -NoNewline }
$binary = Join-Path $WorkDir 'binary.bin'
if (-not (Test-Path $binary)) { $b = New-Object byte[] 150000; (New-Object Random 7).NextBytes($b); [IO.File]::WriteAllBytes($binary, $b) }

function Run-Auto([string[]]$autoArgs) {
  Get-Process PaperBak -ErrorAction SilentlyContinue | Stop-Process -Force
  & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $here 'drive-original.ps1') -Exe $exe @autoArgs 2>&1 | Out-Null
  return $LASTEXITCODE
}
function Same($a, $b) { (Test-Path $a) -and (Test-Path $b) -and ((Get-FileHash $a).Hash -eq (Get-FileHash $b).Hash) }

$cases = @(
  @{ Name = 'text';         File = $text;   Compression = 2; Password = '' },
  @{ Name = 'binary-raw';   File = $binary; Compression = 0; Password = '' },
  @{ Name = 'text-fast';    File = $text;   Compression = 1; Password = '' },
  @{ Name = 'text-crypt';   File = $text;   Compression = 2; Password = 'hunter2' },
  @{ Name = 'text-umlaut';  File = $text;   Compression = 2; Password = "p$([char]0xE4)ssw$([char]0xF6)rt" },
  @{ Name = 'binary-crypt'; File = $binary; Compression = 0; Password = 'passw0rt' }
)
$results = @()
foreach ($c in $cases) {
  if ($Only -and $c.Name -ne $Only) { continue }
  $tag = $c.Name
  $pass = if ($c.Password) { $c.Password } else { '__none__' }
  Get-ChildItem $out -Filter "o-$tag*" -ErrorAction SilentlyContinue | ForEach-Object { $_.Delete() }
  $code = Run-Auto @('-Action', 'save', '-InputList', $c.File, '-OutputPath', "$out\o-$tag.bmp", '-Compression', $c.Compression, '-Password', $pass, '-TimeoutSec', 120)
  $pages = @(Get-ChildItem $out -Filter "o-$tag*.bmp" | Sort-Object Name | ForEach-Object FullName)
  $ok = $false; $note = "original exit $code, $($pages.Count) page(s)"
  if ($pages.Count -gt 0) {
    $pwArgs = @(); if ($c.Password) { $env:PB_TEST_PW = $c.Password; $pwArgs = @('--password-env', 'PB_TEST_PW') }
    & $pb decode @pages -o "$out\o-$tag.restored" @pwArgs 2>&1 | Out-Null
    $ok = Same $c.File "$out\o-$tag.restored"; $note += ", paperback.rs exit $LASTEXITCODE"
  }
  $results += [pscustomobject]@{ Original = $Label; Case = $tag; Direction = 'original -> paperback.rs'; Pass = $ok; Note = $note }

  Get-ChildItem $out -Filter "w-$tag*" -ErrorAction SilentlyContinue | ForEach-Object { $_.Delete() }
  $encArgs = @('encode', $c.File, '-o', "$out\w-$tag-%02d.bmp", '--printer-dpi', '300', '--dot-dpi', '200',
               '--compression', @('none', 'fast', 'max')[$c.Compression])
  if ($c.Password) { $env:PB_TEST_PW = $c.Password; $encArgs += @('--password-env', 'PB_TEST_PW') }
  & $pb @encArgs 2>&1 | Out-Null
  $encExit = $LASTEXITCODE
  $ours = @(Get-ChildItem $out -Filter "w-$tag-*.bmp" | Sort-Object Name | ForEach-Object FullName)
  $restored = "$out\w-$tag.restored"
  if (Test-Path $restored) { [IO.File]::Delete($restored) }
  $code = Run-Auto @('-Action', 'open', '-InputList', ($ours -join '|'), '-OutputPath', $restored, '-Password', $pass, '-TimeoutSec', 150)
  $results += [pscustomobject]@{ Original = $Label; Case = $tag; Direction = 'paperback.rs -> original'; Pass = ($(if ($Legacy -and $c.Password) { -not (Same $c.File $restored) } else { Same $c.File $restored })); Note = "paperback.rs exit $encExit, $($ours.Count) page(s), original exit $code" }
}
$results | Format-Table -AutoSize | Out-String -Width 200
if ($results | Where-Object { -not $_.Pass }) { exit 1 }

