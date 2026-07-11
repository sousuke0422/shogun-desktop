# E2E: prove the TSF path is ON BY DEFAULT (the terminal.tsf setting), with no
# SHOGUN_TSF env override in play. Launch --shell-window with SHOGUN_TSF unset
# but SHOGUN_TSF_LOG set, bring it foreground (activation auto-focuses the
# terminal -> on_input_focus -> TSF engages iff enabled()), and require the log
# to show TSF focus activity. Empty log == TSF did not engage == default is off.
# Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\shogun-desktop.exe",
    [int]$WaitSec = 12
)

$ErrorActionPreference = 'Stop'
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class E2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    public delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr hWnd, StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
}
"@
[E2E]::SetProcessDPIAware() | Out-Null

$log = Join-Path $env:TEMP 'shogun-tsf\tsf-default-on.log'
New-Item -ItemType Directory -Force -Path (Split-Path $log) | Out-Null
Remove-Item $log -ErrorAction SilentlyContinue

# Test the SETTING default, not the env override — make sure SHOGUN_TSF is unset.
Remove-Item Env:\SHOGUN_TSF -ErrorAction SilentlyContinue
$env:SHOGUN_TSF_LOG = $log
Write-Output "SHOGUN_TSF is set: $([bool]$env:SHOGUN_TSF)"

$proc = Start-Process -FilePath $ExePath -ArgumentList '--shell-window' -PassThru
Start-Sleep -Seconds $WaitSec
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
# Foreground it so the activation observer auto-focuses the terminal in a
# foreground context (TSF SetFocus only binds while the window is frontmost).
[E2E]::SetForegroundWindow($script:hwnd) | Out-Null
Start-Sleep -Seconds 4

Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

if (-not (Test-Path $log)) { Write-Output 'FAIL: no TSF log (TSF did not engage -> default OFF)'; exit 1 }
$content = Get-Content $log -ErrorAction SilentlyContinue
$focusHits = ($content | Select-String -Pattern 'focus|AdviseSink|GetWnd' -AllMatches).Count
Write-Output "--- TSF log (tail) ---"
$content | Select-Object -Last 20 | ForEach-Object { Write-Output $_ }
Write-Output "--- focus/AdviseSink/GetWnd hits: $focusHits ---"
if ($focusHits -ge 1) {
    Write-Output 'TSF-DEFAULT-ON-OK'
    exit 0
} else {
    Write-Output 'FAIL: TSF log has no focus activity (default did not engage TSF)'
    exit 1
}
