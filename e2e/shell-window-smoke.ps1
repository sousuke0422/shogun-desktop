# E2E probe: launch `shogun-desktop.exe --shell-window` (the lean single-shell
# surface added for pane testing), wait for the SSH connect, and screenshot the
# initial state. Proves the --shell-window path renders (migrated pane_overlay)
# without hunting UI buttons in the full app. Reports whether the shell reached
# a live prompt so the caller can decide whether a drag-select run is viable.
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\shogun-desktop.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$ConnectWaitSec = 14
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class E2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$proc = Start-Process -FilePath $ExePath -ArgumentList '--shell-window' -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $ConnectWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited'; exit 1 }

$targetPid = $proc.Id
$script:hwnd = [IntPtr]::Zero
$script:title = ''
$cb = {
    param($h, $l)
    $wpid = 0
    [E2E]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
    if ($wpid -eq $targetPid -and [E2E]::IsWindowVisible($h)) {
        $sb = New-Object System.Text.StringBuilder 256
        [E2E]::GetWindowText($h, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:hwnd = $h; $script:title = $sb.ToString(); return $false }
    }
    return $true
}
[E2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
$hwnd = $script:hwnd
Write-Output "TITLE=$($script:title)"
[E2E]::MoveWindow($hwnd, 200, 200, 1100, 700, $true) | Out-Null
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 1200

$r = New-Object E2E+RECT
[E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$bmp = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$shot = Join-Path $OutDir 'shell-window-initial.png'
$bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$shot"

Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output 'PROBE-DONE'
