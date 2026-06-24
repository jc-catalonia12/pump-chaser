#!/usr/bin/env bash
# Propagate repo VERSION to Cargo.toml files and tauri.conf.json.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION_FILE="$ROOT/VERSION"

if [[ ! -f "$VERSION_FILE" ]]; then
  echo "ERROR: Missing $VERSION_FILE" >&2
  exit 1
fi

VERSION="$(tr -d ' \n\r' < "$VERSION_FILE")"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "ERROR: VERSION must be semver major.minor.patch (got: $VERSION)" >&2
  exit 1
fi

if [[ "$(uname -s)" == "Darwin" ]]; then
  sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" "$ROOT/Cargo.toml"
  sed -i '' "s/^version = \".*\"/version = \"$VERSION\"/" "$ROOT/desktop/src-tauri/Cargo.toml"
else
  sed -i "s/^version = \".*\"/version = \"$VERSION\"/" "$ROOT/Cargo.toml"
  sed -i "s/^version = \".*\"/version = \"$VERSION\"/" "$ROOT/desktop/src-tauri/Cargo.toml"
fi

python3 - "$ROOT/desktop/src-tauri/tauri.conf.json" "$VERSION" <<'PY'
import json, sys
path, version = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as f:
    data = json.load(f)
data["version"] = version
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY

echo "==> Version synced to $VERSION"
