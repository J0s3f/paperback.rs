# Regenerates crates/paperback-rs/tests/fixtures/damaged/*.png: pages of every profile with one column of
# cells wiped white on the first page, as by a fold or a streak of the scanner.
#
#   powershell -File tools\make-damaged-fixtures.ps1 -Paperback target\release\paperback-rs.exe -WorkDir C:\temp\pbdmg
#
# The input is deterministic (keep it in sync with tests/damaged.rs) and carries a fixed time, so the pages are the
# same on every run. Windows only (System.Drawing writes the PNG files).
param(
  [Parameter(Mandatory)][string]$Paperback,
  [Parameter(Mandatory)][string]$WorkDir,
  [string]$FixtureDir = ''
)
Add-Type -AssemblyName System.Drawing
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $FixtureDir) { $FixtureDir = Join-Path (Split-Path -Parent $here) 'crates\paperback-rs\tests\fixtures\damaged' }
New-Item -ItemType Directory -Force $WorkDir, $FixtureDir | Out-Null
$pb = (Resolve-Path $Paperback).Path

$source = Join-Path $WorkDir 'pattern.bin'
$bytes = New-Object byte[] 90000
for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte](($i * 131 + [math]::Floor($i / 3)) % 256) }
[IO.File]::WriteAllBytes($source, $bytes)
(Get-Item $source).LastWriteTimeUtc = [DateTime]::SpecifyKind([DateTime]'2024-01-02 03:04:05', [DateTimeKind]::Utc)

# Whitens a column of cells in an 8-bit BMP from the top to the bottom of the picture: the dots only (from x0 to x1)
# or, with the wider range, the grid lines around them as well. 300 dpi printer, 200 dpi dots: cells are 70 pixels
# wide behind a border of 25.
function Wipe([string]$path, [int]$x0, [int]$x1) {
  $b = [IO.File]::ReadAllBytes($path)
  $offset = [BitConverter]::ToInt32($b, 10); $width = [BitConverter]::ToInt32($b, 18)
  $height = [Math]::Abs([BitConverter]::ToInt32($b, 22)); $stride = ($width + 3) -band -4
  if ([BitConverter]::ToInt16($b, 28) -ne 8) { throw "$path is not an 8-bit picture" }
  for ($y = 0; $y -lt $height; $y++) { for ($x = $x0; $x -lt $x1; $x++) { $b[$offset + $y * $stride + $x] = 255 } }
  [IO.File]::WriteAllBytes($path, $b)
}
function Save-Png([string]$bmp, [string]$png) {
  $image = [Drawing.Image]::FromFile($bmp); $image.Save($png, [Drawing.Imaging.ImageFormat]::Png); $image.Dispose()
}

$cases = @(
  @{ Name = 'legacy';   Args = @('--legacy') },
  @{ Name = 'balanced'; Args = @() },
  @{ Name = 'parity';   Args = @('--redundancy', '0', '--parity', '40') },
  @{ Name = 'modern';   Args = @('--modern') }
)
foreach ($c in $cases) {
  Get-ChildItem $WorkDir -Filter "$($c.Name)-*.bmp" | ForEach-Object { $_.Delete() }
  & $pb encode $source --name pattern.bin -o (Join-Path $WorkDir "$($c.Name)-%d.bmp") --printer-dpi 300 --dot-dpi 200 `
    --compression none @($c.Args)
  if ($LASTEXITCODE -ne 0) { throw "encode $($c.Name) failed" }
  $first = Join-Path $WorkDir "$($c.Name)-1.bmp"
  if ($c.Name -eq 'legacy') {
    $lines = Join-Path $WorkDir 'legacy-lines-1.bmp'
    Copy-Item $first $lines -Force
    Wipe $lines 375 445
    Save-Png $lines (Join-Path $FixtureDir 'legacy-lines-1.png')
  }
  Wipe $first 381 441
  foreach ($page in Get-ChildItem $WorkDir -Filter "$($c.Name)-*.bmp") {
    Save-Png $page.FullName (Join-Path $FixtureDir ($page.BaseName + '.png'))
  }
}
Get-ChildItem $FixtureDir -Filter *.png | ForEach-Object { '{0} {1:N0} KB' -f $_.Name, ($_.Length / 1KB) }
