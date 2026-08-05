---
type: Domain Model
title: Operation, Recovery, and Trash Model
description: Defines Operation Plans, snapshots, Activity, undo, Trash retention, and terminal outcomes.
status: accepted
tags: [skills-hub, m0, domain, operations, recovery]
requirements: [IMP-03, IMP-07, DPL-05, DPL-07, DPL-08, DEL-01, DEL-02, DEL-03, DEL-04, DEL-06]
timestamp: 2026-07-23T00:00:00Z
---

# Operation is the mutation boundary

Every user-visible filesystem mutation is represented by one Operation. An Operation owns its immutable reviewed plan, preconditions, ordered steps, snapshots, durable journal, result, and user-facing Activity entries.

Read-only scans and previews are Jobs, not Operations. They may emit progress but cannot create a mutation step.

# Implemented M0-005 slice

The generic Rust Operation kernel now implements immutable canonical plans, exact destructive Snapshot registration, durable per-step journals, serialized stage/commit/verify/finalize, reverse compensation, terminal replay, conservative cleanup, and a read-only startup classifier. Its tests use real trees, durability failpoints, and parent-driven child-process kills. Product intent-to-step builders, takeover/deployment semantics, Operation Tauri commands, startup recovery action execution, and Activity UI remain owned by M0-006 through M0-008 and later UI tasks.

# Implemented M0-006 slice

The first product builder now maps takeover domain IDs and explicit choices into schema-v2 plans and executes them through the M0-005 kernel. Reopened plans carry all source, Target-authority, working-container, selected-replacement, Snapshot, manifest, and projection evidence needed for execution and replay. The generic fingerprint contract gained an explicit optional safe Bundle subpath so the executor can verify an atomically activated UUID container without inferring takeover semantics; schema v1 omits and rejects that extension. Automatic execution of classified startup actions is delivered in M0-008.

# M0-008 implementation evidence

Schema v4 adds a distinct hash domain for one reviewed 2–20 Target deployment or its reviewed inverse. It seals one Skill revision, deterministic per-Target authority/capability/mode/deployment evidence, one operation-level Activity identity and Snapshot identity, and optional inverse bindings to the completed source Operation, source step, and protected reference. The runtime now drives startup classification to a durable terminal result before exposing mutations, and Activity list/detail is a bounded append-only projection that can be rebuilt idempotently from terminal journals.

# Operation Plan

The plan is generated from domain IDs, never from UI-supplied action paths. It contains:

- Operation ID, kind, schema version, creation/expiry time, and plan digest;
- selected Skills, Targets, deployments, and ownership choices;
- exact paths that will be created, replaced, removed, or left untouched;
- requested and resolved deployment mode for each target;
- before fingerprints and required preconditions;
- ordered stage/commit/verify/rollback steps;
- collision, permission, unsupported-content, and drift blockers;
- active Bundle cap policy and observed depth/entry/regular-byte/single-file statistics, plus staging/Snapshot/rollback disk estimate;
- recovery points that will be created;
- consequences that cannot be atomic across filesystems.

Execution requires both `operation_id` and `plan_digest`. Any changed precondition produces `StalePlan`; the user receives a newly generated plan instead of a best-effort execution.

## Canonical plan representation

M0 plan schema v1 hashes the compact UTF-8 JSON bytes of the plan content, prefixed by `skills-hub-operation-plan\0v1\0`. Struct field order is fixed by the schema; selected ID and blocker sets are sorted and deduplicated; ownership choices are sorted and duplicate choices for one Skill are rejected; and the step array order is semantic. No arbitrary map or UI-supplied path enters the canonical content; exact display paths are constructed only from Rust-authorized roots and safe relative paths.

M0 plan schema v2 is the takeover-only extension. It uses the distinct `skills-hub-operation-plan\0v2\0` hash domain and adds typed, canonical evidence for the ownership decision; source and related Observation IDs, adapters, locations, fingerprints, digests, validation errors, and capture times; generated Skill, Activity, Snapshot, Target, and Deployment IDs; deterministic UUID working, baseline-object, and manifest paths; each explicitly selected replacement's full target authority, scope, project/custom/override facts, canonical root, Bundle-relative path, resolved mode, and step order; and an explicit safe Bundle subpath for hashing the otherwise opaque UUID working container. Validation binds those records to the generic steps and rejects a source or physical alias selected for replacement, an already owned or inexact directory, a target nested with the Vault, a changed target authority, content that differs from the reviewed source digest, another final path, or disagreement with the sealed mode and postcondition. The optional fingerprint subpath is omitted from schema-v1 JSON, and schema v1 rejects plans that try to use it, preserving schema-v1 canonical bytes, digest domain, and executor meaning.

M0 plan schema v3 is the deploy/undeploy extension, with the separate `skills-hub-operation-plan\0v3\0` hash domain. It seals one Skill working path and reviewed digest; the complete registered Target authority, project classification, adapter/version, canonical root, and probed capabilities; one Deployment ID, name-relative destination, previous expected evidence, requested/resolved mode, fallback reason, reviewed health, undeploy resolution, manifest path, Activity ID, and optional Snapshot ID. Validation binds this evidence to exactly one generic create/replace/remove/leave-untouched step and rejects unsupported custom authority, unknown project classification, unproven capabilities, silent mode switching, inconsistent managed ownership, and Vault/Target nesting. Schema v3 also permits an explicit `resolved_bundle_digest` only for an exact absolute symlink fingerprint; older schemas omit and reject it. The generic executor uses that field to verify both the raw link and the resolved working Bundle without inferring deployment product semantics.

