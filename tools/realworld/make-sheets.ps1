# Makes the print sheets for real-world decode tests.
#
#   powershell -File tools\realworld\make-sheets.ps1 -Tool target\release\paperback-rs.exe
#
# Writes crates/paperback-rs/tests/fixtures/realworld/{data,sheets}. The data is synthetic (seeded
# pseudo-random bytes, no personal content) and is committed together with the PDFs, so the tests do not
# depend on this script. Print the PDFs at 100% / actual size (not "fit to page"), scan or photograph them
# and put the pictures into scans/ as <case>_<page>.<png|jpg|bmp|pdf>; see the README there.
param(
  [Parameter(Mandatory)][string]$Tool,
  [string]$FixtureDir = ''
)
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $FixtureDir) { $FixtureDir = Join-Path (Split-Path -Parent (Split-Path -Parent $here)) 'crates\paperback-rs\tests\fixtures\realworld' }
$data = Join-Path $FixtureDir 'data'; $sheets = Join-Path $FixtureDir 'sheets'
New-Item -ItemType Directory -Force $data, $sheets, (Join-Path $FixtureDir 'scans') | Out-Null
$fixedTime = [DateTime]::SpecifyKind([DateTime]'2024-01-02 03:04:05', [DateTimeKind]::Utc)

# name, bytes, seed, extra encode options
$cases = @(
  @{ Name = 'a-laser-600-default';  Bytes = 120000; Seed = 1; Options = @('--printer-dpi', '600', '--dot-dpi', '200') },
  @{ Name = 'b-laser-600-dense';    Bytes = 250000; Seed = 2; Options = @('--printer-dpi', '600', '--dot-dpi', '300') },
  @{ Name = 'c-inkjet-300';         Bytes = 30000;  Seed = 3; Options = @('--printer-dpi', '300', '--dot-dpi', '100') },
  @{ Name = 'd-robust-r2-frame';    Bytes = 60000;  Seed = 4; Options = @('--printer-dpi', '600', '--dot-dpi', '150', '--redundancy', '2', '--frame') },
  @{ Name = 'e-two-pages';          Bytes = 300000; Seed = 5; Options = @('--printer-dpi', '600', '--dot-dpi', '200') },
  @{ Name = 'f-encrypted';          Bytes = 20000;  Seed = 6; Options = @('--printer-dpi', '600', '--dot-dpi', '200', '--password-env', 'PB_REALWORLD_PW') },
  @{ Name = 'g-letter-small-dots';  Bytes = 80000;  Seed = 7; Options = @('--printer-dpi', '600', '--dot-dpi', '200', '--paper', 'letter', '--margins-mm', '15', '15', '15', '15', '--dot-size', '60', '--redundancy', '8') }
)
$env:PB_REALWORLD_PW = 'paperback-rs-test'
foreach ($c in $cases) {
  $file = Join-Path $data "$($c.Name).bin"
  $bytes = New-Object byte[] $c.Bytes
  (New-Object Random $c.Seed).NextBytes($bytes)
  [IO.File]::WriteAllBytes($file, $bytes)
  (Get-Item $file).LastWriteTimeUtc = $fixedTime
  $pdf = Join-Path $sheets "$($c.Name).pdf"
  & $Tool encode $file -o $pdf --compression none --name $c.Name @($c.Options)
  if ($LASTEXITCODE -ne 0) { throw "encode failed for $($c.Name)" }
  "{0,-24} {1,8} bytes -> {2}" -f $c.Name, $c.Bytes, (Split-Path -Leaf $pdf)
}
