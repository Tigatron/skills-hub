---
type: Implementation Plan
title: M0 Tasks 015–017 — Release Gates
description: Executable tasks for filesystem/reliability hardening, accessibility and performance gates, and M0 acceptance plus packaging.
status: planned
tags: [skills-hub, m0, tasks, release]
requirements: [IMP-08, SCN-04, IMP-04, IMP-06, DPL-06, DPL-07, DPL-08, DEL-04, DEL-06]
timestamp: 2026-07-23T00:00:00Z
---

# M0-015 — Filesystem and reliability hardening

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-008, M0-009, M0-010, M0-011, M0-012, M0-013, M0-014 |
| PRD coverage | IMP-08 (implementation); hardening evidence for SCN-04, IMP-04/06, DPL-06/07/08, DEL-04/06 and PRD §14.3/§17.3 |
| Design | [Filesystem safety](../security/filesystem-safety.md), [Transaction execution](../workflows/transaction-execution.md), [Takeover and deployment](../workflows/takeover-and-deployment.md), [Testing and acceptance](../quality/testing-and-acceptance.md) |
| Parallelization | Per the roadmap, individual fault suites are added alongside each mutation feature in earlier tasks; this task consolidates, fills gaps, and runs them as one gate. Suite strands (path/link, crash matrix, preflight blockers, cleanup audit) are independent. |

## Deliverables

- Consolidated adversarial path/link suite: property tests for component rejection, normalization/case collisions, containment, nonexistent descendants, broken/retargeted/cyclic/escaping links, and special files across every mutation kind.
- TOCTOU race tests replacing parents/destinations around failpoints, requiring `StalePlan` or `RecoveryRequired` outcomes rather than corruption.
- Complete crash matrix: child-process termination at every durable boundary for takeover, deployment, undeploy, Trash, restore, relocate, and batch operations, with idempotent startup-recovery assertions.
- Preflight blocker coverage: permissions, read-only volumes, disk-capacity estimates, copy byte/count caps, unsupported-filesystem reliability detection, and stale-plan storms under concurrent scans.
- Cleanup ownership audit of every deletion call site, proving containment in journal-owned paths and that cleanup failure never widens scope.
- Best-effort local provenance recovery (IMP-08): inspect parent Git metadata and known local lockfiles without network access, label evidence and confidence, and never block takeover on absence or ambiguity.
- Error envelope and logging completeness pass: every terminal failure carries a stable code, safe paths, and a next safe action; causal chains stay in local logs.
- Local structured `tracing` only: 25 MB/seven-day rolling bound, info default/debug opt-in, Operation correlation, path/content/secret redaction, no telemetry/crash upload, and user-reviewed diagnostic export distinct from Activity/journals.

## Implementation boundary

This task hardens and evidences existing contracts; it changes behavior only where a suite exposes a defect. Fixes land in the owning module with its owning task's tests extended.

## Explicitly excluded

- M1 full static content audit, quarantine, and Trust Sheet.
- Sandboxing claims against same-user hostile processes beyond the documented detect-and-stop response.
- Any network-dependent feature.

## Acceptance conditions

- Every failpoint × operation-kind combination terminates in exactly one documented outcome: `FailedNoWrites`, `FailedRolledBack`, `RecoveryRequired`, or completed finalization on restart.
- No cleanup call site can delete outside its proven operation-owned paths in the audit tests.
- Injected races produce stale/blocked/recovery outcomes with all identifiable content versions preserved.
- Recovered provenance appears with evidence and confidence labels; takeover proceeds identically when recovery finds nothing.
- Blocked and canceled plans demonstrate zero mutation via before/after tree comparison at breadth scale.

## Automated tests

- The consolidated suites above, wired as one required gate in CI alongside the per-task suites they extend.
- Regression tests for every defect found during hardening, attached to the owning module.

## Implementation evidence

