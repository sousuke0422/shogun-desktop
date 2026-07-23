# Builds the portable zip: no installer, no registry, nothing outside the
# folder. The bundled `.portable` marker switches rikka into portable mode
# (config.toml + logs\ beside the exe; %APPDATA% untouched). Default-terminal
# integration is deliberately absent — that requires package registration and
# is what the MSI/MSIX path is for.
param([string]$Version = "0.1.0")
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent          # crates/rikka-terminal
$ws = (Resolve-Path (Join-Path $root '..\..')).Path
$rel = Join-Path $ws 'target\release'

Push-Location $ws
try { cargo build --release -p rikka-terminal | Out-Host } finally { Pop-Location }

$stage = Join-Path $PSScriptRoot 'out\portable'
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

foreach ($f in @('rikka-terminal.exe', 'rt.exe', 'rikka-handoff.exe', 'OpenConsole.exe', 'conpty.dll')) {
    Copy-Item (Join-Path $rel $f) $stage
}
Copy-Item (Join-Path $root 'config.example.toml') $stage
New-Item -ItemType File -Force (Join-Path $stage '.portable') | Out-Null

@'
RikkaTerminal portable

* Everything stays in this folder: config.toml (copy config.example.toml to
  start), and session logs under logs\. %APPDATA% and the registry are never
  touched. The `.portable` marker file is what enables this mode - keep it.
* Saving config.toml applies live to running windows.
* Ctrl+, opens the settings window; Ctrl+Shift+F searches scrollback.
* Default-terminal integration is not available in the portable build - use
  the MSI installer for that.
'@ | Set-Content (Join-Path $stage 'README-portable.txt')

$zip = Join-Path $PSScriptRoot ("out\RikkaTerminal-$Version-portable-x64.zip")
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
# Compress-Archive's * glob skips dotfiles; add the marker explicitly.
Compress-Archive -Path (Join-Path $stage '.portable') -Update -DestinationPath $zip
Write-Output "portable zip: $zip"
