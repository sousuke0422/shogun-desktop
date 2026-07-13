<#
.SYNOPSIS
  Register the RikkaTerminal default-terminal sparse package for ALL USERS
  of a machine (admin). Pairs with the per-MACHINE MSI.

.DESCRIPTION
  Input = the signed sparse .msix and its .cer produced by
  install-default-terminal.ps1 (run that once, anywhere, to build them —
  its per-user registration on the build machine is harmless).

  What this does, machine-wide:
    1. copies the .msix next to the binaries (so an OS image carries it),
    2. trusts the signing cert in LocalMachine\TrustedPeople,
    3. registers for every user:
       - Windows 11 (provisioning API knows -ExternalLocation):
         Add-AppxProvisionedPackage — staged once, auto-registers per user.
       - Windows 10 (API lacks it — e.g. 19045): an Active Setup stub
         (HKLM ...\Active Setup\Installed Components\{GUID}) runs the
         per-user Add-AppxPackage once at each user's next logon.
    4. registers for the CURRENT user immediately.

  WDS note: run this (after the per-machine MSI) BEFORE sysprep/capture —
  the Program Files payload, the LocalMachine cert and the Active Setup
  key all ride the image; every user on every deployed machine gets the
  package at first logon. Selecting RikkaTerminal as the default terminal
  remains per user (HKCU) — Windows has no machine-wide default-terminal
  setting.

  Raise ACTIVE_SETUP_VERSION when shipping a new package so existing
  profiles re-run the stub.
#>
param(
    [string]$Msix = "$PSScriptRoot\out\RikkaTerminal.msix",
    [string]$CerFile = "$PSScriptRoot\out\rikka-dev.cer",
    [string]$ExternalLocation = "C:\Program Files\RikkaTerminal",
    [switch]$DryRun
)
$ErrorActionPreference = 'Stop'
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $DryRun -and -not $isAdmin) { throw "run elevated (or use -DryRun to preview)" }
$ACTIVE_SETUP_VERSION = '1'
$ACTIVE_SETUP_KEY = 'HKLM:\SOFTWARE\Microsoft\Active Setup\Installed Components\{44257F19-69C3-49CF-A9B0-A7A32132C59D}'

foreach ($f in @($Msix, $CerFile)) {
    if (-not (Test-Path $f)) { throw "missing $f — run install-default-terminal.ps1 first to build the msix + cer" }
}
if (-not (Test-Path (Join-Path $ExternalLocation 'rikka-terminal.exe'))) {
    throw "no rikka-terminal.exe in $ExternalLocation — install the per-machine MSI first"
}

$stagedMsix = Join-Path $ExternalLocation 'RikkaTerminal.msix'
$stub = "powershell.exe -NoProfile -ExecutionPolicy Bypass -Command `"Add-AppxPackage -Path '$stagedMsix' -ExternalLocation '$ExternalLocation'`""

$provCmd = Get-Command Add-AppxProvisionedPackage
$canProvision = $provCmd.Parameters.ContainsKey('ExternalLocation')

if ($DryRun) {
    Write-Host "would copy   : $Msix -> $stagedMsix"
    Write-Host "would trust  : $CerFile -> LocalMachine\TrustedPeople"
    if ($canProvision) {
        Write-Host "would run    : Add-AppxProvisionedPackage -Online -PackagePath $stagedMsix -ExternalLocation $ExternalLocation -SkipLicense"
    } else {
        Write-Host "would create : $ACTIVE_SETUP_KEY (v$ACTIVE_SETUP_VERSION)"
        Write-Host "  StubPath   : $stub"
    }
    Write-Host "would run    : Add-AppxPackage -Path $stagedMsix -ExternalLocation $ExternalLocation  (current user)"
    return
}

# 1. stage the msix beside the binaries (rides an OS image).
Copy-Item $Msix $stagedMsix -Force
Write-Host "staged: $stagedMsix"

# 2. machine-wide trust for the (self-signed) signer.
Import-Certificate -FilePath $CerFile -CertStoreLocation Cert:\LocalMachine\TrustedPeople | Out-Null
Write-Host "trusted signer in LocalMachine\TrustedPeople"

# 3. all-users registration.
if ($canProvision) {
    Add-AppxProvisionedPackage -Online -PackagePath $stagedMsix -ExternalLocation $ExternalLocation -SkipLicense | Out-Null
    Write-Host "provisioned machine-wide (users auto-register at logon)"
} else {
    New-Item -Path $ACTIVE_SETUP_KEY -Force | Out-Null
    Set-ItemProperty -Path $ACTIVE_SETUP_KEY -Name '(Default)' -Value 'RikkaTerminal default-terminal registration'
    Set-ItemProperty -Path $ACTIVE_SETUP_KEY -Name 'StubPath' -Value $stub
    Set-ItemProperty -Path $ACTIVE_SETUP_KEY -Name 'Version' -Value $ACTIVE_SETUP_VERSION
    Write-Host "provisioning API on this OS lacks -ExternalLocation (Windows 10);"
    Write-Host "installed Active Setup stub instead — each user registers at next logon"
}

# 4. the admin running this is a user too.
if (-not (Get-AppxPackage -Name 'RikkaTerminal')) {
    Add-AppxPackage -Path $stagedMsix -ExternalLocation $ExternalLocation
    Write-Host "registered for the current user"
}

Write-Host ""
Write-Host "Done. Each user still SELECTS RikkaTerminal in Settings > Privacy &"
Write-Host "security > For developers > Terminal (the choice itself is per-user)."
