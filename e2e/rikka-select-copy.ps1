# E2E: RikkaTerminal drag-select + copy. Echo a marker (pasted as a
# concatenation expression so the clipboard never contains the joined marker
# beforehand), drag across the output rows, Ctrl+Insert, and require the
# joined marker in the clipboard. Keep this file ASCII-only.

param(
    [string]$ExePath = "$PSScriptRoot\..\target\release\rikka-terminal.exe",
    [int]$StartWaitSec = 6
)

$ErrorActionPreference = 'Stop'
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
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hWnd);
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
}
"@
[E2E]::SetProcessDPIAware() | Out-Null

function Press([byte]$vk) {
    [E2E]::keybd_event($vk, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
    [E2E]::keybd_event($vk, 0, 2, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 120
}

$selLog = "$env:TEMP\shogun-tsf\rikka-sel-debug.log"
Remove-Item $selLog -ErrorAction SilentlyContinue
$proc = Start-Process -FilePath $ExePath -RedirectStandardError $selLog -PassThru
Start-Sleep -Seconds $StartWaitSec
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
$hwnd = $script:hwnd
[E2E]::MoveWindow($hwnd, 200, 200, 1100, 700, $true) | Out-Null
[E2E]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 1000
$scale = [E2E]::GetDpiForWindow($hwnd) / 96.0
$r = New-Object E2E+RECT
[E2E]::GetWindowRect($hwnd, [ref]$r) | Out-Null

# Paste the command as a concatenation so the joined marker is NOT in the
# clipboard yet, then run it.
Set-Clipboard -Value "cls; echo ('SELECT-' + 'ME-12345')"
Start-Sleep -Milliseconds 300
[E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
[E2E]::keybd_event(0x10, 0, 0, [UIntPtr]::Zero)
Press 0x56
[E2E]::keybd_event(0x10, 0, 2, [UIntPtr]::Zero)
[E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 400
Press 0x0D
Start-Sleep -Seconds 2

# Drag-select the top rows of the pane (strip 40 + pad, generous sweep).
# Start at col ~1: further left runs into the invisible resize frame
# (WM_NCHITTEST says HTLEFT and the drag becomes a window resize).
$x0 = $r.Left + [int](20 * $scale)
$y0 = $r.Top + [int](50 * $scale)
$x1 = $r.Left + [int](420 * $scale)
$y1 = $y0 + [int](80 * $scale)
[E2E]::SetCursorPos($x0, $y0) | Out-Null
Start-Sleep -Milliseconds 200
[E2E]::mouse_event(2, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 120
Add-Type -AssemblyName System.Drawing
for ($i = 1; $i -le 12; $i++) {
    [E2E]::SetCursorPos($x0 + [int](($x1 - $x0) * $i / 12), $y0 + [int](($y1 - $y0) * $i / 12)) | Out-Null
    Start-Sleep -Milliseconds 25
    if ($i -eq 10) {
        $rm = New-Object E2E+RECT
        [E2E]::GetWindowRect($hwnd, [ref]$rm) | Out-Null
        $bm = New-Object System.Drawing.Bitmap(($rm.Right - $rm.Left), ($rm.Bottom - $rm.Top))
        $gm = [System.Drawing.Graphics]::FromImage($bm)
        $gm.CopyFromScreen($rm.Left, $rm.Top, 0, 0, $bm.Size)
        $gm.Dispose()
        $bm.Save("$env:TEMP\shogun-tsf\rikka-sel-mid.png", [System.Drawing.Imaging.ImageFormat]::Png)
        $bm.Dispose()
        Write-Output "MIDSHOT taken (window at $($rm.Left),$($rm.Top))"
    }
}
Start-Sleep -Milliseconds 150
[E2E]::mouse_event(4, 0, 0, 0, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 300

# Diagnostic: capture the selection highlight state before copying.
Add-Type -AssemblyName System.Drawing
$rr = New-Object E2E+RECT
[E2E]::GetWindowRect($hwnd, [ref]$rr) | Out-Null
$bmp = New-Object System.Drawing.Bitmap(($rr.Right - $rr.Left), ($rr.Bottom - $rr.Top))
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rr.Left, $rr.Top, 0, 0, $bmp.Size)
$g.Dispose()
$bmp.Save("$env:TEMP\shogun-tsf\rikka-sel0.png", [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "SHOT=$env:TEMP\shogun-tsf\rikka-sel0.png"

# Copy with Ctrl+Insert and inspect the clipboard.
[E2E]::keybd_event(0x11, 0, 0, [UIntPtr]::Zero)
Press 0x2D
[E2E]::keybd_event(0x11, 0, 2, [UIntPtr]::Zero)
Start-Sleep -Milliseconds 500
$clip = Get-Clipboard -Raw
# Accept a missing lead char: the drag anchors at col ~1 by design.
if ($clip -match 'LECT-ME-12345') {
    Write-Output 'SELECT-COPY-OK'
} else {
    $head = if ($clip) { $clip.Substring(0, [Math]::Min(120, $clip.Length)) } else { '<empty>' }
    Write-Output "FAIL: clipboard=[$head]"
}
Get-Process -Id $proc.Id -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 400
Write-Output '--- sel debug log (tail) ---'
Get-Content $selLog -Tail 25 -ErrorAction SilentlyContinue
Write-Output 'RUN-DONE'
