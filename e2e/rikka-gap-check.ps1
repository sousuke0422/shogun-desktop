# E2E: verify the terminal grid sits flush under the tab strip (no pane-
# background gap band below the tabs). Launches rikka, screenshots the window.
# Keep this file ASCII-only.
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
public class Gap {
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
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
[Gap]::SetProcessDPIAware() | Out-Null
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$p = Start-Process -FilePath $ExePath -PassThru
Start-Sleep -Seconds 6
$tp = $p.Id
$script:h = [IntPtr]::Zero
$cb = {
    param($hh, $l)
    $wp = 0
    [Gap]::GetWindowThreadProcessId($hh, [ref]$wp) | Out-Null
    if ($wp -eq $tp -and [Gap]::IsWindowVisible($hh)) {
        $sb = New-Object System.Text.StringBuilder 256
        [Gap]::GetWindowText($hh, $sb, 256) | Out-Null
        if ($sb.Length -gt 0) { $script:h = $hh; return $false }
    }
    return $true
}
[Gap]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
if ($script:h -eq [IntPtr]::Zero) { Write-Output 'FAIL: no window'; exit 1 }
[Gap]::MoveWindow($script:h, 150, 150, 1000, 640, $true) | Out-Null
[Gap]::SetForegroundWindow($script:h) | Out-Null
Start-Sleep -Milliseconds 900
$r = New-Object Gap+RECT
[Gap]::GetWindowRect($script:h, [ref]$r) | Out-Null
$b = New-Object System.Drawing.Bitmap(($r.Right - $r.Left), ($r.Bottom - $r.Top))
$g = [System.Drawing.Graphics]::FromImage($b)
$g.CopyFromScreen($r.Left, $r.Top, 0, 0, $b.Size)
$g.Dispose()
$out = Join-Path $OutDir 'gap.png'
$b.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$b.Dispose()
Stop-Process -Id $p.Id -Force
Write-Output "SAVED=$out SIZE=$((Get-Item $out).Length)"
