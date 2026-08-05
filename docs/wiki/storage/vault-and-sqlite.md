---
type: Storage Design
title: Vault and SQLite
description: Defines the transparent Vault layout, durable manifests, SQLite schema, consistency rules, rebuild, and relocation.
status: accepted
tags: [skills-hub, m0, storage, sqlite, vault]
requirements: [VLT-01, VLT-02, VLT-03, VLT-06, VLT-07]
timestamp: 2026-07-23T00:00:00Z
---

# Decision

The Vault is a user-selected ordinary directory. Skill working versions are directly accessible, internal recovery data lives under `.manager`, and SQLite is a replaceable local index. Stable UUID containers solve the PRD requirement that same-named Skills coexist without changing the deployment name visible to agents.

# Default location and identity

The final product identity is `Skills Hub`, slug `skills-hub`, bundle ID `com.terrylan.skillshub`. The default Vault is:

```text
~/Library/Application Support/Skills Hub/Vault
```

First run offers the recommended location or a custom directory and shows its path, available space, and Reveal in Finder action. `.manager/vault.json` stores a generated `vaultId`, schema version, digest version, Trash policy, creation time, and application compatibility range. `~/Library/Application Support/Skills Hub/settings.json` stores device-local active path, Workspace Roots, target overrides/custom local paths, and UI/debug settings. Both JSON contracts use `schemaVersion`, validated atomic writes, and preserve a corrupt original for diagnosis; neither stores credentials. SQLite is not the sole source of configuration.

The Vault must not be nested inside a configured target, and a target must not be placed inside `.manager`.

# Layout

```text
Vault/
├── skills/
│   └── <skill-uuid>/
│       └── <deployment-name>/      # ordinary working Skill Bundle
└── .manager/
    ├── vault.json
    ├── manifests/
    │   ├── skills/<skill-uuid>.json
    │   └── deployments/<deployment-uuid>.json
    ├── objects/sha256-bundle-v1/<prefix>/<digest>/
    ├── staging/<operation-uuid>/
    ├── trash/<skill-uuid>/<trash-entry-uuid>/
    ├── operations/<operation-uuid>/
    ├── locks/vault.lock
    └── index.sqlite
```

`skills/<uuid>/<deployment-name>` is the symlink source for global deployment. The basename remains compatible with agents that validate bundle name while the UUID parent allows name collisions. “Reveal in Finder” opens the bundle directory, not the opaque container.

`locks/vault.lock` is held through an OS advisory exclusive lock for the process lifetime. Its optional owner metadata is diagnostic only; the mere file's presence or age never proves that the lock is active or stale.

# Skill manifest

The readable, atomically written Skill manifest is sufficient to recover stable local identity and working content:

```json
{
  "schemaVersion": 1,
  "skillId": "019…",
  "displayName": "Frontend Design",
  "deploymentName": "frontend-design",
  "workingPath": "skills/019…/frontend-design",
  "workingDigest": "sha256-bundle-v1:…",
  "baselineDigest": "sha256-bundle-v1:…",
  "createdAt": "2026-07-23T00:00:00Z",
  "sources": [
    {
      "kind": "local-observation",
      "path": "/Users/…/.claude/skills/frontend-design",
      "capturedAt": "2026-07-23T00:00:00Z",
      "confidence": "observed"
    }
  ]
}
```

M0 manifests contain local provenance only. M1 may add immutable remote source fields without changing `skillId`. Deployment manifests preserve mode, target identity, expected digest, adapter version, and last finalized Operation for index rebuilding.

# SQLite choice and settings

Use bundled SQLite through `rusqlite`, with:

- foreign keys enabled;
- WAL journal mode;
- bounded busy timeout;
- embedded, ordered, hand-written SQL migrations run transactionally through `rusqlite_migration` or a compatible lightweight runner;
- applied migration checksums, immutable released migrations, and a pre-upgrade database backup;
- `synchronous=NORMAL` for observation/derived-index WAL transactions and an explicit `FULL` boundary for migrations and critical operation finalization;
- one dedicated database executor thread in M0.

