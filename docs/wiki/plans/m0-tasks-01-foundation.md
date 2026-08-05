---
type: Implementation Plan
title: M0 Tasks 001–003 — Foundation
description: Executable tasks for the application shell, domain/hash/path contracts, and durable Vault storage.
status: planned
tags: [skills-hub, m0, tasks, foundation]
requirements: [VLT-01, VLT-02, VLT-03, VLT-04, SCN-03, SCN-04, IMP-05, IMP-07, DPL-06, DPL-09, DPL-10, DEL-01]
timestamp: 2026-07-23T00:00:00Z
---

# M0-001 — Bootstrap the product and contract test harness

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | None |
| PRD coverage | M0 macOS shell; implementation decision in PRD §13.1 |
| Design | [System context](../architecture/system-context.md), [Runtime modules](../architecture/runtime-and-modules.md), [Tauri/UI contract](../interfaces/tauri-and-ui-state.md) |
| Parallelization | Critical-path bootstrap; frontend and Rust harness setup may proceed in parallel after directory/tool choices are fixed. |

## Deliverables

- Initialize Tauri 2, React, strict TypeScript, Vite, and pnpm workspace files without copying CC Switch product code.
- Establish one Rust crate with strict internal modules and frontend feature-shell boundaries from the runtime design; do not create a multi-crate workspace.
- Run the Specta/Tauri Specta command-binding compatibility spike, pin compatible toolchain/dependencies, commit lockfiles, and record required local setup. If blocked, retain Rust-authored generated DTOs and one centralized typed invoke wrapper.
- Add formatting, lint, typecheck, Rust/frontend unit-test, generated-contract, and OKF/link-check commands.
- Configure committed typed Rust→TypeScript DTO/command generation with CI drift checking and one smoke command; raw `invoke` calls are not distributed through UI features.
- Establish React Aria Components, CSS Modules, CSS design tokens, system appearance, reduced-motion, and increased-contrast foundations without a heavy component suite.
- Set final identity (`Skills Hub`, `skills-hub`, `com.terrylan.skillshub`), macOS 14 minimum, local/ad-hoc signing posture, and a native Apple Silicon local package while preserving and compile-checking `x86_64` compatibility where feasible.
- Establish thin async Tauri commands over the Tauri/Tokio runtime plus a bounded blocking-work harness; commands never perform long filesystem or hashing work inline.
- Build disposable temporary HOME/Vault/project fixture helpers and deterministic test-only failpoint interface.
- Create a minimal macOS app window that renders Rust bootstrap state; no product dashboard placeholders.

## Implementation boundary

This task proves the build/test/IPC loop and makes later work independently testable. It owns no real Skill, scanner, or mutation behavior.

## Explicitly excluded

- Vault schema and filesystem layout implementation.
- Real adapter paths, scanning, deployment, product UI styling, remote providers.
- Final `DESIGN.md`; only shell-level accessibility defaults are needed.

## Acceptance conditions

- A clean documented setup launches the Tauri app on macOS.
- One typed command round-trips a version/bootstrap DTO; generated TypeScript is deterministic.
- Bundle metadata uses the accepted product identity/minimum macOS and does not claim notarization, Intel runtime validation, or Universal Binary support.
- Rust and frontend unit tests run independently.
- Fixture helper creates and removes only its temporary root.
- A test can select failpoint name/step index without affecting release builds.

## Automated tests

- Contract-generation snapshot/cleanliness test.
- Compile checks for both macOS Rust targets where the local/CI toolchain permits.
- Tauri command serialization/error smoke test.
- Fixture containment and cleanup test.
- CI/local script that validates every current OKF page/frontmatter/link.

## Implementation evidence

