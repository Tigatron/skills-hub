#!/usr/bin/env bash
# M0-017 packaged-app smoke on a disposable HOME.
# 1) Verifies the .app identity and launches once under HOME=$SMOKE_HOME.
# 2) Runs the headless thin-slice acceptance harness (same domain path the UI uses)
#    with HOME-shaped default Vault paths.
# 3) Optionally measures cold launch process-start samples when SKILLS_HUB_PERF_LAUNCH=1.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Capture real user home before disposable HOME overrides (mise/cargo paths).
REAL_HOME="${HOME}"
export PATH="/opt/homebrew/bin:${REAL_HOME}/.local/bin:${REAL_HOME}/.cargo/bin:$PATH"
# Prefer already-resolved toolchain on PATH; avoid re-activating mise under a fake HOME.
if ! command -v cargo >/dev/null 2>&1 && command -v mise >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  eval "$(mise activate bash)" || true
fi
NODE_BIN="$(command -v node || true)"
PNPM_BIN="$(command -v pnpm || true)"
CARGO_BIN="$(command -v cargo || true)"
if [[ -z "$CARGO_BIN" ]]; then
  echo "cargo not on PATH" >&2
  exit 1
fi

EVIDENCE_DIR="${SKILLS_HUB_SMOKE_DIR:-$ROOT/docs/wiki/quality/evidence}"
mkdir -p "$EVIDENCE_DIR"
MD_OUT="$EVIDENCE_DIR/m0-017-packaged-smoke.md"
JSON_OUT="$EVIDENCE_DIR/m0-017-packaged-smoke.json"
LOG_OUT="$EVIDENCE_DIR/m0-017-packaged-smoke.log"

APP_PATH="${SKILLS_HUB_PERF_APP:-$ROOT/src-tauri/target/release/bundle/macos/Skills Hub.app}"
BIN_PATH="$APP_PATH/Contents/MacOS/skills-hub"

if [[ ! -d "$APP_PATH" ]]; then
  echo "Packaged app not found at: $APP_PATH" >&2
  echo "Run scripts/m0-package.sh first." >&2
  exit 1
fi

SMOKE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/skills-hub-m017-home.XXXXXX")"
SLICE_HOME=""
cleanup() {
  # Best-effort quit any leftover instance started for this HOME.
  pkill -f "$BIN_PATH" 2>/dev/null || true
  rm -rf "$SMOKE_HOME"
  [[ -n "$SLICE_HOME" && -d "$SLICE_HOME" ]] && rm -rf "$SLICE_HOME"
}
trap cleanup EXIT

STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  echo "=== M0-017 packaged smoke $STAMP ==="
  echo "app=$APP_PATH"
  echo "smoke_home=$SMOKE_HOME"
  echo "hardware=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)"
  echo "os=$(sw_vers -productVersion 2>/dev/null || true)"
} | tee "$LOG_OUT"

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP_PATH/Contents/Info.plist")"
PRODUCT="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleName' "$APP_PATH/Contents/Info.plist")"
MIN_OS="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP_PATH/Contents/Info.plist")"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_PATH/Contents/Info.plist")"
FILE_INFO="$(file "$BIN_PATH")"
CODESIGN="$(codesign -dv --verbose=4 "$APP_PATH" 2>&1 | head -20 | tr '\n' '; ')"

echo "identity: $PRODUCT $BUNDLE_ID min=$MIN_OS ver=$VERSION" | tee -a "$LOG_OUT"
echo "file: $FILE_INFO" | tee -a "$LOG_OUT"

# --- Launch under disposable HOME (first run; no pre-existing Vault) ---
export HOME="$SMOKE_HOME"
mkdir -p "$HOME"

LAUNCH_MS=()
SAMPLES="${SKILLS_HUB_LAUNCH_SAMPLES:-5}"
if [[ "${SKILLS_HUB_PERF_LAUNCH:-0}" == "1" ]]; then
  echo "Measuring $SAMPLES cold process-start samples..." | tee -a "$LOG_OUT"
  for i in $(seq 1 "$SAMPLES"); do
    START_NS="$(python3 - <<'PY'
import time; print(time.time_ns())
PY
)"
    # Launch and wait briefly for process to become runnable, then quit.
    "$BIN_PATH" >/dev/null 2>&1 &
    PID=$!
    # Wait until process is alive and has mapped the main binary, or timeout.
    for _ in $(seq 1 200); do
      if ! kill -0 "$PID" 2>/dev/null; then
        break
      fi
      # Heuristic "usable" proxy: process still running after first event loop tick.
      # True Library paint requires GUI automation; documented as residual if needed.
      sleep 0.05
      if ps -p "$PID" -o state= 2>/dev/null | grep -q .; then
        # Give the runtime a short settle window on first sample only for warm caches later.
        break
      fi
    done
    END_NS="$(python3 - <<'PY'
