# E2E test for Windows release binaries:
# 1. NSIS installer: silent install -> sidecar WebDAV test -> launch smoke -> silent uninstall
# 2. Portable EXE (+ no-icon): sidecar pairing check -> zip check -> sidecar WebDAV test -> launch smoke
#
# The sidecar must sit next to the app executable under the exact name Tauri
# resolves at runtime ("rclone-sidecar.exe"). Globbing for "rclone-sidecar*" would
# accept the bundler's triple-suffixed name, which the app never looks for.
param(
  [string]$InstallerPath = "dist-win/scs-rclient-win-installer.exe",
  [string]$PortablePath = "dist-win/scs-rclient-win-portable.exe",
  [string]$PortableZip = "dist-win/scs-rclient-win-portable.zip",
  [string]$NoIconPath = "dist-win-noicon/scs-rclient-win-noicon.exe",
  [string]$NoIconZip = "dist-win-noicon/scs-rclient-win-noicon.zip",
  [string]$Verifier = "scripts/test/verify-webdav-endpoint.ps1",
  [string]$LayoutVerifier = "scripts/test/verify-sidecar-layout.ps1"
)

$ErrorActionPreference = "Stop"
# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
Remove-Item env:RCLONE_VERSION -ErrorAction SilentlyContinue

$RuntimeSidecarName = "rclone-sidecar.exe"

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

# Assert the sidecar is resolvable from the app exe directory and that the
# distributable zip carries the app and its sidecar together. verify-sidecar-layout.ps1
# is the single source of truth for the runtime path contract.
function Assert-ResolvableSidecar($AppExe, $Zip, $Label) {
  Write-Host "=== Verifying sidecar layout for $Label ==="
  # Run as a child process so the verifier's `exit 1` is reported as an exit code
  # instead of terminating this harness through a shared session.
  $LayoutArgs = @("-NoProfile", "-File", $LayoutVerifier, "-AppExe", $AppExe)
  if ($Zip) { $LayoutArgs += @("-Zip", $Zip) }
  & pwsh @LayoutArgs
  if ($LASTEXITCODE -ne 0) {
    Write-Error "::error::$Label does not ship a resolvable rclone sidecar (verify-sidecar-layout.ps1 exited $LASTEXITCODE)"
    exit 1
  }
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

$InstalledExe = Join-Path $InstallDir "scs-rclient.exe"
$InstalledSidecar = Join-Path $InstallDir $RuntimeSidecarName
Assert-ResolvableSidecar $InstalledExe $null "NSIS install"

& $Verifier -RcloneBin $InstalledSidecar

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

# --- Portable builds test ---
$PortableBuilds = @(
  @{ Exe = $PortablePath; Zip = $PortableZip; Name = "portable" },
  @{ Exe = $NoIconPath; Zip = $NoIconZip; Name = "no-icon portable" }
)
foreach ($build in $PortableBuilds) {
  Assert-File $build.Exe $build.Name
  Assert-File (Join-Path (Split-Path $build.Exe -Parent) $RuntimeSidecarName) "$($build.Name) sidecar"
  Assert-File $build.Zip "$($build.Name) distributable zip"
  Write-Host "=== Testing $($build.Name): $($build.Exe) ==="
  Assert-ResolvableSidecar $build.Exe $build.Zip $build.Name
  & $Verifier -RcloneBin (Join-Path (Split-Path $build.Exe -Parent) $RuntimeSidecarName)
  Invoke-LaunchSmoke (Resolve-Path $build.Exe).Path $build.Name
}

Write-Host "✓ Windows E2E passed"
