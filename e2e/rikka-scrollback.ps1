# E2E: local scrollback. Print 50 numbered lines, then wheel UP over the
# terminal body and screenshot — earlier line numbers must come into view
# (and stay; a snap-to-bottom on output would be a bug). ASCII-only.
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
public class ScrollE2E {
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
[ScrollE2E]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
function Press([byte]$vk) {
    [ScrollE2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero); Start-Sleep -Milliseconds 60
    [ScrollE2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 120
}
function Run-Cmd([string]$cmd) {
    Set-Clipboard -Value $cmd
    Start-Sleep -Milliseconds 250
    [ScrollE2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero); [ScrollE2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
    Press 0x56
    [ScrollE2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero); [ScrollE2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
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
    [ScrollE2E]::GetWindowThreadProcessId($hh, [ref]$wp) | Out-Null
    if ($wp -eq $tp -and [ScrollE2E]::IsWindowVisible($hh)) {
        $sb = New-Object System.Text.StringBuilder 256
        [ScrollE2E]::GetWindowText($hh, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:h = $hh; return $false }
    }
    return $true
}
[ScrollE2E]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:h -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
[ScrollE2E]::MoveWindow($script:h, 120, 120, 900, 600, $true) | Out-Null
[ScrollE2E]::SetForegroundWindow($script:h) | Out-Null
Start-Sleep -Milliseconds 900
Run-Cmd '1..50 | ForEach-Object { "line $_" }'
Start-Sleep -Seconds 2
$r = New-Object ScrollE2E+RECT
[ScrollE2E]::GetWindowRect($script:h, [ref]$r) | Out-Null
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b); $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $b.Size); $g.Dispose()
$b.Save((Join-Path $OutDir 'scroll0-bottom.png'), [System.Drawing.Imaging.ImageFormat]::Png); $b.Dispose()

# Wheel UP over the terminal body (well below the tab strip).
[ScrollE2E]::SetCursorPos(($r.Left + 400), ($r.Top + 300)) | Out-Null
Start-Sleep -Milliseconds 300
$WHEEL = 0x0800
for ($k = 0; $k -lt 8; $k++) {
    [ScrollE2E]::mouse_event($WHEEL, 0, 0, [uint32]120, [UIntPtr]::Zero)  # +120 = up
    Start-Sleep -Milliseconds 150
}
Start-Sleep -Milliseconds 400
$b2 = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g2 = [System.Drawing.Graphics]::FromImage($b2); $g2.CopyFromScreen($r.Left, $r.Top, 0, 0, $b2.Size); $g2.Dispose()
$b2.Save((Join-Path $OutDir 'scroll1-up.png'), [System.Drawing.Imaging.ImageFormat]::Png); $b2.Dispose()
Stop-Process -Id $p.Id -Force
Write-Output 'DONE'
