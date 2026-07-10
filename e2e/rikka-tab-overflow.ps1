# E2E: many tabs must not push the caption buttons off; overflow shows the
# left/right scroll arrows. Opens ~10 tabs in a narrow window, screenshots.
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
public class TabE2E {
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
[TabE2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
function ChordCS([byte]$vk) {
    [TabE2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
    [TabE2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    [TabE2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [TabE2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    [TabE2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
    [TabE2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 350
}
$p = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds 6
$tp = $p.Id
$script:h = [IntPtr]::Zero
$cb = {
    param($hh, $l)
    $wp = 0
    [TabE2E]::GetWindowThreadProcessId($hh, [ref]$wp) | Out-Null
    if ($wp -eq $tp -and [TabE2E]::IsWindowVisible($hh)) {
        $sb = New-Object System.Text.StringBuilder 256
        [TabE2E]::GetWindowText($hh, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:h = $hh; return $false }
    }
    return $true
}
[TabE2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:h -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
[TabE2E]::MoveWindow($script:h, 120, 120, 900, 600, $true) | Out-Null
[TabE2E]::SetForegroundWindow($script:h) | Out-Null
Start-Sleep -Milliseconds 900
for ($i = 0; $i -lt $Tabs; $i++) { ChordCS 0x54 }   # Ctrl+Shift+T
Start-Sleep -Seconds 1
$r = New-Object TabE2E+RECT
[TabE2E]::GetWindowRect($script:h, [ref]$r) | Out-Null
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $b.Size)
$g.Dispose()
$out = Join-Path $OutDir 'tab-overflow.png'
$b.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$b.Dispose()

# Click the right chevron (just left of the caption group) a few times, then
# reshoot to confirm the tabs actually scroll and [+] comes into view.
# The window is physical px at 150% DPI: caption group ~207px, the right
# chevron sits ~235px from the right edge (measured from the first shot).
$rx = $r.Right - 235
$ry = $r.Top + 36
for ($k = 0; $k -lt 4; $k++) {
    [TabE2E]::SetCursorPos($rx, $ry) | Out-Null
    Start-Sleep -Milliseconds 150
    [TabE2E]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [TabE2E]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 300
}
[TabE2E]::GetWindowRect($script:h, [ref]$r) | Out-Null
$b2 = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g2 = [System.Drawing.Graphics]::FromImage($b2)
$g2.CopyFromScreen($r.Left, $r.Top, 0, 0, $b2.Size)
$g2.Dispose()
$out2 = Join-Path $OutDir 'tab-overflow-scrolled.png'
$b2.Save($out2, [System.Drawing.Imaging.ImageFormat]::Png)
$b2.Dispose()
Stop-Process -Id $p.Id -Force
Write-Output "SAVED=$out ; $out2"