M0 plan schema v4 is the multi-target deployment/inverse extension, with `skills-hub-operation-plan\0v4\0`. It preserves v1/v2/v3 bytes and semantics, permits 2–20 distinct registered Targets, and binds each canonical entry to one deterministic generic step. An inverse entry additionally binds the source Operation and source step; replacing an original replacement also seals the exact protected object or link reference. Unknown fields, duplicate authorities/paths, inconsistent modes, missing capabilities, unsafe Vault nesting, unbound inverse steps, or missing destructive recovery evidence are rejected before the digest is sealed.

Persisted verification recomputes the digest and also requires the stored content to equal the canonical rebuilt content. Reordered IDs, duplicate set values, or a step `order` that disagrees with its array position therefore fail verification rather than being normalized during confirmation. Any field rename, field reorder, serialization attribute change, or timestamp/enum representation change requires a new plan schema version.

`raw_symlink_target` preconditions use exact UTF-8 strings. An existing non-UTF-8 target is a planning blocker; it is never converted lossily. RFC3339 timestamps retain sub-millisecond precision while the later SQLite Unix-millisecond projection truncates it, so JSON remains the canonical durable representation.

# State machine

```text
planned → preflighted → snapshotted → staged → committing → verifying
                                                               │
                                                               ▼
                                                       committed → finalized

any non-terminal failure → rolling_back → rolled_back → failed
                                      └───────────────→ recovery_required
```

Terminal outcomes are deliberately specific:

| Outcome | Meaning |
| --- | --- |
| `Succeeded` | Filesystem result verified and metadata finalized. |
| `CancelledNoWrites` | User canceled before mutation. |
| `FailedNoWrites` | Planning, preflight, or staging failed without changing active paths. |
| `FailedRolledBack` | Active paths changed, then every committed step was restored and verified. |
| `RecoveryRequired` | Automatic compensation could not safely restore or finalize all paths. |

An Operation never reports generic success with hidden cleanup or rollback failures.

# Snapshots and recovery points

A Snapshot is metadata that references one or more immutable content objects and entry fingerprints. It is not a second mutable copy of a Skill.

Create a protected operation-level recovery point before:

- replacing or removing any managed destination;
- activating takeover content over an existing Vault working version;
- moving an active Skill to Trash;
- a batch undeploy/delete operation;
- an explicit rollback or restore that itself replaces current state.

Unmanaged collisions are blocked, not snapshotted and overwritten, unless the user explicitly changes the operation to a takeover of that exact location.

# Undo

M0 provides operation-level undo rather than arbitrary history editing:

1. Build an inverse plan from the completed journal and protected snapshots.
2. Compare every affected current path with the completed Operation's postcondition.
3. If all match, present the inverse plan for review.
4. If any path changed, refuse silent undo and show the conflict/recovery choices.
5. Execute the inverse as a new Operation, preserving both histories.

Undo availability therefore depends on retained snapshots and unchanged postconditions. M2 may add broader history without changing this safety rule.

For a completed schema-v4 deployment, inverse planning reverses all steps. An original Create becomes a protected Remove; an original Replace restores the exact prior directory object or raw link evidence. Undo finalization restores or deactivates each deployment manifest/relationship as appropriate, writes a separate Operation and Activity, and leaves the original history unchanged.

# Application Trash

Trash is inside the Vault's `.manager/trash`, not Finder's system Trash.

## Move to Trash

- The plan lists every active deployment.
- The user must choose to undeploy affected targets or cancel; “leave broken deployment” is not a default.
- Working content, skill manifest, source provenance, and protected snapshot references move together.
- The Skill ID remains stable and its lifecycle becomes `Trashed`.

## Restore

- Restore returns the same Skill ID to an active working path.
- If the old working path is occupied, the plan selects a new UUID container; identity does not change.
- Recreating prior deployments is a separate, optional reviewed plan.

## Permanent delete

- Available only for a trashed Skill.
- Requires a secondary confirmation that names the Skill and retention consequences.
- Removes Trash working content and active metadata references.
- Immutable objects remain until no Skill, Snapshot, Operation, or retention rule references them and GC verifies the object.

Default retention is 30 days, with configurable choices including never. The UI shows cleanup date and space. Unresolved Operations and protected Snapshots prevent deletion; disk pressure never causes silent early deletion. Settings may lengthen retention, and protected recovery checkpoints can outlive it.

# Activity

Activity is a user-facing append-only SQLite projection of durable facts, not the crash-recovery journal itself. The per-Operation filesystem journal remains the recovery source if SQLite finalization is interrupted; startup reconciliation can finish or rebuild the Activity projection from that evidence. Each entry includes:

- time, operation/job type, affected Skill/Target IDs, and concise summary;
- outcome and recovery availability;
- actual deployment modes and affected paths;
- error code and failed step where applicable;
- links to the Operation Plan, recovery action, or scan diagnostics.

M0 records filesystem and local-job activity. It has no source-provider network operations. Toasts may summarize Activity but never hold its only error or recovery information.

# Retention invariants

- A referenced object cannot be garbage-collected.
- A failed operation's recovery material is protected until resolved or explicitly abandoned after review.
- Cleanup targets exact journal-owned paths and never broad user directories.
- Batch mutation creates one operation-level recovery point even when it references many content objects.
- Reference-aware GC runs opportunistically while the app is open: after startup UI, normally at most once per 24 hours plus relevant delayed triggers, or manually. It is serialized as a mutation, requires a complete verified reference set, skips for `RecoveryRequired` or an offline Vault, and uses pending-delete followed by later revalidation. No daemon runs while the app is closed.

# Related concepts

- [Transaction execution](../workflows/transaction-execution.md)
- [Bundle objects and retention](../storage/bundle-hashing-and-objects.md)
- [Vault and SQLite](../storage/vault-and-sqlite.md)
- [Filesystem safety](../security/filesystem-safety.md)
