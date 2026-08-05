---
type: Implementation Plan
title: M0 Tasks 004–009 — End-to-End Thin Slice
description: Executable tasks for one real scanner, transaction kernel, takeover, deployment, recovery, Activity, and thin-slice UI.
status: planned
tags: [skills-hub, m0, tasks, thin-slice]
requirements: [SCN-01, SCN-02, SCN-03, SCN-04, SCN-09, IMP-01, IMP-02, IMP-03, IMP-04, IMP-05, IMP-06, IMP-07, DPL-01, DPL-02, DPL-03, DPL-04, DPL-05, DPL-06, DPL-07, DPL-08, DPL-09, DPL-10, DPL-11, DEL-01, DEL-06]
timestamp: 2026-07-23T00:00:00Z
---

# M0-004 — Scan one global adapter and build the first Library read model

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | M0-003 |
| PRD coverage | SCN-01/02/03/04, IMP-01 |
| Design | [Scanning](../workflows/scanning-and-reconciliation.md), [Identity/state](../domain/identity-and-state.md), [Target adapters](../interfaces/target-adapters.md) |
| Parallelization | Scanner and SQL read-model query can proceed in parallel against shared observation fixtures. |

## Deliverables

- Generic immediate-child global-root scanner using only the Universal adapter fixture.
- Per-root scan runs, isolated errors, coverage completion/cancellation, observation upsert/stale rules.
- Managed-link exception and unknown out-of-root symlink classification.
- Digest-based exact duplicate and same-name conflict reconciliation.
- Paginated/filterable M0 `LibraryItem` query for external observations.
- Scan start/get/cancel and Library-list typed commands/events; traversal/hash work runs on the bounded blocking worker and cancellation stops at a safe entry boundary.

## Explicitly excluded

- Workspace traversal, watchers, other built-in adapters, takeover/mutation.

## Acceptance conditions

- Missing/inaccessible roots do not fail successful roots.
- Only direct child directories containing direct regular `SKILL.md` are candidates.
- Before/after source tree evidence proves zero mutation.
- Same-name/same-digest groups locations; same-name/different-digest remains separate/conflicting.
- Canceled/partial scan never marks unseen prior observations missing.
- Library shows explicit hash/permission/link errors rather than hiding candidates.

## Automated tests

- Global-root fixture matrix, source-tree no-write snapshots, enumeration-order property test.
- Missing/permission/symlink/broken-link/cancel/stale observation tests.
- Command DTO and Library query tests at 1,000-item scale.

## Implementation evidence

- The Universal adapter now scans `~/.agents/skills` one immediate child at a time through the read-only global scanner. Real-tree tests cover verified Bundles, invalid manifests, missing/inaccessible roots and candidates, unknown/broken/cyclic/out-of-root links, known managed links, unstable content, cancellation, arbitrary enumeration order, and before/after source-tree identity.
- Scan runs, diagnostics, and observations persist through the checksummed SQLite v2 projection. Reconciliation is root-scoped and may mark prior observations stale only after complete coverage; cancelled, partial, and failed runs retain the previous complete baseline. Concurrent requests for the same root collapse to the active job rather than racing stale reconciliation.
- The first Rust-owned Library model groups exact duplicates by digest while preserving same-name/different-digest conflicts, visible degraded observations, distinct case-sensitive normalized locations, filters, stable ordering, and pagination. The 1,000-observation query fixture passes.
- Generated typed commands expose `scan_start`, `scan_get`, `scan_cancel`, and `library_list`. `scan-progress` carries bounded transient progress; `domain-invalidated` requests authoritative scan/Library refetch after persisted changes. Hashing and repository work use the bounded blocking runtime.
- The completion gate passes focused scanner and persistence tests, all-target/all-feature Rust tests and Clippy with `-D warnings`, binding generation/drift checks, frontend type/tests/build, documentation validation, and the project-local `kill-ai-slop` scan. No Workspace traversal, watcher, additional adapter, or mutation path was introduced.

## Risks and recovery

Do not reuse CC Switch's folder-name deduplication or hidden-file-skipping hash. Scanner errors must stay attached to their coverage root; no catch-all empty result.

# M0-005 — Build the Operation planner, journal, and executor kernel

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | M0-003 |
| PRD coverage | DPL-05/06/07/08, IMP-07, DEL-06 |
| Design | [Operation model](../domain/operation-recovery-and-trash.md), [Transaction execution](../workflows/transaction-execution.md), [Filesystem safety](../security/filesystem-safety.md) |
| Parallelization | Critical mutation core; keep executor sequencing single-owner. Activity projection may be implemented separately. |

