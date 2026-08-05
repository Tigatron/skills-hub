---
type: Implementation Plan
title: M0 Delivery Roadmap
description: Defines the M0 dependency graph, vertical thin-slice gate, parallel work, and release Definition of Done.
status: planned
tags: [skills-hub, m0, roadmap]
timestamp: 2026-07-23T00:00:00Z
---

# Outcome

M0 replaces manual local Skill directory management on macOS. A clean installation can discover external Skills without mutation, take selected content into a transparent Vault, deploy/undeploy it safely, expose drift and conflicts, and recover from failed or destructive operations.

# Task dependency graph

```text
M0-001 Project bootstrap ✓
   │
   ▼
M0-002 Domain + hash + path contracts ✓
   │
   ▼
M0-003 Vault + SQLite + object store ✓
   │
   ├───────────────┐
   ▼               ▼
M0-004 Scanner ✓ M0-005 Operation kernel ✓
   │               │
   └──────┬────────┘
          ▼
       M0-006 Takeover ✓
          │
          ▼
       M0-007 Deploy/undeploy/drift ✓
          │
          ▼
       M0-008 Recovery + Activity ✓
          │
          ▼
       M0-009 Thin-slice UI                 ◀── first end-to-end gate

M0-004 ───────────────▶ M0-010 Workspaces + watchers
M0-004 + M0-007 ──────▶ M0-011 Six adapters + custom targets
M0-003 + M0-008 ──────▶ M0-012 Vault lifecycle
M0-008 + M0-012 ──────▶ M0-013 Trash + restore + undo
M0-009..M0-013 ───────▶ M0-014 Complete M0 UI
M0-008..M0-014 ───────▶ M0-015 Filesystem/reliability hardening
M0-014 + M0-015 ──────▶ M0-016 Accessibility + performance
M0-016 ────────────────▶ M0-017 Acceptance + packaging
```

# Delivery phases

## Phase A — foundation (`M0-001`–`M0-003`)

Create a working Tauri shell and freeze the compatibility-bearing contracts first: generated DTOs, stable IDs, canonical bundle hash, path policy, Vault layout, manifests, migrations, and objects. The gate is a testable storage core, not a visually complete empty application.

## Phase B — end-to-end thin slice (`M0-004`–`M0-009`)

Use the Universal Agent Skills adapter and disposable fixtures to prove:

```text
Create Vault
→ scan one known global root read-only
→ show one External Skill
→ Add to Vault
→ deploy to one global and one project target
→ verify drift/undeploy
→ show Activity and rollback evidence
```

`M0-009` is the first product gate. It must use the real Rust services rather than a mock-only demo, even if styling and breadth remain basic.

## Phase C — M0 breadth (`M0-010`–`M0-014`)

Add authorized Workspace discovery, watcher reconciliation, all six adapters, custom targets, Vault lifecycle operations, Trash/restore/undo, and complete Library/Deployments/Activity/Settings surfaces. This phase broadens already-proven semantics rather than introducing a second path for them.

## Phase D — release gates (`M0-015`–`M0-017`)

Stress path safety, crashes, permissions, stale plans, failure compensation, accessibility, reference-scale performance, and all 12 PRD acceptance criteria. Package a reproducible macOS release candidate and document any manual/platform verification that cannot be automated.

# Parallelization

After the thin-slice contracts are stable:

- `M0-010`, `M0-011`, and most of `M0-012` can proceed in parallel on separate modules.
- `M0-013` can begin once Operation recovery and Vault object references are stable.
- `M0-014` can develop surfaces against generated typed fixtures while backend breadth lands, but completion requires real integration.
- Security/fault suites in `M0-015` should be added alongside each mutation feature, then run as one gate; do not postpone all failure testing until the end.
- Accessibility and performance work can be distributed by surface, but `M0-016` owns the integrated result.

Avoid parallel code-writing inside the same transaction executor or schema migration sequence.

# Gate criteria

| Gate | Required evidence |
| --- | --- |
| Foundation | Hash golden vectors frozen; migrations/manifests/objects pass real filesystem/SQLite tests. |
| Thin slice | One real external Skill completes scan → takeover → deploy → undeploy; injected commit failure rolls back and Activity is accurate. |
| Feature complete | Six adapters/custom targets, Workspace, Vault lifecycle, Trash, and all M0 screens integrated. |
| Release candidate | 12/12 M0 acceptance criteria, path/crash hardening, keyboard workflow, and reference-scale measurements pass. |

# M0 Definition of Done

- Every requirement `VLT-01..08`, `SCN-01..09`, `IMP-01..08`, `DPL-01..12`, and `DEL-01..06` has implementation and verification evidence.
- All 12 M0 acceptance criteria pass on macOS.
- Scans and blocked/canceled plans demonstrate zero mutation.
- Fault injection demonstrates full rollback or explicit retained recovery state.
- Rust remains authoritative across generated frontend contracts.
- Core workflows work with network access disabled and no account/telemetry.
- Wiki concept statuses and [traceability](../traceability.md) match implementation reality.
- M1/M2 exclusions remain absent from M0 navigation and code paths except documented extension seams.

# Decisions to verify during implementation

- `M0-001` resolved the Rust 1.89-compatible typed IPC set to Tauri Specta `2.0.0-rc.21`, Specta `2.0.0-rc.22`, and Specta TypeScript `0.0.9`; lockfiles and contract drift tests preserve it.
- Current official paths and behavior of all six agents during `M0-011`.

Product name (`Skills Hub`), slug (`skills-hub`), bundle ID (`com.terrylan.skillshub`), default Vault, React Aria/CSS Modules/token foundation, minimum macOS 14, signing posture, and dual-architecture source compatibility are accepted contracts rather than open verification items.

# Task pages

- [Foundation tasks](m0-tasks-01-foundation.md)
- [Thin-slice tasks](m0-tasks-02-thin-slice.md)
- [Breadth tasks](m0-tasks-03-breadth.md)
- [Release tasks](m0-tasks-04-release.md)
