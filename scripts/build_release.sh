#!/usr/bin/env bash
# Cross-platform entry point — builds installers for the current OS.
#
#   macOS:   ./scripts/build_release.sh
#   Linux:   ./scripts/build_release.sh   (builds if Tauri Linux deps installed)
#   Windows: use scripts\build_installers.ps1 in PowerShell
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

case "$(uname -s)" in
  Darwin|Linux)
    exec bash "$ROOT/scripts/build_installers.sh" "$@"
    ;;
  MINGW*|MSYS*|CYGWIN*)
    echo "On Windows, run: powershell -ExecutionPolicy Bypass -File scripts\\build_installers.ps1"
    exit 1
    ;;
  *)
    echo "Unsupported OS — run build_installers.sh (Unix) or build_installers.ps1 (Windows)"
    exit 1
    ;;
esac