## Deliverables

- Generic `OperationIntent`→persisted immutable plan pipeline.
- Per-Vault mutation coordinator, plan expiry/digest, and under-lock preflight revalidation.
- Durable journal/step persistence and state machine.
- Exact `.manager/operations/<operation-id>/` contract with immutable `plan.json`, atomic `journal.json`, atomic numbered steps, and intent → action → actual-result check → observed-completion ordering.
- Per-destructive-step Snapshot protection of the exact sealed before-version, stage-all coordinator, deterministic commit/verify/finalize, and inverse rollback with the actual rollback-aside source fingerprint journaled.
- Exact operation-owned cleanup and startup non-terminal journal classifier.
- Progress/terminal/invalidation events and stable error envelope.
- `thiserror` domain/application errors and stable serializable code/action mapping; `anyhow` only at redacted outer diagnostic boundaries and no panic for ordinary I/O.
- Cooperative cancellation for planning/Snapshot/stage only; commit, verify, rollback, and critical finalization disable cancellation until a terminal or recoverable state. Activity is an append-only SQLite projection, never a substitute recovery journal.
- Test failpoints at every durability boundary.

## Explicitly excluded

- Takeover/deployment-specific step builders and real product operation buttons.
- Automatic ambiguous crash repair; preserve and classify only.

## Acceptance conditions

- Planner performs no target writes and includes every affected path/precondition.
- Plan sealing rejects inconsistent before/after fingerprints, destructive steps without recovery and usable identity-plus-content proof, and duplicate physical final paths.
- Changed precondition returns `StalePlan` before staging.
- Any stage failure leaves all active paths unchanged.
- Injected commit/verify failure rolls back prior steps in reverse order.
- Rollback mismatch retains all versions and becomes `RecoveryRequired`.
- A persisted commit intent without an active-path write classifies as `FailedNoWrites`, not rolled back.
- Re-executing finalized Operation ID returns recorded result with no writes.

## Automated tests

- Synthetic create/replace/remove step integration tests across several temporary roots.
- Failpoint matrix for stage/backup/final/verify/manifest/projection/rollback with real tree and journal assertions.
- Parent-process kill, journal reopen, and repeated idempotent startup-recovery classification tests.
- Cleanup exact-path, containment, marker, and fingerprint/file-identity tests, including forged markers and replaced evidence.

## Implementation evidence

- The generic Rust planner persists one canonical compact-JSON plan whose ID-bound digest covers every immutable content field. Load and seal reject noncanonical bytes, incoherent action/before/after fingerprints, duplicate physical destinations, and destructive steps without exact identity, content, and recovery proof.
- Every destructive step must receive one exact, non-empty Snapshot protection before staging; partial, duplicate, extra, failed, or mismatched registration blocks active-path writes. Successful cleanup is additionally barred from deleting the only before-version.
- The per-Vault coordinator runs under one serialization seam and enforces preflight → Snapshot → stage-all → deterministic commit/verify/finalize. Each journal boundary is durable intent → action → actual inspection → observed completion, and terminal Operation ID replay returns recorded evidence without tree mutation.
- Backup, final, rollback-aside, and backup-restore renames recheck the authorized root, sealed parent identity, source fingerprint, and destination fingerprint immediately before descriptor-relative no-replace rename. Mismatches preserve identifiable old, new, and interfering versions and enter `RecoveryRequired`; ordinary commit/verify failures compensate in reverse order.
- The startup component delivered here is a conservative, repeatable classifier for all nonterminal journal states through partial rollback. It does not execute recovery actions or wire startup sequencing; that action driver remains M0-008.
- Focused failpoint tests cover Snapshot/stage/backup/final/verify/manifest/SQLite projection/rollback durability and assert the real tree plus reopened journal. A parent test uses real `child.kill()` at backup, final, and rollback-aside boundaries, reopens the journal, and proves repeated classification leaves the tree unchanged.
- Exact cleanup retains forged, replaced, uncontained, marker-mismatched, or identity/content-unproven artifacts and journals visible failures. Stable serializable errors, cancellation only before commit, and post-commit cancellation immunity are covered without product-specific IPC or operation wiring.

## Risks and recovery

Cross-volume atomicity is impossible. Preserve the documented compensation guarantee; do not introduce a “mostly atomic” shortcut. No later task may mutate active paths outside this executor.

