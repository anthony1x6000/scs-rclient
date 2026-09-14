# Shared helper: verify an rclone binary against the live demo WebDAV server (Windows).
# Usage: verify-webdav-endpoint.ps1 -RcloneBin <path-to-rclone.exe>
param(
  [Parameter(Mandatory = $true)]
  [string]$RcloneBin
)

$ErrorActionPreference = "Stop"
# Prevent ambient RCLONE_VERSION from colliding with rclone's boolean --version flag
Remove-Item env:RCLONE_VERSION -ErrorAction SilentlyContinue

if (-not (Test-Path $RcloneBin)) {
  Write-Error "::error::rclone binary not found: $RcloneBin"
  exit 1
}

$WebdavUrl = "https://a.ocv.me/pub/demo/docs/"
$ExpectedFiles = @("5m-iceblaze.ans", "LDA-MIST.ANS", "README.md")
$ExpectedReadmeHeader = "this folder contains stolen content;"

Write-Host "=== Verifying WebDAV endpoint with: $RcloneBin ==="
& $RcloneBin version
if ($LASTEXITCODE -ne 0) { Write-Error "::error::rclone version failed"; exit 1 }

Write-Host "=== Listing remote (:webdav:) ==="
$LsfOutput = & $RcloneBin lsf ":webdav:" --webdav-url $WebdavUrl --webdav-vendor other
if ($LASTEXITCODE -ne 0) { Write-Error "::error::rclone lsf exited with code $LASTEXITCODE"; exit 1 }
Write-Host $LsfOutput

foreach ($expected in $ExpectedFiles) {
  if ($LsfOutput -notmatch [regex]::Escape($expected)) {
    Write-Error "::error::Expected file '$expected' not found in rclone lsf output!"
    exit 1
  }
  Write-Host "Found expected file: $expected"
}

Write-Host "=== Round-trip: copying README.md ==="
$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("webdav-test-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null
try {
  & $RcloneBin copy ":webdav:README.md" $TmpDir --webdav-url $WebdavUrl --webdav-vendor other
  if ($LASTEXITCODE -ne 0) { Write-Error "::error::rclone copy exited with code $LASTEXITCODE"; exit 1 }
  $ReadmePath = Join-Path $TmpDir "README.md"
  if (-not (Test-Path $ReadmePath)) { Write-Error "::error::README.md was not downloaded to $TmpDir"; exit 1 }
  $Content = Get-Content -Raw $ReadmePath
  if ($Content -notmatch [regex]::Escape($ExpectedReadmeHeader)) {
    Write-Error "::error::Downloaded README.md missing expected header '$ExpectedReadmeHeader'"
    exit 1
  }
} finally {
  Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}

Write-Host "✓ WebDAV endpoint verification passed ($RcloneBin)"
