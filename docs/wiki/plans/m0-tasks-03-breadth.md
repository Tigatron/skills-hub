---
type: Implementation Plan
title: M0 Tasks 010–014 — Product Breadth
description: Executable tasks for Workspace discovery, watchers, all six adapters, custom targets, Vault lifecycle, Trash/undo, and the complete M0 UI.
status: planned
tags: [skills-hub, m0, tasks, breadth]
requirements: [VLT-05, VLT-06, VLT-07, VLT-08, SCN-01, SCN-02, SCN-04, SCN-05, SCN-06, SCN-07, SCN-08, SCN-09, IMP-01, IMP-02, IMP-07, DPL-01, DPL-02, DPL-03, DPL-04, DPL-09, DPL-10, DPL-12, DEL-01, DEL-02, DEL-03, DEL-04, DEL-05, DEL-06]
timestamp: 2026-07-23T00:00:00Z
---

# M0-010 — Implement Workspace Roots, project discovery, and watcher reconciliation

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-004 |
| PRD coverage | SCN-04/05/06/07/08/09; watcher foundation later reused for VLT-05 |
| Design | [Scanning and reconciliation](../workflows/scanning-and-reconciliation.md), [Target adapters](../interfaces/target-adapters.md), [Filesystem safety](../security/filesystem-safety.md), [Tauri/UI contract](../interfaces/tauri-and-ui-state.md) |
| Parallelization | Workspace traversal/discovery and the watcher coordinator can proceed in parallel after the scan-boundary and coverage-record contracts freeze. Runs in parallel with M0-011 and most of M0-012. |

## Deliverables

- Workspace Root add/update/pause/remove/rescan commands, persistence, and authorized-path identity per the adapter and scanning designs.
- Ignore-aware bounded traversal with hidden-directory visibility, default pruned components, per-root user ignores, and configurable depth (default 8, range 1–32) recording `CoverageIncomplete` at limits.
- Git project-boundary detection (`.git` directory or worktree file), implicit project detection from adapter project-relative suffixes, and manually added Git/non-Git projects.
- Observation association to the nearest owning project boundary; nested repositories remain distinct.
- Streaming per-project result batches that never make partial output the new absence baseline.
- Narrow `WatchBackend` over `notify`: normalize/coalesce possible-change/coverage-lost/disconnected invalidations, targeted rescans, and proactive startup/resume/wake/overflow/operation-finish-or-rollback reconciliation.
- Per-source coverage diagnostics read model: enabled/paused, last attempt/success, error counts with inspectable examples, and complete/incomplete/stale/never-scanned coverage.

## Implementation boundary

Everything in this task is read-only against user content. The watcher translates events into scan requests; it never mutates ownership, health, or files.

## Explicitly excluded

- Vault working-directory external-edit marking (owned by M0-012 on this watcher foundation).
- Any takeover/deployment behavior change; six-adapter descriptor verification (M0-011).
- Complete Settings surfaces (M0-014); only typed commands and read models are required here.

## Acceptance conditions

- Directory symlinks are never followed during traversal; cycles terminate locally without failing unrelated candidates.
- Ignores prune dependency/build trees but cannot silently hide adapter target components; reduced coverage is reported.
- A depth-limited or permission-limited scan reports incomplete coverage instead of claiming a complete result.
- Canceled or partial Workspace scans never mark unseen prior observations stale.
- Removing a Workspace Root removes authorization and coverage settings without deleting any project file.
- Injected dropped/overflowed watcher events are recovered by targeted or bounded full reconciliation.
- An inaccessible root is distinguishable from an empty root in the diagnostics read model.

## Automated tests

- Workspace fixture matrix: nested Git repositories, dependency/build trees, hidden adapter directories, symlink cycles and escapes, manual projects.
- Property tests for ignore/depth combinations and traversal order independence.
- Fake-watcher fault injection: repeated, reordered, dropped, disconnected, and overflow events, rename storms, and wake-from-sleep reconciliation.
- Pause/resume/rescan idempotence and root add/remove command DTO tests.

## Risks and recovery

Filesystem events are unreliable by design; treat them strictly as invalidation hints backed by reconciliation, never as correctness evidence. Large Workspace trees can be slow: keep bounded blocking workers and progressive results rather than raising depth defaults.

