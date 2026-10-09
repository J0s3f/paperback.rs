# Drives the original PaperBack GUI (1.00 or 1.10) through its file dialogs: either writes a file
# to bitmap pages (-Action save) or reads bitmap pages back into a file (-Action open).
# Windows only. Never prints or scans. The whole run is bounded by -TimeoutSec and the application
# is killed when it overruns.
param(
  [Parameter(Mandatory)][string]$Exe,         # copy of PaperBak.exe in its own working directory
  [Parameter(Mandatory)][ValidateSet('save','open')][string]$Action,
  [Parameter(Mandatory)][string]$InputList,   # save: file to back up; open: bitmaps to read, joined with |
  [Parameter(Mandatory)][string]$OutputPath,  # save: bitmap base path; open: restored file path
  [string]$Password = '',
  [int]$Compression = 2,                      # 0 none, 1 fast, 2 maximal
  [int]$Redundancy = 5,
  [int]$Raster = 200,
  [int]$TimeoutSec = 120
)
Add-Type -AssemblyName System.Windows.Forms
$Inputs = $InputList -split '\|'
if ($Password -eq '__none__') { $Password = '' }
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class W {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr h, EnumProc p, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern IntPtr GetDlgItem(IntPtr h, int id);
  [DllImport("user32.dll", CharSet=CharSet.Auto)] public static extern IntPtr SendMessage(IntPtr h, int m, IntPtr w, string l);
  [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr h, int m, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, int m, IntPtr w, IntPtr l);
  public static List<IntPtr> TopWindows(uint pid) {
    var r = new List<IntPtr>();
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); if (p == pid && IsWindowVisible(h)) r.Add(h); return true; }, IntPtr.Zero);
    return r;
  }
  public static string Cls(IntPtr h) { var s = new StringBuilder(256); GetClassName(h, s, 256); return s.ToString(); }
  public static string Txt(IntPtr h) { var s = new StringBuilder(1024); GetWindowText(h, s, 1024); return s.ToString(); }
  public static List<string> Texts(IntPtr h) {
    var r = new List<string>();
    EnumChildWindows(h, (c, l) => { var t = Txt(c); if (t.Length > 0) r.Add(Cls(c) + ":" + t); return true; }, IntPtr.Zero);
    return r;
  }
}
'@

$WM_SETTEXT = 0x000C; $WM_COMMAND = 0x0111; $WM_CLOSE = 0x0010; $BM_CLICK = 0x00F5
$M_FILE_OPEN = 1001; $M_FILE_SAVEBMP = 1002
$workDir = Split-Path $Exe
$encrypt = if ($Password -ne '' -and $Action -eq 'save') { 1 } else { 0 }
@"
[Settings]
Raster=$Raster
Dot size=70
Compression=$Compression
Redundancy=$Redundancy
Header and footer=0
Border=0
Autosave=1
Best quality=1
Encryption=$encrypt
Open password=1
"@ | Set-Content (Join-Path $workDir 'PaperBak.ini') -Encoding ascii

$proc = Start-Process -FilePath $Exe -WorkingDirectory $workDir -PassThru
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$watchdog = Start-Job -ScriptBlock { param($id, $sec) Start-Sleep -Seconds $sec; Stop-Process -Id $id -Force -ErrorAction SilentlyContinue } -ArgumentList $proc.Id, ($TimeoutSec + 15)
$mainWnd = [IntPtr]::Zero
while ($mainWnd -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) {
  Start-Sleep -Milliseconds 200
  foreach ($h in [W]::TopWindows([uint32]$proc.Id)) { if ([W]::Cls($h) -eq 'P2DMAIN') { $mainWnd = $h } }
}
if ($mainWnd -eq [IntPtr]::Zero) { if (-not $proc.HasExited) { $proc.Kill() }; throw 'main window did not appear' }

$openIndex = 0            # next bitmap to hand to an "Open bitmap" dialog
$pendingMore = $false     # a bitmap was opened; start the next one once the program is idle again
$lastCpu = [TimeSpan]::Zero; $idleSince = $null
$handled = @{}

