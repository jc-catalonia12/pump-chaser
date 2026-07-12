#!/usr/bin/env python3
"""
Generate latest.json for the Tauri auto-updater.

Called by .github/workflows/release.yml after both macOS and Windows
artifacts have been downloaded.  Reads the *.sig files that `cargo tauri build`
produces alongside the update bundles and writes a manifest in the format
expected by tauri-plugin-updater v2.

Usage:
  python3 scripts/gen_update_manifest.py \
      --version 0.2.0          \
      --tag     v0.2.0         \
      --repo    OWNER/REPO     \
      --macos   dist/release-macos   \
      --windows dist/release-windows \
      --out     dist/latest.json
"""

import argparse
import datetime
import json
import pathlib
import re
import sys


RELEASE_BASE = "https://github.com/{repo}/releases/download/{tag}/{filename}"


def find_artifact(directory: pathlib.Path, patterns: list[str]) -> pathlib.Path | None:
    """Return the first file in *directory* whose name matches one of the glob patterns."""
    for pat in patterns:
        matches = sorted(directory.glob(pat))
        if matches:
            return matches[0]
    return None


def read_sig(path: pathlib.Path) -> str:
    """Read a .sig file and return its content stripped of whitespace."""
    sig_path = path.with_suffix(path.suffix + ".sig")
    if not sig_path.exists():
        # Tauri sometimes appends .sig directly (e.g. .app.tar.gz.sig):
        # the file itself has double extension, try the raw .sig path.
        sig_path2 = pathlib.Path(str(path) + ".sig")
        if sig_path2.exists():
            return sig_path2.read_text().strip()
        print(f"  WARNING: signature not found for {path.name}", file=sys.stderr)
        return ""
    return sig_path.read_text().strip()


def artifact_url(repo: str, tag: str, filename: str) -> str:
    return RELEASE_BASE.format(repo=repo, tag=tag, filename=filename)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True, help="Semver version without leading v (e.g. 0.2.0)")
    ap.add_argument("--tag",     required=True, help="Git tag (e.g. v0.2.0)")
    ap.add_argument("--repo",    required=True, help="GitHub OWNER/REPO")
    ap.add_argument("--macos",   required=True, help="Directory containing macOS artifacts")
    ap.add_argument("--windows", required=True, help="Directory containing Windows artifacts")
    ap.add_argument("--out",     default="latest.json", help="Output path for latest.json")
    args = ap.parse_args()

    macos_dir   = pathlib.Path(args.macos)
    windows_dir = pathlib.Path(args.windows)
    out_path    = pathlib.Path(args.out)

    platforms: dict = {}
    missing: list[str] = []

    # ── macOS x86_64 ──────────────────────────────────────────────────────────
    # Tauri update bundles for macOS are .app.tar.gz (not the .dmg).
    mac_x64 = find_artifact(macos_dir, ["*x64*.app.tar.gz", "*x86_64*.app.tar.gz"])
    if mac_x64:
        sig = read_sig(mac_x64)
        platforms["darwin-x86_64"] = {
            "url":       artifact_url(args.repo, args.tag, mac_x64.name),
            "signature": sig,
        }
        print(f"  [darwin-x86_64] {mac_x64.name}")
    else:
        missing.append("darwin-x86_64 (.app.tar.gz)")

    # ── macOS aarch64 (Apple Silicon) ─────────────────────────────────────────
    mac_arm = find_artifact(macos_dir, ["*aarch64*.app.tar.gz", "*arm64*.app.tar.gz"])
    if mac_arm:
        sig = read_sig(mac_arm)
        platforms["darwin-aarch64"] = {
            "url":       artifact_url(args.repo, args.tag, mac_arm.name),
            "signature": sig,
        }
        print(f"  [darwin-aarch64] {mac_arm.name}")
    else:
        print("  [darwin-aarch64] not found — skipping (arm build optional)", file=sys.stderr)

    # ── Windows x86_64 — prefer .msi.zip (smaller delta), fall back to NSIS ──
    win_x64 = find_artifact(windows_dir, ["*.msi.zip", "*_x64_en-US.msi.zip", "*setup*.exe.zip"])
    if win_x64:
        sig = read_sig(win_x64)
        platforms["windows-x86_64"] = {
            "url":       artifact_url(args.repo, args.tag, win_x64.name),
            "signature": sig,
        }
        print(f"  [windows-x86_64] {win_x64.name}")
    else:
        missing.append("windows-x86_64 (.msi.zip)")

    if missing:
        print(f"\nWARNING: some update targets could not be found: {missing}", file=sys.stderr)
        print("  The manifest will be partial.  Check that signing was enabled "
              "(TAURI_SIGNING_PRIVATE_KEY must be set in CI).", file=sys.stderr)

    if not platforms:
        print(
            "WARNING: no signed update artifacts found — skipping latest.json "
            "(installers can still be published without in-app updater).",
            file=sys.stderr,
        )
        sys.exit(0)

    manifest = {
        "version":  args.version,
        "notes":    f"MEXC Trading Bot v{args.version} — see GitHub Releases for details.",
        "pub_date": datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%S.000Z"),
        "platforms": platforms,
    }

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(manifest, indent=2))
    print(f"\nWrote {out_path}")


if __name__ == "__main__":
    main()
