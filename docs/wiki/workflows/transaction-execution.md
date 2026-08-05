---
type: Workflow Design
title: Transaction Execution
description: Defines persisted plans, path fingerprints, stage/commit/verify sequencing, compensation, and startup crash recovery.
status: accepted
tags: [skills-hub, m0, transaction, rollback, journal]
requirements: [IMP-04, IMP-07, DPL-05, DPL-06, DPL-07, DPL-08, DEL-06]
timestamp: 2026-07-23T00:00:00Z
---

# Transaction guarantee

Skills Hub cannot provide one OS-atomic commit across unrelated target volumes. It provides a reviewed and durable compensating transaction:

- every active-path commit step is locally atomic where the filesystem supports sibling rename;
- all targets are staged before the first commit;
- every replacement has a recovery point;
- committed steps are rolled back in reverse order after failure;
- a crash can be classified from a durable journal and actual path fingerprints;
- unresolved ambiguity preserves old/new content and requires recovery instead of guessing.

# Implemented transaction slices

M0-005 implements the generic transaction kernel through persisted terminal outcomes: canonical plan sealing/loading, exact Snapshot attestations, one per-Vault serialization seam, stage-all, deterministic commit/verify/finalization hooks, reverse rollback, exact artifact cleanup, and conservative startup classification. M0-008 adds the idempotent action driver for every classifier result, schema-v4 2–20 Target plans, journal-to-Activity rebuilding, and reviewed inverse Operations. Real-tree failpoint tests cover every target-index stage/backup/final/verify boundary, manifest and SQLite interruption, every rollback durability boundary, and repeated parent-driven child-process kills followed by action-driving reopen.

M0-013 reuses this kernel for schema-v5 internal-Vault MoveToTrash, Restore, and PermanentlyDelete Operations. Their plans use only Rust-derived paths beneath the authorized Vault, protect each destructive source at operation level, stage before commit, verify exact content, finalize durable lifecycle/Activity/Snapshot projections, and inherit rollback, startup finalization recovery, and terminal replay. This does not add Trash-specific inverse planning; the user-facing inverse remains the schema-v4 deployment path with explicit unavailable/conflict results elsewhere.

# Persisted plan and fingerprints

An immutable plan includes a digest over canonical serialized content. Every mutable path has a before fingerprint:

```text
PathFingerprint
├── normalized display path
├── nearest authorized root/target ID
├── expected entry kind: absent/file/directory/symlink
├── raw symlink target when applicable
├── volume ID and file ID when available
├── bundle digest or metadata fingerprint
├── managed Skill/deployment ID when applicable
└── captured time and adapter version
```

The UI executes by Operation ID and plan digest. Before sealing, Create/Replace/Remove/LeaveUntouched steps must have internally consistent before/after fingerprints, and each destructive before-version must carry both usable file/metadata identity and content proof plus a destructive-recovery flag. Two steps may not resolve to the same physical final parent identity and filename. Under the exclusive mutation lock, preflight recaptures fingerprints. Any mismatch returns `StalePlan` before staging.

# Durable journal

Each Operation owns `.manager/operations/<operation-id>/`:

```text
plan.json              # immutable reviewed plan, including final-parent identities
journal.json           # atomically replaced state summary and exact Snapshot protections
steps/<zero-padded-number>.json  # intent, before/after fingerprints, stage/backup/rollback evidence
```

`plan.json` is immutable; `journal.json` and each numbered step are atomically replaced. The strict sequence is persist and fsync intent → perform filesystem action → inspect actual result → persist observed completion. Corrupt or contradictory evidence becomes `RecoveryRequired`; it is never guessed. Journal writes use temporary file → flush/fsync → rename → parent-directory fsync. Activity remains a separate user timeline.

SQLite indexes journal state for queries; it is not the only crash-recovery evidence.

# Execution phases

## 1. Plan

- Resolve all paths from domain IDs and adapter descriptors.
- Classify no-ops, blockers, requested/resolved modes, and recovery needs.
- Persist plan and expose it for user review.
- No target writes occur.

## 2. Preflight

- Acquire the per-Vault mutation lock.
- Revalidate plan digest, expiry, adapters, roots, fingerprints, permissions, disk estimate, and mode capability.
- Abort as stale/blocked on any changed assumption.

## 3. Snapshot

- Capture every managed entry that may be replaced or removed into a verified immutable object or link fingerprint.
- Persist one non-empty Snapshot protection per destructive step before staging. Each protection names the step and attests to the exact sealed before fingerprint; an empty, partial, duplicate, extra, or mismatched registration blocks commit.
- Unmanaged collision remains blocked unless the Operation is an explicit takeover of that entry.

## 4. Stage all

- Create unpredictable, operation-owned hidden siblings under each final target parent, ensuring same-volume final rename.
- Build symlinks or Managed Copies in those siblings.
- Hash/verify every staged entry against expected postconditions.
- If any stage fails, remove only verified operation-owned staging and return `FailedNoWrites`.