## Implementation evidence

Implemented on 2026-08-05 as a read-only Rust-owned Workspace scanner and reconciliation driver. Typed commands persist add/update/pause/remove/rescan authorization, stable filesystem identity, manual Git/non-Git projects, coverage diagnostics, and progressive per-project batch events. Traversal is hidden-aware, ignore-aware, symlink-safe, prunes default dependency/build components, enforces depth 1–32 (default 8), and reports incomplete coverage rather than establishing absence at a traversal, permission, identity, cancellation, or project-batch limit. Git worktrees, nested repositories, implicit adapter suffixes, and manual boundaries retain nearest-project ownership. The `notify` backend only produces invalidations; startup, focus resume, periodic wake fallback, overflow, disconnect, root replacement, and operation completion/rollback drive targeted or bounded reconciliation, with failed backend registration and reconciliation retried. Focused fixtures and property tests cover nested/implicit/manual projects, ignores/depth, symlink escape/cycles, cancellation, replaced identities, unavailable manual projects, watcher coalescing/faults, lifecycle idempotence, and complete-coverage-only absence.

# M0-011 — Verify and ship all six adapters and custom targets

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-004, M0-007 |
| PRD coverage | SCN-01/02, DPL-01/02/03/04/09 |
| Design | [Target adapters](../interfaces/target-adapters.md), [Scanning and reconciliation](../workflows/scanning-and-reconciliation.md), [Takeover and deployment](../workflows/takeover-and-deployment.md) |
| Parallelization | Per-adapter verification and fixtures are independent and can be distributed. Custom-target work is a separate strand. Runs in parallel with M0-010 and most of M0-012. |

## Deliverables

- Six verified adapter descriptors (`universal-agent-skills`, `claude-code`, `openai-codex`, `cursor`, `gemini-cli`, `opencode`) with recorded official-source URL, verification date, supported scopes/modes, and caveats in fixtures/docs.
- Adapter enable/disable and global/project path overrides producing configured Targets tied to adapter version plus override metadata.
- Custom target registration: display name, selected concrete root, scope label, mode preference, optional project association, canonical-identity validation, and blocked mutation until reselection when identity changes.
- Global scan breadth across all six enabled adapter roots plus overrides and custom targets.
- Deployment planning, mode defaults, and post-commit verification exercised against each family's global and project targets.
- Adapter version bump marks related deployments `Unverified` until revalidated, without rewriting paths.

## Implementation boundary

This task broadens data-driven descriptors over the existing generic filesystem adapter, scanner, and planner. It introduces no second scan or deployment code path.

## Explicitly excluded

- Code-specific transformation adapters; none are assumed for the initial six.
- Non-macOS platform behavior and Windows link abstractions (M2 seam).
- M1 deployment-alias derivation for name collisions.

## Acceptance conditions

- Every descriptor fixture passes the contract tests defined in the adapter design, including missing-root read-only coverage.
- Overrides and disabled adapters persist, affect scan coverage, and never authorize paths outside their registered roots.
- Custom targets pass containment, collision, plan, snapshot, and rollback rules identical to built-in targets.
- Global targets default to symlink, Git projects to Managed Copy, non-Git projects to symlink, for every family.
- Deployment rows record adapter ID and version, and adapter version changes flip health to `Unverified`.

## Automated tests

- Descriptor serialization/expansion contract fixtures per adapter.
- Override, disable, and re-enable scan coverage tests.
- Custom-target containment, moved-root identity, and collision tests.
- Six-root scan matrix and per-family deployment mode/verification matrix on temporary HOME/projects.

## Risks and recovery

Upstream path conventions may change between verification and release; the recorded evidence plus user path overrides keep the product usable, and re-verification updates only descriptor data. Do not mark a descriptor `Verified` without a documented source check.

## Implementation evidence

