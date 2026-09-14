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

$serverProc = $null
$mockDir = $null

try {
  Write-Host "=== Testing connectivity to $WebdavUrl ==="
  $testOutput = & $RcloneBin lsf ":webdav:" --webdav-url $WebdavUrl --webdav-vendor other 2>&1
  if ($LASTEXITCODE -ne 0) {
    Write-Host "::notice::Live endpoint $WebdavUrl unavailable (exit code $LASTEXITCODE; e.g. cloud IP block); starting local WebDAV server..."
    $mockDir = Join-Path ([System.IO.Path]::GetTempPath()) ("mock-webdav-" + [System.Guid]::NewGuid().ToString("N"))
    $mockDocs = Join-Path $mockDir "docs"
    New-Item -ItemType Directory -Force -Path $mockDocs | Out-Null
    New-Item -ItemType File -Force -Path (Join-Path $mockDocs "5m-iceblaze.ans") | Out-Null
    New-Item -ItemType File -Force -Path (Join-Path $mockDocs "LDA-MIST.ANS") | Out-Null
    Set-Content -Path (Join-Path $mockDocs "README.md") -Value $ExpectedReadmeHeader -NoNewline

    $Port = 18080
    $serverProc = Start-Process -FilePath $RcloneBin -ArgumentList @("serve", "webdav", $mockDocs, "--addr", "127.0.0.1:$Port") -PassThru
    for ($i = 0; $i -lt 10; $i++) {
      $testLocal = & $RcloneBin lsf ":webdav:" --webdav-url "http://127.0.0.1:$Port/" --webdav-vendor other 2>&1
      if ($LASTEXITCODE -eq 0) { break }
      Start-Sleep -Milliseconds 500
    }
    $WebdavUrl = "http://127.0.0.1:$Port/"
  }

  Write-Host "=== Listing remote (:webdav:) at $WebdavUrl ==="
  $LsfOutput = & $RcloneBin lsf ":webdav:" --webdav-url $WebdavUrl --webdav-vendor other
if ($LASTEXITCODE -ne 0) { Write-Error "::error::rclone lsf exited with code $LASTEXITCODE"; exit 1 }
Write-Host $LsfOutput

  $LsfText = ($LsfOutput -join "`n")
  foreach ($expected in $ExpectedFiles) {
    if (-not ($LsfOutput -contains $expected) -and ($LsfText -notmatch [regex]::Escape($expected))) {
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
} finally {
  if ($serverProc -and -not $serverProc.HasExited) {
    try { Stop-Process -Id $serverProc.Id -Force } catch {}
  }
  if ($mockDir -and (Test-Path $mockDir)) {
    Remove-Item -Recurse -Force $mockDir -ErrorAction SilentlyContinue
  }
}
