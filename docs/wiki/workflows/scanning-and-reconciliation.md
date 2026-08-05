---
type: Workflow Design
title: Scanning and Reconciliation
description: Defines read-only global and Workspace scans, project discovery, observation lifecycle, watchers, and diagnostics.
status: accepted
tags: [skills-hub, m0, scanning, workspace, watcher]
requirements: [SCN-01, SCN-02, SCN-03, SCN-04, SCN-05, SCN-06, SCN-07, SCN-08, SCN-09, IMP-01]
timestamp: 2026-07-23T00:00:00Z
---

# Invariant

Scanning can read metadata and bytes only inside an authorized coverage boundary. It cannot create directories, fix links, copy Skills, claim ownership, or delete stale paths. A scan result explicitly says “No files were changed.”

# Implemented M0-004/M0-010/M0-011 slices

The generic global-root path is implemented for all six verified adapters, enabled default roots, global overrides, and custom global targets. Rust performs immediate-child inspection, Bundle validation/hashing, managed and unknown link classification, cancellation, progress, durable per-root run/diagnostic/observation projection, and complete-coverage-only stale reconciliation. Missing, inaccessible, invalid, unsupported, unstable, and link-error candidates remain visible instead of becoming false absences. Duplicate active requests for the same configured source reuse the current job. Project-scoped custom targets remain in manual-project/Workspace discovery so their observations are not misclassified as global.

The external Library read model groups exact digest duplicates and preserves name conflicts, degraded locations, filtering, stable pagination, and Rust-provided next actions. Typed scan start/all/get/cancel and Library-list commands emit `scan-progress` and `domain-invalidated` events.

M0-010 implements persisted Workspace Root authorization and stable filesystem identity, bounded hidden/ignore-aware traversal, nested Git and implicit adapter project discovery, standalone and Workspace-owned manual projects, nearest project association, progressive `workspace-project-batch` events, and per-source coverage diagnostics. Partial or canceled batches are positive evidence only; only a terminal complete result can establish absence. A narrow `notify` backend coalesces path events into invalidations and drives real targeted or bounded rescans on startup, focus resume, periodic wake fallback, overflow, disconnect, root replacement, and operation completion/rollback. Watch registration and reconciliation failures remain queued for retry. Scanning and watching never mutate user content or independently change ownership/health.

# Scan sources

| Source | Authorization | Traversal |
| --- | --- | --- |
| Built-in global target | Known adapter path, enabled by default | One immediate child level. |
| Adapter path override | User-configured target root | One immediate child level. |
| Workspace Root | User-selected directory | Bounded, ignore-aware project discovery. |
| Manually added project | User-selected project root | Directly check each adapter's project-relative target. |
| Custom target | User-selected concrete directory | One immediate child level unless explicitly registered as a project target. |

The application never scans the entire home directory merely to discover projects.

# Global target algorithm

For each enabled adapter root independently:

1. Expand the path template in Rust and capture its normalized display path.
2. Inspect root metadata without following a root symlink.
3. If missing or inaccessible, record coverage status and continue with other roots.
4. Enumerate immediate children only.
5. For each real child directory, require a direct regular `SKILL.md`.
6. For each child symlink, classify it before any target resolution.
7. Validate and hash the Bundle using [the canonical hash](../storage/bundle-hashing-and-objects.md).
8. Emit/upsert an observation with adapter, scope, path, name, digest/status, and evidence.
9. Complete the root run, then mark previously seen-but-absent observations stale only if coverage was successful.

The one-level rule follows the useful part of CC Switch's local import scanner while replacing folder-name deduplication with normalized location and complete content digests.

# Symlink observations

- Unknown directory symlinks are not recursively traversed.
- If an unknown symlink resolves outside the registered scan root, record `SymlinkOutsideAuthorizedRoot`; do not hash target bytes.
- A known managed deployment is the narrow exception: if its stored deployment ID, raw link, and resolved target exactly match the registered Vault working path, classify and verify it through deployment reconciliation.
- Broken links remain observations with `BrokenLink`; `Path::exists` is not used to hide them.
- Symlink cycles are detected from file identity/visited paths and terminate locally without failing unrelated candidates.

# Workspace Root algorithm

Workspace discovery uses `ignore::WalkBuilder` or equivalent pruning, with hidden traversal enabled so `.agents`, `.claude`, and other target directories remain visible.