function Start-Command { [void][W]::PostMessage($mainWnd, $WM_COMMAND, [IntPtr]$(if ($Action -eq 'save') { $M_FILE_SAVEBMP } else { $M_FILE_OPEN }), [IntPtr]::Zero) }
function Click-Button($dlg, $id) { [void][W]::SendMessage([W]::GetDlgItem($dlg, $id), $BM_CLICK, [IntPtr]::Zero, [IntPtr]::Zero) }
# Pastes the path through the clipboard (much faster than typing it key by key) and presses Enter.
# The previous clipboard text is put back afterwards. The clipboard is shared with everything else on the
# desktop and can be locked at any moment, so a few retries are made and typing is the fallback.
function Send-Path($dlg, $path) {
  [void][W]::SetForegroundWindow($dlg)
  Start-Sleep -Milliseconds 400
  $previous = $null
  try { if ([System.Windows.Forms.Clipboard]::ContainsText()) { $previous = [System.Windows.Forms.Clipboard]::GetText() } } catch { }
  $pasted = $false
  for ($attempt = 0; $attempt -lt 5 -and -not $pasted; $attempt++) {
    try { [System.Windows.Forms.Clipboard]::SetDataObject($path, $true); $pasted = $true } catch { Start-Sleep -Milliseconds 150 }
  }
  if ($pasted) {
    Start-Sleep -Milliseconds 150
    [System.Windows.Forms.SendKeys]::SendWait('^a^v{ENTER}')
  } else {
    $escaped = $path -replace '([+^%~(){}\[\]])', '{$1}'
    [System.Windows.Forms.SendKeys]::SendWait('^a' + $escaped + '{ENTER}')
  }
  Write-Host "  file dialog -> $path"
  Start-Sleep -Milliseconds 150
  if ($pasted) { try { if ($previous -ne $null) { [System.Windows.Forms.Clipboard]::SetDataObject($previous, $true) } else { [System.Windows.Forms.Clipboard]::Clear() } } catch { } }
}
function Handle-Dialog($dlg) {
  $texts = [W]::Texts($dlg) -join ' | '
  $title = [W]::Txt($dlg)
  if ($texts -match 'SHELLDLL_DefView') {
    Write-Host "dialog '$title'"
    switch -Regex ($title) {
      '^Open bitmap'        { Send-Path $dlg $Inputs[$script:openIndex]; $script:openIndex++; $script:pendingMore = ($script:openIndex -lt $Inputs.Count) }
      '^Open file to print' { Send-Path $dlg $Inputs[0] }
      default               { Send-Path $dlg $OutputPath }
    }
  } elseif ([W]::GetDlgItem($dlg, 3202) -ne [IntPtr]::Zero) {
    Write-Host "dialog '$title': password"
    [void][W]::SendMessage([W]::GetDlgItem($dlg, 3202), $WM_SETTEXT, [IntPtr]::Zero, $Password)
    $confirm = [W]::GetDlgItem($dlg, 3203)
    if ($confirm -ne [IntPtr]::Zero) { [void][W]::SendMessage($confirm, $WM_SETTEXT, [IntPtr]::Zero, $Password) }
    Click-Button $dlg 1
  } else {
    Write-Host "dialog '$title': $texts"
    if ([W]::GetDlgItem($dlg, 6) -ne [IntPtr]::Zero) { Click-Button $dlg 6 } else { Click-Button $dlg 1 }
  }
}

Start-Command
$lastSize = -1; $stableSince = $null; $files = @()
$pattern = if ($Action -eq 'save') { [IO.Path]::GetFileNameWithoutExtension($OutputPath) + '*' + [IO.Path]::GetExtension($OutputPath) } else { [IO.Path]::GetFileName($OutputPath) }
try {
  while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 300
    if ($proc.HasExited) { break }
    foreach ($h in [W]::TopWindows([uint32]$proc.Id)) {
      if ([W]::Cls($h) -eq '#32770' -and -not $handled.ContainsKey($h.ToInt64())) {
        $handled[$h.ToInt64()] = $true
        Handle-Dialog $h
      }
    }
    # Only one password is ever offered, so this message is final; waiting for the timeout would only waste time.
    if ($Action -eq 'open' -and (([W]::Texts($mainWnd) -join ' ') -match 'Invalid password')) {
      Write-Host 'original says: Invalid password'
      break
    }
    if ($pendingMore) {
      $proc.Refresh(); $cpu = $proc.TotalProcessorTime
      if ($cpu -eq $lastCpu) { if (-not $idleSince) { $idleSince = Get-Date } elseif (((Get-Date) - $idleSince).TotalSeconds -ge 2) { $pendingMore = $false; $idleSince = $null; Start-Command } }
      else { $idleSince = $null; $lastCpu = $cpu }
    }
    $files = @(Get-ChildItem -Path (Split-Path $OutputPath) -Filter $pattern -ErrorAction SilentlyContinue)
    $size = ($files | Measure-Object Length -Sum).Sum
    if ($files.Count -gt 0 -and $size -gt 0 -and -not $pendingMore) {
      if ($size -eq $lastSize) { if (-not $stableSince) { $stableSince = Get-Date } elseif (((Get-Date) - $stableSince).TotalSeconds -ge 3) { break } }
      else { $stableSince = $null; $lastSize = $size }
    }
  }
} finally {
  Write-Host ("result files: " + (($files | ForEach-Object { "$($_.Name) ($($_.Length))" }) -join ', '))
  if (-not $proc.HasExited) {
    [void][W]::PostMessage($mainWnd, $WM_CLOSE, [IntPtr]::Zero, [IntPtr]::Zero)
    if (-not $proc.WaitForExit(5000)) { $proc.Kill() }
  }
  Stop-Job $watchdog -ErrorAction SilentlyContinue; Remove-Job $watchdog -Force -ErrorAction SilentlyContinue
}
if ($files.Count -eq 0) { exit 2 }






