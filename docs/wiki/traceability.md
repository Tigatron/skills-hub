---
type: Traceability Matrix
title: M0 Traceability
description: Maps every M0 PRD requirement to its owning design concepts, implementing tasks, and verification evidence, and maps the 12 M0 acceptance criteria to tasks.
status: accepted
tags: [skills-hub, m0, traceability]
timestamp: 2026-07-23T00:00:00Z
---

# How to read this matrix

Each row maps one [PRD v0.1](../PRD-v0.1.md) M0 requirement to the design concepts that own its contract, the tasks that implement it, and where its verification evidence is gated. Task IDs `M0-001`–`M0-017` are defined once each in the [foundation](plans/m0-tasks-01-foundation.md), [thin-slice](plans/m0-tasks-02-thin-slice.md), [breadth](plans/m0-tasks-03-breadth.md), and [release](plans/m0-tasks-04-release.md) pages; the delivery order lives in the [roadmap](plans/m0-roadmap.md). Detailed acceptance evidence definitions live in [testing and acceptance](quality/testing-and-acceptance.md); this page maps ownership and does not restate them.

Every mutation-bearing requirement additionally passes the consolidated hardening gate in `M0-015` and the final acceptance sweep in `M0-017`; the Verification column names only the primary evidence owner.

# Vault and indexing (VLT)

| Requirement | Priority | Design concepts | Implementing tasks | Verification |
| --- | --- | --- | --- | --- |
| VLT-01 | P0 | [Vault and SQLite](storage/vault-and-sqlite.md) | [M0-003](plans/m0-tasks-01-foundation.md), first-run UI in [M0-009](plans/m0-tasks-02-thin-slice.md) | M0-003 init/idempotence tests; acceptance criterion 1 in M0-017 |
| VLT-02 | P0 | [Vault and SQLite](storage/vault-and-sqlite.md) | [M0-003](plans/m0-tasks-01-foundation.md) | M0-003 layout/manifest readability tests |
| VLT-03 | P0 | [Vault and SQLite](storage/vault-and-sqlite.md), [System context](architecture/system-context.md) | [M0-003](plans/m0-tasks-01-foundation.md) | M0-003 no-content-blob schema test |
| VLT-04 | P0 | [Bundle hashing and objects](storage/bundle-hashing-and-objects.md) | [M0-002](plans/m0-tasks-01-foundation.md) (hash), [M0-003](plans/m0-tasks-01-foundation.md) (object store) | M0-002 golden vectors; M0-003 dedupe/corruption tests |
| VLT-05 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md) (watchers), [Bundle hashing and objects](storage/bundle-hashing-and-objects.md) (working versions) | Watcher foundation in [M0-010](plans/m0-tasks-03-breadth.md); Vault edit marking implemented in [M0-012](plans/m0-tasks-03-breadth.md) | M0-012 real-Vault digest/manifest/index and byte-tree no-overwrite test passes |
| VLT-06 | P0 | [Vault and SQLite](storage/vault-and-sqlite.md) (verify/repair/relocate) | Lifecycle backend implemented in [M0-012](plans/m0-tasks-03-breadth.md); Settings entry points in [M0-014](plans/m0-tasks-03-breadth.md) | M0-012 exact-path read-only verify, repair refusal, relocation preflight/recovery/cleanup tests pass |
| VLT-07 | P1 | [Vault and SQLite](storage/vault-and-sqlite.md) (index rebuild) | Implemented in [M0-012](plans/m0-tasks-03-breadth.md) | M0-012 manifest/journal rebuild, retained-backup, and restart-required contract |
| VLT-08 | P1 | [Bundle hashing and objects](storage/bundle-hashing-and-objects.md) (retention/GC) | Implemented in [M0-012](plans/m0-tasks-03-breadth.md) | M0-012 conservative reference pass, two-phase ownership/containment, and unhealthy/divergent-index refusal tests |

# Scanning and discovery (SCN)

