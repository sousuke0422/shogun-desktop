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

$user = Join-Path $PSScriptRoot "RikkaTerminal-$Version.msi"
wix build (Join-Path $PSScriptRoot "installer.wxs") `
    -d "BinDir=$BinDir" -d "Version=$Version" `
    -o $user
if ($LASTEXITCODE -ne 0) { throw "wix build failed (per-user)" }

$machine = Join-Path $PSScriptRoot "RikkaTerminal-$Version-machine.msi"
wix build (Join-Path $PSScriptRoot "installer-machine.wxs") `
    -d "BinDir=$BinDir" -d "Version=$Version" `
    -o $machine
if ($LASTEXITCODE -ne 0) { throw "wix build failed (per-machine)" }

Write-Host "built: $user"
Write-Host "built: $machine"
Write-Host ""
Write-Host "per-user   (no admin):  msiexec /i `"$user`""
Write-Host "per-machine (admin):    msiexec /i `"$machine`""
Write-Host "then, for default-terminal (by hand):"
Write-Host "  per-user:    .\install-default-terminal.ps1"
Write-Host "  per-machine: .\install-default-terminal.ps1 -ExternalLocation `"C:\Program Files\RikkaTerminal`""
Write-Host "NOTE: pick ONE scope — installing both leaves two copies."
