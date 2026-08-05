---
type: Workflow Design
title: Takeover and Deployment
description: Defines explicit ownership choices, takeover staging, deployment modes, collisions, drift resolution, and undeploy.
status: accepted
tags: [skills-hub, m0, takeover, deployment]
requirements: [IMP-01, IMP-02, IMP-03, IMP-04, IMP-05, IMP-06, IMP-07, IMP-08, DPL-01, DPL-02, DPL-03, DPL-04, DPL-05, DPL-06, DPL-09, DPL-10, DPL-11, DPL-12]
timestamp: 2026-07-23T00:00:00Z
---

# Ownership choices

For an external observation, the UI exposes three semantically distinct choices:

| Choice | Vault content | Existing location | Deployment |
| --- | --- | --- | --- |
| Keep external | None | Untouched | None created. |
| Add to Vault | New working Skill + local baseline | Untouched | None created. |
| Add and manage | New working Skill + local baseline | Only selected locations are replaced after separate confirmation | Managed relationships created. |

No default checkbox converts “Add to Vault” into deployment. Same-name Vault content never becomes an overwrite target merely because names match.

# Implemented M0-006 slice

The Universal-fixture takeover path now implements all three choices with persisted plans and the shared Operation kernel. Add to Vault publishes a verified local baseline, atomically activates the UUID working container, writes the Skill manifest, and finalizes the Skill/Object/Revision/source/Operation/Activity projection without creating a deployment. Add and manage adds only explicitly selected same-digest external locations, protects each original, stages symlink or Managed Copy content from the verified Vault revision, verifies every final, and atomically projects Target/Deployment/Snapshot relationships. Real-tree failpoint, rollback, replay, Target-authority, and child-kill/reopen tests cover this baseline slice.

# M0-007 implementation evidence

The Universal-fixture deployment seam registers Global, Git-project, and non-Git personal-project Targets and builds schema-v3 single-target deploy or undeploy plans from Skill/Target/Deployment IDs. The shared Operation executor is the only active-path writer: it stages an absolute link or exact Managed Copy, rechecks Target authority, capability, case/Unicode collision identity, and Vault/Target separation before commit, verifies the final link/digest, protects replacement/removal with exact Snapshot evidence, and idempotently finalizes the deployment manifest, SQLite relationship, Operation, Snapshot, and Activity. Targeted verification returns Rust-owned E/V/T or link health, explanation, drift direction, allowed actions, and disabled reasons. Clean removal and explicit preserve-target undeploy affect exactly one relationship; startup evidence is classified but not automatically acted on.

# M0-008 implementation evidence

The same fixture Target authority now builds one schema-v4 plan for 2–20 explicitly selected Target IDs and requested modes. Entries are ordered deterministically, mixed absolute symlinks and Managed Copies stage completely before the first active rename, all destructive entries share one operation-level Snapshot set, and target-index failures compensate in reverse. Batch manifest publication is replayable, all relationship/Operation/Snapshot/Activity rows finalize in one critical SQLite transaction, and startup reopen continues verification/finalization or compensation from journal evidence without an in-memory plan map.

# Takeover plan

The preview contains:

- selected source observation and every exact/same-name related observation;
- digest, unsupported entries, and read/validation errors;
- new stable Skill ID and Vault working path;
- baseline object path and recovery protection;
- all target locations proposed for replacement, or an explicit “none”;
- actual deployment mode per selected target;
- original-content recovery object and retained source paths;
- collision/blocker list and operation-level rollback behavior.

## Add to Vault execution

1. Revalidate the source fingerprint under the mutation lock.
2. Copy the Bundle into Vault-owned staging without following links.
3. Validate `SKILL.md`, paths, entry types, and contained symlinks.
4. Compute the canonical digest.
5. Publish the immutable local baseline object.
6. Stage the Skill manifest and ordinary working Bundle.
7. Atomically activate the UUID working container.
8. Verify working digest, finalize manifest/SQLite, and record Activity.

The source remains byte-for-byte untouched. A failed activation leaves no active Skill; journal-owned staging is recoverable/cleanable.

## Add and manage execution

This is one Operation that includes the Add-to-Vault steps and selected deployment steps. The original selected target is captured as the takeover baseline/recovery point before replacement. Unselected identical locations remain external.

The source observation and every physical alias of its directory are never eligible replacement locations; replacement applies only to separately selected duplicate locations.

# Local provenance recovery

M0 stores the observed path and capture time. As a best-effort P1 enhancement, it may inspect parent Git metadata or known local lockfiles without network access. Recovered repository/commit data is labeled with evidence and confidence; absence or ambiguity never blocks takeover and never becomes verified publisher identity.