| Requirement | Priority | Design concepts | Implementing tasks | Verification |
| --- | --- | --- | --- | --- |
| SCN-01 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md), [Target adapters](interfaces/target-adapters.md) | One adapter complete in [M0-004](plans/m0-tasks-02-thin-slice.md); all six complete in [M0-011](plans/m0-tasks-03-breadth.md) | M0-004 Universal-root and M0-011 six-root no-write matrices pass; acceptance criterion 2 |
| SCN-02 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md) | [M0-004](plans/m0-tasks-02-thin-slice.md), breadth in [M0-011](plans/m0-tasks-03-breadth.md) | M0-004 missing/inaccessible-root and visible-error tests pass |
| SCN-03 | P0 | [Identity and state](domain/identity-and-state.md), [Bundle hashing and objects](storage/bundle-hashing-and-objects.md) | Classifier/hash in [M0-002](plans/m0-tasks-01-foundation.md); reconciliation in [M0-004](plans/m0-tasks-02-thin-slice.md) | M0-002/M0-004 digest, conflict, case-sensitive identity, and order-independence tests pass; acceptance criterion 4 |
| SCN-04 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md), [Filesystem safety](security/filesystem-safety.md) | Path policy in [M0-002](plans/m0-tasks-01-foundation.md); global scan in [M0-004](plans/m0-tasks-02-thin-slice.md); Workspace in [M0-010](plans/m0-tasks-03-breadth.md) | M0-004 global link escape/cycle/read-only tests pass; M0-010 Workspace fixtures and M0-015 hardening remain |
| SCN-05 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md), [Tauri and UI state](interfaces/tauri-and-ui-state.md) | [M0-010](plans/m0-tasks-03-breadth.md); Settings UI in [M0-014](plans/m0-tasks-03-breadth.md) | M0-010 add/pause/remove/rescan tests |
| SCN-06 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md) | [M0-010](plans/m0-tasks-03-breadth.md) | M0-010 project-discovery fixture matrix |
| SCN-07 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md) | [M0-010](plans/m0-tasks-03-breadth.md) | M0-010 ignore/prune tests; acceptance criterion 3 |
| SCN-08 | P1 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md), [Runtime and modules](architecture/runtime-and-modules.md) | [M0-010](plans/m0-tasks-03-breadth.md) | M0-010 watcher fault-injection tests |
| SCN-09 | P1 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md) (diagnostics) | Activity diagnostics in [M0-008](plans/m0-tasks-02-thin-slice.md); coverage model in [M0-010](plans/m0-tasks-03-breadth.md); UI in [M0-014](plans/m0-tasks-03-breadth.md) | M0-010 coverage-state tests; M0-014 diagnostics rendering tests |

# Import and takeover (IMP)

| Requirement | Priority | Design concepts | Implementing tasks | Verification |
| --- | --- | --- | --- | --- |
| IMP-01 | P0 | [Scanning and reconciliation](workflows/scanning-and-reconciliation.md), [Identity and state](domain/identity-and-state.md) | Read model complete in [M0-004](plans/m0-tasks-02-thin-slice.md); Library UI in [M0-009](plans/m0-tasks-02-thin-slice.md)/[M0-014](plans/m0-tasks-03-breadth.md) | M0-004 before/after trees, 1,000-item pagination, duplicate/conflict, and degraded-observation tests pass |
| IMP-02 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Tauri and UI state](interfaces/tauri-and-ui-state.md) | [M0-006](plans/m0-tasks-02-thin-slice.md); UI in [M0-009](plans/m0-tasks-02-thin-slice.md) | M0-006 distinct-intent, zero-deployment Vaulting, and selected-only management tests pass; M0-009 flow tests remain |
| IMP-03 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Operation model](domain/operation-recovery-and-trash.md) | [M0-006](plans/m0-tasks-02-thin-slice.md); plan review UI in [M0-009](plans/m0-tasks-02-thin-slice.md) | M0-006 frozen schema-v2 digest, complete evidence, reopen, and authority-validation tests pass |
| IMP-04 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Transaction execution](workflows/transaction-execution.md) | [M0-006](plans/m0-tasks-02-thin-slice.md) | M0-006 exact-copy, object, nested-container, staging/activation, rollback, and child-kill tests pass; acceptance criterion 5 |
| IMP-05 | P0 | [Identity and state](domain/identity-and-state.md), [Takeover and deployment](workflows/takeover-and-deployment.md) | Classifier in [M0-002](plans/m0-tasks-01-foundation.md); coexistence in [M0-006](plans/m0-tasks-02-thin-slice.md) | M0-006 same-name/different-content coexistence and ID/path isolation tests pass |
| IMP-06 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Filesystem safety](security/filesystem-safety.md) | [M0-006](plans/m0-tasks-02-thin-slice.md) | M0-006 source byte/entry/metadata comparison plus source-ID/physical-alias rejection tests pass; hardening remains M0-015 |
| IMP-07 | P0 | [Operation model](domain/operation-recovery-and-trash.md), [Transaction execution](workflows/transaction-execution.md) | Objects in [M0-003](plans/m0-tasks-01-foundation.md); exact destructive protection registrar complete in [M0-005](plans/m0-tasks-02-thin-slice.md); product use in [M0-006](plans/m0-tasks-02-thin-slice.md)/[M0-013](plans/m0-tasks-03-breadth.md) | M0-006 takeover Snapshot/object/rollback/finalization tests pass; M0-013 Trash recovery-point tests remain |
| IMP-08 | P1 | [Takeover and deployment](workflows/takeover-and-deployment.md) (local provenance recovery) | [M0-015](plans/m0-tasks-04-release.md) | M0-015 provenance confidence-label and non-blocking tests |