- `pnpm check` passes formatting, ESLint/Clippy, strict TypeScript, frontend and Rust tests, generated-contract drift, OKF validation, and renderer build.
- Rust 1.89 compiles both `aarch64-apple-darwin` and `x86_64-apple-darwin`; the native package remains an Apple Silicon artifact rather than claiming a Universal Binary.
- Tauri Specta `2.0.0-rc.21`, Specta `2.0.0-rc.22`, and Specta TypeScript `0.0.9` are pinned because the newer RC requires a Rust standard-library API unavailable in the accepted 1.89 toolchain.
- `pnpm tauri build --bundles app` produces one ad-hoc-signed `Skills Hub.app` with identifier `com.terrylan.skillshub`, minimum macOS `14.0`, and only the `skills-hub` executable.
- A `pnpm dev` smoke run rendered the real Rust bootstrap response as Connected/ready, generated contract schema 1, and four bounded workers without an error state.
- The disposable HOME/Vault/project fixture and named step failpoint tests pass; failpoint support exists only in the integration-test harness.

## Risks and recovery

Dependency incompatibility is the main risk. Resolve it here and lock versions; do not spread temporary adapters across later tasks. If DTO generation is not Tauri-2 compatible, choose one alternative generator and update the runtime concept before domain DTOs proliferate.

# M0-002 — Implement domain values, canonical hashing, and path policy

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | M0-001 |
| PRD coverage | SCN-03/04, IMP-05, DPL-06/09/10, DEL-01; foundation for VLT-04 |
| Design | [Identity/state](../domain/identity-and-state.md), [Bundle hashing](../storage/bundle-hashing-and-objects.md), [Operation model](../domain/operation-recovery-and-trash.md), [Filesystem safety](../security/filesystem-safety.md) |
| Parallelization | Hash/path implementation and pure state-model implementation can proceed separately after shared value types freeze. |

## Deliverables

- Typed IDs and safe values for Skill, Observation, Adapter, Target, Deployment, Operation, Snapshot, digest, deployment name, and Bundle-relative path.
- UUIDv7 IDs for Skill, Operation, Deployment, Snapshot, and Activity; content-digest identity for Revisions.
- Typed UTC timestamp and duration values: RFC3339 at JSON/DTO boundaries, Unix milliseconds for the later SQLite projection, and monotonic time for in-process elapsed/timeout measurement.
- Ownership, lifecycle, deployment-health, Operation, and terminal-outcome state machines.
- Duplicate/name-conflict classifier independent of enumeration order.
- `sha256-bundle-v1` walker/encoder plus committed golden fixtures.
- Stable-read detection and explicit unsupported-entry errors.
- Default Bundle caps (depth 64, 10,000 entries, 1 GiB total regular bytes, 256 MiB/file), observed statistics, and pre-content-read blocking.
- Lexical/canonical authorized-path policy, symlink classification, normalization/case collision key, and path fingerprints.
- Pure Operation Plan value/serialization/digest with preconditions, actions, blockers, and recovery summary.

## Implementation boundary

Functions are pure or read-only against caller-provided paths. No SQLite and no active-path mutation is introduced.

## Explicitly excluded

- Object publication, scan scheduling, Operation execution, Tauri product commands.
- Full M1 security content audit or trust scoring.

## Acceptance conditions

- Hash output is invariant to creation order/mtime but changes for bytes, paths, empty directories, entry type, or executable bit.
- Non-UTF-8/special/escaping entries produce precise errors and never lossy identities.
- All expected/Vault/target health combinations match the accepted truth table.
- Same-name/same-content and same-name/different-content classify correctly independent of input order.
- Random untrusted relative paths cannot escape an authorized root in property tests.
- Plans serialize deterministically and changed fingerprints invalidate confirmation.

## Automated tests

- Golden vectors for every canonical hash rule.
- Table/property tests for ownership, duplicate, health, Operation states, and inverse steps.
- Path traversal, nonexistent descendant, broken-link, Unicode normalization, and case-collision tests.
- Stable-read mutation and symlink-cycle fixtures.

## Implementation evidence