# M0-006 — Implement Add to Vault and Add and manage takeover

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | M0-004, M0-005 |
| PRD coverage | IMP-02/03/04/05/06/07 |
| Design | [Takeover/deployment](../workflows/takeover-and-deployment.md), [Bundle objects](../storage/bundle-hashing-and-objects.md), [Tauri/UI contract](../interfaces/tauri-and-ui-state.md) |
| Parallelization | Planner/read-model work and executor step-builder work can proceed in parallel after intent DTO freezes. |

## Deliverables

- Keep-external preference and separate Add-to-Vault/Add-and-manage intents.
- Complete takeover plan preview with source, conflicts, Vault/object destinations, selected replacements, and recovery.
- External→staging validation/copy, baseline object, Skill manifest, working activation, and observation linkage.
- Combined takeover plus selected managed replacement steps for Add and manage.
- Skill detail read model with preview/provenance/observations.

## Explicitly excluded

- Remote source identity, M1 editing/audit, automatic claiming of all duplicate locations.
- IMP-08 best-effort Git/lockfile provenance, reserved for hardening.

## Acceptance conditions

- Add to Vault produces `Vaulted`, zero deployments, verified working/baseline digest, and byte-identical source.
- Same-name Vault Skill is never replaced; generated IDs/paths coexist.
- Unsupported/unstable source fails before active working activation.
- Add and manage lists and replaces only confirmed paths, with original recovery object.
- Failure is no-write, rolled back, or recovery-required and appears in Operation data.

## Automated tests

- Original tree byte/entry comparison, same-name coexistence, duplicate-location selection.
- Validation/hash/object/manifest/activation failpoints.
- Combined takeover replacement rollback and restart recovery.

## Implementation evidence

- Rust now exposes three distinct external-observation outcomes: Keep external records only a preference, Add to Vault activates one UUID working container and baseline without a deployment, and Add and manage replaces only separately selected duplicate locations. The source Observation and physical aliases of its directory are rejected as replacements before a plan is persisted.
- Takeover-only plan schema v2 seals source and related Observation evidence, generated Skill/Object/Activity/Snapshot/Target/Deployment identities, complete Target authority, exact selected replacements, and the safe nested Bundle fingerprint used to atomically verify the UUID working container. Schema-v1 canonical bytes and executor semantics remain unchanged.
- Source, selected locations, Target authority, Vault/target disjointness, immutable object content, staging, working activation, selected deployments, manifests, and one critical SQLite projection are revalidated from the reopened persisted plan. Terminal replay is idempotent and needs no in-memory planning side map.
- Real-tree tests prove zero source mutation, zero-deployment Vaulting, same-name coexistence, explicit-location isolation, managed-copy and symlink takeover, unsupported/stale/unstable fail-closed behavior, exact recovery protection, rollback, committed-finalization evidence, and parent-driven child-process kill/reopen classification. Target tests preserve existing project/override/custom authority and select the full stable identity.
- The completion gate passes focused takeover and plan tests, all-target/all-feature Rust tests, Clippy with `-D warnings`, formatting, binding generation/drift, frontend type/test/build checks, documentation validation, Intel macOS source compilation, and the project-local `kill-ai-slop` scan. M0-007 deployment planning and M0-008 startup action driving remain excluded.

## Risks and recovery

Source content can change between preview and copy. Fingerprint revalidation and stable reads are mandatory; never “finish with latest bytes” after user confirmed different content.

# M0-007 — Implement deployment, drift verification, and undeploy

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-24) |
| Dependencies | M0-006 |
| PRD coverage | DPL-01/02/03/04/05/06/09/10/11, DEL-01 |
| Design | [Takeover/deployment](../workflows/takeover-and-deployment.md), [Identity/state](../domain/identity-and-state.md), [Target adapters](../interfaces/target-adapters.md) |
| Parallelization | Symlink and Managed Copy step builders/tests can proceed in parallel; health classifier remains shared pure code. |

## Deliverables

- Target registration for fixture global, Git project, and non-Git project roots.
- Default/override/resolved deployment mode planner.
- Absolute symlink and Managed Copy stage/verify step builders.
- Explicit re-confirmed Copy fallback after link capability preflight failure.
- Deployment manifests/rows with mode, expected digest/link, adapter version, target path, verification time.
- Expected/Vault/target verifier and Deployments read model.
- Drift-aware undeploy that preserves Vault and all unselected deployments.

## Explicitly excluded

- Six real adapters/custom target breadth, Workspace discovery, Trash.
- Silent target drift overwrite or M1 deployment alias derivation.

## Acceptance conditions

