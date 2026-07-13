# Build the per-user MSI for the RikkaTerminal binaries.
#
#   .\build-installer.ps1 [-BinDir <dir>] [-Version <x.y.z>]
#
# Defaults assume the lock-dodging release tree:
#   cargo build --release --target-dir target-deploy -p rikka-terminal -p rikka-terminal-windows-integration
# (build.rs drops conpty.dll / OpenConsole.exe beside the exes there.)
#
# The MSI carries the BINARIES ONLY. The default-terminal registration is
# the separate sparse MSIX (install-default-terminal.ps1), installed by
# hand afterwards — it points Windows at the files this MSI lays down.
param(
    [string]$BinDir = (Join-Path $PSScriptRoot "..\..\..\target-deploy\release"),
    [string]$Version = "0.1.0"
)
$ErrorActionPreference = "Stop"

$BinDir = (Resolve-Path $BinDir).Path
foreach ($f in @("rikka-terminal.exe", "rt.exe", "rikka-handoff.exe", "conpty.dll", "OpenConsole.exe")) {
    if (-not (Test-Path (Join-Path $BinDir $f))) {
        throw "missing $f in $BinDir — run the release build first"
    }
}

$out = Join-Path $PSScriptRoot "RikkaTerminal-$Version.msi"
wix build (Join-Path $PSScriptRoot "installer.wxs") `
    -d "BinDir=$BinDir" -d "Version=$Version" `
    -o $out
if ($LASTEXITCODE -ne 0) { throw "wix build failed" }

Write-Host "built: $out"
Write-Host "install (per-user, no admin):  msiexec /i `"$out`""
Write-Host "then, for default-terminal:    .\install-default-terminal.ps1  (by hand)"
