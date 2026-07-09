# E2E: RikkaTerminal prototype smoke — launch the standalone terminal, let the
# local shell start, paste a command (ctrl+shift+v), and screenshot the grid.
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$StartWaitSec = 6
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
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hWnd, uint msg, UIntPtr wParam, IntPtr lParam);
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
Start-Sleep -Seconds $StartWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited during startup'; exit 1 }

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
if ($script:hwnd -eq [IntPtr]::Zero) { Write-Output 'FAIL: window not found'; exit 1 }
[E2E]::SetForegroundWindow($script:hwnd) | Out-Null
Start-Sleep -Milliseconds 800

$rect = New-Object E2E+RECT
[E2E]::GetWindowRect($script:hwnd, [ref]$rect) | Out-Null

function Shot([string]$name) {
    $w = $rect.Right - $rect.Left
    $h = $rect.Bottom - $rect.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $p = Join-Path $OutDir $name
    $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "SHOT=$p"
}

Shot 'rikka0-startup.png'
Run-Cmd 'echo RIKKA-OK; 1..5 | ForEach-Object { "line $_" }'
Start-Sleep -Seconds 2
Shot 'rikka1-output.png'

[E2E]::PostMessageW($script:hwnd, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
Start-Sleep -Seconds 2
$left = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
if ($left) { $left | Stop-Process -Force }
Write-Output 'RUN-DONE'
