---
type: Architecture
title: Rust Runtime and Module Boundaries
description: Defines the initial crate structure, dependency direction, concurrency model, and foundational library choices.
status: accepted
tags: [skills-hub, m0, rust, tauri]
requirements: [VLT-03, VLT-05, SCN-08, DPL-07, DPL-08]
timestamp: 2026-07-23T00:00:00Z
---

# Decision

Begin with one Tauri Rust crate and one React application. Split the Rust crate by responsibility, not into multiple crates before reuse or compile-time boundaries justify it. Domain code stays independent of Tauri, SQLite, and concrete filesystem implementations.

```text
src-tauri/src/
├── commands/               # thin IPC boundary and DTO conversion
├── domain/                 # IDs, entities, value objects, state machines
├── application/            # use-case orchestration
├── adapters/               # target descriptors and registry
├── scanner/                # global/workspace scan and reconciliation
├── operations/             # planner, executor, journal, startup recovery
├── filesystem/             # path policy, hashing, copy, atomic switch, objects
├── persistence/            # SQLite repositories, migrations, manifests
├── platform/               # macOS link/path/volume behavior
└── runtime.rs              # tasks, locks, cancellation, app lifecycle
```

The frontend should organize by product feature (`library`, `deployments`, `activity`, `settings`, `operations`) plus shared generated contracts and components. It must not mirror every Rust module.

# Dependency direction

```text
commands ───────▶ application ───────▶ domain
                      │                  ▲
                      ├──▶ repository ports
                      ├──▶ filesystem ports
                      └──▶ operation ports

infrastructure implementations ────────┘
adapters/scanner/operations use domain values and infrastructure ports
```

Rules:

- `domain` imports only standard-library and serialization/value-type dependencies.
- `application` refers to repositories and filesystem behavior through narrow traits or concrete service interfaces that are replaceable in tests.
- `commands` performs schema-level validation and error conversion, then calls one application use case.
- No Tauri command executes SQL or manipulates a path directly.
- `scanner` cannot obtain the mutation service or operation executor.
- `operations` is the only owner of multi-path mutation sequencing.

# Initial technology choices

| Concern | Choice | Rationale |
| --- | --- | --- |
| Desktop shell | Tauri 2 | Confirmed product decision and native filesystem integration. |
| Frontend | React + TypeScript + Vite | Confirmed product decision. |
| Query cache | TanStack Query | Explicit server-state model; easy invalidation after Rust events. |
| SQLite | `rusqlite` with bundled SQLite | Small local application, explicit transactions, no reason to expose SQL to the UI. |
| Migrations | `rusqlite_migration` or a similarly lightweight rusqlite-compatible runner over embedded, ordered, hand-written SQL | Transactional, checksummed migrations remain explicit and reviewable. |
| Async runtime | Tauri/Tokio runtime | Background scans, progress events, cancellation, and blocking-work scheduling. |
| Filesystem events | `notify` | Cross-platform abstraction; events remain hints, not correctness evidence. |
| Tree traversal | `walkdir` for bounded bundle walks; `ignore` for Workspace traversal | Explicit link policy for bundles and efficient ignore-aware project discovery. |
| Digest | SHA-256 via `sha2` | Portable, stable object keys and future package compatibility. |
| IDs | UUIDv7 | Stable generated identity with useful chronological ordering. |
| Errors | `thiserror` in domain/application; `anyhow` only at outer diagnostic boundaries | Stable typed contracts cross layers and IPC; causal context remains available without leaking it. |
| DTO generation | Rust-authored DTOs and commands via Specta/Tauri Specta when compatible, otherwise generated DTOs | Prevents hand-maintained Rust/TypeScript drift; generated files are committed and CI drift-checked. |

Package versions are chosen and locked in `M0-001`; this concept owns the selection criteria, not version numbers.

# Runtime services

The Tauri managed state contains long-lived service handles rather than raw database connections:

- `DbExecutor`: one dedicated blocking thread owns the primary `rusqlite::Connection`; requests cross a channel and return typed results.
- `OperationCoordinator`: holds the per-Vault mutation lock and permits one mutation at a time.
- `ScanScheduler`: deduplicates global, Workspace, and targeted reconciliation jobs.
- `WatcherCoordinator`: translates coalesced filesystem events into scan requests.
- `EventPublisher`: emits progress and invalidation events after durable state changes.
- `CancellationRegistry`: creates cancellation tokens for read-only jobs and pre-commit operation stages.

M0-006 wires a long-lived `TakeoverService` from the open Vault and shared `OperationCoordinator`. Thin Tauri commands schedule its blocking filesystem/SQLite use cases through the bounded runtime; they do not receive repositories, raw mutation paths, or an executor directly. The in-memory cancellation registry is advisory only—the persisted plan, journal, and Rust read models remain durable truth.

# Concurrency contract

- SQLite writes are serialized by `DbExecutor`; reads may use bounded read-only connections only if measurements later justify them.
- CPU/blocking filesystem work runs through `spawn_blocking` with bounded concurrency. Hashing does not occupy async runtime worker threads.
- M0 permits exactly one mutation Operation per Vault. Controlled read-only scan/hash work may run in parallel; this deliberately trades mutation throughput for understandable recovery.
- Read-only scans may run during planning but are paused for roots being committed. Reconciliation resumes after verification.
- A confirmed plan is revalidated under the mutation lock immediately before staging.
- Watcher events caused by the application are coalesced, but never permanently ignored; each operation schedules a final targeted reconciliation.

# Cancellation

| Phase | Cancellation behavior |
| --- | --- |
| Scan/hash | Stop at the next entry boundary; retain prior complete index state. |
| Planning/preflight | Stop immediately; no filesystem writes occurred. |
| Snapshot/stage | Stop after the current safe step; remove only journal-owned staging. |
| Commit/verify/rollback | User cancellation is disabled; executor reaches a terminal or recoverable state. |

Closing the window follows the same rule: it cannot abandon an unjournaled commit.

# Error ownership

Domain and application errors derive from `thiserror` and are converted once into stable serializable codes, path-safe context, and suggested actions. `anyhow` is permitted only at outer diagnostic boundaries and never appears in domain or IPC contracts. Ordinary I/O failures return errors rather than panic. The UI receives stable codes and user-facing summaries; logs retain redacted causal chains. See [Tauri and UI state](../interfaces/tauri-and-ui-state.md).

# Verification implications

- Domain and planner tests do not start Tauri.
- Persistence tests use a real temporary SQLite database and embedded migrations.
- Filesystem integration tests use temporary roots and the same infrastructure implementation used in production.
- Command contract tests regenerate TypeScript and fail on an uncommitted contract diff.
- Failpoints live at operation durability boundaries, not in UI mocks.

# Related concepts

- [System context](system-context.md)
- [Vault and SQLite](../storage/vault-and-sqlite.md)
- [Scanning and reconciliation](../workflows/scanning-and-reconciliation.md)
- [Transaction execution](../workflows/transaction-execution.md)
