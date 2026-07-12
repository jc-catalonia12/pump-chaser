# Build Windows desktop installers (.msi + NSIS setup) with bundled ML models.
#
# Prerequisites:
#   - Rust 1.85+ (rustup)
#   - Visual Studio C++ Build Tools
#   - cargo install tauri-cli --version "^2.0.0"
#   - Git Bash (for version sync + asset staging), or commit data/models/*.onnx
#
# Usage (PowerShell):
#   .\scripts\build_installers.ps1 -NoBump
#   .\scripts\build_installers.ps1 -SetVersion 2.0.0
#
# Output:
#   dist\windows\        — copied .msi + setup.exe
#   desktop\src-tauri\target\release\bundle\
param(
    [switch]$NoBump,
    [string]$SetVersion = ""
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $Root

$env:CARGO_TARGET_DIR = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $Root "desktop\src-tauri\target" }

$HasBash = $null -ne (Get-Command bash -ErrorAction SilentlyContinue)

if ($HasBash) {
    if ($SetVersion) {
        Set-Content -Path "$Root\VERSION" -Value $SetVersion -NoNewline
        bash "$Root/scripts/sync_version.sh"
    } elseif ($NoBump) {
        bash "$Root/scripts/sync_version.sh"
    } else {
        bash "$Root/scripts/bump_version.sh"
    }
    $ReleaseVersion = (Get-Content "$Root/VERSION" -Raw).Trim()
    bash "$Root/scripts/prepare_release_assets.sh"
} else {
  Write-Warning "bash not found — skipping version bump and full asset staging."
  Write-Warning "Install Git for Windows or run prepare_release_assets.sh manually."
  if ($SetVersion) {
    Set-Content -Path "$Root\VERSION" -Value $SetVersion -NoNewline
  }
  $ReleaseVersion = if (Test-Path "$Root/VERSION") { (Get-Content "$Root/VERSION" -Raw).Trim() } else { "0.0.0" }

  $Staging = Join-Path $Root "release-assets\models"
  $SrcProduction = Join-Path $Root "data\models\production.onnx"
  $SrcOnnx = Join-Path $Root "data\models\supervised.onnx"
  $SrcOnline = Join-Path $Root "data\models\online_model.json"
  $SrcSchema = Join-Path $Root "data\models\feature_schema.json"

  New-Item -ItemType Directory -Force -Path $Staging | Out-Null
  Get-ChildItem $Staging -File | Remove-Item -Force

  $Primary = $null
  if (Test-Path $SrcProduction) { $Primary = $SrcProduction }
  elseif (Test-Path $SrcOnnx) { $Primary = $SrcOnnx }
  else {
    Write-Error "Missing ONNX model: need data\models\production.onnx or supervised.onnx"
  }
  Copy-Item $Primary (Join-Path $Staging "production.onnx") -Force
  Copy-Item $Primary (Join-Path $Staging "supervised.onnx") -Force
  Write-Host "    staged production.onnx + supervised.onnx"

  if (Test-Path $SrcSchema) {
    Copy-Item $SrcSchema (Join-Path $Staging "feature_schema.json") -Force
  }
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

Write-Host "==> Building Windows installer v$ReleaseVersion (release)..."
Set-Location (Join-Path $Root "desktop\src-tauri")
# Tauri 2 builds release by default — do not pass cargo's --release flag here.
cargo tauri build
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Bundle = Join-Path $Root "desktop\src-tauri\target\release\bundle"
$Dist = Join-Path $Root "dist\windows"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null
Get-ChildItem $Dist -File -ErrorAction SilentlyContinue | Remove-Item -Force

Get-ChildItem (Join-Path $Bundle "msi") -Filter *.msi -ErrorAction SilentlyContinue |
  Copy-Item -Destination $Dist -Force
Get-ChildItem (Join-Path $Bundle "nsis") -Filter *setup*.exe -ErrorAction SilentlyContinue |
  Copy-Item -Destination $Dist -Force

Write-Host ""
Write-Host "==> Build complete — v$ReleaseVersion"
Write-Host "    Installers: $Dist"
Get-ChildItem $Dist -ErrorAction SilentlyContinue | Format-Table Name, Length -AutoSize
