---
type: Quality Plan
title: Testing and M0 Acceptance
description: Defines automated test layers, fixtures, failure injection, accessibility, performance, and the M0 release gate.
status: accepted
tags: [skills-hub, m0, testing, accessibility, performance]
requirements: [VLT-01, SCN-01, SCN-04, IMP-04, DPL-07, DPL-08, DPL-10, DEL-01]
timestamp: 2026-07-23T00:00:00Z
---

# Quality strategy

Correctness evidence concentrates below the UI: pure domain tests prove state semantics, real temporary filesystems/SQLite prove integration, and deterministic failpoints prove compensation. UI tests prove that the authoritative states remain understandable and accessible.

# Test layers

## 1. Pure Rust unit and property tests

- Skill/Target/deployment name value objects and UUID serialization.
- Ownership transitions and duplicate/name-conflict classification independent of input order.
- Full Managed Copy and symlink health truth tables.
- Operation state machine, terminal outcomes, and inverse-plan eligibility.
- Canonical bundle hash golden vectors and stable-read detection.
- Path lexical validation, normalization/case collision, containment, and cleanup ownership.
- Plan canonical serialization/digest and stale fingerprint comparison.

Use `proptest` for hash/path/state invariants, random path components, observation order, state combinations, and the rule that a verified rollback is the left inverse of its commit step.

## 2. Rust integration tests with real infrastructure

Every test uses real `tempfile` filesystem infrastructure to create an isolated temporary HOME, Vault, adapter roots, Workspace, Git/non-Git projects, and SQLite database. Fixtures include:

- six global/project adapter roots;
- exact duplicate and same-name conflict Bundles;
- hidden files, executable files, empty directories, contained/escaping links;
- inaccessible/missing roots and unstable Bundle mutation;
- nested Git projects, ignored dependency/build trees, and symlink cycles;
- clean, drifted, missing, broken, retargeted, and conflicting deployments;
- Trash, retained objects, and unresolved journal states.

Tests compare filesystem trees before/after read-only and blocked operations, not only returned values.

## 3. Fault-injection and crash tests

Inject failure after every durable transaction boundary defined in [transaction execution](../workflows/transaction-execution.md). For each target index `N`, prove:

- staging failure changes no active path;
- commit/verify failure restores every prior committed managed target;
- rollback failure yields `RecoveryRequired` with old/new bytes retained;
- database-finalize failure is completed from journal on restart;
- startup recovery is idempotent.

At critical boundaries, launch an executor child process, terminate it, reopen the same Vault, and assert recovery. Returned test errors alone do not model process death.

## 4. Command and frontend tests

- Generated Rust→TypeScript contracts remain current.
- Tauri command tests reject arbitrary paths, stale plan digests, and invalid IDs.
- React Testing Library tests Library, detail, Operation Plan, Deployments, Activity, Settings, and Trash states.
- TanStack Query tests prove events invalidate/refetch rather than overwrite authoritative data.
- UI tests cover loading, empty, partial coverage, conflict, no-write failure, rolled-back failure, recovery-required, and long-path states.

## 5. Desktop smoke tests

Run a packaged or dev Tauri app on macOS against a disposable test HOME/Vault for first run and the complete thin slice. Automate stable portions; preserve a short repeatable manual script for native file pickers, Finder reveal, system appearance, and platform accessibility behaviors that the web harness cannot faithfully simulate.

No meaningless global coverage percentage is a release gate. Each risk is closed by evidence at its owning layer.

# M0 acceptance mapping

| PRD M0 criterion | Primary automated evidence |
| --- | --- |
| 1. Create/select Vault | clean-config integration + first-run UI test |
| 2. Scan all six global adapters without mutation | descriptor fixture matrix + before/after filesystem snapshot |
| 3. Workspace ignores and symlink cycles | Workspace traversal integration/property tests |
| 4. Same-name same/different content | digest/reconciliation table tests |
| 5. Add external Skill while original remains | takeover integration byte/tree comparison |
| 6. Global symlink and Git project Managed Copy | mode-specific deployment integration tests |
| 7. Collision plan and no writes before confirmation | planner + blocked-operation tree comparison |
| 8. Injected commit failure restores earlier targets | N-target failpoint and child-process recovery tests |
| 9. Target edits and broken links visible | verifier truth table + Deployments UI tests |
| 10. Undeploy/Trash/restore/delete distinct | domain/integration tests + copy/action UI tests |
| 11. Activity outcome and recovery accurate | operation journal→Activity projection tests |
| 12. Core workflows keyboard accessible | automated keyboard flow + macOS screen-reader/manual checklist |

No criterion is closed by a screenshot or happy-path unit test alone.

# Performance verification

Use generated fixtures at the PRD reference scale:

- 1,000 Vault Skills;
- 5,000 observations/deployments;
- 200 projects across several Workspace Roots;
- 20 targets in one Operation Plan.

Measure release builds on a documented contemporary Apple Silicon Mac:

| Target | Gate |
| --- | --- |
| Warm launch to usable Library | under 1.5 seconds |
| Warm metadata global scan | under 1 second |
| Workspace progressive first result | under 2 seconds |
| Local Library query/search | under 100 ms |
| Deployment plan excluding required new hashing | under 500 ms |

The UI must remain responsive during hashing, scanning, and operations. Measurements report dataset, hardware, build, and percentile/sample count; a single best run is not evidence.

# Accessibility verification

- Run static accessibility checks on component tests and manually verify native integration.
- Complete scan → takeover → plan → deploy → verify → undeploy → Trash → restore using keyboard only.
- Verify visible focus and focus return after cancel, completion, and dialog close.
- Verify tables or list alternatives expose headers, status text, and actions to assistive technology.
- Verify status never relies on color alone.
- Check 900×600, long paths/names, system light/dark, increased contrast, reduced motion, and text scaling.
- Treat inaccessible recovery or destructive confirmation as a release blocker.

# Reliability gates

- Repeated scans are idempotent and read-only.
- Replanning a satisfied clean deployment produces no writes.
- A crash during staging leaves active paths unchanged.
- A crash during commit is diagnosable and recoverable.
- No success signal occurs before post-commit verification.
- Network-disabled core workflow passes.
- Cleanup failure never widens deletion scope.

# Continuous integration

Once the application is initialized, required checks should include:

- Markdown/OKF/link validation for `docs/wiki`;
- Rust format, lint, unit, integration, and migration tests;
- frontend format/lint/typecheck/unit tests;
- generated-contract cleanliness;
- macOS-specific filesystem/failpoint suite;
- release-build smoke and accessibility checklist before M0 packaging.
- compile-check both `aarch64-apple-darwin` and `x86_64-apple-darwin` where CI tooling permits, without claiming Intel runtime validation.

Linux CI may run portable pure tests later, but it cannot replace macOS path/symlink evidence for M0.

# Definition of Done for a task

A task is done only when:

1. listed behavior and explicit exclusions are respected;
2. targeted automated tests pass;
3. no-write/rollback claims are demonstrated with filesystem evidence;
4. typed contracts and migrations are current;
5. affected Wiki concepts and traceability reflect implementation;
6. remaining risk or manual verification is recorded honestly.

# Related concepts

- [M0 roadmap](../plans/m0-roadmap.md)
- [Traceability](../traceability.md)
- [Filesystem safety](../security/filesystem-safety.md)
- [Tauri and UI state](../interfaces/tauri-and-ui-state.md)
