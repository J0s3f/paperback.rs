# Pages of paperback.rs with damaged record cells, and pages of this version read by an older one.
#
#   powershell -File tools\interop\damaged.ps1 -WorkDir C:\temp\pbwork -Paperback target\release\paperback-rs.exe `
#       [-OriginalExe C:\path\PaperBak.exe -Label v110] [-Older C:\path\older\paperback-rs.exe]
#
# The pages are written with the record cells at the start of the page, at both ends or only the hash
# record wiped. They must be read by the original program (-OriginalExe, optional), by an older
# paperback.rs (-Older, optional: for example the 1.1 build) and by this version, which also has to
# say what became of the hash. Intact pages, with and without a password and over several pages, go to
# the older build as well. Windows only; every run of an original is bounded by a timeout.
param(
  [Parameter(Mandatory)][string]$WorkDir,
  [Parameter(Mandatory)][string]$Paperback,
  [string]$OriginalExe = '',
  [string]$Label = 'original',
  [string]$Older = ''
)
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Resolve-Path (Join-Path $here '..\..')
$pb = (Resolve-Path $Paperback).Path
$out = Join-Path $WorkDir 'damaged'
New-Item -ItemType Directory -Force $out | Out-Null

function Sha256Of($path) {
  $sha = [Security.Cryptography.SHA256]::Create(); $stream = [IO.File]::OpenRead($path)
  try { return [BitConverter]::ToString($sha.ComputeHash($stream)) } finally { $stream.Dispose(); $sha.Dispose() }
}
function Same($a, $b) { (Test-Path $a) -and (Test-Path $b) -and ((Sha256Of $a) -eq (Sha256Of $b)) }

$text = Join-Path $WorkDir 'sample.txt'
if (-not (Test-Path $text)) { ("Lorem ipsum paper test line`r`n" * 400) | Set-Content $text -NoNewline }
$binary = Join-Path $WorkDir 'binary.bin'
if (-not (Test-Path $binary)) { $b = New-Object byte[] 150000; (New-Object Random 7).NextBytes($b); [IO.File]::WriteAllBytes($binary, $b) }
$inputs = @{ text = $text; binary = $binary }

# The pages with wiped cells come from a test of the library, which knows where the cells are.
foreach ($name in $inputs.Keys) {
  $dir = Join-Path $out $name
  Get-ChildItem $dir -ErrorAction SilentlyContinue | ForEach-Object { $_.Delete() }
  New-Item -ItemType Directory -Force $dir | Out-Null
  $env:PB_INPUT = $inputs[$name]; $env:PB_OUT = $dir
  Push-Location $root
  try { & cargo test -p paperback-rs --lib write_pages_with_damaged_records -- --ignored 2>&1 | Out-Null } finally { Pop-Location }
}

# What this version has to say about each case.
$expected = @{ head = 'matches the SHA-256'; hash = 'matches the SHA-256'; ends = 'none was read' }
$results = @()
foreach ($name in $inputs.Keys) {
  foreach ($case in $expected.Keys) {
    $pages = @(Get-ChildItem (Join-Path $out $name) -Filter "$case-*.bmp" | Sort-Object Name | ForEach-Object FullName)
    $tag = "$name-$case"
    if ($pages.Count -eq 0) { $results += [pscustomobject]@{ Case = $tag; Reader = 'pages'; Pass = $false; Note = 'no pages written' }; continue }
    $own = Join-Path $out "$tag.own"
    $said = & $pb decode @pages -v -o $own 2>&1 | Out-String
    $results += [pscustomobject]@{ Case = $tag; Reader = 'this version'; Pass = ((Same $inputs[$name] $own) -and $said.Contains($expected[$case])); Note = "$($pages.Count) page(s)" }
    if ($Older) {
      $old = Join-Path $out "$tag.older"
      & $Older decode @pages -o $old 2>&1 | Out-Null
      $results += [pscustomobject]@{ Case = $tag; Reader = 'older paperback.rs'; Pass = (Same $inputs[$name] $old); Note = "exit $LASTEXITCODE" }
    }
    if ($OriginalExe) {
      $run = Join-Path $WorkDir $Label
      New-Item -ItemType Directory -Force $run | Out-Null
      Copy-Item $OriginalExe (Join-Path $run 'PaperBak.exe') -Force
      $restored = Join-Path $out "$tag.$Label"
      if (Test-Path $restored) { [IO.File]::Delete($restored) }
      Get-Process PaperBak -ErrorAction SilentlyContinue | Stop-Process -Force
      & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $here 'drive-original.ps1') -Exe (Join-Path $run 'PaperBak.exe') -Action open -InputList ($pages -join '|') -OutputPath $restored -Password '__none__' -TimeoutSec 150 2>&1 | Out-Null
      $results += [pscustomobject]@{ Case = $tag; Reader = "original $Label"; Pass = (Same $inputs[$name] $restored); Note = "exit $LASTEXITCODE" }
    }
  }
}

# Intact pages of this version for the older build: plain, over several pages, with a password.
if ($Older) {
  $cases = @(
    @{ Tag = 'plain'; File = $text; Args = @() },
    @{ Tag = 'two-pages'; File = $binary; Args = @('--compression', 'none') },
    @{ Tag = 'password'; File = $text; Args = @('--password-env', 'PB_TEST_PW') },
    @{ Tag = 'no-extensions'; File = $text; Args = @('--no-extensions') }
  )
  $env:PB_TEST_PW = 'hunter2'
  foreach ($c in $cases) {
    $prefix = Join-Path $out "intact-$($c.Tag)"
    Get-ChildItem "$out" -Filter "intact-$($c.Tag)-*" | ForEach-Object { $_.Delete() }
    & $pb encode $c.File -o "$prefix-%02d.bmp" --printer-dpi 300 --dot-dpi 200 @($c.Args) 2>&1 | Out-Null
    $pages = @(Get-ChildItem $out -Filter "intact-$($c.Tag)-*.bmp" | Sort-Object Name | ForEach-Object FullName)
    $old = "$prefix.older"
    $pwArgs = if ($c.Tag -eq 'password') { @('--password-env', 'PB_TEST_PW') } else { @() }
    & $Older decode @pages -o $old @pwArgs 2>&1 | Out-Null
    $results += [pscustomobject]@{ Case = "intact $($c.Tag)"; Reader = 'older paperback.rs'; Pass = (Same $c.File $old); Note = "$($pages.Count) page(s), exit $LASTEXITCODE" }
  }
}

$results | Select-Object Case, Reader, @{ n = 'Result'; e = { if ($_.Pass) { 'PASS' } else { 'FAIL' } } }, Note | Format-Table -AutoSize | Out-String -Width 200
if ($results | Where-Object { -not $_.Pass }) { exit 1 }