# Deployment (DPL)

| Requirement | Priority | Design concepts | Implementing tasks | Verification |
| --- | --- | --- | --- | --- |
| DPL-01 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Target adapters](interfaces/target-adapters.md) | Fixture target complete in [M0-007](plans/m0-tasks-02-thin-slice.md); all six complete in [M0-011](plans/m0-tasks-03-breadth.md) | M0-007 fixture and M0-011 six-adapter global/project matrices pass; acceptance criterion 6 |
| DPL-02 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Target adapters](interfaces/target-adapters.md) | Fixture target complete in [M0-007](plans/m0-tasks-02-thin-slice.md); all six in [M0-011](plans/m0-tasks-03-breadth.md) | M0-007 Git-project Managed Copy default/digest tests pass; M0-011 matrix and acceptance criterion 6 remain |
| DPL-03 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Target adapters](interfaces/target-adapters.md) | Fixture behavior complete in [M0-007](plans/m0-tasks-02-thin-slice.md); adapter breadth in [M0-011](plans/m0-tasks-03-breadth.md) | M0-007 non-Git default and explicit override tests pass |
| DPL-04 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Target adapters](interfaces/target-adapters.md) | Complete in [M0-007](plans/m0-tasks-02-thin-slice.md) | Proven unsupported-link fallback changes the sealed reviewed plan; unknown/changed capability blocks without silent mode switching |
| DPL-05 | P0 | [Operation model](domain/operation-recovery-and-trash.md), [Transaction execution](workflows/transaction-execution.md) | Generic immutable plan/no-write kernel complete in [M0-005](plans/m0-tasks-02-thin-slice.md); deployment plan construction complete in [M0-007](plans/m0-tasks-02-thin-slice.md); review UI in [M0-009](plans/m0-tasks-02-thin-slice.md) | M0-005 canonical seal/load and stale preflight plus M0-007 frozen schema-v3, complete authority, consequence, and no-write tests pass |
| DPL-06 | P0 | [Identity and state](domain/identity-and-state.md), [Transaction execution](workflows/transaction-execution.md), [Filesystem safety](security/filesystem-safety.md) | Collision keys in [M0-002](plans/m0-tasks-01-foundation.md); generic blockers in [M0-005](plans/m0-tasks-02-thin-slice.md); unmanaged product collision handling complete in [M0-007](plans/m0-tasks-02-thin-slice.md) | Exact and case/Unicode-normalized unmanaged collision tests prove no target write; acceptance criterion 7 UI remains |
| DPL-07 | P0 | [Transaction execution](workflows/transaction-execution.md) | Generic stage-all/deterministic transaction kernel complete in [M0-005](plans/m0-tasks-02-thin-slice.md); product multi-target recovery driving in [M0-008](plans/m0-tasks-02-thin-slice.md) | M0-005 stage/backup/final/verify/finalization failpoint matrix; M0-008 product multi-target tests |
| DPL-08 | P0 | [Transaction execution](workflows/transaction-execution.md), [Operation model](domain/operation-recovery-and-trash.md) | Generic reverse rollback and startup classifier complete in [M0-005](plans/m0-tasks-02-thin-slice.md); startup action driver and product recovery in [M0-008](plans/m0-tasks-02-thin-slice.md) | M0-005 rollback matrix, version-retention tests, and real child-kill/reopen classification; M0-008 action-driver tests; acceptance criterion 8 |
| DPL-09 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Identity and state](domain/identity-and-state.md) | Values in [M0-002](plans/m0-tasks-01-foundation.md); fixture manifests/rows complete in [M0-007](plans/m0-tasks-02-thin-slice.md); adapter versions in [M0-011](plans/m0-tasks-03-breadth.md) | Mode, expected digest/link, path, adapter version, verification time, manifest/SQLite consistency, and replay tests pass for M0-007 |
| DPL-10 | P0 | [Identity and state](domain/identity-and-state.md) (health truth table), [Takeover and deployment](workflows/takeover-and-deployment.md) | Truth table in [M0-002](plans/m0-tasks-01-foundation.md); Rust verifier/read model complete in [M0-007](plans/m0-tasks-02-thin-slice.md); UI in [M0-009](plans/m0-tasks-02-thin-slice.md)/[M0-014](plans/m0-tasks-03-breadth.md) | Full E/V/T and link truth tables plus missing/broken/retargeted/preserve tests pass; acceptance criterion 9 UI remains |
| DPL-11 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Identity and state](domain/identity-and-state.md) | Complete in [M0-007](plans/m0-tasks-02-thin-slice.md) | Clean removal, changed/missing/broken preserve, Snapshot rollback, Vault preservation, and other-target isolation tests pass |
| DPL-12 | P1 | [Takeover and deployment](workflows/takeover-and-deployment.md) (dry-run export), [Tauri and UI state](interfaces/tauri-and-ui-state.md) | [M0-014](plans/m0-tasks-03-breadth.md) | M0-014 plan-export snapshot test |