- The shared executor's full stage/backup/final/verify/finalization/rollback failpoint matrix remains the authoritative durable-boundary cross-product. Product recovery tests connect takeover, deploy, undeploy, Trash, restore, permanent delete, and batch plans to that kernel; real child-process kill/reopen tests now cover takeover, deploy, undeploy, Trash, and restore. The separate relocation executor has its own complete injected-boundary matrix plus child-process kill/reopen and idempotent destination-journal recovery.
- Adversarial replacement tests preserve content when a relocation source Vault, GC pending object, lifecycle staging tree, capability probe, or atomic-write temporary file no longer has its reviewed path and device/inode identity. Old-Vault removal first renames the exact reviewed Vault to an operation-derived quarantine sibling, revalidates identity, and restores a raced replacement rather than deleting it.
- Local provenance recovery reads bounded Git `HEAD`/ref and known lockfile evidence without following links or making network requests. Plans and Skill manifests structurally validate evidence kind, confidence, digest/revision, and absolute source path; absent, ambiguous, or linked metadata does not block takeover.
- Preflight and no-write evidence covers Bundle entry/depth/single-file/total-byte caps, create/write/rename/link capability, capacity, inaccessible roots, read-only parents, stale source/destination/authority, unsupported or changed capability, and Vault/Target containment. Error-envelope regression tests require a stable code, safe redacted message, and next action for actionable failures.
- Local structured diagnostics use a process-local `tracing` subscriber with `info` default and durable debug opt-in, Operation-ID propagation through blocking work, a 25 MB/seven-day rolling bound, descriptor-bound/no-follow storage, and fail-closed export reconstruction. Arbitrary messages, unknown string fields, nested values, Skill content, secrets, and absolute/home paths are redacted; foreign entries or root/segment replacement block writes and deletion. Prepare freezes digest-bound review bytes and save publishes only to a new descriptor-relative destination. Activity and recovery journals remain separate stores; there is no telemetry or crash upload.
- Final gate: `cargo fmt --check`, strict all-target/all-feature Clippy, 270 Rust library tests with five intentional child-helper ignores, generated binding and harness tests, 44 frontend tests, documentation validation, renderer build, and the full `pnpm check` repository gate pass.

## Deliberately consolidated or deferred coverage

- Child-kill tests do not duplicate every generic executor boundary for every product kind. The exhaustive shared failpoint matrix proves those mechanics; each product kind has connection/finalization coverage, and destructive representative boundaries have real process-kill evidence. Undeploy killed after backup rename reaches the documented safe rolled-back outcome with the target preserved.
- Real read-only-volume mounts and unreliable/network filesystem mounts are environment-dependent and are not created by the portable suite. Read-only-parent, capability, capacity, containment, identity-replacement, and unsupported/unknown capability cases provide deterministic no-mutation evidence.
- Same-user hostile-process isolation is not claimed. Diagnostics and cleanup use unpredictable quarantine names, no-follow descriptors, durable identity checks, and detect-and-stop behavior; an undetected actor with the same account retains the residual race capability documented by the filesystem threat model.

## Risks and recovery

The combination space is large; prioritize one full boundary sweep per operation kind over exhaustive cross-products, and document any deliberately skipped combination. Defects found here may reopen earlier tasks — treat that as the gate working, not as scope creep.

# M0-016 — Accessibility and performance gate

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-06) |
| Dependencies | M0-014, M0-015 |
| PRD coverage | M0 acceptance criterion 12; PRD §17 performance/reliability targets and §18 accessibility requirements |
| Design | [Testing and acceptance](../quality/testing-and-acceptance.md), [Tauri/UI contract](../interfaces/tauri-and-ui-state.md) |
| Parallelization | Accessibility and performance work can be distributed by surface, but this task owns the integrated measured result on one documented machine and dataset. |

## Deliverables