- Rust now exposes typed UUIDv7 entity IDs, digest-backed Revision identity, adapter IDs, safe deployment/Bundle names, RFC3339/Unix-millisecond timestamps, monotonic durations, ownership/duplicate/health classifiers, and guarded lifecycle/Operation transitions without adding product IPC commands.
- `sha256-bundle-v1` implements the documented domain separator, fixed numeric entry/mode tags, NFC path encoding, bytewise ordering, hidden and empty entries, semantic executable mode, raw link payloads, precise caps/statistics, and stable reads for files and links. Golden vectors are frozen at `92bfb918…e05515` (minimal) and `2f2e92ed…011db8` (all entry/mode classes).
- Real temporary-directory tests cover creation-order/mtime invariance, content/path/mode/link changes, unsupported sockets, non-UTF-8 path rejection without lossy conversion, missing/invalid `SKILL.md`, cap blocking, absolute/escaping/broken/cyclic links, and mid-read file/link mutation.
- Authorized roots canonicalize the nearest existing ancestor, reject intermediate links and non-directory ancestors, permit terminal-link inspection without following it, retain volume/file metadata, and pass arbitrary-string containment property tests. Unicode full case-fold + NFC collision tests cover target-sensitive and target-insensitive policies.
- Pure Operation Plans can only capture paths derived from `AuthorizedPath`; plans include IDs, modes, fingerprints, ordered actions, blockers, active caps/statistics, recovery estimates, and cross-filesystem consequences. Compact JSON/digests are deterministic, inverse steps swap pre/postconditions, changed fingerprints alter confirmation, and noncanonical persisted content fails digest verification.
- `cargo fmt --check`, Clippy with `-D warnings`, all-target/all-feature Rust tests, the Intel macOS library compile check, and the repository-wide `pnpm check` pass. The completed Rust suite contains 44 library tests plus generated-binding and fixture-harness integration tests.
- A focused read-only protocol/safety review found three blockers (noncanonical plan verification, link stable-read consistency, and hard-coded link-validation caps); all three were corrected before the hash vectors and schema decisions were recorded here.

## Risks and recovery

The hash format becomes durable protocol. Do not change golden outputs after objects exist; introduce a new schema and migration instead. Path behavior must be validated on real macOS filesystems, not only mocked paths.

# M0-003 — Implement Vault, SQLite, manifests, objects, and migrations

| Field | Value |
| --- | --- |
| Status | Complete (2026-07-23) |
| Dependencies | M0-002 |
| PRD coverage | VLT-01/02/03/04, IMP-07 |
| Design | [Vault/SQLite](../storage/vault-and-sqlite.md), [Bundle objects](../storage/bundle-hashing-and-objects.md), [Runtime modules](../architecture/runtime-and-modules.md) |
| Parallelization | Migration/repositories and filesystem/object implementation may proceed in parallel against frozen domain contracts. |

## Deliverables

- Default (`~/Library/Application Support/Skills Hub/Vault`)/custom guided Vault initialization, device-local `~/Library/Application Support/Skills Hub/settings.json`, `.manager/vault.json`, layout creation, identity/nesting validation, device single-instance focus behavior, and process-lifetime OS advisory exclusive `.manager/locks/vault.lock` whose metadata is never treated as stale-lock proof; lock failure blocks opening/mutation.
- `rusqlite_migration`-compatible embedded ordered hand-written SQL, applied checksums, immutable released migrations, pre-upgrade backup, and initial logical schema.
- Dedicated database executor thread that exclusively owns the primary `rusqlite::Connection`, with foreign keys, WAL, busy timeout, batched `synchronous=NORMAL` derived-index writes, and explicit `FULL` migration/critical-finalization boundaries.
- Device settings JSON outside the Vault and Vault-readable `vault.json`/manifests, all schema-versioned, atomic, validated, credential-free, and preserving corrupt originals.
- Atomic readable Skill/deployment manifest read/write/version validation.
- SQLite timestamps stored as UTC Unix-millisecond `INTEGER`; JSON manifests use RFC3339 UTC. Manifest/object publication follows temp write → file fsync → atomic rename → parent-directory fsync.
- Immutable object publication/reuse/verification from Operation-owned staging.
- Repository methods for Skills, observations, targets, deployments, Operations, steps, Snapshots, Activity, Workspace roots, and projects.
- Temporary-database backup/replace utility used by later rebuild work.

