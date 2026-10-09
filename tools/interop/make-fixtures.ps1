# Regenerates crates/paperback-rs/tests/fixtures/original/* with the original PaperBack executables.
#
#   powershell -File tools\interop\make-fixtures.ps1 `
#       -Exe100 C:\path\v1.00\PaperBak.exe -Exe110 C:\path\v1.10\PaperBak.exe -WorkDir C:\temp\pbfix
#
# The input files are deterministic and neutral (see tests/original_pages.rs, which regenerates the
# same bytes), carry a fixed modification time and generic names, so the pages contain no personal
# data: the original stores only the file name, size, time and the data itself. Pages are kept as the
# original wrote them (BMP) for the first case and as lossless PNG for the others to save space.
# Windows only. Never prints or scans.
param(
  [Parameter(Mandatory)][string]$Exe100,
  [Parameter(Mandatory)][string]$Exe110,
  [Parameter(Mandatory)][string]$WorkDir,
  [string]$FixtureDir = ''
)
Add-Type -AssemblyName System.Drawing
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
if (-not $FixtureDir) { $FixtureDir = Join-Path (Split-Path -Parent (Split-Path -Parent $here)) 'crates\paperback-rs\tests\fixtures\original' }
$fixedTime = [DateTime]::SpecifyKind([DateTime]'2024-01-02 03:04:05', [DateTimeKind]::Utc)
New-Item -ItemType Directory -Force $WorkDir | Out-Null

# Inputs: keep the formulas in sync with tests/original_pages.rs.
$sample = Join-Path $WorkDir 'sample.txt'
[IO.File]::WriteAllText($sample, ("Lorem ipsum paper test line`r`n" * 400), (New-Object Text.ASCIIEncoding))
$pattern = Join-Path $WorkDir 'pattern.bin'
$bytes = New-Object byte[] 120000
for ($i = 0; $i -lt $bytes.Length; $i++) { $bytes[$i] = [byte](($i * 131 + [math]::Floor($i / 3)) % 256) }
[IO.File]::WriteAllBytes($pattern, $bytes)
foreach ($f in $sample, $pattern) { (Get-Item $f).LastWriteTimeUtc = $fixedTime; (Get-Item $f).CreationTimeUtc = $fixedTime }

$cases = @(
  @{ Name = 'text';        Input = $sample;  Compression = 2; Password = '';              AsBmp = $true  },
  @{ Name = 'pattern';     Input = $pattern; Compression = 0; Password = '';              AsBmp = $false },
  @{ Name = 'text-crypt';  Input = $sample;  Compression = 2; Password = 'correct horse'; AsBmp = $false },
  @{ Name = 'text-umlaut'; Input = $sample;  Compression = 2; Password = "p$([char]0xE4)ssw$([char]0xF6)rt";      AsBmp = $false }
)

foreach ($version in @(@{ Label = 'v100'; Exe = $Exe100 }, @{ Label = 'v110'; Exe = $Exe110 })) {
  $run = Join-Path $WorkDir $version.Label
  $out = Join-Path $run 'out'
  New-Item -ItemType Directory -Force $out | Out-Null
  Copy-Item $version.Exe (Join-Path $run 'PaperBak.exe') -Force
  $target = Join-Path $FixtureDir $version.Label
  New-Item -ItemType Directory -Force $target | Out-Null
  foreach ($c in $cases) {
    Get-ChildItem $out -Filter "$($c.Name)*" -ErrorAction SilentlyContinue | ForEach-Object { $_.Delete() }
    Get-Process PaperBak -ErrorAction SilentlyContinue | Stop-Process -Force
    $pass = if ($c.Password) { $c.Password } else { '__none__' }
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $here 'drive-original.ps1') `
      -Exe (Join-Path $run 'PaperBak.exe') -Action save -InputList $c.Input -OutputPath (Join-Path $out "$($c.Name).bmp") `
      -Compression $c.Compression -Password $pass -TimeoutSec 120 | Out-Null
    $pages = @(Get-ChildItem $out -Filter "$($c.Name)*.bmp" | Sort-Object Name)
    if ($pages.Count -eq 0) { throw "no pages for $($version.Label) $($c.Name)" }
    $number = 0
    foreach ($page in $pages) {
      $number++
      $stem = "$($c.Name)-$number"
      if ($c.AsBmp) {
        Copy-Item $page.FullName (Join-Path $target "$stem.bmp") -Force
      } else {
        $bitmap = New-Object System.Drawing.Bitmap $page.FullName
        $bitmap.Save((Join-Path $target "$stem.png"), [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
      }
    }
    Write-Host "$($version.Label) $($c.Name): $($pages.Count) page(s)"
  }
}