The frontend never receives a SQL connection or uses `tauri-plugin-sql`.

## Implemented v1 contract

M0 embeds one immutable hand-written [initial migration](../../../src-tauri/src/persistence/migrations/0001_initial.sql). Its released SHA-256 checksum is:

```text
6bbfca126d052df3a78e21e9345ac2cb33e2c50b8623a3a279fa316a3d926a9c
```

`schema_migrations` stores contiguous version/name/checksum/application time, while SQLite `user_version` must match the latest applied row. Unknown future versions, gaps, or changed historical checksums block opening. All pending migrations run in one `IMMEDIATE` transaction under `synchronous=FULL`; an existing version is backed up first through SQLite's online backup API. Ordinary connections return to `NORMAL` after migration or critical finalization.

Database replacement for later rebuild work accepts only a closed, integrity-checked sibling database and retains the prior main file as a caller-named backup. A remaining `-wal` or `-shm` sidecar blocks replacement: the caller must recover/checkpoint it first so old WAL frames can never be replayed into an unrelated replacement.

JSON schema versions are `1` for device settings, `vault.json`, Skill manifests, and deployment manifests. Device settings contain the active Vault path, Workspace Root paths, adapter path overrides, custom target paths, appearance, and debug-log preference. `vault.json` contains Vault UUIDv7 identity, digest version, Trash policy, creation time, and compatible application range. Future schemas are not overwritten. Invalid device settings are regenerable only after their exact bytes are durably copied to a `.corrupt-<uuid>` sibling; invalid Vault identity remains fail-closed.

Deployment manifest v1 contains deployment/Skill/Target IDs, deployment name, mode, target path, expected digest, optional absolute expected link target, adapter version, last finalized Operation ID, and verification time. Symlink mode requires a link target; Managed Copy forbids one.

# Logical schema

| Table | Purpose and key fields |
| --- | --- |
| `schema_migrations` | Applied version and checksum. |
| `skills` | ID, names, relative working path, current/baseline digest, lifecycle, timestamps. |
| `skill_sources` | Local observation provenance and confidence. |
| `objects` | Digest, relative object path, size/count, verification time. |
| `skill_revisions` | Skill/object relation, kind, creating Operation, time. |
| `scan_runs` | Root, scope, started/completed state, coverage, cancellation. |
| `scan_errors` | Isolated path/error evidence for one run. |
| `observations` | Adapter, scope, project, normalized/canonical path identity, name, digest/status, last successful run. |
| `workspace_roots` | Authorized path identity, pause state, depth, ignore configuration, scan status. |
| `projects` | Root, discovery evidence, Git classification, manual flag. |
| `targets` | Adapter/version, scope, root/project, override/custom configuration. |
| `deployments` | Skill/Target, name, path, mode, expected digest/link target, health, last verification and Operation. |
| `operations` | Plan digest, type, state, outcome, recovery state, timestamps, durable journal path. |
| `operation_steps` | Ordinal, action, fingerprints, stage/backup references, commit/rollback status. |
| `snapshots` | Operation-level checkpoint, retention/protection state. |
| `snapshot_items` | Snapshot-to-object or entry-fingerprint references. |
| `activity` | Append-only user-facing job/operation projection. |
| `settings` | Non-secret Vault-local preferences; never Skill content or credentials. |

Required uniqueness includes:

- `skills.id`;
- canonical observation location within its scope;
- `targets.id` and stable configured target identity;
- active `(target_id, normalized_deployment_name)`;
- object digest under its hash schema.

Case-insensitive deployment collision keys use Unicode NFC plus a documented macOS case-folding strategy. Actual user-facing spelling remains unchanged.

Persistent SQLite times are UTC Unix milliseconds in `INTEGER` columns. JSON manifests, journals, and DTOs use RFC3339 UTC strings; the UI localizes them. Process monotonic time owns elapsed durations and timeouts.

