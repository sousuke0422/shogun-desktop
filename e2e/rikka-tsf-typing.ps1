# E2E: RikkaTerminal TSF typing — IME on (Zenkaku/Hankaku), type romaji,
# expect the preedit inline, Enter commits to the PTY. Screenshots at each
# stage for a vision pass. Keep this file ASCII-only.

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

function Press([byte]$vk, [byte]$sc = 0) {
    [E2E]::keybd_event($vk, $sc, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, $sc, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
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
$hwnd = $script:hwnd
[E2E]::MoveWindow($hwnd, 200, 200, 1100, 700, $true) | Out-Null
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 1000

function Shot([string]$name) {
    $r = New-Object E2E+RECT
    [E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null
    $w = $r.Right - $r.Left
    $h = $r.Bottom - $r.Top
    $bmp = New-Object System.Drawing.Bitmap($w, $h)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
    $g.Dispose()
    $p = Join-Path $OutDir $name
    $bmp.Save($p, [System.Drawing.Imaging.ImageFormat]::Png)
    $bmp.Dispose()
    Write-Output "SHOT=$p"
}

# IME on (Zenkaku/Hankaku), type aiueo -> preedit, shot, Enter -> commit.
Press 0xF4 0x29
Start-Sleep -Milliseconds 600
Press 0x41; Press 0x49; Press 0x55; Press 0x45; Press 0x4F   # a i u e o
Start-Sleep -Milliseconds 800
Shot 'rikka-tsf0-preedit.png'
# Convert + open the candidate list (MS-IME: first Space converts, second
# opens the list). The list must open at the terminal caret — that is the
# caret-rect (GetTextExt) path.
Press 0x20; Press 0x20
Start-Sleep -Milliseconds 900
Shot 'rikka-tsf1-candidates.png'
Press 0x1B    # Escape back to the plain composition
Start-Sleep -Milliseconds 400
Press 0x0D    # Enter: commit the composition
Start-Sleep -Milliseconds 800
Shot 'rikka-tsf2-committed.png'
# IME back off so the machine is left as found.
Press 0xF4 0x29
Start-Sleep -Milliseconds 400

Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Write-Output 'RUN-DONE'
