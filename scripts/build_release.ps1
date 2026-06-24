# Backward-compatible alias — use scripts/build_installers.ps1
param(
    [switch]$NoBump
)
& (Join-Path $PSScriptRoot "build_installers.ps1") @PSBoundParameters