Completed on 2026-08-05 through the existing generic scanner and Operation kernel. Six versioned descriptors carry current official-source evidence; persisted adapter configuration controls enablement and safe global/project overrides; configured global scans cover defaults, overrides, and custom global targets independently; and custom registration records display/scope/mode/project metadata plus canonical filesystem identity. Deployment planning applies the same family-independent mode defaults and post-commit verifier, while changed adapter-version evidence returns `Unverified` without rewriting the recorded target path. Generated bindings expose descriptor/configuration, scan-all, override, and custom-target commands. Automated matrices cover descriptor identity/path serialization, six-root no-write scanning, disable/override/custom/re-enable breadth, all-family global/Git/personal mode defaults and post-commit verification, custom root replacement and explicit reselection, stale takeover authority refusal, and adapter-version invalidation. Acceptance re-review passed the full 231-test Rust suite (228 passed, 3 intentionally ignored), strict Clippy, generated binding check, and OKF documentation check.

# M0-012 — Implement Vault lifecycle: watch, verify, repair, relocate, rebuild, and GC

| Field | Value |
| --- | --- |
| Status | Complete (2026-08-05) |
| Dependencies | M0-003, M0-008 |
| PRD coverage | VLT-05/06/07/08 |
| Design | [Vault and SQLite](../storage/vault-and-sqlite.md), [Bundle objects and retention](../storage/bundle-hashing-and-objects.md), [Operation model](../domain/operation-recovery-and-trash.md), [Transaction execution](../workflows/transaction-execution.md) |
| Parallelization | Verify/rebuild, relocate, and GC are separable strands. The Vault working-root watcher registers through the coordinator introduced in M0-010; coordinate on the shared scan-boundary contract or land that piece after M0-010 merges. |

## Deliverables

- Vault working-directory monitoring that marks external edits by recomputing the working digest and updating deployment health, never overwriting user changes (VLT-05).
- Reveal in Finder for the working Bundle directory.
- Read-only Vault verify job comparing layout, manifests, objects, working paths, and index references (VLT-06).
- Repair plans limited to safe automatic actions: rebuilding derived index rows or restoring a manifest from unambiguous indexed data; ambiguous identity is refused, not guessed.
- Relocate as one global Operation: cross-volume copy into destination staging, full digest/manifest/SQLite verification, quiesced cutover, device-config switch, repair of every managed absolute symlink, deployment re-verification, and old-Vault authority/retention until explicit success. Interruption supports resume/rollback/restart (VLT-06).
- Explicit index rebuild from manifests/journals with old-database backup and atomic swap (VLT-07).
- Object garbage collection with reference verification, retention window, two-phase pending-delete, Activity evidence, and automatic disable when the index is unhealthy (VLT-08).
- Behavioral destination capability preflight (write/dir/symlink/executable bit/atomic rename/file+dir fsync/lock/case), persisted `Supported`/`Unsupported`/`Unknown` result, commit recheck, and M0 blocking for unknown/network/cloud destinations.
- Opportunistic reference-aware GC after startup UI and normally at most once per 24 hours plus relevant delayed/manual triggers; serialize it as mutation, skip offline/RecoveryRequired, and run no closed-app daemon.

## Implementation boundary

All mutating lifecycle actions (relocate, repair that writes, GC physical deletion) run through the M0-005 transaction executor and its cleanup contract. Verify and rebuild-planning are read-only against active content.

## Explicitly excluded

- M2 cross-device migration, Git backup, and full Vault export.
- Packed object storage formats; objects remain directory trees.
- Any editing of Skill working content.

## Acceptance conditions

- An external edit to a working Bundle is marked and reflected in health without any byte being overwritten.
- Verify makes no writes; a corrupted object or missing manifest is reported with exact paths.
- Deleting the SQLite index and rebuilding from manifests restores Skill IDs, deployment relationships, and digests; absent provenance is not invented.
- Relocation preserves `vaultId`, Skill IDs, deployment IDs, and digests; every managed symlink points at the new Vault and re-verifies; a mid-relocate failure leaves the old Vault authoritative.
- GC never removes an object referenced by a Skill baseline, Snapshot, protected Operation, Trash entry, or unresolved journal; pending-delete precedes physical deletion.

## Automated tests

- External-edit fixture with digest/health assertions and no-overwrite tree comparison.
- Rebuild-from-manifests after database deletion, including unresolved journal states.
- Relocate integration: capacity failure, mid-copy failure, symlink rewrite verification, and confirmation-gated old-Vault cleanup.
- GC reference matrix and index-unhealthy disable test.
- Repair-plan refusal test for ambiguous identity.

