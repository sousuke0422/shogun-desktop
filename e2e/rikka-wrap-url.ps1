# Repro: a long URL that soft-wraps at the window width. Screenshots the grid
# so we can see how the wrapped URL renders (dotted underline should span both
# rows if soft-wrap link detection works). Keep this file ASCII-only.
param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf"
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class WrapE2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern void keybd_event(byte bVk, byte bScan, uint dwFlags, UIntPtr dwExtraInfo);
    public delegate bool EnumWindowsProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc cb, IntPtr l);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out int pid);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] public static extern int GetWindowText(IntPtr h, StringBuilder s, int m);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h, int x, int y, int w, int ht, bool rp);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[WrapE2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
function Press([byte]$vk) {
    [WrapE2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    [WrapE2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 120
}
function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 250
    [WrapE2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero); [WrapE2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    Press 0x56
    [WrapE2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero); [WrapE2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 400
    Press 0x0D
}
$p = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds 6
$tp = $p.Id
$script:h = [IntPtr]::Zero
$cb = {
    param($hh, $l)
    $wp = 0
    [WrapE2E]::GetWindowThreadProcessId($hh, [ref]$wp) | Out-Null
    if ($wp -eq $tp -and [WrapE2E]::IsWindowVisible($hh)) {
        $sb = New-Object System.Text.StringBuilder 256
        [WrapE2E]::GetWindowText($hh, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:h = $hh; return $false }
    }
    return $true
}
[WrapE2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:h -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
# Narrow window so the URL wraps.
[WrapE2E]::MoveWindow($script:h, 120, 120, 460, 420, $true) | Out-Null
[WrapE2E]::SetForegroundWindow($script:h) | Out-Null
Start-Sleep -Milliseconds 900
Run-Cmd 'Write-Host "https://example.com/some/really/long/path/that/wraps?q=value&more=1"'
Start-Sleep -Seconds 2
$r = New-Object WrapE2E+RECT
[WrapE2E]::GetWindowRect($script:h, [ref]$r) | Out-Null
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $b.Size)
$g.Dispose()
$out = Join-Path $OutDir 'wrap-url.png'
$b.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$b.Dispose()
Stop-Process -Id $p.Id -Force
Write-Output "SAVED=$out"