Observation and derived-index writes may use batched WAL transactions at `synchronous=NORMAL`. Manifests, journals, objects, Snapshots, and critical publication use temp write → file fsync → atomic rename → parent-directory fsync; critical SQLite finalization uses the stronger `FULL` boundary.

# Write consistency

SQLite cannot atomically commit with arbitrary filesystems. The ordering contract is:

1. Write and fsync staged content, manifests, and operation journal.
2. Commit and verify active filesystem paths.
3. Atomically publish updated manifests.
4. Finalize related SQLite rows in one transaction.
5. Mark the durable journal finalized.

If step 4 fails, the verified filesystem and journal are authoritative enough for startup reconciliation to finish metadata finalization. Do not roll back verified user-visible paths merely because an index transaction failed.

# Index rebuild

Rebuild is explicit and creates a backup of the old database before replacement:

1. Validate `vault.json` and lock the Vault against mutations.
2. Read Skill and deployment manifests.
3. Verify working paths and recompute current digests.
4. Read terminal and unresolved Operation journals and Snapshot references.
5. Build a new SQLite database in staging and run integrity checks.
6. Atomically swap the new index into place.
7. Rescan machine-specific global/Workspace roots to restore observations and current health.

Workspace authorization and custom machine paths may require re-selection if machine-local configuration was also lost. “Rebuild where possible” does not invent absent provenance.

# Verify, repair, and relocate

- **Verify** is read-only and compares layout, manifests, objects, working paths, and index references.
- **Repair** presents a plan. Safe automatic repairs include rebuilding derived index rows or restoring a missing manifest from unambiguous indexed data; ambiguous identity is never guessed.
- **Relocate** is one global Operation. It performs capability preflight, copies cross-volume into destination staging, verifies all digests/manifests/SQLite, quiesces for cutover, switches device configuration, repairs every managed absolute symlink, verifies deployments, and keeps the old Vault authoritative until explicit success/confirmation. Interruption permits resume, rollback, or restart without selecting the partial destination as truth.

Relocation preserves `vaultId`, Skill IDs, deployment IDs, and content digests. It is not M2 cross-device migration or Git backup.

## Implemented lifecycle and recovery contract

M0 lifecycle planning writes reviewed plans and journals only under `.manager/lifecycle-operations/<operation-id>/`; this namespace is intentionally separate from the generic target-operation store while sharing its single-writer coordinator. Verify and index-rebuild planning do not mutate working content or the active index. Repair execution accepts only a digest-matching reviewed plan and an absent exact manifest path owned by one indexed Skill; changed, mismatched, or ambiguous evidence is refused. See [Bundle hashing and objects](bundle-hashing-and-objects.md) for external-edit and GC reference behavior and [Transaction execution](../workflows/transaction-execution.md) for mutation serialization.

Index rebuild creates and integrity-checks a staged database, checkpoints the live database, retains `index-before-rebuild-<operation-id>.sqlite`, and atomically replaces the file. Skill and deployment IDs/digests come from manifests; durable standard Operation journals restore operation identity and unresolved state without inventing missing provenance. The result is explicitly restart-required because already-open repository handles remain bound to the retained old database.

Relocation persists capability and step evidence before mutation, copies into an operation-owned sibling staging directory, verifies Vault identity/manifests/working and object digests/index integrity, then switches device settings and rewrites only managed absolute links that still match their recorded old targets. Failure before cutover leaves settings and the old Vault authoritative; interrupted cutover remains recovery-blocking with both versions retained. Success preserves stable identities and also requires restart. Deleting the old Vault is a separate digest-confirmed operation and is never implied by relocation success.

# Secrets and privacy

M0 stores no credentials. Absolute local source/target paths may exist in the local index and internal manifests because they are required for operation, but they are not portable export content. Future diagnostics must redact them according to the PRD.

# Related concepts

- [Bundle hashing and objects](bundle-hashing-and-objects.md)
- [Identity and state](../domain/identity-and-state.md)
- [Transaction execution](../workflows/transaction-execution.md)
- [Filesystem safety](../security/filesystem-safety.md)