## Risks and recovery

Relocation has the largest blast radius of any M0 Vault operation; the old Vault must remain a complete recovery point until the user confirms the verified result. A GC defect destroys history silently, which is why deletion is two-phase and reference verification must pass in the same run.

## Implementation evidence

Implemented on 2026-08-05 in the Rust lifecycle service and typed command boundary. Real temporary-Vault tests exercise external-edit re-hashing with byte/tree no-overwrite evidence, exact-path read-only verification, reviewed unambiguous manifest restoration and stale-plan refusal, lifecycle evidence isolation/recovery classification, GC containment/index-divergence refusal, and relocation capability/retained-authority/confirmation checks. The service also implements manifest/journal-driven index replacement with a retained old-index backup, conservative digest reference discovery across manifests, baselines/revisions, Snapshots, Trash, protected/unresolved standard journals, and lifecycle journals, two-phase pending deletion, destination capability preflight, verified relocation cutover, managed absolute-link rewriting, and confirmation-gated old-Vault cleanup. Index rebuild and successful relocation deliberately return `restartRequired: true`: existing runtime handles remain attached to the retained recoverable old database/Vault until restart rather than being silently rebound.

# M0-013 — Implement Trash, restore, permanent delete, and operation undo

| Field | Value |
| --- | --- |
| Status | Planned |
| Dependencies | M0-008, M0-012 |
| PRD coverage | DEL-01/02/03/04/05/06, IMP-07 |
| Design | [Operation model](../domain/operation-recovery-and-trash.md), [Transaction execution](../workflows/transaction-execution.md), [Filesystem safety](../security/filesystem-safety.md), [Identity and state](../domain/identity-and-state.md) |
| Parallelization | Trash/restore/delete and the undo surface are separable after the Trash entry layout and lifecycle transitions freeze. |

## Deliverables

- Move-to-Trash Operation: plan lists every active deployment and requires undeploy-or-cancel resolution; working content, manifest, provenance, and protected snapshot references move together into `.manager/trash`; lifecycle becomes `Trashed` with a stable Skill ID (DEL-02, IMP-07).
- Configurable retention with default 30 days and a never choice; show cleanup date/space, prevent referenced/protected deletion, and never delete early under disk pressure (DEL-03).
- Restore returning the same Skill ID to an active working path, selecting a new UUID container when the old path is occupied; recreating deployments is a separate optional reviewed plan.
- Permanent delete available only from Trash, with secondary confirmation naming the Skill and retention consequences, removing Trash content and metadata references while immutable objects await reference-aware GC (DEL-04).
- Distinct action vocabulary and behavior for undeploy, move to Trash, and permanently delete across commands and read models (DEL-01).
- External unmanaged Skills expose ignore and take-over actions only; no routine direct-filesystem-delete command exists (DEL-05).
- Batch undeploy/delete producing one operation-level recovery point (DEL-06).
- User-facing operation undo on the M0-008 inverse-plan kernel: postcondition comparison, reviewed inverse plan, refusal with conflict choices when any path changed.

## Implementation boundary

All Trash transitions are Operations through the transaction executor. Trash is application-internal under `.manager/trash`; it never uses the macOS system Trash.

## Explicitly excluded

- M2 snapshot history browsing and batch-operation recovery beyond single-step undo.
- Object physical deletion policy (owned by M0-012 GC).

## Acceptance conditions

- A deployed Skill cannot reach Trash without resolving every listed deployment; “leave broken deployment” is not an option.
- A trashed Skill is presented as `Trashed`, not `External`/`Vaulted`/`Managed`, and retains content, provenance, and snapshot references for the retention period.
- Restore preserves the Skill ID even when a new container path is required.
- Permanent delete outside Trash is impossible; inside Trash it requires the secondary confirmation.
- Batch undeploy/delete failure recovers from one operation-level recovery point.
- Undo executes only when every postcondition still matches, runs as a new Operation, and preserves both histories.

## Automated tests

- Trash/restore/permanent-delete integration with tree comparison and stable-ID assertions.
- Occupied-container restore and deployment-recreation-as-separate-plan tests.
- Batch operation recovery-point and failpoint tests.
- Undo postcondition-mismatch refusal and successful-inverse tests.
- Retention expiry and protected-checkpoint retention tests.