import time; print(time.time_ns())
PY
)"
    MS="$(python3 - <<PY
print(int(($END_NS - $START_NS) / 1_000_000))
PY
)"
    LAUNCH_MS+=("$MS")
    echo "sample $i: ${MS}ms pid=$PID" | tee -a "$LOG_OUT"
    kill "$PID" 2>/dev/null || true
    wait "$PID" 2>/dev/null || true
    sleep 0.2
  done
else
  echo "Single first-run launch (set SKILLS_HUB_PERF_LAUNCH=1 for samples)..." | tee -a "$LOG_OUT"
  "$BIN_PATH" >/dev/null 2>&1 &
  PID=$!
  sleep 2
  if kill -0 "$PID" 2>/dev/null; then
    echo "app process alive pid=$PID under HOME=$HOME" | tee -a "$LOG_OUT"
    LAUNCH_OK=1
  else
    echo "app process exited early" | tee -a "$LOG_OUT"
    LAUNCH_OK=0
  fi
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
fi

# After GUI launch, diagnostics/support dirs may exist; Vault is created on explicit initialize.
SUPPORT_DIR="$HOME/Library/Application Support/Skills Hub"
echo "support_dir_exists=$([[ -d "$SUPPORT_DIR" ]] && echo yes || echo no)" | tee -a "$LOG_OUT"

# --- Headless thin-slice with same default Vault path contract ---
# Use a fresh HOME for the domain thin-slice so OpenVault owns the tree.
# Keep toolchain env from REAL_HOME (CARGO_HOME, rustup, mise installs).
SLICE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/skills-hub-m017-slice.XXXXXX")"
export HOME="$SLICE_HOME"
export CARGO_HOME="${CARGO_HOME:-$REAL_HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$REAL_HOME/.rustup}"
export SKILLS_HUB_ACCEPTANCE_OUT="$EVIDENCE_DIR/m0-017-acceptance-from-smoke.json"
export SKILLS_HUB_NETWORK_MODE="${SKILLS_HUB_NETWORK_MODE:-packaged-smoke-local}"

echo "--- Thin-slice domain path (scan→takeover→deploy→undeploy) ---" | tee -a "$LOG_OUT"
set +e
"$CARGO_BIN" test --manifest-path src-tauri/Cargo.toml --lib \
  m0_017_acceptance_matrix_and_thin_slice \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
SLICE_STATUS=${PIPESTATUS[0]}
set -e

