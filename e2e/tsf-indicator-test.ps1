# E2E: TSF taskbar IME indicator tracking (M1a) against a live build.
#
# Launches the exe with SHOGUN_TSF=1 (+ log), clicks into the terminal pane so
# the app-driven TSF focus fires, toggles the IME with the hankaku/zenkaku key,
# and screenshots the taskbar tray corner around each step. The script cannot
# judge the indicator glyph itself - a human (or vision model) compares the
# tray crops; the TSF log printed at the end tells whether TSF engaged the
# text store regardless.
#
# Same caveats as drag-copy-test.ps1: interactive desktop required, synthetic
# keys follow keyboard focus, keep this file ASCII-only.
#
# Usage (from WSL):
#   pwsh.exe -NoProfile -File 'C:\...\shogun-desktop\e2e\tsf-indicator-test.ps1'

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\shogun-desktop.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$ConnectWaitSec = 12
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
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
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$logPath = Join-Path $OutDir 'tsf.log'
Remove-Item -Path $logPath -ErrorAction SilentlyContinue

# Physical pixels everywhere: without this, coordinates on a scaled 4K display
# are DPI-virtualized and the tray crop lands on the wrong region entirely.
[E2E]::SetProcessDPIAware() | Out-Null

# Tray crop: bottom-right strip of the primary screen (clock + input-mode chip).
# The taskbar here is always visible, so no reveal dance; the cursor is left
# wherever the test put it so no tray tooltip pops over the chip.
$screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
function Save-Tray([string]$name) {
    $w = 520; $h = 60
    $x = $screen.Right - $w
    $y = $screen.Bottom - $h
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($x, $y, 0, 0, $bmp.Size)
    $g.Dispose()
    $p = Join-Path $OutDir $name
    $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "SHOT=$p"
}

# Launch with the TSF gate + diagnostics in the child environment.
$env:SHOGUN_TSF = '1'
$env:SHOGUN_TSF_LOG = $logPath
$proc = Start-Process -FilePath $ExePath -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $ConnectWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: process exited during startup'; exit 1 }

function Stop-App {
    $proc.CloseMainWindow() | Out-Null
    Start-Sleep -Seconds 2
    if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}

# Find the app window by PID (title-based FindWindow is unreliable).
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

Save-Tray 'shot0-app-fg-no-terminal-focus.png'

# Click into the terminal pane (left side, mid height) to focus terminal_focus.
$rect = New-Object E2E+RECT
[E2E]::GetWindowRect($script:hwnd, [ref]$rect) | Out-Null
$cx = $rect.Left + [int](($rect.Right - $rect.Left) * 0.08)
$cy = $rect.Top + [int](($rect.Bottom - $rect.Top) * 0.45)
Write-Output "CLICK ($cx,$cy)"
[E2E]::SetCursorPos($cx, $cy) | Out-Null
Start-Sleep -Milliseconds 250
[E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 80
[E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 600

Save-Tray 'shot1-terminal-focused.png'

# Keys follow keyboard focus - make sure we still own the foreground.
if ([E2E]::GetForegroundWindow() -ne $script:hwnd) {
    [E2E]::SetForegroundWindow($script:hwnd) | Out-Null
    Start-Sleep -Milliseconds 500
}
if ([E2E]::GetForegroundWindow() -ne $script:hwnd) {
    Write-Output 'FAIL: app lost foreground before IME toggle'
    Stop-App
    exit 1
}

# Hankaku/zenkaku toggle (VK 0xF4, scancode 0x29), twice with tray shots.
function Toggle-Ime {
    [E2E]::keybd_event(0xF4, 0x29, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
    [E2E]::keybd_event(0xF4, 0x29, 2, [UIntPtr]::Zero)
}
Toggle-Ime
Start-Sleep -Milliseconds 900
Save-Tray 'shot2-after-toggle1.png'
Toggle-Ime
Start-Sleep -Milliseconds 900
Save-Tray 'shot3-after-toggle2.png'

# Full-window screenshot for app-state sanity.
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
$g.Dispose()
$winShot = Join-Path $OutDir 'shot-window.png'
$bmp.Save($winShot, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$winShot"

Stop-App

Write-Output '=== TSF LOG ==='
if (Test-Path $logPath) { Get-Content $logPath } else { Write-Output '(no log written)' }
Write-Output '=== DONE ==='
exit 0
