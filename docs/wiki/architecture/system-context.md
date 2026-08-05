---
type: Architecture
title: M0 System Context
description: Defines the M0 component boundaries, system-wide invariants, lifecycle, and milestone boundary.
status: accepted
tags: [skills-hub, m0, architecture]
requirements: [VLT-01, VLT-03, SCN-01, IMP-01, DPL-05, DPL-08, DEL-01]
timestamp: 2026-07-23T00:00:00Z
---

# Decision summary

M0 is one local desktop process. The React application requests use cases through typed Tauri commands; Rust owns all domain decisions and filesystem access; ordinary files hold Skill content; SQLite and readable manifests index relationships and recovery state.

```text
┌───────────────────────────────────────────────────────────────────┐
│ Tauri desktop process                                             │
│                                                                   │
│  ┌────────────────────────┐    typed commands/events              │
│  │ React + TypeScript UI  │◀──────────────────────────────┐       │
│  │ TanStack Query cache   │                               │       │
│  └────────────────────────┘                               ▼       │
│                                               ┌────────────────┐  │
│                                               │ Rust commands  │  │
│                                               └───────┬────────┘  │
│                                                       ▼           │
│  ┌─────────────────────────────────────────────────────────────┐  │
│  │ Rust application and domain services                        │  │
│  │ inventory · takeover · deployment · recovery · trash       │  │
│  └──────┬──────────────┬───────────────┬───────────────────────┘  │
│         ▼              ▼               ▼                          │
│  ┌────────────┐  ┌───────────┐  ┌─────────────────────────────┐  │
│  │ Vault and  │  │ SQLite +  │  │ Adapter registry, scanner,  │  │
│  │ object     │  │ manifests │  │ watcher, operation executor │  │
│  │ store      │  │ + journal │  │                             │  │
│  └────────────┘  └───────────┘  └──────────────┬──────────────┘  │
└────────────────────────────────────────────────┼─────────────────┘
                                                 ▼
                           ┌──────────────────────────────────────┐
                           │ Authorized local filesystem          │
                           │ agent roots · projects · custom dirs │
                           └──────────────────────────────────────┘
```

# Authority boundaries

| Concern | Authority | Consequence |
| --- | --- | --- |
| Skill bytes | Vault working directories and captured objects | SQLite never contains Skill content blobs. |
| Identity and relationships | Rust domain plus durable manifests | UI labels and paths are not identities. |
| Query index | SQLite | The index may be rebuilt from manifests and readable content where possible. |
| Deployment health | Rust verifier | React never derives health from path or digest fields. |
| Mutation outcome | Operation executor, journal, and post-write verification | An event or optimistic UI state cannot declare success. |
| Filesystem authorization | Rust path policy based on registered roots and domain IDs | Commands do not accept arbitrary mutation paths. |
| UI cache | TanStack Query | Events invalidate queries; they do not become a second source of truth. |

# System-wide invariants

1. A scan has no filesystem mutation capability.
2. A discovered path does not become owned merely because it was observed.
3. A Skill ID is generated and stable; no display name, folder name, digest, or scan order becomes its identity.
4. An unmanaged destination is never silently replaced.
5. Every mutation begins with a persisted plan bound to observed preconditions.
6. A plan whose preconditions changed is stale and performs no writes.
7. Important replaced state has a verified recovery point before commit.
8. All targets are staged before the first multi-target commit.
9. Multi-volume changes use logged compensation; the application does not claim impossible filesystem-wide atomicity.
10. Success is terminal only after filesystem verification and metadata finalization.
11. Hash, permission, or scan failure is `Unverified` or an explicit error, never `Clean`.
12. Undeploy, move to application Trash, and permanently delete remain distinct operations.

# Application lifecycle

## Startup order

1. Enforce one Skills Hub instance on the device (a second launch focuses it), then acquire an OS advisory exclusive lock for the selected Vault for the process lifetime. Lock metadata is diagnostic only and is never deleted as proof of staleness; lock failure blocks opening and mutation rather than offering an inconsistent read-only mode.
2. Load machine-local configuration and locate the Vault.
3. Open SQLite, enable migrations and integrity settings, and validate Vault identity.
4. Recover or classify every non-terminal operation journal before accepting new mutations.
5. Reconcile skill and deployment manifests with the index.
6. Expose the first Library read model.
7. Start background global scans, configured Workspace scans, and filesystem watchers.

The UI may become usable before background scans finish, but mutation commands remain disabled while startup recovery requires a decision.

## Shutdown behavior

- Stop accepting new mutations.
- Let a commit-phase operation finish or reach a durable recoverable boundary; cancellation never interrupts an atomic rename halfway through a step.
- Cancel read-only scans at safe checkpoints. A canceled scan never marks unseen observations missing.
- Flush operation and manifest writes before closing SQLite.

# M0 boundary

M0 includes local discovery, explicit takeover, deployment, drift, recovery, Activity, Trash, basic details, first run, settings, six adapters, and custom paths. It deliberately excludes:

- remote Git or skills.sh acquisition;
- archive import and package export;
- built-in editing and update comparison;
- full static security auditing and quarantine;
- Collections and project reproducibility manifests;
- hosted accounts, telemetry, backup, and sync;
- Windows/Linux packaging.

The architecture leaves provider, auditor, package, and platform-link extension points, but M0 must not ship placeholder navigation or partial implementations for them.

# Platform and release baseline

M0 requires macOS 14 Sonoma. Source remains compatible with Apple Silicon and Intel (`aarch64` and `x86_64`); the current local package is native Apple Silicon, CI compile-checks both where feasible, and M0 makes no Intel runtime-validation or Universal Binary claim. This is a source/local-build-first milestone: ad-hoc/local signing and package configuration are sufficient, with no Developer ID, notarization, or auto-update requirement. Documentation states unsigned status and never advises disabling Gatekeeper while retaining a future signing seam.

# Local diagnostics

Structured `tracing` logs stay local with bounded rolling retention (25 MB or seven days, whichever limit is reached first), `info` by default and debug only by opt-in. Records correlate Operation IDs and redact home/absolute paths where possible; they never contain Skill content, tokens, environment values, or credentials. M0 sends no telemetry or crash uploads. Diagnostic export is user-initiated and previewed. Activity, recovery journals, and diagnostic logs remain separate stores with separate purposes.

# Extension seams, not pre-implementations

- `SourceProvider` can be introduced beside local observations in M1 without changing Skill identity.
- Additional adapter behavior can implement the adapter contract without changing the operation engine.
- Content audit results can later bind to the versioned bundle digest.
- Collections can later produce a multi-Skill Operation Plan; they do not change deployment semantics.
- Windows can later add Junction/copy capabilities behind platform operations without changing domain modes.

# Related concepts

- [Rust runtime and modules](runtime-and-modules.md)
- [Identity and state](../domain/identity-and-state.md)
- [Operation model](../domain/operation-recovery-and-trash.md)
- [Transaction execution](../workflows/transaction-execution.md)
- [Tauri and UI state](../interfaces/tauri-and-ui-state.md)
