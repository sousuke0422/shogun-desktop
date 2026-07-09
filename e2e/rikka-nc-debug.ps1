# Debug: single close-button click with the app's stderr captured, to see
# which half of the NC click path breaks. Requires the RIKKA-DEBUG build.
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [int]$StartWaitSec = 6
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class E2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
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

$log = "$env:TEMP\shogun-tsf\rikka-nc-debug.log"
New-Item -ItemType Directory -Force -Path (Split-Path $log) | Out-Null
Remove-Item $log -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $ExePath -RedirectStandardError $log -PassThru
Start-Sleep -Seconds $StartWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited'; Get-Content $log; exit 1 }

$targetPid = $proc.Id
$script:hwnd = [IntPtr]::Zero
$cb = {
    param($h, $l)
    $wpid = 0
    [E2E]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
    if ($wpid -eq $targetPid -and [E2E]::IsWindowVisible($h)) {
        $sb = New-Object System.Text.StringBuilder 256
        [E2E]::GetWindowText($h, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:hwnd = $h; return $false }
    }
    return $true
}
[E2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
$hwnd = $script:hwnd
[E2E]::MoveWindow($hwnd, 200, 200, 1100, 700, $true) | Out-Null
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 800

$r = New-Object E2E+RECT
[E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null

# Hover the close button, then click it.
[E2E]::SetCursorPos($r.Right - 46, $r.Top + 38) | Out-Null
Start-Sleep -Milliseconds 500
[E2E]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 100
[E2E]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Seconds 3

if ($proc.HasExited) { Write-Output 'CLOSE-OK: process exited' } else { Write-Output 'CLOSE-FAIL: still alive' }
Get-Process -Name 'rikka-terminal' -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500
Write-Output '--- app stderr ---'
Get-Content $log -ErrorAction SilentlyContinue
Write-Output 'DEBUG-DONE'
