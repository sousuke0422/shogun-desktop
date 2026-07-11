#Requires -Version 5.1
<#
  install-default-terminal.ps1  (RUN ON YOUR OWN MACHINE — not by the agent)

  Build -> sign -> install the RikkaTerminal *sparse* (external-location)
  package so RikkaTerminal appears in the Windows 11 "Default terminal
  application" dropdown.

  SAFETY: this does NOT change the active default. HKCU\Console\%%Startup is
  left untouched, so installing this only ADDS a candidate to the dropdown — it
  will not hijack your consoles. Selecting RikkaTerminal (which flips the
  default) is a separate, deliberate step you take by hand (that's P3).

  REQUIRES: Windows 10 2004 (build 19041) or newer, incl. Windows 11; the
  Windows SDK (MakeAppx.exe + SignTool.exe); and — for the one-time cert-trust
  step only — an elevated (Administrator) shell.

  BEFORE FIRST RUN:
    * Edit AppxManifest.xml: replace the REPLACE_ME Publisher and the two GUIDs
      (generate fresh ones with [guid]::NewGuid()).
    * Make -Publisher below match the manifest's <Identity Publisher="..."> EXACTLY.
    * Build the binaries first:  cargo build --release -p shogun-desktop  (rt +
      rikka-handoff live in target\release; OpenConsole.exe/conpty.dll are the
      vendored pair under crates\rikka-terminal\assets\conpty).

  Keep this file ASCII-only.
#>
param(
    [string]$Publisher        = "CN=RikkaTerminal Dev",
    [string]$ExternalLocation = "$env:LOCALAPPDATA\RikkaTerminal",
    [string]$BuildDir         = "$PSScriptRoot\..\..\..\target\release",
    [string]$ConPtyDir        = "$PSScriptRoot\..\assets\conpty",
    [string]$PackagingDir     = $PSScriptRoot,
    [string]$OutDir           = "$PSScriptRoot\out",
    [string]$PfxPassword      = "rikka-dev",
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'

if ($Uninstall) {
    Get-AppxPackage -Name 'RikkaTerminal' | ForEach-Object {
        Write-Host "Removing $($_.PackageFullName)"
        Remove-AppxPackage $_.PackageFullName
    }
    Write-Host "Done. (The active default terminal was never changed by this script.)"
    return
}

# --- 0. locate the SDK tools ------------------------------------------------
function Find-SdkTool([string]$name) {
    $root = "${env:ProgramFiles(x86)}\Windows Kits\10\bin"
    if (-not (Test-Path $root)) { return $null }
    Get-ChildItem $root -Recurse -Filter $name -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } |
        Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
}
$makeappx = Find-SdkTool 'MakeAppx.exe'
$signtool = Find-SdkTool 'SignTool.exe'
if (-not $makeappx -or -not $signtool) {
    throw "Windows SDK MakeAppx.exe / SignTool.exe not found. Install the Windows 10/11 SDK."
}
Write-Host "MakeAppx : $makeappx"
Write-Host "SignTool : $signtool"

# --- 1. populate the external location with the unpackaged binaries ---------
# The sparse .msix carries ONLY the manifest + Images; the exes live out here.
New-Item -ItemType Directory -Force -Path $ExternalLocation | Out-Null
$need = @(
    @{ src = "$BuildDir\rikka-terminal.exe"; name = 'rikka-terminal.exe' },
    @{ src = "$BuildDir\rikka-handoff.exe";  name = 'rikka-handoff.exe'  },
    @{ src = "$ConPtyDir\OpenConsole.exe";   name = 'OpenConsole.exe'    },
    @{ src = "$ConPtyDir\conpty.dll";        name = 'conpty.dll'         }
)
foreach ($f in $need) {
    if (-not (Test-Path $f.src)) { throw "Missing binary: $($f.src) — build first." }
    Copy-Item $f.src (Join-Path $ExternalLocation $f.name) -Force
}
Write-Host "External location populated: $ExternalLocation"

# --- 2. self-signed code-signing cert (created once, reused after) ----------
$cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $Publisher } | Select-Object -First 1
if (-not $cert) {
    Write-Host "Creating self-signed code-signing cert for $Publisher"
    $cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject $Publisher `
        -KeyUsage DigitalSignature -CertStoreLocation Cert:\CurrentUser\My `
        -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$pfx = Join-Path $OutDir 'rikka-dev.pfx'
$cer = Join-Path $OutDir 'rikka-dev.cer'
$pw  = ConvertTo-SecureString -String $PfxPassword -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $pw | Out-Null
Export-Certificate    -Cert $cert -FilePath $cer | Out-Null

# --- 3. trust the cert so the sparse package will install (NEEDS ADMIN) -----
# Sideloading a self-signed MSIX requires the signing cert in LocalMachine's
# Trusted People (or Root). This is the ONE step that needs an elevated shell.
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
         ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($admin) {
    Import-Certificate -FilePath $cer -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
    Write-Host "Cert trusted in LocalMachine\TrustedPeople."
} else {
    Write-Warning "Not elevated: skipping cert trust. Run this ONCE from an admin shell, or:"
    Write-Warning "  Import-Certificate -FilePath '$cer' -CertStoreLocation Cert:\LocalMachine\TrustedPeople"
}

# --- 4. pack the sparse package (manifest + Images only) --------------------
# /nv = skip validation: the manifest's Executable= targets are EXTERNAL, so
# the packer must not require them inside the package.
$msix = Join-Path $OutDir 'RikkaTerminal.msix'
& $makeappx pack /o /d $PackagingDir /p $msix /nv
if ($LASTEXITCODE -ne 0) { throw "MakeAppx pack failed ($LASTEXITCODE)." }

# --- 5. sign ----------------------------------------------------------------
& $signtool sign /fd SHA256 /a /f $pfx /p $PfxPassword $msix
if ($LASTEXITCODE -ne 0) { throw "SignTool sign failed ($LASTEXITCODE)." }

# --- 6. install, pointing at the external binaries --------------------------
Add-AppxPackage -Path $msix -ExternalLocation $ExternalLocation

# --- 7. verify --------------------------------------------------------------
Write-Host ""
Write-Host "== Installed. =="
Write-Host "Open  Windows Terminal > Settings > Startup > Default terminal"
Write-Host "application  (on Win11 also Settings > System > For developers >"
Write-Host "Terminal) and confirm 'RikkaTerminal' is listed as a choice."
Write-Host ""
Write-Host "DO NOT select it yet — that flips the active default (P3). This"
Write-Host "script never wrote HKCU\Console\%%Startup, so your current default"
Write-Host "is unchanged. Uninstall anytime:  .\install-default-terminal.ps1 -Uninstall"
