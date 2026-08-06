#!/usr/bin/env bash
# M0-017 acceptance matrix runner.
# Executes the headless thin-slice harness, focused criterion suites, keyboard
# frontend tests, and records JSON + markdown evidence. Supports network-disabled
# documentation via SKILLS_HUB_NETWORK_MODE.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

export PATH="/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
if command -v mise >/dev/null 2>&1; then
  eval "$(mise activate bash)"
fi

EVIDENCE_DIR="${SKILLS_HUB_ACCEPTANCE_DIR:-$ROOT/docs/wiki/quality/evidence}"
mkdir -p "$EVIDENCE_DIR"
JSON_OUT="${SKILLS_HUB_ACCEPTANCE_OUT:-$EVIDENCE_DIR/m0-017-acceptance.json}"
MD_OUT="${SKILLS_HUB_ACCEPTANCE_MD:-$EVIDENCE_DIR/m0-017-acceptance.md}"
LOG_OUT="$EVIDENCE_DIR/m0-017-acceptance.log"
export SKILLS_HUB_ACCEPTANCE_OUT="$JSON_OUT"
export SKILLS_HUB_NETWORK_MODE="${SKILLS_HUB_NETWORK_MODE:-local-no-client-deps}"
export SKILLS_HUB_RUSTC="$(rustc -V 2>/dev/null || true)"

STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
{
  echo "=== M0-017 acceptance $STAMP ==="
  echo "network_mode=$SKILLS_HUB_NETWORK_MODE"
  echo "node=$(command -v node) $(node -v)"
  echo "pnpm=$(command -v pnpm) $(pnpm -v)"
  echo "rustc=$(rustc -V)"
  echo "hardware=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || true)"
  echo "os=$(sw_vers -productVersion 2>/dev/null || true)"
} | tee "$LOG_OUT"

# Optional hard network isolation when sandbox-exec profile is provided.
run_cmd() {
  if [[ -n "${SKILLS_HUB_SANDBOX_PROFILE:-}" && -f "${SKILLS_HUB_SANDBOX_PROFILE}" ]]; then
    sandbox-exec -f "$SKILLS_HUB_SANDBOX_PROFILE" "$@"
  else
    "$@"
  fi
}
if [[ -n "${SKILLS_HUB_SANDBOX_PROFILE:-}" && -f "${SKILLS_HUB_SANDBOX_PROFILE}" ]]; then
  export SKILLS_HUB_NETWORK_MODE="sandbox-exec:${SKILLS_HUB_SANDBOX_PROFILE}"
  echo "Using sandbox-exec profile $SKILLS_HUB_SANDBOX_PROFILE" | tee -a "$LOG_OUT"
fi

echo "--- Headless acceptance matrix (criteria 1–12 core path) ---" | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib m0_017_ \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"

echo "--- Criterion 8 failpoint evidence ---" | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  each_batch_target_commit_failure_rolls_back_every_prior_target \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  failpoint_matrix_covers_stage_backup_final_and_verify_durability \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  commit_and_verify_failures_rollback_in_reverse_order \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"

echo "--- Criterion 10 Trash/restore/delete distinctness ---" | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  permanent_delete_is_guarded_exact_and_retains_objects_and_history \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  restore_preserves_identity_and_never_recreates_deployments \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  permanent_delete_confirmation_contract_is_exact \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"

echo "--- Criterion 4 digest / same-name ---" | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  same_named_skills_coexist \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"
run_cmd cargo test --manifest-path src-tauri/Cargo.toml --lib \
  collision_key_uses_nfc \
  -- --nocapture 2>&1 | tee -a "$LOG_OUT"

echo "--- Criterion 12 keyboard workflow (frontend) ---" | tee -a "$LOG_OUT"
pnpm exec vitest run src/app/keyboard-workflow.test.tsx 2>&1 | tee -a "$LOG_OUT"

# Network residual check: host snapshot is informational only (other apps may have sockets).
if command -v lsof >/dev/null 2>&1; then
  LSOF_SNAP="$(lsof -nP -iTCP -sTCP:ESTABLISHED 2>/dev/null | head -20 || true)"
  echo "--- established TCP snapshot (host, not app-bound) ---" | tee -a "$LOG_OUT"
  echo "${LSOF_SNAP:-"(none listed)"}" | tee -a "$LOG_OUT"
fi

export M017_JSON_OUT="$JSON_OUT"
export M017_MD_OUT="$MD_OUT"
python3 - <<'PY'
import json, pathlib, datetime, os, platform, subprocess

json_path = pathlib.Path(os.environ["M017_JSON_OUT"])
md_path = pathlib.Path(os.environ["M017_MD_OUT"])
data = json.loads(json_path.read_text()) if json_path.exists() else {}

def sh(cmd: str) -> str:
    try:
        return subprocess.check_output(cmd, shell=True, text=True).strip()
    except Exception:
        return ""

hardware = data.get("hardware") or sh("sysctl -n machdep.cpu.brand_string")
osver = data.get("os") or f"macOS {sh('sw_vers -productVersion')}"
criteria = data.get("criteria") or []
by_id = {c["id"]: c for c in criteria}
network_mode = data.get("network_mode") or os.environ.get("SKILLS_HUB_NETWORK_MODE", "local-no-client-deps")
now = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
day = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")