# Deletion and recovery (DEL)

| Requirement | Priority | Design concepts | Implementing tasks | Verification |
| --- | --- | --- | --- | --- |
| DEL-01 | P0 | [Operation model](domain/operation-recovery-and-trash.md), [Tauri and UI state](interfaces/tauri-and-ui-state.md) | Outcome model in [M0-002](plans/m0-tasks-01-foundation.md); undeploy behavior complete in [M0-007](plans/m0-tasks-02-thin-slice.md); Trash/delete in [M0-013](plans/m0-tasks-03-breadth.md); copy/UI in [M0-014](plans/m0-tasks-03-breadth.md) | M0-007 exposes a distinct ID-based undeploy intent and preserves Vault content; M0-013/M0-014 distinct Trash/delete actions and acceptance criterion 10 remain |
| DEL-02 | P0 | [Operation model](domain/operation-recovery-and-trash.md) | [M0-013](plans/m0-tasks-03-breadth.md) | M0-013 deployment-resolution-required tests |
| DEL-03 | P0 | [Operation model](domain/operation-recovery-and-trash.md) | [M0-013](plans/m0-tasks-03-breadth.md) | M0-013 retention and protected-checkpoint tests |
| DEL-04 | P0 | [Operation model](domain/operation-recovery-and-trash.md), [Filesystem safety](security/filesystem-safety.md) | [M0-013](plans/m0-tasks-03-breadth.md) | M0-013 Trash-only delete and secondary-confirmation tests; hardening in M0-015 |
| DEL-05 | P0 | [Takeover and deployment](workflows/takeover-and-deployment.md), [Filesystem safety](security/filesystem-safety.md) | Keep-external in [M0-006](plans/m0-tasks-02-thin-slice.md); action vocabulary in [M0-013](plans/m0-tasks-03-breadth.md)/[M0-014](plans/m0-tasks-03-breadth.md) | M0-013 no-direct-delete surface tests |
| DEL-06 | P0 | [Operation model](domain/operation-recovery-and-trash.md), [Transaction execution](workflows/transaction-execution.md) | Operation-level protection and retained rollback evidence complete in [M0-005](plans/m0-tasks-02-thin-slice.md); multi-target driver in [M0-008](plans/m0-tasks-02-thin-slice.md); batch Trash/undeploy in [M0-013](plans/m0-tasks-03-breadth.md) | M0-005 exact protection/reverse rollback tests; M0-008/M0-013 product batch recovery-point tests |

