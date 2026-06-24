#!/usr/bin/env bash
# Backward-compatible alias — use scripts/build_release.sh
exec "$(cd "$(dirname "$0")" && pwd)/build_release.sh" "$@"
