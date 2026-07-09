# E2E: Windows yazi image preview inside rikka-terminal over the sideloaded
# ConPTY. Stage 1 captures `yazi --debug` (adapter/emulator detection);
# stage 2 opens yazi on a generated test image and screenshots the preview
# pane. PASS criteria (vision): debug shows a sixel-capable adapter, and the
# preview pane renders the red/blue gradient without shredding the file list
# (the staircase regression). Keep this file ASCII-only.

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
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
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

# Distinctive test image: red->blue gradient with a white diagonal.
$imgDir = Join-Path $OutDir 'yazi-test'
New-Item -ItemType Directory -Force -Path $imgDir | Out-Null
$imgPath = Join-Path $imgDir 'gradient.png'
$bmp = New-Object System.Drawing.Bitmap(400, 300)
for ($x = 0; $x -lt 400; $x++) {
    $r = [int](255 - ($x * 255 / 399)); $b = [int]($x * 255 / 399)
    for ($y = 0; $y -lt 300; $y++) {
        $bmp.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(255, $r, 0, $b))
    }
}
for ($d = 0; $d -lt 300; $d++) { $bmp.SetPixel([int]($d * 399 / 299), $d, [System.Drawing.Color]::White) }
$bmp.Save($imgPath, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()

function Press([byte]$vk) {
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 120
}
function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 250
    [E2E]::keybd_event(0x11,0,0,[UIntPtr]::Zero); [E2E]::keybd_event(0x10,0,0,[UIntPtr]::Zero)
    Press 0x56
    [E2E]::keybd_event(0x10,0,2,[UIntPtr]::Zero); [E2E]::keybd_event(0x11,0,2,[UIntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Press 0x0D
}

$env:RIKKA_PTY_DUMP = "$OutDir\yazi-pty.bin"
Remove-Item $env:RIKKA_PTY_DUMP -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds $StartWaitSec
$targetPid = $proc.Id
$script:hwnd = [IntPtr]::Zero
$cb = { param($h, $l)
    $wpid = 0; [E2E]::GetWindowThreadProcessId($h, [ref]$wpid) | Out-Null
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
[E2E]::MoveWindow($hwnd, 150, 150, 1400, 850, $true) | Out-Null
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 800

function Shot([string]$name) {
    $r = New-Object E2E+RECT
    [E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null
    $bmp = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $p = Join-Path $OutDir $name
    $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "SHOT=$p"
}

# Stage 1: adapter detection (prints and exits).
Run-Cmd 'yazi --debug'
Start-Sleep -Seconds 3
Shot 'yazi0-debug.png'

# Stage 2: open yazi on the test image; the hovered file gets previewed.
Run-Cmd "yazi `"$imgPath`""
Start-Sleep -Seconds 6
Shot 'yazi1-preview.png'

# Quit yazi cleanly, then end.
Press 0x51   # q
Start-Sleep -Seconds 1
Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output 'RUN-DONE'
