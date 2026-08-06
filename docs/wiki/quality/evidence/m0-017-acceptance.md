---
type: Quality Evidence
title: M0-017 Acceptance Matrix
description: Executed 12-criterion M0 acceptance sweep with filesystem evidence, focused suite filters, and network-disabled posture notes.
status: implemented
tags: [skills-hub, m0, acceptance, packaging]
requirements: []
timestamp: 2026-08-06T02:48:17Z
---

# M0-017 acceptance matrix

Executed on a clean disposable Vault/HOME via the headless harness in `src-tauri/src/application/m0_acceptance.rs`, plus focused cargo/vitest filters. No criterion is closed by screenshot or happy-path unit test alone.

## Environment

| Field | Value |
| --- | --- |
| Date (UTC) | 2026-08-06 |
| Hardware | Apple M4 |
| OS | macOS 26.6 |
| Arch | aarch64 |
| Rustc | 1.89 |
| Node | v26.5.1 |
| pnpm | 10.28.1 |
| Network mode | local-no-client-deps |
| JSON evidence | [m0-017-acceptance.json](m0-017-acceptance.json) |
| Log | [m0-017-acceptance.log](m0-017-acceptance.log) |

## Network-disabled verification

- Product `Cargo.toml` has no HTTP client crates (`reqwest` / `ureq` / client `hyper`). Guarded by `m0_017_no_network_client_in_release_deps_contract`.
- Core acceptance harness ran under `SKILLS_HUB_NETWORK_MODE=local-no-client-deps` without product telemetry, accounts, or remote catalog calls.
- Optional hard isolation: set `SKILLS_HUB_SANDBOX_PROFILE` to a `sandbox-exec` profile that denies network-outbound; the script wraps cargo invocations when present.
- Host `lsof` snapshots may show unrelated apps; they are not product sockets.

## 12-criterion results

| # | Criterion | Result | Evidence |
| --- | --- | --- | --- |
| 1 | Clean install creates or selects a Vault | **pass** | default Vault at /var/folders/1_/0mpt_xg93y7ghjqdhc3qsd780000gn/T/.tmpvU228d/home/Library/Application Support/Skills Hub/Vault vault.json=true settings=true root_matches=true |
| 2 | All six adapters scanned without mutation | **pass** | configured_roots=6 unique_fs_roots=5 observations=8 trees_unchanged=true |
| 3 | Workspace Root indexed with ignores and symlink cycles handled | **pass** | root_id=019fd4f8-4a06-72a2-992a-1bd39f6b8e5e coverage=complete projects=1 skills=2 errors=0 |
| 4 | Same-name same/different content distinguished | **pass** | digest_same=true digest_diff_distinct=true |
| 5 | External Skill added to Vault, original untouched | **pass** | skill_id=019fd4f8-4a0e-77a0-94fa-9bd9e9d10fe6 working=/private/var/folders/1_/0mpt_xg93y7ghjqdhc3qsd780000gn/T/.tmpvU228d/home/Library/Application Support/Skills Hub/Vault/skills/019fd4f8-4a0e-77a0-94fa-9bd9e9d10fe6/m017-thin-slice source_unchanged=true |
| 6 | Global symlink and Git-project Managed Copy deployment | **pass** | symlink=/private/var/folders/1_/0mpt_xg93y7ghjqdhc3qsd780000gn/T/.tmpvU228d/home/targets/global-skills/m017-thin-slice copy=/private/var/folders/1_/0mpt_xg93y7ghjqdhc3qsd780000gn/T/.tmpvU228d/home/targets/git-project-skills/m017-thin-slice link_is_symlink=true copy_is_dir=true |
| 7 | Collision produces a plan/block and no writes before confirmation | **pass** | plan_err=true tree_unchanged=true foreign=keep-me |
| 8 | Injected commit failure restores earlier committed targets | **pass** | delegated to M0-005/M0-008/M0-015 failpoint matrices; see acceptance script cargo filters | cargo: each_batch_target_commit_failure_rolls_back_every_prior_target; failpoint_matrix_covers_stage_backup_final_and_verify_durability; commit_and_verify_failures_rollback_in_reverse_order |
| 9 | Target edits and broken links appear in Deployments | **pass** | copy_health=target_modified symlink_health=conflict copy_expl=The target changed while Vault still matches the last verified digest. link_expl=The managed link was retargeted away from the Vault. |
| 10 | Undeploy, Trash, restore, and permanent delete are distinct | **pass** | copy_health_before=clean link_health_before=clean undeploy_copy_ok=true undeploy_link_ok=true vault_preserved=true; Trash/restore/delete: cargo filters in m0-acceptance.sh | cargo: permanent_delete_*; restore_preserves_identity_*; harness undeploy thin-slice |
| 11 | Activity accurately reports operation outcome and recovery | **pass** | succeeded_count=17 sample=id=019fd4f8-4df2-7613-a684-239421aa00c7 kind=undeploy state=completed outcome=Some("succeeded") |
| 12 | Core workflows keyboard accessible | **pass** | automated: src/app/keyboard-workflow.test.tsx; manual VO: m0-017-voiceover.md / m0-016-manual-a11y.md | vitest: keyboard-workflow.test.tsx; VO: m0-017-voiceover.md |

**Matrix result:** 12/12 pass

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