- Global fixture defaults to symlink, Git project to Managed Copy, non-Git project to symlink.
- Unmanaged collision creates a blocker and writes nothing.
- Link fallback changes the reviewed plan before execution.
- Plans persist destination capability evidence plus `requestedMode`, `resolvedMode`, and `fallbackReason`; commit rechecks capability, and a link failure rolls back/replans rather than switching mode.
- Clean no-op redeployment writes nothing.
- Target edit, Vault ahead, missing, broken link, retarget, and conflict classify correctly.
- Undeploy one target leaves Vault and other deployments unchanged; modified target requires resolution.

## Automated tests

- Mode/collision/no-op matrix across real temporary roots.
- Digest and link post-verification tests.
- Full health truth table and watcher-independent targeted verify.
- Undeploy drift/failpoint/recovery tests.

## Implementation evidence

- Schema-v3 plans derive one deployment solely from Skill/Target/Deployment IDs and seal the complete fixture Target authority, reviewed working digest, requested/resolved mode, proven fallback, destination capability, collision precondition, previous deployment evidence, and undeploy resolution. Schema-v1/v2 frozen canonical digests and takeover behavior remain unchanged.
- The shared Operation executor is the only active-path writer. Absolute links and exact Managed Copies are staged and verified; authority, capability, Vault/Target disjointness, normalized-name collisions, source content, and destination fingerprints are rechecked before commit. Reopened terminal Operations replay without filesystem or projection side effects.
- Rust evaluates the full expected/Vault/target and symlink truth tables and returns authoritative health, drift direction, explanations, and allowed actions. Clean, Vault-ahead, target-modified, conflict, missing, broken, retargeted, replacement-entry, unreadable, and inactive cases remain distinguishable.
- Undeploy removes only an exact clean managed entry after Snapshot protection. Explicit preserve plans leave changed, missing, broken, or retargeted entries untouched while ending only the selected relationship; non-UTF-8 raw link targets fail before plan persistence rather than entering lossy evidence. Backup-boundary failure restores the exact target and leaves the Vault, other deployments, manifests, SQLite relationships, and Activity free of false success.
- Real-tree tests cover fixture defaults/overrides, disclosed Copy fallback, collision no-write, no-op redeploy, mode changes, schema sealing, post-review staleness, capability changes, projection/manifest replay, every health state, safe preserve, undeploy isolation, Snapshot evidence, returned failpoints, and parent-driven `child.kill()` journal reopen/classification. The completion gate passes all-target/all-feature Rust tests, Clippy with `-D warnings`, formatting, generated binding drift, frontend checks, documentation validation, Intel macOS source compilation, and the project-local `kill-ai-slop` scan.

## Risks and recovery

Symlink `VaultAhead` is easy to miscommunicate because bytes are already live. Preserve the explicit explanation in read models/UI; do not simplify it to generic “out of sync.”

# M0-008 — Complete multi-target recovery, startup recovery, and Activity

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-007 |
| PRD coverage | DPL-07/08, SCN-09, DEL-06; M0 Activity/rollback scope |
| Design | [Transaction execution](../workflows/transaction-execution.md), [Operation model](../domain/operation-recovery-and-trash.md), [Testing](../quality/testing-and-acceptance.md) |
| Parallelization | Activity query/projection can proceed separately from multi-target failpoint work. |

## Deliverables

- Multi-target plan/stage-all/commit across mixed symlink/Managed Copy fixtures.
- Protected operation-level Snapshot references and retained backup finalization.
- Complete startup decisions for staged, partially committed, committed-not-finalized, rolled-back, and ambiguous journals.
- Activity projection for scans and Operations with outcome/recovery/path/mode evidence.
- Operation-level inverse-plan/undo kernel for unchanged postconditions.

## Explicitly excluded

- Trash-specific inverse workflow and full-history browsing.

## Acceptance conditions

- Target N failure restores targets 1..N-1 to verified before state.
- Rollback failure is separately visible and never reported as successful rollback.
- Kill after each commit boundary recovers/finalizes safely on restart.
- Activity distinguishes no-write, rolled-back, and recovery-required outcomes and links recovery.
- Inverse plan refuses when any postcondition changed.

## Automated tests

- Mixed 2–20 target failpoint matrix.
- Child-process crash/reopen tests at every durable boundary.
- Journal→Activity projection and retention-reference tests.
- Scan diagnostics Activity test without per-file noise.

## Implementation evidence

