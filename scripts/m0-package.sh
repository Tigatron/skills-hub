#!/usr/bin/env bash
# M0-017 reproducible Apple Silicon package build.
# Produces Skills Hub.app (and DMG when Tauri/hdiutil succeed) under
# src-tauri/target/release/bundle/. Documents ad-hoc signing; never suggests
# disabling Gatekeeper.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export PATH="/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
if command -v mise >/dev/null 2>&1; then
  eval "$(mise activate bash)"
fi

EVIDENCE_DIR="${SKILLS_HUB_PACKAGE_EVIDENCE_DIR:-$ROOT/docs/wiki/quality/evidence}"
mkdir -p "$EVIDENCE_DIR"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REPORT="$EVIDENCE_DIR/m0-017-package-build.json"
LOG="$EVIDENCE_DIR/m0-017-package-build.log"

arch_expected="arm64"
arch_actual="$(uname -m)"
if [[ "$arch_actual" != "$arch_expected" ]]; then
  echo "warning: packaging on $arch_actual (M0 local package target is Apple Silicon arm64)" >&2
fi

{
  echo "=== M0-017 package build $STAMP ==="
  echo "cwd: $ROOT"
  echo "arch: $arch_actual"
  echo "macos: $(sw_vers -productVersion 2>/dev/null || true) ($(sw_vers -buildVersion 2>/dev/null || true))"
  echo "hardware: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)"
  echo "node: $(command -v node) $(node -v 2>/dev/null || true)"
  echo "pnpm: $(command -v pnpm) $(pnpm -v 2>/dev/null || true)"
  echo "rustc: $(command -v rustc) $(rustc -V 2>/dev/null || true)"
  echo "cargo: $(command -v cargo) $(cargo -V 2>/dev/null || true)"
  echo "tauri-cli: $(pnpm exec tauri --version 2>/dev/null || true)"
} | tee "$LOG"

echo "Building release package (pnpm build → tauri build)..." | tee -a "$LOG"
set +e
pnpm build 2>&1 | tee -a "$LOG"
BUILD_STATUS=${PIPESTATUS[0]}
set -e

APP_PATH="$ROOT/src-tauri/target/release/bundle/macos/Skills Hub.app"
DMG_PATH="$(find "$ROOT/src-tauri/target/release/bundle/dmg" -name '*.dmg' 2>/dev/null | head -1 || true)"
BIN_PATH="$APP_PATH/Contents/MacOS/skills-hub"

identity_ok=false
bundle_id=""
product_name=""
min_macos=""
version=""
codesign_status="missing"
file_info=""
if [[ -d "$APP_PATH" ]]; then
  identity_ok=true
  bundle_id="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
  product_name="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleName' "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
  min_macos="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
  version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
  codesign_status="$(codesign -dv --verbose=4 "$APP_PATH" 2>&1 | tr '\n' '|' || true)"
  file_info="$(file "$BIN_PATH" 2>/dev/null || true)"
fi

python3 - <<PY | tee -a "$LOG"
import json, os, pathlib, time
report = {
  "schemaVersion": 1,
  "task": "M0-017",
  "stamp": "$STAMP",
  "buildStatus": int("$BUILD_STATUS"),
  "arch": "$arch_actual",
  "hardware": """$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)""".strip(),
  "os": """$(sw_vers -productVersion 2>/dev/null || true)""".strip(),
  "osBuild": """$(sw_vers -buildVersion 2>/dev/null || true)""".strip(),
  "toolchains": {
    "node": """$(node -v 2>/dev/null || true)""".strip(),
    "pnpm": """$(pnpm -v 2>/dev/null || true)""".strip(),
    "rustc": """$(rustc -V 2>/dev/null || true)""".strip(),
    "cargo": """$(cargo -V 2>/dev/null || true)""".strip(),
    "tauriCli": """$(pnpm exec tauri --version 2>/dev/null || true)""".strip(),
  },
  "commands": ["pnpm build"],
  "artifacts": {
    "app": "$APP_PATH" if pathlib.Path("$APP_PATH").exists() else None,
    "dmg": "$DMG_PATH" if "$DMG_PATH" else None,
    "binary": "$BIN_PATH" if pathlib.Path("$BIN_PATH").exists() else None,
  },
  "identity": {
    "productName": "$product_name",
    "bundleId": "$bundle_id",
    "version": "$version",
    "minimumSystemVersion": "$min_macos",
    "defaultVaultPath": "~/Library/Application Support/Skills Hub/Vault",
    "applicationSupportPath": "~/Library/Application Support/Skills Hub",
  },
  "signing": {
    "configuredIdentity": "-",
    "status": "ad-hoc",
    "codesign": """$codesign_status""",
    "notarized": False,
    "developerId": False,
    "gatekeeperAdvice": "Do not disable Gatekeeper. Ad-hoc local builds are for developer machines and CI evidence only. Future seam: Developer ID + notarization.",
  },
  "binary": {
    "file": """$file_info""",
    "universalBinary": False,
    "intelRuntimeValidated": False,
  },
  "notes": [
    "M0 ships a native Apple Silicon package; x86_64 is source/compile-check only.",
    "DMG may be absent if hdiutil packaging fails; .app alone is acceptable M0 evidence.",
  ],
}
path = pathlib.Path("$REPORT")
path.write_text(json.dumps(report, indent=2) + "\n")
print(f"Wrote {path}")
print(json.dumps(report["artifacts"], indent=2))
print(json.dumps(report["identity"], indent=2))
PY

if [[ "$BUILD_STATUS" -ne 0 ]]; then
  echo "package build failed with status $BUILD_STATUS" >&2
  exit "$BUILD_STATUS"
fi
if [[ ! -d "$APP_PATH" ]]; then
  echo "expected app bundle missing: $APP_PATH" >&2
  exit 1
fi
if [[ "$bundle_id" != "com.terrylan.skillshub" ]]; then
  echo "bundle id mismatch: $bundle_id" >&2
  exit 1
fi
if [[ "$product_name" != "Skills Hub" ]]; then
  echo "product name mismatch: $product_name" >&2
  exit 1
fi
if [[ "$min_macos" != "14.0" ]]; then
  echo "minimum macOS mismatch: $min_macos" >&2
  exit 1
fi

echo "Package OK: $APP_PATH"
[[ -n "$DMG_PATH" ]] && echo "DMG: $DMG_PATH" || echo "DMG: not produced (app-only is acceptable for M0)"