## Implementation boundary

Expose storage services to application code and tests. This task may create fixture working Skills/objects through internal test APIs but does not implement user takeover or target deployment.

## Explicitly excluded

- Vault watcher, relocate/repair/rebuild UI, object GC.
- Scanner, transaction executor, Trash workflow.

## Acceptance conditions

- Clean/default and selected custom Vaults initialize idempotently.
- Same-name Skills occupy separate UUID containers while retaining deployment basename.
- No Skill file bytes are stored as SQLite content blobs.
- Same verified digest publishes/reuses one object; corrupted existing object is rejected.
- Atomic manifest failure leaves either old or complete new manifest, never truncated JSON.
- Deleting a copied test SQLite database leaves working Bundles/manifests human-readable.
- Migrations pass empty/history/failure/checksum/idempotence cases, preserve the pre-upgrade database on failure, and reject changed historical checksums.

## Automated tests

- Real temporary SQLite migration/repository/foreign-key tests.
- Filesystem tree, permission, manifest crash-boundary, object dedupe/corruption tests.
- Vault nesting/alias/lock conflict tests.
- A schema test that asserts no content-blob columns or frontend SQL path exist.

## Implementation evidence

- [`OpenVault`](../../../src-tauri/src/persistence/vault.rs) initializes/reopens the transparent layout, validates target/settings nesting and aliases, holds an OS advisory lock, retains stable Vault identity, updates schema-versioned device settings, and exposes the database/manifests/object services without adding a Tauri mutation command.
- The embedded [v1 SQL migration](../../../src-tauri/src/persistence/migrations/0001_initial.sql) creates 17 projection tables in addition to checksummed `schema_migrations`. The lightweight [migration runner](../../../src-tauri/src/persistence/migrations.rs) freezes SHA-256 checksum `6bbfca126d052df3a78e21e9345ac2cb33e2c50b8623a3a279fa316a3d926a9c`, validates `user_version` and history, backs up before upgrades, runs pending migrations in one `FULL`/IMMEDIATE transaction, and refuses database replacement while hot WAL/SHM sidecars exist.
- A dedicated [`DbExecutor`](../../../src-tauri/src/persistence/executor.rs) owns the sole primary connection. Typed [repositories](../../../src-tauri/src/persistence/repositories.rs) cover every initial projection and provide an atomic `FULL` Operation/Activity finalization boundary; schema evidence asserts no Skill content/blob columns.
- [Manifest contracts](../../../src-tauri/src/persistence/manifests.rs) use strict versioned readable JSON and durable sibling-temp replacement. Unsupported future schemas are never overwritten; malformed replaceable settings are copied to a diagnostic sibling before regeneration, while Vault identity remains fail-closed.
- The [object store](../../../src-tauri/src/filesystem/objects.rs) independently enforces copy caps, preserves contained links and executable semantics, recomputes the staged digest, writes and flushes `object.json`, atomically publishes by digest, removes owner-write permission, verifies dedupe reuse, and rejects corrupt existing objects.
- 73 Rust library tests plus binding/harness tests pass across empty/reopen/history/failure/checksum/future-version migrations, foreign keys and rollback, manifest crash boundaries, Vault lock/nesting/readability, and object dedupe/corruption/interrupted staging. Format, Clippy `-D warnings`, frontend/type/binding/docs checks, and Intel library compilation form the completion gate.

## Risks and recovery

Filesystem and SQLite cannot share one atomic transaction. Keep journal/manifests as durable reconstruction inputs and do not add ad hoc dual writes outside later Operation execution. Migration errors preserve the previous database and block mutation.

# Foundation exit gate

The gate is closed: M0-001–003 tests pass together, hash vectors/path policy remain frozen, and a temporary Vault is created, reopened, locked, migrated, and verified without Tauri UI state. `M0-004` may proceed.