names = {
    1: "Clean install creates or selects a Vault",
    2: "All six adapters scanned without mutation",
    3: "Workspace Root indexed with ignores and symlink cycles handled",
    4: "Same-name same/different content distinguished",
    5: "External Skill added to Vault, original untouched",
    6: "Global symlink and Git-project Managed Copy deployment",
    7: "Collision produces a plan and no writes before confirmation",
    8: "Injected commit failure restores earlier committed targets",
    9: "Target edits and broken links appear in Deployments",
    10: "Undeploy, Trash, restore, and permanent delete are distinct",
    11: "Activity accurately reports operation outcome and recovery",
    12: "Core workflows keyboard accessible",
}

rows = []
for i in range(1, 13):
    c = by_id.get(i)
    if c:
        result = "pass" if c.get("passed") else "FAIL"
        evidence = c.get("evidence", "")
        name = c.get("name", names[i])
    else:
        result = "missing"
        evidence = "not present in harness JSON"
        name = names[i]
    if i == 8:
        evidence += (
            " | cargo: each_batch_target_commit_failure_rolls_back_every_prior_target; "
            "failpoint_matrix_covers_stage_backup_final_and_verify_durability; "
            "commit_and_verify_failures_rollback_in_reverse_order"
        )
    if i == 10:
        evidence += (
            " | cargo: permanent_delete_*; restore_preserves_identity_*; harness undeploy thin-slice"
        )
    if i == 12:
        evidence += " | vitest: keyboard-workflow.test.tsx; VO: m0-017-voiceover.md"
    rows.append(f"| {i} | {name} | **{result}** | {evidence} |")

all_pass = bool(data.get("allPassed") or data.get("all_passed")) and all(
    by_id.get(i, {}).get("passed") for i in range(1, 13)
)

md = f"""---
type: Quality Evidence
title: M0-017 Acceptance Matrix
description: Executed 12-criterion M0 acceptance sweep with filesystem evidence, focused suite filters, and network-disabled posture notes.
status: implemented
tags: [skills-hub, m0, acceptance, packaging]
requirements: []
timestamp: {now}
---

# M0-017 acceptance matrix

Executed on a clean disposable Vault/HOME via the headless harness in `src-tauri/src/application/m0_acceptance.rs`, plus focused cargo/vitest filters. No criterion is closed by screenshot or happy-path unit test alone.

## Environment

| Field | Value |
| --- | --- |
| Date (UTC) | {day} |
| Hardware | {hardware} |
| OS | {osver} |
| Arch | {data.get('arch') or platform.machine()} |
| Rustc | {data.get('rustc') or sh('rustc -V')} |
| Node | {sh('node -v')} |
| pnpm | {sh('pnpm -v')} |
| Network mode | {network_mode} |
| JSON evidence | [m0-017-acceptance.json](m0-017-acceptance.json) |
| Log | [m0-017-acceptance.log](m0-017-acceptance.log) |

## Network-disabled verification

- Product `Cargo.toml` has no HTTP client crates (`reqwest` / `ureq` / client `hyper`). Guarded by `m0_017_no_network_client_in_release_deps_contract`.
- Core acceptance harness ran under `SKILLS_HUB_NETWORK_MODE={network_mode}` without product telemetry, accounts, or remote catalog calls.
- Optional hard isolation: set `SKILLS_HUB_SANDBOX_PROFILE` to a `sandbox-exec` profile that denies network-outbound; the script wraps cargo invocations when present.
- Host `lsof` snapshots may show unrelated apps; they are not product sockets.

## 12-criterion results

| # | Criterion | Result | Evidence |
| --- | --- | --- | --- |
{chr(10).join(rows)}

**Matrix result:** {"12/12 pass" if all_pass else "INCOMPLETE/FAIL — see JSON"}

## Thin-slice path exercised

```text
disposable HOME → default Vault init
→ seed six adapter roots → scan each (before/after tree fingerprint)
→ Workspace Root with node_modules ignore + symlink cycle
→ digest same-name cases
→ takeover Add to Vault (source byte/tree compare)
→ deploy symlink + Managed Copy
→ unmanaged collision no-write
→ drift + broken link verify
→ undeploy both targets (Vault preserved)
→ Activity succeeded outcomes listed
```

## Related evidence

- [m0-017-package-build.json](m0-017-package-build.json) — package identity and ad-hoc signing
- [m0-017-packaged-smoke.md](m0-017-packaged-smoke.md) — disposable HOME packaged smoke
- [m0-017-release-notes.md](m0-017-release-notes.md) — scope and M1 boundary
- [m0-016-perf.json](m0-016-perf.json) — reference-scale performance
- [m0-016-manual-a11y.md](m0-016-manual-a11y.md) / [m0-017-voiceover.md](m0-017-voiceover.md) — a11y residuals
"""
md_path.write_text(md)
print(f"Wrote {md_path}")
print(f"all_passed={all_pass} criteria={len(by_id)}")
if not all_pass:
    raise SystemExit(1)
PY

echo "Acceptance evidence written to $JSON_OUT and $MD_OUT"
