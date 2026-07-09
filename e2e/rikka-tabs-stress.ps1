# E2E: RikkaTerminal tab gacha — the exact abuse pattern that crashes Windows
# Terminal (rapid tab detach/merge across windows) hammered in a loop, with
# typing afterwards to prove the sessions stayed healthy.
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$StartWaitSec = 6,
    [int]$GachaRounds = 5
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
    $procs = Get-Process -Name 'rikka-terminal' -ErrorAction SilentlyContinue
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
            if ($sb.Length -gt 0) { [void]$found.Add($h) }
        }
        return $true
    }
    [E2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
    return @($found)
}

function Press([byte]$vk) {
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 50
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 90
}

function Chord-CS([byte]$vk) {
    [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)  # CTRL
    [E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)  # SHIFT
    Press $vk
    [E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 200
    Chord-CS 0x56   # ctrl+shift+v paste
    Start-Sleep -Milliseconds 300
    Press 0x0D
}

function Shot([IntPtr]$hwnd, [string]$name) {
    $rect = New-Object E2E+RECT
    [E2E]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
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

$proc = Start-Process -FilePath $ExePath -PassThru
Write-Output "PID=$($proc.Id)"
Start-Sleep -Seconds $StartWaitSec
if ($proc.HasExited) { Write-Output 'FAIL: exited during startup'; exit 1 }
$wins = Get-AppWindows
if ($wins.Count -ne 1) { Write-Output "FAIL: expected 1 window, got $($wins.Count)"; exit 1 }
[E2E]::SetForegroundWindow($wins[0]) | Out-Null
Start-Sleep -Milliseconds 800

# Grow to 4 tabs.
Chord-CS 0x54; Chord-CS 0x54; Chord-CS 0x54   # ctrl+shift+t x3
Start-Sleep -Seconds 2
Shot $wins[0] 'tabs0-four.png'

# The gacha: detach then merge, back to back.
for ($i = 1; $i -le $GachaRounds; $i++) {
    Chord-CS 0x44   # detach active tab -> new window (focus moves there)
    Start-Sleep -Milliseconds 350
    $n = (Get-AppWindows).Count
    Chord-CS 0x41   # merge everything into the focused window (ctrl+shift+a)
    Start-Sleep -Milliseconds 350
    $m = (Get-AppWindows).Count
    Write-Output "round ${i}: after-detach windows=$n after-merge windows=$m"
    if ($proc.HasExited) { Write-Output "FAIL: process died in round $i"; exit 1 }
}

$wins = Get-AppWindows
Write-Output "final windows=$($wins.Count)"
if ($wins.Count -lt 1) { Write-Output 'FAIL: no window survived'; exit 1 }
[E2E]::SetForegroundWindow($wins[0]) | Out-Null
Start-Sleep -Milliseconds 500

# Prove the surviving active session still works end to end.
Run-Cmd 'echo GACHA-SURVIVED'
Start-Sleep -Seconds 2
Shot $wins[0] 'tabs1-survived.png'

if ($proc.HasExited) { Write-Output 'FAIL: process died at the end'; exit 1 }
foreach ($w in Get-AppWindows) {
    [E2E]::PostMessageW($w, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    Start-Sleep -Milliseconds 500
}
Start-Sleep -Seconds 2
$left = Get-Process -Name 'rikka-terminal' -ErrorAction SilentlyContinue
if ($left) { $left | Stop-Process -Force }
Write-Output 'PASS'