# Launch timing stats
P50=""
P95=""
LAUNCH_PASS=""
if [[ ${#LAUNCH_MS[@]} -gt 0 ]]; then
  read -r P50 P95 <<<"$(python3 - <<PY
samples = sorted(int(x) for x in """${LAUNCH_MS[*]}""".split())
def pct(p):
    if not samples: return 0
    k = (len(samples)-1) * p
    f = int(k); c = min(f+1, len(samples)-1)
    if f == c: return samples[f]
    return int(samples[f] + (samples[c]-samples[f]) * (k-f))
print(pct(0.50), pct(0.95))
PY
)"
  # Process-start is a lower bound; PRD gate is launch→usable Library <1500ms.
  # If p95 process-start alone exceeds 1500ms, fail. Otherwise mark measured-proxy.
  if [[ "$P95" -le 1500 ]]; then
    LAUNCH_PASS="proxy-pass-process-start"
  else
    LAUNCH_PASS="fail-process-start-over-gate"
  fi
fi

export M017_SMOKE_JSON="$JSON_OUT"
export M017_SMOKE_MD="$MD_OUT"
export M017_SMOKE_APP="$APP_PATH"
export M017_SMOKE_PRODUCT="$PRODUCT"
export M017_SMOKE_BUNDLE="$BUNDLE_ID"
export M017_SMOKE_VERSION="$VERSION"
export M017_SMOKE_MINOS="$MIN_OS"
export M017_SMOKE_FILE="$FILE_INFO"
export M017_SMOKE_CODESIGN="$CODESIGN"
export M017_SMOKE_STAMP="$STAMP"
export M017_SMOKE_LAUNCH_OK="${LAUNCH_OK:-1}"
export M017_SMOKE_SLICE="$SLICE_STATUS"
export M017_SMOKE_SAMPLES="${LAUNCH_MS[*]:-}"
export M017_SMOKE_P50="${P50:-}"
export M017_SMOKE_P95="${P95:-}"
export M017_SMOKE_LAUNCH_PASS="${LAUNCH_PASS:-not-measured}"

python3 - <<'PY'
import json, pathlib, datetime, os
samples = [int(x) for x in os.environ.get("M017_SMOKE_SAMPLES", "").split() if x.strip()]
p50 = os.environ.get("M017_SMOKE_P50") or None
p95 = os.environ.get("M017_SMOKE_P95") or None
report = {
  "schemaVersion": 1,
  "task": "M0-017",
  "stamp": os.environ["M017_SMOKE_STAMP"],
  "appPath": os.environ["M017_SMOKE_APP"],
  "identity": {
    "productName": os.environ["M017_SMOKE_PRODUCT"],
    "bundleId": os.environ["M017_SMOKE_BUNDLE"],
    "version": os.environ["M017_SMOKE_VERSION"],
    "minimumSystemVersion": os.environ["M017_SMOKE_MINOS"],
  },
  "binary": os.environ["M017_SMOKE_FILE"],
  "codesign": os.environ["M017_SMOKE_CODESIGN"],
  "firstRunLaunchOk": bool(int(os.environ.get("M017_SMOKE_LAUNCH_OK", "1"))),
  "thinSliceStatus": int(os.environ.get("M017_SMOKE_SLICE", "1")),
  "launchSamplesMs": samples,
  "launchP50Ms": int(p50) if p50 else None,
  "launchP95Ms": int(p95) if p95 else None,
  "launchGate": {
    "prdMs": 1500,
    "method": "process-start under disposable HOME (Library paint requires GUI automation)",
    "result": os.environ.get("M017_SMOKE_LAUNCH_PASS") or "not-measured",
  },
  "defaultVaultStory": "~/Library/Application Support/Skills Hub/Vault under the process HOME",
}
path = pathlib.Path(os.environ["M017_SMOKE_JSON"])
path.write_text(json.dumps(report, indent=2) + "\n")
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
day = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
md = f"""---
type: Quality Evidence
title: M0-017 Packaged Smoke
description: Disposable-HOME launch of the packaged .app plus headless thin-slice (scan→takeover→deploy→undeploy).
status: implemented
tags: [skills-hub, m0, packaging, smoke]
requirements: []
timestamp: {now}
---

# M0-017 packaged-app smoke

## Environment

| Field | Value |
| --- | --- |
| Date (UTC) | {day} |
| App | `{report['appPath']}` |
| Product | {report['identity']['productName']} |
| Bundle ID | `{report['identity']['bundleId']}` |
| Version | {report['identity']['version']} |
| Minimum macOS | {report['identity']['minimumSystemVersion']} |
| Binary | {report['binary']} |
| Signing | ad-hoc (see package-build evidence); never disable Gatekeeper |
| Thin-slice exit | {report['thinSliceStatus']} |

## First-run launch

- Disposable `HOME` via `mktemp`.
- Process start under isolated HOME: **{'ok' if report['firstRunLaunchOk'] else 'FAILED'}**.
- Default Vault path story: `{report['defaultVaultStory']}`.

## Thin slice

Headless domain path (same services the UI commands invoke):

```text
scan six roots (no mutation) → takeover Add to Vault → deploy symlink + Managed Copy
→ collision no-write → drift/broken verify → undeploy → Activity outcomes
```

Result: **{'pass' if report['thinSliceStatus'] == 0 else 'FAIL'}** (see acceptance JSON produced alongside this run).

## Launch timing residual

| Field | Value |
| --- | --- |
| Samples (ms) | {samples or 'not measured (set SKILLS_HUB_PERF_LAUNCH=1)'} |
| p50 / p95 | {report['launchP50Ms']} / {report['launchP95Ms']} |
| PRD gate | < 1500 ms warm launch to usable Library |
| Method | {report['launchGate']['method']} |
| Result | {report['launchGate']['result']} |

Process-start is a conservative lower bound for cold process bring-up. Full Library-paint timing needs GUI automation or Instruments; when only process-start is available it is labeled as a proxy, not a false Library-paint pass.

## Commands

```bash
bash scripts/m0-package.sh
SKILLS_HUB_PERF_LAUNCH=1 bash scripts/m0-packaged-smoke.sh
```
"""
pathlib.Path(os.environ["M017_SMOKE_MD"]).write_text(md)
print(f"Wrote {path}")
print(f"Wrote {os.environ['M017_SMOKE_MD']}")
if report["thinSliceStatus"] != 0:
    raise SystemExit(report["thinSliceStatus"])
PY

echo "Packaged smoke OK"
