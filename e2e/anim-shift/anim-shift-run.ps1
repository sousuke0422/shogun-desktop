# E2E: replay a `script --log-timing` recording inside rt (RikkaTerminal) and
# measure whether an animated region drifts, with anim-shift-probe.
# Keep this file ASCII-only.
#
# 1. launches rikka-terminal with `-e wsl.exe bash replay-in-rt.sh TIMING TYPESCRIPT`
#    (the replay drives the screen itself: no synthetic input, no focus games)
# 2. waits StartDelayMs, then `anim-shift-probe capture` records one PNG per
#    window update for DurationMs
# 3. if -Roi is given, `anim-shift-probe analyze` writes shift.csv, summary.txt
#    and side-by-side / diff PNGs for drifted frames
# 4. closes the window with WM_CLOSE (never kills a process)
#
# Pick the ROI from OutDir\frames\frame_00000.png (client pixels, x,y,w,h).
# Re-analysis without re-capturing:
#   anim-shift-probe analyze --in OutDir\frames --roi x,y,w,h [--mask ...] --out OutDir\result
#
# -Isolated (default on) passes --window-process so the window is hosted by
# THIS rikka-terminal.exe even when another rt is already running: without it
# the launch is forwarded to the running instance and drawn by its exe. The
# isolated window still heartbeats the running instance's window directory
# while it is open (and would take over coordination if that instance quit).
#
# Paths for -Timing / -Typescript are WSL paths (they are read by scriptreplay).
param(
    [Parameter(Mandatory = $true)][string]$Timing,
    [Parameter(Mandatory = $true)][string]$Typescript,
    [Parameter(Mandatory = $true)][string]$OutDir,
    [string]$Roi = '',
    # Masks as one string, ';'-separated: 'x,y,w,h;x,y,w,h' (pwsh -File joins
    # array arguments, so an array parameter cannot carry them).
    [string]$Mask = '',
    [int]$Radius = 8,
    [int]$DurationMs = 12000,
    [int]$StartDelayMs = 2500,
    [string]$Size = '100,30',
    [string]$RtExe = "$PSScriptRoot\..\..\target\release\rikka-terminal.exe",
    [string]$ProbeExe = "$PSScriptRoot\..\..\target\release\anim-shift-probe.exe",
    [bool]$Isolated = $true
)
$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class AnimShiftE2E {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hWnd, uint msg, IntPtr w, IntPtr l);
}
"@

foreach ($p in @($RtExe, $ProbeExe)) {
    if (-not (Test-Path $p)) { throw "not found: $p (cargo build --release -p rikka-terminal -p anim-shift-probe)" }
}
$RtExe = (Resolve-Path $RtExe).Path
$ProbeExe = (Resolve-Path $ProbeExe).Path
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$frames = Join-Path $OutDir 'frames'
$result = Join-Path $OutDir 'result'

# replay-in-rt.sh as a WSL path
$here = (Resolve-Path $PSScriptRoot).Path
$replay = (& wsl.exe wslpath -a ($here -replace '\\', '/') ) + '/replay-in-rt.sh'
$replay = $replay.Trim()

$title = 'anim-shift-probe-' + [Guid]::NewGuid().ToString('N').Substring(0, 8)
$rtArgs = @()
if ($Isolated) { $rtArgs += '--window-process' }
# Global options (--size) must lead; --title and -e belong to the tab.
$rtArgs += @('--size', $Size, '--title', $title, '-e', 'wsl.exe', 'bash', $replay, $Timing, $Typescript)
Write-Host "launch: $RtExe $($rtArgs -join ' ')"
$proc = Start-Process -FilePath $RtExe -ArgumentList $rtArgs -PassThru

$hwnd = [IntPtr]::Zero
for ($i = 0; $i -lt 100 -and $hwnd -eq [IntPtr]::Zero; $i++) {
    Start-Sleep -Milliseconds 100
    $p = Get-Process -Id $proc.Id -ErrorAction SilentlyContinue
    if ($null -eq $p) { throw "rikka-terminal exited early (forwarded to a running instance? use -Isolated)" }
    $hwnd = $p.MainWindowHandle
}
if ($hwnd -eq [IntPtr]::Zero) { throw 'no window within 10 s' }
Write-Host ("window: 0x{0:x} pid {1}" -f $hwnd.ToInt64(), $proc.Id)

$rc = 0
try {
    Start-Sleep -Milliseconds $StartDelayMs
    & $ProbeExe capture --hwnd ('0x{0:x}' -f $hwnd.ToInt64()) --duration-ms $DurationMs --out $frames
    if ($LASTEXITCODE -ne 0) { throw "capture failed ($LASTEXITCODE)" }
    if ($Roi -ne '') {
        $a = @('analyze', '--in', $frames, '--roi', $Roi, '--radius', $Radius, '--out', $result)
        foreach ($m in ($Mask -split ';')) { if ($m.Trim() -ne '') { $a += @('--mask', $m.Trim()) } }
        & $ProbeExe @a
        if ($LASTEXITCODE -ne 0) { throw "analyze failed ($LASTEXITCODE)" }
    } else {
        Write-Host "no -Roi: frames only. Pick the ROI from $frames\frame_00000.png"
    }
} catch {
    Write-Host "ERROR: $_"
    $rc = 1
} finally {
    # WM_CLOSE: ask the window to close; never kill.
    [AnimShiftE2E]::PostMessage($hwnd, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    if (-not $proc.WaitForExit(10000)) {
        Write-Host "WARNING: rikka-terminal pid $($proc.Id) still running; close its window by hand"
    }
}
exit $rc