Default pruned directories are exact components such as:

```text
.git  node_modules  vendor  target  dist  build  .cache  .next
DerivedData  coverage  out
```

Users may add ignore patterns per root. Product ignores are applied before user additions; target directory components themselves cannot be ignored silently without showing reduced coverage.

The default maximum project-root depth is 8 relative directory levels, configurable per Workspace Root from 1–32. A depth limit records `CoverageIncomplete` rather than claiming a complete scan.

During traversal:

1. Never follow directory symlinks.
2. Record Git project boundaries when `.git` is a directory or worktree file.
3. Detect an implicit project when a traversed path matches an adapter's project-relative target suffix; the ancestor before that suffix is the project root.
4. For every detected/manual project, inspect only immediate children of known adapter project target directories.
5. Associate each observation with the nearest explicit project boundary that owns its target; nested repositories remain distinct.
6. Stream completed project batches without making partial results the new absence baseline.

A manually added non-Git project bypasses project discovery depth but still uses the same adapter target checks and path safety.

# Scan run and observation lifecycle

```text
queued → running → completed
                 ├→ completed_with_errors
                 ├→ cancelled
                 └→ failed
```

Only `completed` and successfully covered portions of `completed_with_errors` may mark unseen observations stale. Each root/subtree has an independent coverage record so one permission error does not invalidate the whole Workspace.

An observation stores:

- scan run and source root IDs;
- adapter, scope, and project ID;
- display, normalized, and available canonical/file identity;
- folder/deployment name and parsed `SKILL.md` display metadata;
- digest or explicit validation/hash error;
- entry/symlink classification;
- first seen, last seen, and stale time.

Re-running a stable scan is idempotent and does not create duplicate observations or Activity noise.

# Watchers are invalidation hints

Use `notify` behind a narrow internal `WatchBackend` adapter after initial indexing:

- watch Vault working roots, existing enabled global roots, authorized Workspace Roots, and active deployment parents;
- normalize, debounce, and coalesce events into root/path `PossibleChange`, `CoverageLost`, or `Disconnected` invalidations for the smallest known scan boundary;
- never mutate ownership or health directly from an event;
- trigger a targeted scan for changed Skill/target parents;
- proactively reconcile after startup, resume/wake, overflow, root replacement, disconnect, unreliable rename sequences, and every Operation finish or rollback;
- run startup and application-reactivation reconciliation even if no event was observed.

Operation-owned writes use an event-suppression/coalescing window only to avoid duplicate work. Every finalized or rolled-back Operation schedules post-operation verification, so suppressing a watcher event cannot suppress correctness.

# Cancellation and concurrency

- A scan checks cancellation between directory and bundle boundaries.
- A canceled run retains the prior complete index and any safely upserted current observations, but it marks no prior observation missing.
- Hashing and Workspace traversal use bounded blocking workers.
- Mutations pause scans only for affected roots, then request reconciliation.
- Duplicate scheduled scans for one boundary collapse into the newest request.

# Diagnostics and UI contract

For every source, expose:

- enabled/paused state and authorized root;
- last attempt and last successful complete scan;
- running progress and cancelability;
- projects/Skills observed;
- ignored paths/error counts and inspectable examples;
- depth/ignore/permission limitations;
- whether current coverage is complete, incomplete, stale, or never scanned.

An inaccessible root is not an empty root. UI copy must not render both as “0 Skills.”

# Verification

- Snapshot candidate trees before/after scanning and assert byte/metadata identity for files in scope.
- Test missing and inaccessible roots without losing successful results from other adapters.
- Test same-name/same-digest and same-name/different-digest independent of enumeration order.
- Test Workspace ignores, nested Git repositories, hidden adapter directories, depth limits, cancel, and symlink escape/cycles.
- Inject dropped/overflow watcher events and prove targeted/full reconciliation restores state.
- Fake-watcher tests repeat, reorder, drop, disconnect, and overflow events; only reconciliation may establish truth.

# Related concepts

- [Target adapters](../interfaces/target-adapters.md)
- [Identity and state](../domain/identity-and-state.md)
- [Bundle hashing](../storage/bundle-hashing-and-objects.md)
- [Filesystem safety](../security/filesystem-safety.md)
