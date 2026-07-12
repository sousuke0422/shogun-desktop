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
    * Build the binaries first:
        cargo build --release -p rikka-terminal -p rikka-terminal-windows-integration
      (rikka-terminal.exe + rikka-handoff.exe land in target\release;
      OpenConsole.exe/conpty.dll are the vendored pair under
      crates\rikka-terminal\assets\conpty).

  Keep this file ASCII-only.
#>
param(
    [string]$Publisher        = "CN=sousuke0422",
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
    if ($PSVersionTable.PSVersion.Major -ge 6) {
        Import-Module Appx -UseWindowsPowerShell -WarningAction SilentlyContinue
    }
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

# --- 1b. re-brand the DEPLOYED OpenConsole's baked console-handoff CLSID -----
# The vendored OpenConsole is a Stable-branding build: as a -Embedding COM
# server it registers WT Stable's console CLSID {2EACA947-...}. Declaring that
# CLSID in our manifest would collide with an installed Windows Terminal, so
# the deployed copy (never the pristine vendored asset) gets its two .rdata
# GUID constants patched to OUR console CLSID. This is what makes selecting
# RikkaTerminal in the Settings UI work end to end (the UI writes BOTH pair
# values from our package). MIT-licensed binary; patch is 2x16 bytes.
Add-Type -TypeDefinition @'
public static class RikkaGuidPatch {
    public static int Patch(byte[] data, byte[] oldB, byte[] newB) {
        int n = 0;
        for (int i = 0; i <= data.Length - 16; i++) {
            bool hit = true;
            for (int j = 0; j < 16; j++) { if (data[i + j] != oldB[j]) { hit = false; break; } }
            if (hit) { System.Array.Copy(newB, 0, data, i, 16); n++; i += 15; }
        }
        return n;
    }
}
'@
$ocPath  = Join-Path $ExternalLocation 'OpenConsole.exe'
$ocBytes = [IO.File]::ReadAllBytes($ocPath)
# Guid.ToByteArray() yields the in-memory layout (Data1-3 LE + Data4 raw).
$ocOld = ([Guid]'2EACA947-7F5F-4CFA-BA87-8F7FBEEFBE69').ToByteArray()
$ocNew = ([Guid]'77F531BA-46BD-4E80-B0DF-8E45E1F7183B').ToByteArray()
$ocHits = [RikkaGuidPatch]::Patch($ocBytes, $ocOld, $ocNew)
if ($ocHits -lt 1) {
    throw ("OpenConsole CLSID patch found no occurrences - vendored binary " +
           "changed branding? Re-verify its baked GUIDs before installing.")
}
[IO.File]::WriteAllBytes($ocPath, $ocBytes)
Write-Host "OpenConsole console-handoff CLSID re-branded ($ocHits sites)."

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
# The Appx module loads only in Windows PowerShell, not PowerShell 7 — there
# Add-AppxPackage fails with 0x80131539 ("not supported on this platform"). On
# pwsh 7 bridge to Windows PowerShell; on Windows PowerShell 5.1 it's native.
if ($PSVersionTable.PSVersion.Major -ge 6) {
    Import-Module Appx -UseWindowsPowerShell -WarningAction SilentlyContinue
}
Add-AppxPackage -Path $msix -ExternalLocation $ExternalLocation

# The pwsh7 Appx bridge has failed SILENTLY here (no deployment event, no
# error, package absent) - trust nothing, verify the registration for real.
if (-not (Get-AppxPackage -Name 'RikkaTerminal')) {
    throw ("Package did not register despite no error. Run this script from " +
           "native Windows PowerShell 5.1 (powershell.exe), not pwsh 7.")
}

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
