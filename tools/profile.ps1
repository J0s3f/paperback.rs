# Samples what the decoder spends its time on, with the Windows Performance Toolkit (xperf).
# Needs an elevated PowerShell (the kernel profiler is for administrators only).
#
#   powershell -File tools\profile.ps1 -OutDir C:\temp\profile -Exe target-prof\release\paperback-rs.exe `
#       -Pictures a.jpg,b.png [-IntervalUs 1000]
#
# Build the program with symbols first, in its own target directory:
#   $env:CARGO_PROFILE_RELEASE_DEBUG = 'true'; $env:CARGO_TARGET_DIR = 'target-prof'
#   cargo build --release -p paperback-rs-cli
# Every picture is decoded by a separate process, one after the other, so that the report tells the
# pictures apart by process id; the list of pictures and the time each took are in times.txt.
# Every step is bounded: one decode may take -TimeoutSec seconds.
param(
  [Parameter(Mandatory)][string]$OutDir,
  [Parameter(Mandatory)][string]$Exe,
  [Parameter(Mandatory)][string[]]$Pictures,
  [int]$IntervalUs = 1000,
  [int]$TimeoutSec = 900
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force $OutDir | Out-Null
$exe = (Resolve-Path $Exe).Path
$etl = Join-Path $OutDir 'decode.etl'
$times = Join-Path $OutDir 'times.txt'
Remove-Item $etl, $times, "$OutDir\done.txt" -ErrorAction SilentlyContinue
# The sampling interval is given in units of 100 ns.
# A session left over from an earlier run would block a new one; with none, xperf complains, which is fine.
cmd /c 'xperf -stop >nul 2>&1'
& xperf -on PROC_THREAD+LOADER+PROFILE -stackwalk Profile -SetProfInt ($IntervalUs * 10) -BufferSize 2048 -MinBuffers 256 -MaxBuffers 1024
try {
  foreach ($picture in $Pictures) {
    $started = Get-Date
    $process = Start-Process -FilePath $exe -ArgumentList @('decode', "`"$picture`"", '-o', "`"$OutDir\restored.bin`"") -PassThru -WindowStyle Hidden
    if (-not $process.WaitForExit($TimeoutSec * 1000)) { $process.Kill(); $note = 'timed out' } else { $note = "exit $($process.ExitCode)" }
    $seconds = [int]((Get-Date) - $started).TotalSeconds
    "$($process.Id)`t$seconds s`t$note`t$picture" | Add-Content $times
  }
} finally {
  & xperf -d $etl
}
$env:_NT_SYMBOL_PATH = Split-Path $exe
& xperf -i $etl -symbols -o (Join-Path $OutDir 'report.txt') -a profile -detail
'done' | Set-Content "$OutDir\done.txt"
