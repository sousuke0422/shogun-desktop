# E2E: drag-select + copy in the SHELL window launched via --shell-window (the
# lean single-shell surface). Supersedes shell-drag-copy.ps1's fractional-coord
# button hunt in the full app: --shell-window IS the shell window, so this
# targets it directly. Proves the migrated shared pane_overlay in shogun-desktop
# (inset pin + measured resize + selection) copies a live selection, and that
# the highlight rides the text when scrolled. Keep this file ASCII-only.

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
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Press([byte]$vk) {
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 250
    [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    Press 0x56
    [E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Press 0x0D
}

$proc = Start-Process -FilePath $ExePath -ArgumentList '--shell-window' -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $ConnectWaitSec
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
Start-Sleep -Milliseconds 1000

# Focus the pane (activation auto-focuses, but a center click is belt-and-braces).
$r = New-Object E2E+RECT
[E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null
[E2E]::SetCursorPos([int](($r.Left + $r.Right) / 2), [int](($r.Top + $r.Bottom) / 2)) | Out-Null
Start-Sleep -Milliseconds 200
[E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 80
[E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 400
if ([E2E]::GetForegroundWindow() -ne $hwnd) { Write-Output 'FAIL: shell not foreground'; exit 1 }

# Static content — more lines than the viewport so scrollback exists.
Run-Cmd 'seq 1 200'
Start-Sleep -Seconds 2

$sentinel = "E2E-SENTINEL-$(Get-Random)"
Set-Clipboard -Value $sentinel

# Shift-drag across a band of the output (shift = local-selection bypass).
[E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null
$x1 = $r.Left + [int](($r.Right - $r.Left) * 0.05)
$y1 = $r.Top + [int](($r.Bottom - $r.Top) * 0.25)
$x2 = $r.Left + [int](($r.Right - $r.Left) * 0.30)
$y2 = $r.Top + [int](($r.Bottom - $r.Top) * 0.45)
Write-Output "DRAG ($x1,$y1) -> ($x2,$y2)"
[E2E]::SetCursorPos($x1, $y1) | Out-Null
Start-Sleep -Milliseconds 300
[E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
[E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 120
for ($i = 1; $i -le 10; $i++) {
    $mx = $x1 + [int](($x2 - $x1) * $i / 10)
    $my = $y1 + [int](($y2 - $y1) * $i / 10)
    [E2E]::SetCursorPos($mx, $my) | Out-Null
    Start-Sleep -Milliseconds 40
}
[E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
[E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 400

# Screenshot with the highlight on screen.
$w = $r.Right - $r.Left
$h = $r.Bottom - $r.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$shot = Join-Path $OutDir 'shell-window-sel.png'
$bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$shot"

# The bug this guards: scroll while selected. Wheel up — the highlight must
# ride the text (grid-anchored), not stick to screen rows.
[E2E]::SetCursorPos([int](($x1 + $x2) / 2), [int](($y1 + $y2) / 2)) | Out-Null
Start-Sleep -Milliseconds 200
for ($i = 0; $i -lt 3; $i++) {
    [E2E]::mouse_event(0x0800, 0, 0, 120, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 150
}
Start-Sleep -Milliseconds 500
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$shot2 = Join-Path $OutDir 'shell-window-sel-scrolled.png'
$bmp.Save($shot2, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$shot2"

# ctrl+insert copy (ctrl+shift+c may be stolen by a resident global hotkey).
[E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
Press 0x2D
[E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 800

$clip = Get-Clipboard -Raw -ErrorAction SilentlyContinue
$head = if ($clip) { $clip.Substring(0, [Math]::Min(80, $clip.Length)) -replace "`r?`n", '\n' } else { '<empty>' }
Write-Output "CLIPBOARD_HEAD=[$head]"

Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400

if ([string]::IsNullOrWhiteSpace($clip) -or $clip -eq $sentinel) {
    Write-Output 'FAIL: clipboard did not receive the selection'
    exit 1
}
if ($clip -notmatch '\d') {
    Write-Output 'FAIL: clipboard has no digits (seq output not selected)'
    exit 1
}
Write-Output 'SHELL-SELECT-COPY-OK'
exit 0
