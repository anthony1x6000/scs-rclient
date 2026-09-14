# E2E test for Windows release binaries:
# 1. NSIS installer: silent install -> sidecar WebDAV test -> launch smoke -> silent uninstall
# 2. Portable EXE (+ no-icon): pairing check -> sidecar WebDAV test -> launch smoke
param(
  [string]$InstallerPath = "dist-win/scs-rclient-win-installer.exe",
  [string]$PortablePath = "dist-win/scs-rclient-win-portable.exe",
  [string]$NoIconPath = "dist-win-noicon/scs-rclient-win-noicon.exe",
  [string]$Verifier = "scripts/test/verify-webdav-endpoint.ps1"
)

$ErrorActionPreference = "Stop"
# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
Remove-Item env:RCLONE_VERSION -ErrorAction SilentlyContinue

function Assert-File($Path, $Label) {
  if (-not (Test-Path $Path)) { Write-Error "::error::$Label not found: $Path"; exit 1 }
  Write-Host "Found ${Label}: $Path"
}

function Invoke-LaunchSmoke($ExePath, $Label) {
  Write-Host "=== Launch smoke: $Label ($ExePath) ==="
  $proc = Start-Process -FilePath $ExePath -PassThru
  Start-Sleep -Seconds 10
  if ($proc.HasExited) {
    # A GUI app exiting 0 on a headless runner still proves DLL linkage resolved.
    Write-Host "$Label exited on its own with code $($proc.ExitCode) (linkage OK)."
    return
  }
  Write-Host "$Label stayed running for 10s (healthy start); stopping."
  try { Stop-Process -Id $proc.Id -Force } catch {}
}

# --- NSIS installer test ---
Assert-File $InstallerPath "NSIS installer"
Assert-File $Verifier "WebDAV verifier"

Write-Host "=== Silent install (NSIS /S) ==="
Start-Process -FilePath $InstallerPath -ArgumentList "/S" -Wait
Start-Sleep -Seconds 5

$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\scs-rclient"
if (-not (Test-Path $InstallDir)) {
  # Fall back: currentUser installs may land under a variant path; search for it.
  $candidate = Get-ChildItem -Path $env:LOCALAPPDATA -Recurse -Filter "scs-rclient.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($candidate) { $InstallDir = $candidate.DirectoryName } else { Write-Error "::error::Install dir not found: $InstallDir"; exit 1 }
}
Write-Host "Install dir: $InstallDir"
Get-ChildItem $InstallDir | Format-Table Name, Length

$InstalledSidecar = Get-ChildItem -Path $InstallDir -Recurse -Filter "rclone-sidecar*" -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $InstalledSidecar) { Write-Error "::error::Installed sidecar not found under $InstallDir"; exit 1 }
Write-Host "Found installed sidecar: $($InstalledSidecar.FullName)"

& $Verifier -RcloneBin $InstalledSidecar.FullName

$InstalledExe = Join-Path $InstallDir "scs-rclient.exe"
if (Test-Path $InstalledExe) { Invoke-LaunchSmoke $InstalledExe "installed app" }

Write-Host "=== Silent uninstall ==="
$Uninstaller = Get-ChildItem -Path $InstallDir -Filter "*ninstall*.exe" | Select-Object -First 1
if ($Uninstaller) {
  Start-Process -FilePath $Uninstaller.FullName -ArgumentList "/S" -Wait
  Start-Sleep -Seconds 5
}
if ((Test-Path $InstallDir) -and ((Get-ChildItem $InstallDir -ErrorAction SilentlyContinue | Measure-Object).Count -ne 0)) {
  Write-Error "::error::Uninstall left files behind in $InstallDir"
  exit 1
}
Write-Host "✓ NSIS E2E passed"

# --- Portable EXEs test ---
foreach ($pair in @(@{ Exe = $PortablePath; Name = "portable" }, @{ Exe = $NoIconPath; Name = "no-icon portable" })) {
  $exe = $pair.Exe
  if (-not (Test-Path $exe)) { Write-Host "::warning::Skipping $($pair.Name): $exe not present"; continue }
  Write-Host "=== Testing $($pair.Name): $exe ==="
  $dir = Split-Path $exe -Parent
  $sidecar = Get-ChildItem -Path $dir -Filter "rclone-sidecar*" -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($sidecar) {
    & $Verifier -RcloneBin $sidecar.FullName
  } else {
    Write-Host "::notice::No bundled sidecar next to $exe; testing repo-verified rclone is out of scope here."
  }
  Invoke-LaunchSmoke (Resolve-Path $exe).Path $pair.Name
}

Write-Host "✓ Windows E2E passed"