- Reference-scale fixture generator: 1,000 Vault Skills, 5,000 observations/deployed locations, 200 projects across several Workspace Roots, 20 targets in one plan.
- Release-build performance measurements with documented hardware, dataset, build, and percentile/sample counts against the quality-plan gates (launch < 1.5 s, warm global scan < 1 s, Workspace first result < 2 s, Library search < 100 ms, plan generation < 500 ms excluding required new hashing).
- Virtualization, query, and indexing tuning required to meet those gates while the UI stays interactive during hashing, scans, and Operations.
- Complete keyboard path through scan → takeover → plan → deploy → verify → undeploy → Trash → restore, with visible focus and focus return after cancel/completion/dialog close.
- Accessible table/list semantics, text-plus-icon status labels, reduced-motion and increased-contrast behavior, system appearance, text scaling, and 900×600 layout verification.
- macOS screen-reader and native-integration manual checklist with recorded results.

## Implementation boundary

This task tunes and verifies; feature behavior and mutation semantics do not change. Performance fixes that require contract changes go back through the owning concept page first.

## Explicitly excluded

- Non-macOS performance evidence and telemetry-based measurement (the product has none).
- New surfaces or visual redesign.

## Acceptance conditions

- All performance gates pass with percentile evidence on the documented reference machine; a single best run is not evidence.
- The complete core workflow is keyboard-only operable; an inaccessible recovery or destructive confirmation is a release blocker.
- Automated static accessibility checks pass on component suites; the manual checklist has no open blocker.
- UI interactivity holds during a reference-scale scan plus a running Operation.

## Automated tests

- Repeatable performance harness with committed fixture generation and threshold assertions.
- Keyboard flow and focus-management integration tests.
- Static accessibility checks in component tests; the manual checklist remains a documented artifact.

## Implementation evidence

- **Reference-scale fixture** (`src-tauri/src/application/reference_scale.rs`): generates 1,000 active Vault Skills, 5,000 observations, 4,000 active deployments, 200 projects across 4 Workspace Roots, and 20 targets under a disposable Vault. Inventory assertions and generation stay under two minutes.
- **Query/index tuning**: schema migration `0006_library_perf_indexes` adds external-observation, active-skill, and active-deployment indexes. `library_list` loads active skills + deployment counts (not full deployment rows) and SQL-prefilters by search needle before in-process grouping.
- **UI interactivity**: Library search is debounced (160 ms); Library remains virtualized (page size 100); StatusPill carries text + tone icon; list rows use native `role=option`/`aria-selected` under listbox parents; Operation dismiss/cancel restores focus via `focusReturnRef`; reduced-motion and increased-contrast CSS cover shell controls; rem-based type scale and 900×600 floor retained.
- **Automated a11y/keyboard**: `keyboard-workflow.test.tsx` covers scan → takeover → plan → deploy, Deployments verify/undeploy, and Trash restore; component tests cover StatusPill, LoadingBlock live region, listbox selection, and focus return after dismiss.
- **Performance harness**: `scripts/perf-harness.sh` runs `reference_scale_ci_gates_and_optional_evidence`. Debug builds use smoke bounds; **release** enforces PRD gates. Evidence JSON: [m0-016-perf.json](../quality/evidence/m0-016-perf.json).

### Release measurement (2026-08-06)

| Field | Value |
| --- | --- |
| Hardware | Apple M4 / 24 GB RAM |
| OS | macOS 26.6 |
| Build | `cargo test --release` (library) |
| Dataset | 1000 skills, 5000 observations (+ scan churn), 4000 active deployments, 200 projects / 4 roots, 20+ targets |
| Samples | 11 per gate (p50/p95/max; gate on p95) |