# Deployment mode selection

| Target | Default M0 mode | Rationale |
| --- | --- | --- |
| Global agent target on macOS | Absolute directory symlink | One live Vault source and immediate transparent edits. |
| Git project | Managed Copy | Project remains self-contained for collaborators/Git. |
| Non-Git personal project | Absolute directory symlink | Avoid unnecessary drift for private local work. |
| Custom target | User-selected supported mode; default follows declared scope | Escape hatch remains explicit. |

The user may override the default when the adapter supports the requested mode.

Copy fallback occurs only when preflight proves a link capability failure. A newly generated plan records `requestedMode`, `resolvedMode`, `fallbackReason`, Copy drift consequences, and changed paths, then asks for confirmation again. The executor never silently switches modes during commit; a commit-time link failure rolls back and requires replanning.

# Deployment planning

Given Skill IDs and Target IDs, Rust derives target paths and builds one [Operation Plan](../domain/operation-recovery-and-trash.md). Preflight checks:

- current Vault digest and supported Bundle entries;
- target adapter version, root authorization, and deployment-name validity;
- destination absence/type, managed ownership, and case/Unicode collisions;
- expected state for existing managed deployment;
- parent permissions, disk capacity estimate, same-volume staging ability, and link capability;
- nested Vault/target and symlink escape restrictions;
- exact no-op detection.

An unmanaged collision is a blocker. Allowed user responses are cancel, choose another deployment name by creating a separate local derived Skill in M1, or explicitly take over that exact external content. M0 does not create an invalid alias by renaming only the target folder.

Writable destinations, including a Vault relocation destination, undergo capability preflight by behavior, not filesystem label: temporary file/directory/symlink creation, executable-bit preservation, same-directory atomic rename, file and directory fsync, exclusive lock, and case behavior. Results are `Supported`, `Unsupported`, or `Unknown`; M0 blocks `Unknown`, persists the result in the plan, and rechecks at commit. Local APFS is primary; network/cloud storage and multi-machine writes are unsupported.

# Mode-specific staging and verification

## Symlink

- Stage an absolute symlink to `Vault/skills/<skill-id>/<deployment-name>` as a hidden sibling of the final destination.
- Verify raw and resolved target, target entry type, working Bundle digest, and adapter path.
- Persist expected link target and verified digest.
- Vault edits are live; they produce `VaultAhead` until the new digest is explicitly verified, with copy explaining that linked agents already see the bytes.

## Managed Copy

- Copy only from the verified Vault working Bundle/object into target-parent sibling staging.
- Preserve regular files, semantic executable bits, directories, and validated contained symlinks.
- Recompute staged and final target digest.
- Persist `expected_digest`; later compare expected, Vault, and target values using [the health truth table](../domain/identity-and-state.md).

# Deploy and redeploy

- A new deployment creates the target and relationship after verification.
- Redeploy of a clean/no-op target produces no filesystem write.
- `VaultAhead` Managed Copy redeploy replaces the old managed copy after snapshot.
- `TargetModified` and `Conflict` require explicit resolution; redeploy cannot silently discard target changes.
- Multi-target deployment shares one plan, journal, and operation-level recovery point.

M0-007 implements one Target per reviewed schema-v3 plan. M0-008 implements the explicit 2–20 Target schema-v4 path without changing schema-v3 single-target behavior.

# Undeploy

Undeploy removes only selected managed relationships:

1. Verify the current target against deployment expectations.
2. If target drift exists, show preserve/takeover/cancel choices; never delete modified bytes silently.
3. Snapshot the managed entry or link metadata required for undo.
4. Stage removal by atomic rename to an operation-owned sibling backup.
5. Verify target absence and keep other deployments and the Vault Skill unchanged.
6. Finalize metadata and Activity; protected backup becomes snapshot content before cleanup.

External unmanaged Skills have “ignore” and “take over,” not routine direct-delete actions.

# Dry-run export

DPL-12 exports the already generated plan as human-readable JSON. It contains display paths, actions, modes, preconditions, blockers, and recovery summary, but no credentials or arbitrary executable instructions. Exporting a plan does not authorize or execute it.

# Failure behavior

All takeover/deployment/undeploy mutations use [transaction execution](transaction-execution.md). A failure is either no-write, fully rolled back, or explicitly recovery-required. Originals and managed destinations are never deleted as cleanup before a successful verified final state exists.

# Related concepts

- [Identity and state](../domain/identity-and-state.md)
- [Operation model](../domain/operation-recovery-and-trash.md)
- [Transaction execution](transaction-execution.md)
- [Target adapters](../interfaces/target-adapters.md)
