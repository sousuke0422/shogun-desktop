# E2E: drag-select + copy in the SHELL window (quiet pane, no TUI redraws).
# The main-window variant (drag-copy-test.ps1) targets a live tmux pane whose
# TUI repaints constantly; grid-truthful selections (alacritty Selection)
# clear when the text under them is rewritten, so that pane can only prove
# the reporting path. This one proves select -> highlight survives -> copy:
# prints static seq output, shift-drags across it, ctrl+insert, checks the
# clipboard. Keep this file ASCII-only.

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
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

function Get-AppWindows {
    $procs = Get-Process -Name 'shogun-desktop' -ErrorAction SilentlyContinue
    if (-not $procs) { return @() }
    $pids = @($procs | ForEach-Object { $_.Id })
    $found = New-Object System.Collections.ArrayList
    $cb = {
        param($h, $l)
        $wpid = 0
        [E2E]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
        if ($pids -contains $wpid -and [E2E]::IsWindowVisible($h)) {
            $sb = New-Object System.Text.StringBuilder 256
            [E2E]::GetWindowText($h, $sb, 256) | Out-Null
            if ($sb.Length -gt 0) {
                [void]$found.Add([pscustomobject]@{ Hwnd = $h; Title = $sb.ToString() })
            }
        }
        return $true
    }
    [E2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return @($found)
}

function Click-Frac([IntPtr]$hwnd, [double]$fx, [double]$fy) {
    $rect = New-Object E2E+RECT
    [E2E]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $x = $rect.Left + [int](($rect.Right - $rect.Left) * $fx)
    $y = $rect.Top + [int](($rect.Bottom - $rect.Top) * $fy)
    [E2E]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 250
    [E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
    [E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

function Press([byte]$vk) {
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 200
    [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    Press 0x56
    [E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Press 0x0D
}

$proc = Start-Process -FilePath $ExePath -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $ConnectWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited'; exit 1 }
$wins = Get-AppWindows
$main = $wins[0].Hwnd
[E2E]::SetForegroundWindow($main) | Out-Null
Start-Sleep -Milliseconds 800
Click-Frac $main 0.583 0.963
Start-Sleep -Milliseconds 700
Click-Frac $main 0.22 0.90
Write-Output 'waiting for shell...'
Start-Sleep -Seconds 10
$wins = Get-AppWindows
$shell = $wins | Where-Object { [Int64]$_.Hwnd -ne [Int64]$main } | Select-Object -First 1
if (-not $shell) { Write-Output 'FAIL: no shell window'; exit 1 }
[E2E]::SetForegroundWindow($shell.Hwnd) | Out-Null
Start-Sleep -Milliseconds 600
Click-Frac $shell.Hwnd 0.5 0.5
if ([E2E]::GetForegroundWindow() -ne $shell.Hwnd) {
    Write-Output 'FAIL: shell not foreground'; exit 1
}

# Static content to select — more lines than the viewport so scrollback
# exists for the scroll-while-selected check below.
Run-Cmd 'seq 1 200'
Start-Sleep -Seconds 2

$sentinel = "E2E-SENTINEL-$(Get-Random)"
Set-Clipboard -Value $sentinel

# Shift-drag across a band of the output (shift = local-selection bypass).
$rect = New-Object E2E+RECT
[E2E]::GetWindowRect($shell.Hwnd, [ref]$rect) | Out-Null
$x1 = $rect.Left + [int](($rect.Right - $rect.Left) * 0.05)
$y1 = $rect.Top + [int](($rect.Bottom - $rect.Top) * 0.25)
$x2 = $rect.Left + [int](($rect.Right - $rect.Left) * 0.30)
$y2 = $rect.Top + [int](($rect.Bottom - $rect.Top) * 0.45)
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
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
$g.Dispose()
$shot = Join-Path $OutDir 'sel-highlight.png'
$bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$shot"

# THE bug this guards: scroll while selected. Wheel up a few lines — the
# highlight must ride the text (grid-anchored), not stick to screen rows.
[E2E]::SetCursorPos([int](($x1 + $x2) / 2), [int](($y1 + $y2) / 2)) | Out-Null
Start-Sleep -Milliseconds 200
for ($i = 0; $i -lt 3; $i++) {
    [E2E]::mouse_event(0x0800, 0, 0, 120, [UIntPtr]::Zero)  # WHEEL up 1 tick
    Start-Sleep -Milliseconds 150
}
Start-Sleep -Milliseconds 500
$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
$g.Dispose()
$shot2 = Join-Path $OutDir 'sel-scrolled.png'
$bmp.Save($shot2, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$shot2"

# ctrl+insert copy (ctrl+shift+c is stolen by a resident global hotkey).
[E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
Press 0x2D
[E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 800

$clip = Get-Clipboard -Raw -ErrorAction SilentlyContinue
Write-Output "CLIPBOARD>>>$clip<<<END"

foreach ($w2 in Get-AppWindows) {
    [E2E]::PostMessageW($w2.Hwnd, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    Start-Sleep -Milliseconds 800
}
Start-Sleep -Seconds 2
$left = Get-Process -Name 'shogun-desktop' -ErrorAction SilentlyContinue
if ($left) { $left | Stop-Process -Force }

if ([string]::IsNullOrWhiteSpace($clip) -or $clip -eq $sentinel) {
    Write-Output 'FAIL: clipboard did not receive the selection'
    exit 1
}
Write-Output 'PASS'
exit 0
