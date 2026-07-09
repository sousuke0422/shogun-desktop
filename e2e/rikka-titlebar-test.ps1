# E2E: RikkaTerminal titlebar-integrated tabs — the strip IS the titlebar.
# Verifies the native behaviors bought by window_control_area hitboxes:
#   1. dragging the empty strip moves the window (HTCAPTION)
#   2. caption min button click minimizes (HTMINBUTTON, native handling)
#   3. maximize via WM_SYSCOMMAND keeps the strip visible (NCCALCSIZE insets)
#   4. caption close button click closes the window AND exits the process
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$StartWaitSec = 6
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class E2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Find-Window([int]$targetPid) {
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
    return $script:hwnd
}

function Chord-CS([byte]$vk) {
    [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 250
}

function Get-Rect([IntPtr]$h) {
    $r = New-Object E2E+RECT
    [E2E]::GetWindowRect($h, [ref]$r) | Out-Null
    return $r
}

function Shot([IntPtr]$h, [string]$name) {
    $r = Get-Rect $h
    $w = $r.Right - $r.Left
    $ht = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $ht)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $p = Join-Path $OutDir $name
    $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "SHOT=$p"
}

function Click([int]$x, [int]$y) {
    [E2E]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 150
    [E2E]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)   # LEFTDOWN
    Start-Sleep -Milliseconds 80
    [E2E]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)   # LEFTUP
    Start-Sleep -Milliseconds 200
}

$proc = Start-Process -FilePath $ExePath -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $StartWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited during startup'; exit 1 }
$hwnd = Find-Window $proc.Id
if ($hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: window not found'; exit 1 }
[E2E]::MoveWindow($hwnd, 200, 200, 1100, 700, $true) | Out-Null
Start-Sleep -Milliseconds 400
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 600

# Physical px from the window's ACTUAL DPI (the window may land on any
# monitor; 200%-hardcoded coordinates miss buttons on a 150% display).
$scale = [E2E]::GetDpiForWindow($hwnd) / 96.0
Write-Output ("scale={0}" -f $scale)
$stripMidY = [int](19 * $scale)   # middle of the 40-logical strip
$btnW = [int](46 * $scale)        # caption button width

# -- 1. drag the empty strip (single tab -> plenty of filler) ---------------
$r0 = Get-Rect $hwnd
$dragX = $r0.Right - 3 * $btnW - 120   # well left of the buttons, in filler
$dragY = $r0.Top + $stripMidY
[E2E]::SetCursorPos($dragX, $dragY) | Out-Null
Start-Sleep -Milliseconds 200
[E2E]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 150
for ($i = 1; $i -le 10; $i++) {
    [E2E]::SetCursorPos($dragX + $i * 15, $dragY + $i * 8) | Out-Null
    Start-Sleep -Milliseconds 30
}
Start-Sleep -Milliseconds 150
[E2E]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 400
$r1 = Get-Rect $hwnd
$dx = $r1.Left - $r0.Left
$dy = $r1.Top - $r0.Top
Write-Output "drag delta: dx=$dx dy=$dy"
if ($dx -lt 100 -or $dy -lt 40) { Write-Output 'FAIL: strip drag did not move the window' } else { Write-Output 'DRAG-OK' }

# -- 2. grow to 3 tabs, screenshot the integrated titlebar ------------------
Chord-CS 0x54; Chord-CS 0x54
Start-Sleep -Seconds 2
Shot $hwnd 'tb0-normal.png'

# hover the close button (should show the red hover, window must NOT close)
$r = Get-Rect $hwnd
[E2E]::SetCursorPos($r.Right - [int]($btnW / 2), $r.Top + $stripMidY) | Out-Null
Start-Sleep -Milliseconds 600
Shot $hwnd 'tb1-close-hover.png'

# -- 3. maximize (native SC_MAXIMIZE) — strip must stay visible -------------
[E2E]::PostMessageW($hwnd, 0x0112, [UIntPtr]0xF030, [IntPtr]::Zero) | Out-Null
Start-Sleep -Milliseconds 900
Shot $hwnd 'tb2-maximized.png'
[E2E]::PostMessageW($hwnd, 0x0112, [UIntPtr]0xF120, [IntPtr]::Zero) | Out-Null   # SC_RESTORE
Start-Sleep -Milliseconds 600

# -- 4. min button click (native HTMINBUTTON handling) ----------------------
$r = Get-Rect $hwnd
Click ($r.Right - [int](2.5 * $btnW)) ($r.Top + $stripMidY)
Start-Sleep -Milliseconds 700
if ([E2E]::IsIconic($hwnd)) { Write-Output 'MIN-OK' } else { Write-Output 'FAIL: min button did not minimize' }
[E2E]::PostMessageW($hwnd, 0x0112, [UIntPtr]0xF120, [IntPtr]::Zero) | Out-Null
Start-Sleep -Milliseconds 800
if ([E2E]::IsIconic($hwnd)) { Write-Output 'FAIL: window did not restore' }

# -- 5. close button click -> window closes AND process exits ---------------
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 300
$r = Get-Rect $hwnd
Click ($r.Right - [int]($btnW / 2)) ($r.Top + $stripMidY)
$deadline = (Get-Date).AddSeconds(6)
while (-not $proc.HasExited -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
if ($proc.HasExited) { Write-Output 'CLOSE-OK: process exited' } else { Write-Output 'FAIL: process still alive after close click' }

$left = Get-Process -Name 'rikka-terminal' -ErrorAction SilentlyContinue
if ($left) { $left | Stop-Process -Force; Write-Output 'note: leftover processes killed' }
Write-Output 'RUN-DONE'
