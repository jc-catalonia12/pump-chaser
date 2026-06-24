# Build Windows desktop installers (.msi + NSIS setup) with bundled ONNX model.
#
# Prerequisites:
#   - Rust 1.85+ (rustup)
#   - Visual Studio C++ Build Tools
#   - cargo install tauri-cli --version "^2.0.0"
#
# Usage (PowerShell):
#   .\scripts\build_installers.ps1
#
# Output:
#   dist\windows\        — copied .msi + setup.exe
#   desktop\src-tauri\target\release\bundle\
$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

# prepare_release_assets.sh via bash (Git Bash / WSL) or inline PowerShell fallback.
$PrepareSh = Join-Path $Root "scripts\prepare_release_assets.sh"
if (Get-Command bash -ErrorAction SilentlyContinue) {
    bash $PrepareSh
} else {
    $Staging = Join-Path $Root "release-assets\models"
    $SrcOnnx = Join-Path $Root "data\models\supervised.onnx"
    $SrcOnline = Join-Path $Root "data\models\online_model.json"

    New-Item -ItemType Directory -Force -Path $Staging | Out-Null
    Get-ChildItem $Staging -File | Remove-Item -Force

    if (-not (Test-Path $SrcOnnx)) {
        Write-Error "Missing ONNX model: $SrcOnnx`nExport or copy supervised.onnx into data\models\ first."
    }
    Copy-Item $SrcOnnx (Join-Path $Staging "supervised.onnx") -Force
    Write-Host "    staged supervised.onnx"

    if (Test-Path $SrcOnline) {
        Copy-Item $SrcOnline (Join-Path $Staging "online_model.json") -Force
        Write-Host "    staged online_model.json"
    }
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found — install Rust from https://rustup.rs/"
}

if (-not (cargo tauri --version 2>$null)) {
    Write-Host "==> Installing tauri-cli..."
    cargo install tauri-cli --version "^2.0.0" --locked
}

Write-Host "==> Building Windows installer (release)..."
Set-Location (Join-Path $Root "desktop")
cargo tauri build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Bundle = Join-Path $Root "desktop\src-tauri\target\release\bundle"
$Dist = Join-Path $Root "dist\windows"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

Get-ChildItem (Join-Path $Bundle "msi") -Filter *.msi -ErrorAction SilentlyContinue |
    Copy-Item -Destination $Dist -Force
Get-ChildItem (Join-Path $Bundle "nsis") -Filter *setup*.exe -ErrorAction SilentlyContinue |
    Copy-Item -Destination $Dist -Force

Write-Host ""
Write-Host "==> Build complete"
Write-Host "    Installers: $Dist"
Get-ChildItem $Dist | Format-Table Name, Length -AutoSize
