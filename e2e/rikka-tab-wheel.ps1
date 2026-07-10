# E2E: mouse wheel over the tab strip scrolls the tabs horizontally.
# Opens ~10 tabs (overflow), hovers the strip, wheels down, screenshots.
# Keep this file ASCII-only.
param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [string]$OutDir = "$env:TEMP\shogun-tsf",
    [int]$Tabs = 10
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public class WheelE2E {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);
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
[WheelE2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
function ChordCS([byte]$vk) {
    [WheelE2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [WheelE2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    [WheelE2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [WheelE2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    [WheelE2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [WheelE2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 350
}
$p = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds 6
$tp = $p.Id
$script:h = [IntPtr]::Zero
$cb = {
    param($hh, $l)
    $wp = 0
    [WheelE2E]::GetWindowThreadProcessId($hh, [ref]$wp) | Out-Null
    if ($wp -eq $tp -and [WheelE2E]::IsWindowVisible($hh)) {
        $sb = New-Object System.Text.StringBuilder 256
        [WheelE2E]::GetWindowText($hh, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:h = $hh; return $false }
    }
    return $true
}
[WheelE2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:h -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
[WheelE2E]::MoveWindow($script:h, 120, 120, 900, 600, $true) | Out-Null
[WheelE2E]::SetForegroundWindow($script:h) | Out-Null
Start-Sleep -Milliseconds 900
for ($i = 0; $i -lt $Tabs; $i++) { ChordCS 0x54 }
Start-Sleep -Milliseconds 800
$r = New-Object WheelE2E+RECT
[WheelE2E]::GetWindowRect($script:h, [ref]$r) | Out-Null

# Hover the tab strip (over the tabs, not the caption group), wheel DOWN.
[WheelE2E]::SetCursorPos(($r.Left + 250), ($r.Top + 25)) | Out-Null
Start-Sleep -Milliseconds 300
$WHEEL = 0x0800
$down = [uint32]4294967176   # -120 (one notch down) as unsigned
for ($k = 0; $k -lt 6; $k++) {
    [WheelE2E]::mouse_event($WHEEL, 0, 0, $down, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 200
}
Start-Sleep -Milliseconds 400
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $b.Size)
$g.Dispose()
$out = Join-Path $OutDir 'tab-wheel.png'
$b.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$b.Dispose()
Stop-Process -Id $p.Id -Force
Write-Output "SAVED=$out"
