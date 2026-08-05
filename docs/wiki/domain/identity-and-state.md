---
type: Domain Model
title: Skill Identity and State
description: Defines stable entities, ownership, observation reconciliation, name conflicts, and deployment health.
status: accepted
tags: [skills-hub, m0, domain, identity, deployment]
requirements: [SCN-03, IMP-01, IMP-05, DPL-06, DPL-09, DPL-10, DPL-11]
timestamp: 2026-07-23T00:00:00Z
---

# Core distinction

A Skill is an owned logical asset. An observation is evidence that Skill-shaped content exists at one path. A deployment is an application-managed relationship between an owned Skill and a target. These are not interchangeable.

# Stable identifiers

| Entity | Identifier | Identity rule |
| --- | --- | --- |
| Skill | generated UUIDv7 | Never derived from a name, path, digest, or source. |
| Observation | generated UUIDv7 plus canonical path identity | One observed location; may later link to a Skill. |
| Revision/Object | versioned bundle digest | Immutable content identity, not logical Skill identity. |
| Adapter | stable string plus schema version | Example: `claude-code@1`. |
| Target | generated UUIDv7 | Concrete adapter + scope + root/project location. |
| Deployment | generated UUIDv7 | One Skill exposed under one name at one Target. |
| Operation | generated UUIDv7 | One reviewed mutation transaction. |
| Snapshot | generated UUIDv7 | One immutable recovery relation. |
| Activity | generated UUIDv7 | One durable user-timeline fact. |

Entity IDs cross JSON, manifests, and IPC as canonical hyphenated UUID strings. Parsers reject non-v7 UUIDs as well as malformed values; an ID's Rust type remains part of the contract even when two entity types are constructed from the same UUID bytes. Adapter IDs cross durable boundaries as one schema-qualified string such as `claude-code@1`, not as a display name or decomposed JSON object.

Display name and deployment name are separate. `deployment_name` is exactly one safe path component and is the folder name visible to the target. Two Skills may share it in the Vault, but `UNIQUE(target_id, normalized_deployment_name)` ensures only one can occupy a target at once.

# Entities and durable relationships

```text
Observation ──optional evidence-for──▶ Skill ──has──▶ Working Version
      │                                  │                 │
      └── adapter/scope/path/digest      ├──references────▶ Immutable Objects
                                         │
                                         └──deploys-through──▶ Deployment ──to──▶ Target
```

- An external observation can exist without a Skill row.
- Taking over content creates a Skill and a local baseline object; it does not delete the observation.
- One Skill may retain several source observations and several deployments.
- One digest may be referenced by several Skills without merging their logical identities.

# Ownership is computed

Ownership is not a mutable badge stored independently from facts:

| Ownership | Rule |
| --- | --- |
| `External` | Observation is not linked to an active Vault Skill. |
| `Vaulted` | Active Skill has a working version and no active deployment. |
| `Managed` | Active Skill has at least one active application-managed deployment. |

Trash is an independent lifecycle state. A trashed Skill is not presented as `External`, `Vaulted`, or `Managed`; active deployments must be resolved before the Trash transition commits.

# Observation reconciliation

Each scan root creates a run with a stable coverage boundary. For every candidate:

1. Normalize its display path without losing the original path.
2. Capture canonical identity of existing components and volume/file identity where available.
3. Validate direct `SKILL.md` presence and classify unsupported entries.
4. Compute the canonical bundle digest or an explicit unverified/error state.
5. Upsert the observation for that location.
6. Classify relationships only after all successful candidates in the run are known.

Missing observations are marked stale only after their root completes successfully. A canceled, partial, or permission-failed scan does not erase the last known evidence.

# Duplicate and conflict classification

| Name relation | Digest relation | Classification | Automatic action |
| --- | --- | --- | --- |
| Same | Same | Exact duplicate content at multiple locations | Group observations; do not silently choose ownership or source. |
| Same | Different | Name conflict | Keep separate; block target replacement until reviewed. |
| Different | Same | Probable duplicate or rename | Suggest relationship; never merge automatically. |
| Different | Different | Unrelated | None. |
| Any | Digest unavailable | Unverified | Do not claim duplicate or clean state. |

Folder name alone is never sufficient. Scan order never breaks ties. Taking over one observation does not automatically claim every same-digest path; the plan lists which observations become managed.

# Working and expected content

For deployment health, retain three values:

- `E`: digest successfully verified when the deployment was last finalized.
- `V`: current Vault working digest.
- `T`: current deployed target digest, when applicable.

## Managed Copy health

Evaluate structural errors first, then content:

| Condition | Health |
| --- | --- |
| Target path missing | `MissingTarget` |
| Entry type/ownership no longer matches | `Conflict` |
| Target cannot be read or hashed | `Unverified` |
| `T = E` and `V = E` | `Clean` |
| `T = E` and `V ≠ E` | `VaultAhead` |
| `T ≠ E` and `V = E` | `TargetModified` |
| `T ≠ E`, `V ≠ E`, and `T ≠ V` | `Conflict` |
| `T = V` and both differ from `E` | `Unverified` until an explicit re-verification updates `E` |

## Symlink health

1. Use `symlink_metadata`; a dangling symlink is not the same as an absent path.
2. Missing link entry is `MissingTarget`.
3. A link whose target does not exist is `BrokenLink`.
4. A link retargeted away from the expected Vault working directory, or replaced with a regular entry, is `Conflict`.
5. A correct link with `V = E` is `Clean`.
6. A correct link with `V ≠ E` is `VaultAhead` until the changed working content is explicitly verified.

The UI must explain that a symlink exposes the changed Vault bytes immediately even while the deployment's last verified digest is behind. `VaultAhead` does not imply that the target still serves old bytes in link mode.

# Implemented M0-007 deployment state

The Universal fixture now computes these states in Rust from the persisted expected digest/link plus current Vault and target evidence. Targeted verification and deployment listing return the same authoritative health, evidence, explanation, drift direction, and allowed actions; verification may update indexed health but does not mutate an active target. Clean redeploy is a target no-op, Vault-ahead link verification advances expected evidence without replacing the link, and target changes cannot be overwritten or removed silently. Explicit preserve undeploy can end one relationship while leaving a changed, missing, broken, retargeted, or replacement entry untouched; unreadable evidence remains `Unverified` and non-UTF-8 raw link evidence fails closed.

# Allowed state transitions

```text
external observation
      │ Add to Vault
      ▼
    vaulted ───── Deploy ─────▶ managed
       ▲                            │
       └──────── Undeploy last ─────┘

vaulted/managed ── resolve deployments + move ──▶ trash
trash ── restore ──▶ vaulted
trash ── secondary confirmation ──▶ permanently removed
```

`Keep external` records only the user's current choice/ignore preference. It does not mutate the observation. “Add and manage” is one reviewed Operation containing both the Vault activation and selected deployment replacements.

# Invariants

- Stable Skill identity survives display-name changes, Vault relocation, Trash/restore, and digest changes.
- A content digest can prove byte-level equality under one hash schema, not publisher identity or trust.
- A deployment points to one Skill, Target, mode, deployment name, and last verified digest.
- Health and ownership are orthogonal; one managed Skill can have both clean and conflicting deployments.
- Any unreadable state remains visible as error evidence rather than disappearing from inventory.

# Related concepts

- [Bundle hashing](../storage/bundle-hashing-and-objects.md)
- [Scanning and reconciliation](../workflows/scanning-and-reconciliation.md)
- [Takeover and deployment](../workflows/takeover-and-deployment.md)
- [Operation model](operation-recovery-and-trash.md)