# M0 acceptance criteria mapping

The evidence definition for each criterion is owned by [testing and acceptance](quality/testing-and-acceptance.md); [M0-017](plans/m0-tasks-04-release.md) executes the full sweep on the packaged build.

| # | PRD §19.1 criterion | Requirements | Primary tasks |
| --- | --- | --- | --- |
| 1 | Clean install creates or selects a Vault | VLT-01 | [M0-003](plans/m0-tasks-01-foundation.md), [M0-009](plans/m0-tasks-02-thin-slice.md) |
| 2 | All six adapters scanned without mutation | SCN-01, SCN-02, IMP-01 | [M0-004](plans/m0-tasks-02-thin-slice.md), [M0-011](plans/m0-tasks-03-breadth.md) |
| 3 | Workspace Root indexed with ignores and symlink cycles handled | SCN-04, SCN-05, SCN-06, SCN-07 | [M0-010](plans/m0-tasks-03-breadth.md) |
| 4 | Same-name same/different content distinguished | SCN-03, IMP-05 | [M0-002](plans/m0-tasks-01-foundation.md), [M0-004](plans/m0-tasks-02-thin-slice.md) |
| 5 | External Skill added to Vault, original untouched | IMP-02, IMP-04, IMP-06 | [M0-006](plans/m0-tasks-02-thin-slice.md) |
| 6 | Global symlink and Git-project Managed Copy deployment | DPL-01, DPL-02, DPL-03 | [M0-007](plans/m0-tasks-02-thin-slice.md), [M0-011](plans/m0-tasks-03-breadth.md) |
| 7 | Collision produces a plan and no writes before confirmation | DPL-05, DPL-06 | [M0-005](plans/m0-tasks-02-thin-slice.md), [M0-007](plans/m0-tasks-02-thin-slice.md) |
| 8 | Injected commit failure restores earlier committed targets | DPL-07, DPL-08 | [M0-005](plans/m0-tasks-02-thin-slice.md), [M0-008](plans/m0-tasks-02-thin-slice.md), [M0-015](plans/m0-tasks-04-release.md) |
| 9 | Target edits and broken links appear in Deployments | DPL-09, DPL-10 | [M0-007](plans/m0-tasks-02-thin-slice.md), [M0-014](plans/m0-tasks-03-breadth.md) |
| 10 | Undeploy, Trash, restore, and permanent delete are distinct | DEL-01, DEL-02, DEL-03, DEL-04, DEL-05 | [M0-007](plans/m0-tasks-02-thin-slice.md), [M0-013](plans/m0-tasks-03-breadth.md), [M0-014](plans/m0-tasks-03-breadth.md) |
| 11 | Activity reports outcome and recovery accurately | SCN-09, DEL-06 | [M0-008](plans/m0-tasks-02-thin-slice.md), [M0-014](plans/m0-tasks-03-breadth.md) |
| 12 | Core workflows keyboard accessible | PRD §18 | [M0-009](plans/m0-tasks-02-thin-slice.md), [M0-016](plans/m0-tasks-04-release.md) |

# Coverage summary

- All 43 M0 requirements (VLT-01..08, SCN-01..09, IMP-01..08, DPL-01..12, DEL-01..06) have at least one owning design concept, implementing task, and verification owner above.
- All 12 PRD §19.1 acceptance criteria map to primary tasks and are finally verified in [M0-017](plans/m0-tasks-04-release.md).
- Every Task ID M0-001..017 appears in exactly one task page; `M0-001` carries no single requirement row because it delivers the shell and harness all rows depend on.

# Maintenance rules

- Update this page in the same change that moves a requirement's implementation or verification owner.
- A requirement row may claim `implemented` evidence only when its owning task's Definition of Done in [testing and acceptance](quality/testing-and-acceptance.md) is met.
- M1/M2 requirement families (SRC, EDT, UPD, SEC, COL, PKG, GIT, PRJ) are deliberately absent from this matrix until their milestones open.
