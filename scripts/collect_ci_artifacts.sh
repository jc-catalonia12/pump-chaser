#!/usr/bin/env bash
# Collect Tauri installer bundles after CI build (macOS or Windows).
# Searches the whole workspace — target dir varies by runner / CARGO_TARGET_DIR.
set -euo pipefail

PLATFORM="${1:-}"
OUT_DIR="${2:-}"

if [[ -z "$PLATFORM" || -z "$OUT_DIR" ]]; then
  echo "Usage: $0 <macos|windows> <output-dir>" >&2
  exit 2
fi

mkdir -p "$OUT_DIR"

case "$PLATFORM" in
  macos)
    PATTERNS=( -name "*.dmg" -o -name "*.app.tar.gz" -o -name "*.app.tar.gz.sig" )
    ;;
  windows)
    PATTERNS=( -name "*.msi" -o -name "*.msi.zip" -o -name "*.sig" -o -name "*setup*.exe" )
    ;;
  *)
    echo "Unknown platform: $PLATFORM" >&2
    exit 2
    ;;
esac

count=0
while IFS= read -r -d '' f; do
  # Installers only — skip intermediate build outputs outside bundle folders.
  [[ "$f" == *"/bundle/"* ]] || continue
  cp -f "$f" "$OUT_DIR/"
  echo "  + $f"
  count=$((count + 1))
done < <(
  find . \
    -type f \( "${PATTERNS[@]}" \) \
    ! -path "./.git/*" \
    ! -path "./dist/*" \
    -print0 2>/dev/null || true
)

if [ "$count" -eq 0 ]; then
  echo "::error::No ${PLATFORM} installer artifacts found in workspace"
  echo "=== bundle directories ==="
  find . -type d -name bundle ! -path "./.git/*" 2>/dev/null | head -30 || true
  echo "=== candidate installer files ==="
  find . -type f \( -name "*.dmg" -o -name "*.msi" -o -name "*setup*.exe" -o -name "*.app.tar.gz" \) \
    ! -path "./.git/*" ! -path "./dist/*" 2>/dev/null | head -40 || true
  echo "=== target trees (first 40 files) ==="
  find . -type d \( -name target -o -path "*/src-tauri/target" \) ! -path "./.git/*" 2>/dev/null | while read -r td; do
    echo "-- $td"
    find "$td" -type f 2>/dev/null | head -40 || true
  done
  exit 1
fi

echo "=== ${PLATFORM} artifacts (${count}) ==="
ls -lh "$OUT_DIR/"
