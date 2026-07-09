# E2E: local sixel through the sideloaded ConPTY pair. Launches
# rikka-terminal, emits a raw sixel DCS from pwsh, and screenshots the grid.
# PASS criteria (vision): shell banner + prompt visible (pair handshake OK)
# AND a red block rendered between the command and SIXEL-AFTER (DCS passed
# through and decoded). A silent/empty pane = mismatched conpty pair.
# Keep this file ASCII-only.

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

$proc = Start-Process -FilePath 'C:\Users\aki\work\shogun-desktop\target\release\rikka-terminal.exe' -PassThru
Start-Sleep -Seconds 6
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
[E2E]::MoveWindow($script:hwnd, 200, 200, 1000, 640, $true) | Out-Null
[E2E]::SetForegroundWindow($script:hwnd) | Out-Null
Start-Sleep -Milliseconds 800

# Emit a small red sixel block via raw DCS from inside the ConPTY app.
Run-Cmd '$e=[char]27; [Console]::Out.Write("$e" + "P0;0;0q#0;2;100;0;0!40~-!40~-!40~" + "$e" + "\"); [Console]::Out.Write("`nSIXEL-AFTER`n")'
Start-Sleep -Seconds 3

$r = New-Object E2E+RECT
[E2E]::GetWindowRect($script:hwnd, [ref]$r) | Out-Null
$bmp = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $bmp.Size)
$g.Dispose()
$bmp.Save("$env:TEMP\shogun-tsf\sixel-local0.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output 'SHOT-DONE'
Stop-Process -Id $proc.Id -Force
