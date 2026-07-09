# E2E: TSF composition -> preedit -> commit against a live build (M1b).
#
# Staged driver: each stage is one short run; the app keeps running between
# stages and a vision pass on the screenshots picks the next coordinates.
#   launch    - start the exe gated (SHOGUN_TSF=1 + log), shot the main window
#   click     - click at (-FX,-FY) fractions of the main window, shot
#   openshell - click at (-FX,-FY), wait for the shell window, shot it
#   type      - focus the shell window, click in, IME on, type aiueo,
#               shot (preedit), Enter (commit), shot, ctrl+c, IME off
#   close     - close every window of the app gracefully
# Keep this file ASCII-only (PowerShell 5.1 parses BOM-less .ps1 as ANSI).

param(
    [Parameter(Mandatory = $true)][string]$Stage,
    [double]$FX = 0.5,
    [double]$FY = 0.5,
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
$logPath = Join-Path $OutDir 'tsf.log'
$mainHwndFile = Join-Path $OutDir 'main.hwnd'

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

function Shot-Window([IntPtr]$hwnd, [string]$name) {
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

function Click-Frac([IntPtr]$hwnd, [double]$fx, [double]$fy) {
    $rect = New-Object E2E+RECT
    [E2E]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
    $x = $rect.Left + [int](($rect.Right - $rect.Left) * $fx)
    $y = $rect.Top + [int](($rect.Bottom - $rect.Top) * $fy)
    Write-Output "CLICK ($x,$y)"
    [E2E]::SetCursorPos($x, $y) | Out-Null
    Start-Sleep -Milliseconds 250
    [E2E]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
    [E2E]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 500
}

function Press([byte]$vk, [byte]$scan = 0) {
    [E2E]::keybd_event($vk, $scan, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, $scan, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

function Tail-Log {
    Write-Output '=== TSF LOG (tail) ==='
    if (Test-Path $logPath) { Get-Content $logPath -Tail 25 } else { Write-Output '(no log)' }
}

switch ($Stage) {
    'launch' {
        Remove-Item -Path $logPath -ErrorAction SilentlyContinue
        $env:SHOGUN_TSF = '1'
        $env:SHOGUN_TSF_LOG = $logPath
        $proc = Start-Process -FilePath $ExePath -PassThru
        Write-Output "PID=$($proc.Id)"
        Start-Sleep -Seconds $ConnectWaitSec
        if ($proc.HasExited) { Write-Output 'FAIL: exited during startup'; exit 1 }
        $wins = Get-AppWindows
        if ($wins.Count -eq 0) { Write-Output 'FAIL: no window'; exit 1 }
        $main = $wins[0].Hwnd
        Set-Content -Path $mainHwndFile -Value ([Int64]$main)
        [E2E]::SetForegroundWindow($main) | Out-Null
        Start-Sleep -Milliseconds 800
        Shot-Window $main 'typ0-main.png'
    }
    'click' {
        $main = [IntPtr]([Int64](Get-Content $mainHwndFile))
        [E2E]::SetForegroundWindow($main) | Out-Null
        Start-Sleep -Milliseconds 500
        Click-Frac $main $FX $FY
        Start-Sleep -Milliseconds 700
        Shot-Window $main 'typ1-after-click.png'
    }
    'openshell' {
        $main = [IntPtr]([Int64](Get-Content $mainHwndFile))
        [E2E]::SetForegroundWindow($main) | Out-Null
        Start-Sleep -Milliseconds 500
        Click-Frac $main $FX $FY
        Write-Output "waiting for shell window + SSH..."
        Start-Sleep -Seconds 10
        $wins = Get-AppWindows
        foreach ($w in $wins) { Write-Output "WIN hwnd=$([Int64]$w.Hwnd) title=$($w.Title)" }
        $shell = $wins | Where-Object { [Int64]$_.Hwnd -ne [Int64]$main } | Select-Object -First 1
        if (-not $shell) { Write-Output 'FAIL: shell window not found'; Tail-Log; exit 1 }
        Set-Content -Path (Join-Path $OutDir 'shell.hwnd') -Value ([Int64]$shell.Hwnd)
        [E2E]::SetForegroundWindow($shell.Hwnd) | Out-Null
        Start-Sleep -Milliseconds 800
        Shot-Window $shell.Hwnd 'typ2-shell.png'
        Tail-Log
    }
    'type' {
        $shell = [IntPtr]([Int64](Get-Content (Join-Path $OutDir 'shell.hwnd')))
        [E2E]::SetForegroundWindow($shell) | Out-Null
        Start-Sleep -Milliseconds 600
        Click-Frac $shell 0.5 0.5
        if ([E2E]::GetForegroundWindow() -ne $shell) {
            Write-Output 'FAIL: shell not foreground'; exit 1
        }
        # IME on (hankaku/zenkaku), compose a i u e o, shot the preedit.
        Press 0xF4 0x29
        Start-Sleep -Milliseconds 600
        foreach ($vk in 0x41, 0x49, 0x55, 0x45, 0x4F) { Press ([byte]$vk) }
        Start-Sleep -Milliseconds 900
        Shot-Window $shell 'typ3-preedit.png'
        # Commit with Enter, shot the result at the prompt.
        Press 0x0D
        Start-Sleep -Milliseconds 900
        Shot-Window $shell 'typ4-committed.png'
        # Clean the prompt line and restore IME off.
        [E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
        Press 0x43
        [E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
        Start-Sleep -Milliseconds 300
        Press 0xF4 0x29
        Start-Sleep -Milliseconds 400
        Shot-Window $shell 'typ5-cleaned.png'
        Tail-Log
    }
    'close' {
        foreach ($w in Get-AppWindows) {
            Write-Output "closing hwnd=$([Int64]$w.Hwnd) title=$($w.Title)"
            # WM_CLOSE = graceful click-the-X.
            [E2E]::PostMessageW($w.Hwnd, 0x0010, [UIntPtr]::Zero, [IntPtr]::Zero) | Out-Null
            Start-Sleep -Milliseconds 800
        }
        Start-Sleep -Seconds 2
        $left = Get-Process -Name 'shogun-desktop' -ErrorAction SilentlyContinue
        if ($left) { $left | Stop-Process -Force }
        Tail-Log
    }
    default { Write-Output "unknown stage: $Stage"; exit 1 }
}
Write-Output 'STAGE-DONE'
