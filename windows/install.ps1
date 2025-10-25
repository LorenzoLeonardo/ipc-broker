# Windows counterpart of your Linux installation script
# Run this in PowerShell as Administrator

$ErrorActionPreference = "Stop"

# === Config ===
$DestDir = "$env:USERPROFILE\bin\ipc-broker"
$ExecutableName = "ipc-broker.exe"
$ServiceName = "ipc-broker"
$NssmPath = "C:\nssm\win64\nssm.exe"  # <-- Adjust path if NSSM is installed elsewhere

Write-Host "=== Uninstalling previous service and binaries... ==="

# Stop and remove existing service
if (Get-Service -Name $ServiceName -ErrorAction SilentlyContinue) {
    Write-Host "Stopping service $ServiceName..."
    Stop-Service -Name $ServiceName -Force
    Write-Host "Removing service $ServiceName..."
    & $NssmPath remove $ServiceName confirm
} else {
    Write-Host "Service $ServiceName not found, skipping..."
}

# Remove old executable and directory
if (Test-Path "$DestDir\$ExecutableName") {
    Write-Host "Removing old executable..."
    Remove-Item "$DestDir\$ExecutableName" -Force
}

if (Test-Path $DestDir) {
    Write-Host "Removing old directory..."
    Remove-Item $DestDir -Recurse -Force
}

# === Build, Check, Test ===
Write-Host "=== Building project ==="

cargo fmt --check
if ($LASTEXITCODE -ne 0) { throw "cargo fmt failed" }

cargo clippy
if ($LASTEXITCODE -ne 0) { throw "cargo clippy failed" }

cargo clippy --tests
if ($LASTEXITCODE -ne 0) { throw "cargo clippy --tests failed" }

cargo test --all-features
if ($LASTEXITCODE -ne 0) { throw "cargo test failed" }

cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# === Install new binaries ===
Write-Host "=== Installing new binaries ==="

New-Item -ItemType Directory -Force -Path $DestDir | Out-Null

Copy-Item "$PSScriptRoot\..\target\release\$ExecutableName" -Destination $DestDir

# === Register new Windows service ===
Write-Host "=== Registering new service using NSSM ==="

& $NssmPath install $ServiceName "$DestDir\$ExecutableName"
& $NssmPath set $ServiceName AppDirectory $DestDir
& $NssmPath set $ServiceName Start SERVICE_AUTO_START
& $NssmPath start $ServiceName

# Verify
Write-Host "=== Checking service status ==="
Get-Service -Name $ServiceName
Write-Host "Installation complete ✅"
