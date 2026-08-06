#!/usr/bin/env bash
# M0-016 performance harness wrapper.
# Runs the committed reference-scale Rust gate and optionally writes JSON evidence.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$PATH"
if command -v mise >/dev/null 2>&1; then
  eval "$(mise activate bash)"
fi

OUT="${SKILLS_HUB_PERF_OUT:-$ROOT/docs/wiki/quality/evidence/m0-016-perf.json}"
mkdir -p "$(dirname "$OUT")"
export SKILLS_HUB_PERF_OUT="$OUT"

PROFILE_FLAG=()
if [[ "${1:-}" == "--release" ]]; then
  PROFILE_FLAG=(--release)
  shift
fi

echo "Running reference-scale performance gates (evidence → $OUT)"
cargo test \
  --manifest-path src-tauri/Cargo.toml \
  --lib \
  "${PROFILE_FLAG[@]}" \
  reference_scale::tests::reference_scale_ci_gates_and_optional_evidence \
  -- --nocapture "$@"

echo
echo "Evidence written to $OUT"
if [[ -f "$OUT" ]]; then
  python3 - <<'PY' "$OUT"
import json, sys
path = sys.argv[1]
data = json.load(open(path))
print(f"hardware: {data.get('hardware')}")
print(f"os:       {data.get('os')}")
print(f"build:    {data.get('build')}")
print(f"dataset:  {data.get('dataset')}")
print(f"samples:  {data.get('sampleCount')}")
for item in data.get("measurements", []):
    status = "PASS" if item.get("passed") else "FAIL"
    print(
        f"  [{status}] {item['name']}: p50={item['p50Ms']:.1f}ms "
        f"p95={item['p95Ms']:.1f}ms gate={item['gateMs']:.0f}ms"
    )
print()
print("Launch-to-usable-Library (<1.5s) requires a packaged release app and is")
print("recorded separately in docs/wiki/quality/evidence/m0-016-manual-a11y.md.")
PY
fi
