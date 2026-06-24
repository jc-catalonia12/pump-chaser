#!/usr/bin/env bash
# Bump patch in VERSION (0.1.0 → 0.1.1) and sync to Cargo + Tauri.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION_FILE="$ROOT/VERSION"

current="$(tr -d ' \n\r' < "$VERSION_FILE")"
IFS=. read -r major minor patch <<< "$current"
patch="${patch:-0}"
new_version="$major.$minor.$((patch + 1))"
printf '%s\n' "$new_version" > "$VERSION_FILE"
echo "==> Bumped version $current → $new_version"
bash "$ROOT/scripts/sync_version.sh"
