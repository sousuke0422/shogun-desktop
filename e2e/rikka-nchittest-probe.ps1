# Probe: ask the window directly what WM_NCHITTEST returns over the caption
# buttons / drag filler / a tab. Separates "hit test broken" from "NC click
# handling broken". Keep this file ASCII-only.

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
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] public static extern IntPtr SendMessageW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null

$proc = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds $StartWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited'; exit 1 }
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
Write-Output "rect: $($r.Left),$($r.Top) - $($r.Right),$($r.Bottom)"

function Probe([string]$name, [int]$x, [int]$y) {
    # Park the cursor there first so gpui's mouse_hit_test is fresh.
    [E2E]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 500
    $lp = [IntPtr](($y -shl 16) -bor ($x -band 0xFFFF))
    $hit = [E2E]::SendMessageW($hwnd, 0x0084, [UIntPtr]::Zero, $lp)
    Write-Output "$name at ($x,$y): hit=$($hit.ToInt64())"
}

# HT codes: 1=CLIENT 2=CAPTION 8=MINBUTTON 9=MAXBUTTON 20=CLOSE 12=TOP
$midY = $r.Top + 38
Probe 'close ' ($r.Right - 46) $midY
Probe 'max   ' ($r.Right - 138) $midY
Probe 'min   ' ($r.Right - 230) $midY
Probe 'filler' ($r.Right - 420) $midY
Probe 'tab   ' ($r.Left + 150) $midY
Probe 'pane  ' ($r.Left + 400) ($r.Top + 300)

Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
Write-Output 'PROBE-DONE'
