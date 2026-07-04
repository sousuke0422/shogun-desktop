# E2E: mouse selection + ctrl-shift-c copy against a live shogun-desktop build.
#
# Launches the exe, drags across the terminal pane with synthetic mouse input,
# screenshots the window (for highlight verification via scan-highlight.ps1),
# presses ctrl+shift+c, and PASSes iff the clipboard changed from the sentinel
# to non-empty text.
#
# Requirements / caveats:
#   - Must run on the interactive desktop (synthetic input drives the real
#     cursor); the app window will pop up and steal the pointer briefly.
#   - Synthetic MOUSE events route by cursor position, but synthetic KEYS route
#     by keyboard focus. The script verifies GetForegroundWindow before sending
#     keys and fails loudly if focus went elsewhere.
#   - The pane content comes from the configured SSH/tmux session, so the test
#     needs a working connection (same as normal app usage).
#   - Keep this file ASCII-only, or save as UTF-8 WITH BOM: Windows PowerShell
#     5.1 parses BOM-less .ps1 as ANSI and multibyte literals break the parser.
#
# Usage (from WSL; repo path abbreviated):
#   pwsh.exe -NoProfile -File 'C:\...\shogun-desktop\e2e\drag-copy-test.ps1' \
#     [-ExePath <path>] [-ScreenshotPath <path>]

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\shogun-desktop.exe",
    [string]$ScreenshotPath = "$env:TEMP\shogun-e2e-sel.png",
    # Seconds to wait for the SSH/tmux session to connect and render.
    [int]$ConnectWaitSec = 12
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Text;
using System.Runtime.InteropServices;
public class E2E {
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")]
    public static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)]
    public static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);
    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);
    [DllImport("user32.dll")]
    public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
    [DllImport("user32.dll")]
    public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@

$sentinel = "E2E-SENTINEL-$(Get-Random)"
Set-Clipboard -Value $sentinel

$proc = Start-Process -FilePath $ExePath -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $ConnectWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: process exited during startup'; exit 1 }

function Stop-App { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }

# Find the app window by PID: FindWindow by (Japanese) title is unreliable, so
# enumerate top-level windows and take the first visible titled one.
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
if ($script:hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: window not found'; Stop-App; exit 1 }
[E2E]::SetForegroundWindow($script:hwnd) | Out-Null
Start-Sleep -Milliseconds 800

$rect = New-Object E2E+RECT
[E2E]::GetWindowRect($script:hwnd, [ref]$rect) | Out-Null

# Drag across the terminal area: 8%,35% -> 90%,55% of the window.
# Wide on purpose: rows strictly between the endpoints are selected
# full-width, so any text row inside the band yields non-empty copy text.
# (A narrow band once landed entirely on blank cells: the highlight showed
# but the trimmed selection text was empty, so nothing reached the
# clipboard and the run false-FAILed.)
$x1 = $rect.Left + [int](($rect.Right - $rect.Left) * 0.08)
$y1 = $rect.Top  + [int](($rect.Bottom - $rect.Top) * 0.35)
$x2 = $rect.Left + [int](($rect.Right - $rect.Left) * 0.90)
$y2 = $rect.Top  + [int](($rect.Bottom - $rect.Top) * 0.55)
Write-Output "DRAG ($x1,$y1) -> ($x2,$y2)"

[E2E]::SetCursorPos($x1, $y1) | Out-Null
Start-Sleep -Milliseconds 300
[E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)  # LEFTDOWN
Start-Sleep -Milliseconds 120
for ($i = 1; $i -le 10; $i++) {
    $mx = $x1 + [int](($x2 - $x1) * $i / 10)
    $my = $y1 + [int](($y2 - $y1) * $i / 10)
    [E2E]::SetCursorPos($mx, $my) | Out-Null
    Start-Sleep -Milliseconds 40
}
[E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)  # LEFTUP
Start-Sleep -Milliseconds 400

# Screenshot while the highlight is on screen (verify with scan-highlight.ps1).
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
$g.Dispose()
$bmp.Save($ScreenshotPath, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SCREENSHOT=$ScreenshotPath"

# Keys go to the foreground window; bail if the click didn't focus the app.
# The user's active window (e.g. the terminal driving this script) can win the
# focus race after the synthetic click, so re-assert foreground once before
# giving up, and name the thief in the failure message.
if ([E2E]::GetForegroundWindow() -ne $script:hwnd) {
    [E2E]::SetForegroundWindow($script:hwnd) | Out-Null
    Start-Sleep -Milliseconds 500
}
if ([E2E]::GetForegroundWindow() -ne $script:hwnd) {
    $fg = [E2E]::GetForegroundWindow()
    $sb = New-Object System.Text.StringBuilder 256
    [E2E]::GetWindowText($fg, $sb, 256) | Out-Null
    Write-Output "FAIL: app lost foreground before keystrokes (foreground='$($sb.ToString())')"
    Stop-App
    exit 1
}

# ctrl+shift+c
[E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
[E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
[E2E]::keybd_event(0x43, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 100
[E2E]::keybd_event(0x43, 0, 2, [UIntPtr]::Zero)
[E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
[E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 1000

$clip = Get-Clipboard -Raw -ErrorAction SilentlyContinue
$viaCsc = -not ([string]::IsNullOrWhiteSpace($clip) -or $clip -eq $sentinel)

# Fallback: ctrl+insert (also bound to copy). A resident app can hold
# ctrl+shift+c as a RegisterHotKey global hotkey, in which case the app
# never receives the key at all (observed 2026-07-04).
if (-not $viaCsc) {
    [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x2D, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 100
    [E2E]::keybd_event(0x2D, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 1000
    $clip = Get-Clipboard -Raw -ErrorAction SilentlyContinue
}
Write-Output "CLIPBOARD>>>$clip<<<END"
Stop-App

if ([string]::IsNullOrWhiteSpace($clip) -or $clip -eq $sentinel) {
    Write-Output 'FAIL: clipboard did not receive the selection'
    exit 1
}
if (-not $viaCsc) {
    Write-Output 'WARN: ctrl+shift+c was swallowed system-wide (global hotkey?); copy worked via ctrl+insert'
}
Write-Output 'PASS'
exit 0