| Gate | p50 | p95 | Limit | Result |
| --- | --- | --- | --- | --- |
| Library search | 2.4 ms | 2.5 ms | 100 ms | pass |
| Warm global scan | 20.4 ms | 27.1 ms | 1000 ms | pass |
| Workspace first result | 0.06 ms | 0.13 ms | 2000 ms | pass |
| Plan generation | 46.5 ms | 52.9 ms | 500 ms | pass |
| Launch → usable Library | — | — | 1500 ms | **env-gated** — requires packaged `.app`; method + template in [manual a11y checklist](../quality/evidence/m0-016-manual-a11y.md); scheduled for M0-017 packaged smoke |

- **Manual checklist artifact**: [m0-016-manual-a11y.md](../quality/evidence/m0-016-manual-a11y.md) — template plus recorded automated/pass and remaining VoiceOver/native rows for packaging smoke.
- Plan generation still hashes the tiny fixture working tree for drift detection; content is single-file `SKILL.md` so hashing is not the dominant cost. No mutation semantics changed.

## Risks and recovery

Measurement flakiness invites gate erosion; fix the harness or document variance rather than widening thresholds. If a gate cannot be met without a schema or read-model change, update the owning design page and its tests instead of patching around the measurement. Launch timing remains honestly deferred until a packaged artifact exists.

# M0-017 — M0 acceptance verification and packaging

| Field | Value |
| --- | --- |
| Status | Planned |
| Dependencies | M0-016 |
| PRD coverage | All 12 PRD §19.1 acceptance criteria; M0 Definition of Done sweep across VLT-01..08, SCN-01..09, IMP-01..08, DPL-01..12, DEL-01..06 |
| Design | [M0 roadmap](m0-roadmap.md), [Testing and acceptance](../quality/testing-and-acceptance.md), [Traceability](../traceability.md) |
| Parallelization | Acceptance evidence collection, packaging, and documentation sync are separable, but the release decision is a single serial gate. |

## Deliverables

- Execute the 12-criterion acceptance mapping from the quality plan on a clean macOS environment, recording evidence per criterion.
- Network-disabled verification of every core workflow, with no account and no telemetry.
- Verify the accepted product name, bundle ID, default Vault, and minimum macOS metadata in produced artifacts.
- Reproducible native Apple Silicon macOS 14+ local build and packaged artifact (app bundle/DMG) from a documented clean build, preserving source compatibility and compile checks for both macOS architectures where feasible.
- Document ad-hoc/local or unsigned status and the future signing seam; Developer ID, notarization, auto-update, Universal Binary, and Intel runtime validation are not M0 requirements. Never instruct users to disable Gatekeeper.
- Final documentation sync: concept statuses and [traceability](../traceability.md) updated to implementation reality, with remaining manual/platform verification recorded honestly.
- Release notes covering scope, known limitations, and the M1 boundary.

## Implementation boundary

This task ships evidence and artifacts. Product code changes are limited to release-blocking defects found during acceptance, each fixed in its owning module with tests.

## Explicitly excluded

- Windows/Linux packaging and auto-update infrastructure.
- Any M1 feature slipped in as a “release extra.”

## Acceptance conditions

- 12/12 M0 acceptance criteria pass on a clean macOS machine with recorded evidence; no criterion closes on a screenshot or happy-path unit test alone.
- The traceability matrix shows implementation and verification evidence for all 43 M0 requirements.
- The packaged artifact installs on a clean HOME and completes the full thin-slice workflow.
- The M0 Definition of Done in the [roadmap](m0-roadmap.md) is satisfied line by line.

## Automated tests

- The full CI gate set from the quality plan, run against the release build.
- A packaged-app smoke script for first run and the thin slice on a disposable HOME.

## Risks and recovery

Local/ad-hoc packaging must not accidentally imply notarization or Intel runtime evidence. If acceptance exposes a contract defect, reopen the owning task and concept page rather than annotating the failure away.

# Release exit gate

M0 ships only when this page's three gates pass together: the hardening suites are green as one gate, the accessibility/performance evidence is recorded, and 12/12 acceptance criteria plus the 43-requirement traceability sweep hold on the packaged build.