## 5. Commit in deterministic order

Order by normalized target identity/path so retries and tests are deterministic. For each destination:

1. Recheck the sealed final-parent identity and destination fingerprint immediately before the backup rename.
2. Journal `commit-intent`.
3. If an existing managed entry is replaced, rename it to a unique operation-owned sibling backup.
4. Recheck the sealed final-parent identity and destination again immediately before renaming staged entry to final.
5. Capture and journal the resulting fingerprint.

Each target is now locally switched, but the overall Operation is not yet successful.

## 6. Verify all

- Recheck entry type, link target, Bundle digest, expected absence, and adapter-specific postconditions.
- Preserve sibling backups until every target verifies.
- Any mismatch enters rollback.

## 7. Finalize

1. Publish updated Skill/deployment manifests atomically.
2. Finalize deployments, Snapshots, Operation, and Activity in one SQLite transaction.
3. Mark the filesystem journal finalized.
4. Remove sibling backups only after their exact before-versions are independently protected by the persisted per-step Snapshot attestations; cleanup may not delete the only before-version.
5. Schedule targeted reconciliation and publish invalidation events.

The UI receives success only after step 3.

# Rollback

On stage/commit/verify failure:

1. Stop later commits.
2. Iterate committed steps in reverse order.
3. Before restoring, ensure the current final path still equals the Operation's observed post-step fingerprint.
4. Persist the actual current source fingerprint, recheck parent and destination immediately before the rename, then rename the current operation-owned final aside or remove it only when ownership is proven.
5. Recheck parent and destination again immediately before restoring the sibling backup, then verify the original fingerprint.
6. Record each rollback completion/failure durably.

If a path changed unexpectedly during the Operation, do not overwrite it. Preserve all identifiable versions, set `RecoveryRequired`, and present exact paths and next safe actions.

A `Committing` journal with durable intent but no active-path write classifies as no-writes. Contradictory or incomplete durability evidence is never upgraded to rolled back or successful.

# Startup recovery

Before watchers or new mutations start, inspect every non-terminal journal:

| Actual state | Automatic action |
| --- | --- |
| Only verified staging exists; active path unchanged | Remove staging, mark failed/no writes. |
| Old backup exists; final not switched | Restore old and verify. |
| Final matches planned postcondition; all other steps committed | Continue verification and metadata finalization. |
| Some finals committed and backups match before state | Roll back committed steps in reverse order. |
| Final/backup/staging contradict journal or contain unknown changes | Preserve all and mark recovery required. |

Recovery is idempotent. Killing the process repeatedly at the same recovery boundary must not lose another copy.

The process-lifetime runtime runs this action driver while opening the configured Vault, before scan/watch or mutation services become available. If any nonterminal Operation remains unclassified or cannot be reconciled, mutation services stay blocked while the typed startup report remains readable. A terminal journal replay never re-inspects or rewrites its target paths.

# Idempotency and retry

- Planning against an already satisfied clean deployment creates a no-op step.
- Executing the same finalized Operation ID returns its recorded result and performs no writes.
- A stale or failed plan is never “resumed” as a new intent; generate a new Operation.
- Startup recovery may finish the same journal because it is reconciling a previously authorized intent.
- A completed multi-target deploy can produce a new reviewed schema-v4 inverse plan. The inverse binds each reversed step to the completed source Operation and, for original replacements, to its exact protected Snapshot reference. Planning verifies every current postcondition before persisting the inverse; execution uses this same kernel and retains both histories.

# Cleanup contract

Cleanup may touch only exact stage/backup paths recorded by the journal and proven to live under an authorized operation or target parent. Orphan cleanup requires retention expiry, an ownership marker/fingerprint, no live journal reference, and containment verification. Failure to clean is Activity evidence, not a reason for a broader recursive retry.

M0-015 applies the same contract to specialized lifecycle cleanup: the exact reviewed old Vault is first quarantined by operation-derived no-replace rename and its device/inode is rechecked before recursive deletion; relocation staging binds path, operation marker, Vault identity, and inode; GC recovery accepts only the operation/digest-derived pending-delete path; and capability/atomic-write temporary artifacts are removed only while the created inode remains. Copied markers and path replacements are retained for recovery rather than treated as cleanup authority.

# Failpoints

Production code exposes test-only deterministic failpoints after each durable boundary:

- snapshot publication;
- stage target N;
- backup rename N;
- final rename N;
- verify target N;
- manifest publication;
- SQLite finalize;
- rollback step N.

Tests pair returned errors with child-process termination to cover both compensation and real crash recovery.

# Related concepts

- [Operation model](../domain/operation-recovery-and-trash.md)
- [Takeover and deployment](takeover-and-deployment.md)
- [Vault and SQLite](../storage/vault-and-sqlite.md)
- [Filesystem safety](../security/filesystem-safety.md)