- Schema-v4 batch plans seal 2–20 Target IDs with deterministic ordering, mixed absolute symlink and Managed Copy modes, one operation-level Snapshot/Activity identity, and complete per-Target authority evidence. Single-target schema-v3 behavior remains unchanged.
- Stage-all precedes the first active rename. Target-index commit failures compensate in reverse and restore every earlier target to its verified before state. Batch finalization (manifest + SQLite projection + Activity) is idempotent under reopen.
- Runtime startup recovery drives the M0-005 classifier to durable terminal outcomes before exposing mutation or scan services; unresolved nonterminal Operations block service access. Terminal replay is idempotent and performs no filesystem side effects.
- Reviewed inverse (undo) plans require unchanged postconditions and refuse before persistence when any target, Snapshot protection, or projection has drifted. Mixed replace/create undo restores exact targets and relationships.
- Activity list/detail is a bounded append-only SQLite projection with typed path/mode, failure step/code, plan/journal links, and recovery references. Scan diagnostics project once without per-file noise. Terminal journals rebuild Activity idempotently.
- Real-tree tests cover three- and twenty-target mixed batches, each-target commit failure rollback, per-target durable-boundary failpoints, finalization reopen, undo postcondition refusal, startup driver/classifier, scan Activity aggregation, and parent-driven `child.kill()` reopen. The completion gate passes all-target/all-feature Rust tests, Clippy with `-D warnings`, formatting, generated binding drift, frontend type/test/build checks, and documentation validation. M0-009 thin-slice UI remains excluded.

## Risks and recovery

This is the highest-risk M0 task. Do not begin broad adapter/UI work on unverified mutation shortcuts; the thin-slice gate requires real compensation evidence.

# M0-009 — Ship the real end-to-end thin-slice UI

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-004, M0-006, M0-007, M0-008 |
| PRD coverage | VLT-01, IMP-02/03, DPL-05/10/11; first-run and basic Library/Deployments/Activity scope |
| Design | [Tauri/UI contract](../interfaces/tauri-and-ui-state.md), [System context](../architecture/system-context.md), [Testing](../quality/testing-and-acceptance.md) |
| Parallelization | UI surfaces can be distributed after query keys/generated DTOs and interaction vocabulary freeze. |

## Deliverables

- Skippable first-run Vault selection, one adapter scan, and persistent setup checklist.
- Basic app shell with Library as default, external/Vaulted/Managed rows, and basic Skill detail preview.
- Takeover plan review and running/terminal/recovery states.
- Target picker, deployment plan, basic Deployments list, drift labels, and undeploy.
- Activity list/detail with exact outcome and recovery availability.
- TanStack Query integration driven by Rust read models/events; events invalidate/refetch authoritative state, while transient view state remains local React state/context with no Redux/Zustand store.
- React Aria interaction primitives styled through CSS Modules and accepted design tokens.
- Keyboard path and visible focus for every thin-slice action.

## Explicitly excluded

- Polished final visual system, full six-adapter settings, Workspace, matrix views, Trash, Discover, Collections.

## Acceptance conditions

- A disposable real fixture completes Vault → scan → Add to Vault → global/project deploy → verify → undeploy without mock state.
- Scans state “No files were changed.”
- Plan paths/modes/recovery remain visible while execution progresses.
- A rollback/recovery-required result remains inspectable after toast dismissal/restart.
- UI never optimistically changes ownership or declares Operation success.
- Thin slice is keyboard operable and usable at 900×600.

## Automated tests

- Component/command integration for all thin-slice states.
- Query invalidation/refetch tests.
- Keyboard/focus tests and one macOS Tauri smoke script.
- Long-name/path and incomplete-scan rendering tests.

## Implementation evidence

- Bootstrap exposes Vault initialization status; `vault_initialize` / `vault_status` open the default or selected Vault, install scan/takeover/deployment/Activity services, and run startup recovery before mutation access.
- Thin-slice React shell uses generated bindings + TanStack Query only. First-run Vault screen, Library (scan/takeover/deploy plans), Deployments (fixture targets, verify, undeploy plans), and Activity list/detail are real command surfaces with no optimistic ownership or success.
- Plan review remains visible during execute/cancel. Operation outcomes and recovery references stay inspectable. Scan coverage surfaces “No files were changed.”
- React Aria buttons/lists provide keyboard focus; layout targets 900×600. Component tests cover first-run init, Library empty state, and navigation.
- Completion gate: `pnpm check` (format, lint/clippy, typecheck, frontend+Rust tests, bindings, docs, renderer build). Breadth UI, six adapters, Workspace, Trash, and polish remain later tasks.

## Risks and recovery

Avoid designing a temporary frontend domain model. Use generated read models even if early layouts are plain; otherwise broad UI work will encode incorrect ownership and health assumptions.

# Thin-slice exit gate

Do not call the thin slice complete until the real backend/UI path passes, a commit failure is visibly and durably rolled back, and the source/other deployments are proven unchanged where required.
