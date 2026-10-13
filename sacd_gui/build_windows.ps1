# Build script for the SACD ISO -> DSF Extractor (Windows).
#
# Steps:
#   1. Compile the Rust helper `sacd-tool` in release mode.
#   2. Copy the resulting binary into sacd_gui/tool/ so the frozen app
#      can locate it via worker.find_tool().
#   3. Run PyInstaller in one-file mode to produce dist/SACD-Extractor.exe.
#
# Usage:  pwsh -File build_windows.ps1

$ErrorActionPreference = "Stop"

$root      = Split-Path -Parent $PSScriptRoot          # project root
$guiDir    = Join-Path $root "sacd_gui"
$toolDir   = Join-Path $root "sacd_tool"
$releaseBin = Join-Path $toolDir "target\release\sacd-tool.exe"
$bundleDir  = Join-Path $guiDir "tool"
$distDir    = Join-Path $guiDir "dist"
$iconFile   = Join-Path $root "resources\icon.ico"

Write-Host "==> Building Rust helper (sacd-tool) ..."
Push-Location $toolDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
} finally {
    Pop-Location
}

if (-not (Test-Path $releaseBin)) {
    throw "Rust binary not found at $releaseBin"
}

Write-Host "==> Staging binary into $bundleDir ..."
New-Item -ItemType Directory -Force -Path $bundleDir | Out-Null
Copy-Item $releaseBin (Join-Path $bundleDir "sacd-tool.exe") -Force

Write-Host "==> Running PyInstaller ..."
Push-Location $guiDir
try {
    Remove-Item -Recurse -Force (Join-Path $guiDir "build") -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $distDir -ErrorAction SilentlyContinue

    python -m PyInstaller `
        --noconfirm `
        --onefile `
        --windowed `
        --name "SACD-Extractor" `
        --icon $iconFile `
        --add-data "worker.py;." `
        --add-data "tool;tool" `
        app.py
    if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed" }
} finally {
    Pop-Location
}

$exe = Join-Path $distDir "SACD-Extractor.exe"
if (Test-Path $exe) {
    Write-Host ""
    Write-Host "==> DONE: $exe"
} else {
    throw "Expected output not found: $exe"
}