## Risks and recovery

Deletion code must never generalize its scope: cleanup stays bound to exact journal-owned Trash paths per the safety contract. Undo against drifted targets is refused rather than merged; offer recovery choices instead of best-effort restoration.

# M0-014 — Complete the M0 UI surfaces

| Field | Value |
| --- | --- |
| Status | Planned |
| Dependencies | M0-009, M0-010, M0-011, M0-012, M0-013 |
| PRD coverage | SCN-05/09, IMP-01/02, DPL-10/12, DEL-01/02/03/04/05, VLT-06; PRD §9 information architecture and §12 UX brief |
| Design | [Tauri/UI contract](../interfaces/tauri-and-ui-state.md), [System context](../architecture/system-context.md), [Testing and acceptance](../quality/testing-and-acceptance.md) |
| Parallelization | Surfaces (Library, Deployments, detail, Activity, Settings, Trash) can be distributed by feature after read models and interaction vocabulary freeze; completion requires real backend integration, not fixtures. |

## Deliverables

- Complete Library: required columns/compact fields, filters/search, duplicate/conflict grouping, external/Vaulted/Managed rows, and virtualization at reference scale.
- Deployments with by-Agent/by-Project/by-Skill views over one data set, keyboard-navigable matrix, accessible status labels, and a non-matrix list fallback.
- Complete Skill detail: preview, provenance, observations, per-deployment health with drift direction, Snapshots, Activity, Trash/undo availability, allowed actions with disabled reasons.
- Complete Activity with outcome/recovery links and scan diagnostics; persistent recovery-required states.
- Settings: Vault (reveal/verify/repair/relocate/rebuild entry points), adapters with overrides and confidence labels, Workspace Roots with coverage diagnostics, custom targets, and Trash retention.
- Trash surface with restore and guarded permanent delete.
- Completed first-run flow and persistent setup checklist beyond the M0-009 thin slice.
- Operation Plan dry-run export action over `operation_plan_export`, producing human-readable JSON with paths, actions, modes, preconditions, blockers, and recovery summary, and no credentials (DPL-12).
- React Aria, CSS Modules, and design-token implementation across every surface; TanStack Query remains the sole server-state cache and event consumers always retain a refetch path.
- Long-path/long-name handling with reveal/copy access; Discover and Collections remain absent from navigation rather than shown dead.

## Implementation boundary

The UI renders Rust-provided read models and capabilities only; it computes no ownership, health, or success locally. New backend work in this task is limited to completing read-model/command coverage (including plan export) that the surfaces require.

## Explicitly excluded

- Discover, Collections, editor, update-review, and audit surfaces (M1).
- Final visual token system; `DESIGN.md` remains a separate deliverable after the shell exists.
- New mutation semantics of any kind.

## Acceptance conditions

- Every M0 surface operates against real backend data end to end; no fixture-only screens ship.
- The Deployments matrix and its list fallback expose identical data and actions, both keyboard operable with accessible labels.
- Exported plan JSON matches the persisted plan digest content and is stable across exports of the same plan.
- Stale/incomplete coverage, no-write failures, rolled-back failures, and recovery-required states each render distinctly and persist beyond toasts.
- All surfaces remain usable at 900×600 with long names/paths.

## Automated tests

- Component/integration suites per surface, including empty, partial-coverage, conflict, and recovery states.
- Query invalidation/refetch tests for every event scope.
- Matrix and list-fallback keyboard/focus tests.
- Plan-export snapshot test against a fixture Operation.
- Long-path and reference-scale rendering tests.

## Risks and recovery

The main risk is the UI quietly becoming a second source of truth as breadth grows. Every action button must be driven by Rust-provided capabilities and blocker reasons; if a needed capability is missing, extend the read model rather than inferring in TypeScript.

# Breadth exit gate

Feature-complete M0 requires: all six adapters and custom targets verified, Workspace discovery and watcher reconciliation stable, Vault lifecycle and Trash/undo operational through the shared executor, and every M0 surface integrated against real services. Only then do the [release-gate tasks](m0-tasks-04-release.md) begin their final sweeps.
