<#
.SYNOPSIS
  Assert that a packaged Windows build can resolve its rclone sidecar at runtime.

.DESCRIPTION
  Tauri's shell plugin turns the frontend call

      Command.sidecar("binaries/rclone-sidecar")

  into

      <directory of the running app executable>\rclone-sidecar.exe

    * `tauri-plugin-shell`'s `commands.rs` matches the JS-supplied `program`
      verbatim against `bundle.externalBin`, so the configured entry must stay
      "binaries/rclone-sidecar".
    * `scope.rs::prepare_sidecar` reduces that entry to its last path component,
      which drops the "binaries/" prefix but does NOT touch a "-<target-triple>"
      suffix -> "rclone-sidecar".
    * `process/mod.rs::relative_command_path` joins `current_exe()`'s parent
      with that name and appends ".exe" on Windows.

  So a Windows build that ships only `rclone-sidecar-x86_64-pc-windows-msvc.exe`
  next to `scs-rclient.exe` will fail at runtime with
  "No usable rclone binary found (sidecar and system both unavailable)".

.PARAMETER AppExe
  Path to the packaged app executable, or to a directory that is expected to
  contain the app executable's sidecar.

.PARAMETER ExpectedVersion
  Expected rclone version, e.g. "1.74.3" or "v1.74.3". Defaults to
  $env:TARGET_RCLONE_VERSION; the assertion is skipped when empty.

.PARAMETER Zip
  Optional distributable zip that is supposed to carry the app executable and
  its sidecar side by side. When provided, the archive contents are asserted
  too, because extracting the zip is the only way a user obtains the pair.
#>
param(
  [Parameter(Mandatory = $true)][string]$AppExe,
  [string]$ExpectedVersion = $env:TARGET_RCLONE_VERSION,
  [int]$MinSidecarBytes = 1048576,
  [string]$Zip
)

$ErrorActionPreference = "Stop"
# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
Remove-Item Env:RCLONE_VERSION -ErrorAction SilentlyContinue

# The name Tauri resolves at runtime, with the target-triple suffix stripped.
$SidecarName = "rclone-sidecar.exe"

if (Test-Path -LiteralPath $AppExe -PathType Container) {
  # Useful for build trees where the app binary cannot be executed.
  $ExeDir = (Resolve-Path -LiteralPath $AppExe).Path
  $AppLabel = "directory $AppExe"
} else {
  if (-not (Test-Path -LiteralPath $AppExe)) {
    Write-Error "::error::app executable not found: $AppExe"
    exit 1
  }
  $AppLabel = (Resolve-Path -LiteralPath $AppExe).Path
  $ExeDir = Split-Path -Parent $AppLabel
}

$Sidecar = Join-Path $ExeDir $SidecarName

Write-Host "=== Sidecar layout check ==="
Write-Host "app:            $AppLabel"
Write-Host "exe directory:  $ExeDir"
Write-Host "expected name:  $SidecarName (triple suffix stripped, no binaries/ prefix)"
Write-Host "expected path:  $Sidecar"

if (-not (Test-Path -LiteralPath $Sidecar)) {
  Write-Error "::error::rclone sidecar missing at $Sidecar -- the app would report 'No usable rclone binary found (sidecar and system both unavailable)'"
  exit 1
}
if (-not (Test-Path -LiteralPath $Sidecar -PathType Leaf)) {
  Write-Error "::error::rclone sidecar at $Sidecar is not a regular file"
  exit 1
}

$Size = (Get-Item -LiteralPath $Sidecar).Length
Write-Host "sidecar size:   $Size bytes"
if ($Size -lt $MinSidecarBytes) {
  Write-Error "::error::sidecar at $Sidecar is only $Size bytes (< $MinSidecarBytes): looks like a build.rs placeholder or a truncated download"
  exit 1
}

$Reported = (& $Sidecar version 2>$null | Select-Object -First 1)
Write-Host "sidecar reports: $(if ($Reported) { $Reported } else { '<no output>' })"
if ($ExpectedVersion) {
  $Wanted = $ExpectedVersion.TrimStart('v')
  if ($Reported -notlike "*rclone v$Wanted*") {
    Write-Error "::error::sidecar at $Sidecar does not report the expected version v$Wanted"
    exit 1
  }
} elseif (-not $Reported) {
  Write-Error "::error::sidecar at $Sidecar produced no version output -- the binary is not runnable"
  exit 1
}

Write-Host "OK sidecar resolvable at the path the Tauri shell plugin actually uses"

if ($Zip) {
  Write-Host ""
  Write-Host "=== Distributable zip check ==="
  if (-not (Test-Path -LiteralPath $Zip -PathType Leaf)) {
    Write-Error "::error::distributable zip not found: $Zip"
    exit 1
  }
  $AppEntryName = Split-Path -Leaf $AppLabel
  Write-Host "zip:            $Zip"
  Write-Host "expected entries at zip root: $AppEntryName, $SidecarName"

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $Archive = [System.IO.Compression.ZipFile]::OpenRead((Resolve-Path -LiteralPath $Zip).Path)
  try {
    $Names = $Archive.Entries | ForEach-Object { $_.Name }
    Write-Host "zip entries:    $($Names -join ', ')"

    # Tauri resolves the sidecar as a sibling of the app executable, so both
    # files must sit at the same level of the extracted archive.
    foreach ($Expected in @($AppEntryName, $SidecarName)) {
      $Entry = $Archive.Entries | Where-Object { $_.Name -eq $Expected -and $_.FullName -notmatch '/' } | Select-Object -First 1
      if (-not $Entry) {
        Write-Error "::error::$Zip has no '$Expected' at its root; after extraction the app could not resolve its sidecar"
        exit 1
      }
    }
    $SidecarEntry = $Archive.Entries | Where-Object { $_.Name -eq $SidecarName -and $_.FullName -notmatch '/' } | Select-Object -First 1
    Write-Host "zip sidecar size: $($SidecarEntry.Length) bytes"
    if ($SidecarEntry.Length -lt $MinSidecarBytes) {
      Write-Error "::error::$Zip contains a $($SidecarEntry.Length)-byte '$SidecarName' (< $MinSidecarBytes): looks like a placeholder"
      exit 1
    }
  } finally {
    $Archive.Dispose()
  }
  Write-Host "OK distributable zip carries the app and its sidecar together"
}
