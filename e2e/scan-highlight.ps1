# Pixel-scan a drag-copy-test.ps1 screenshot for the selection-highlight color.
#
# The highlight is a translucent steel-blue quad (SELECTION_RGBA 0x3465a480 in
# renderer.rs) over a near-black background, so selected cells read as pixels
# with a clear blue dominance. A visible selection produces tens of thousands
# of matches (32,668 sampled at the 2026-07-03 fix verification); an invisible
# one produces only stray anti-aliasing hits (63 in the broken build).
#
# PASS threshold: 1000 sampled matches.
#
# Usage (repo path abbreviated):
#   pwsh.exe -NoProfile -File 'C:\...\shogun-desktop\e2e\scan-highlight.ps1' \
#     [-ImagePath <png>] [-Threshold <n>]

param(
    [string]$ImagePath = "$env:TEMP\shogun-e2e-sel.png",
    [int]$Threshold = 1000
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$bmp = [System.Drawing.Bitmap]::FromFile($ImagePath)
$count = 0
$samples = @()
for ($y = 0; $y -lt $bmp.Height; $y += 2) {
    for ($x = 0; $x -lt $bmp.Width; $x += 2) {
        $p = $bmp.GetPixel($x, $y)
        if ($p.B -gt ($p.R + 25) -and $p.B -gt ($p.G + 20) -and $p.B -gt 45) {
            $count++
            if ($samples.Count -lt 5) { $samples += "($x,$y) R=$($p.R) G=$($p.G) B=$($p.B)" }
        }
    }
}
$bmp.Dispose()

Write-Output "bluish pixels (every 2nd px both axes): $count"
$samples | ForEach-Object { Write-Output $_ }
if ($count -ge $Threshold) { Write-Output 'PASS'; exit 0 }
Write-Output "FAIL: below threshold $Threshold"
exit 1
