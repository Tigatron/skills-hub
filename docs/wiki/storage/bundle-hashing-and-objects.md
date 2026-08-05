---
type: Storage Design
title: Bundle Hashing and Immutable Objects
description: Defines the canonical Skill Bundle digest, entry validation, object publication, snapshots, and retention.
status: accepted
tags: [skills-hub, m0, storage, hashing, snapshots]
requirements: [VLT-04, VLT-05, VLT-08, SCN-03, IMP-04, IMP-07, DPL-09, DPL-10]
timestamp: 2026-07-23T00:00:00Z
---

# Why this is a protocol

The bundle digest drives duplicate detection, immutable object keys, drift, snapshots, and later package checksums. Its encoding is a compatibility contract, not an implementation detail. M0 defines `sha256-bundle-v1` and freezes golden vectors before storing production objects.

# Logical bundle boundary

A Skill Bundle is one directory whose direct `SKILL.md` is a regular UTF-8 file. The digest covers the complete directory tree, including hidden files and empty directories. It does not cover the UUID Vault container or Skill manifest outside the bundle.

M0 has no broad ignore list inside a Skill. Files such as `.DS_Store` therefore change the digest until a deliberate, versioned exclusion policy is accepted; silently skipping hidden entries would make safety and drift incomplete.

# Canonical encoding

The digest is SHA-256 over an unambiguous byte stream:

```text
"skills-hub-bundle\0v1\0"

for each entry sorted by normalized relative path bytes:
    entry_type_u8
    path_length_u64_be
    path_bytes
    mode_class_u8
    payload_length_u64_be
    payload_bytes
```

The byte tags are part of schema v1 and are not inferred from labels:

| Entry | `entry_type` | `mode_class` | Payload |
| --- | --- | --- | --- |
| Directory | `0x01` | `0x01` (`directory`) | empty; directories are encoded so empty directories matter. |
| Regular file | `0x02` | `0x02` (`regular`) or `0x03` (`executable`) | exact file bytes. |
| Symbolic link | `0x03` | `0x04` (`symlink`) | raw link target bytes; the target is never followed for hashing. |

- Relative path components are NFC-normalized, encoded as UTF-8 with `/` separators, and sorted bytewise. Trees whose distinct on-disk names normalize to one path are rejected rather than merged; NFC/NFD-only spelling differences otherwise produce the same logical digest.
- Empty, `.`, `..`, NUL, control-character, and `<`, `>`, `:`, `"`, `/`, `\`, `|`, `?`, or `*` components are invalid. This stricter portable-name policy is deliberate even where APFS would accept some of those characters.
- M0 reports non-UTF-8 names as `UnsupportedName`; it does not use lossy conversion that can collide.
- Modification time, owner, group, ordinary read/write permission differences, ACLs, and extended attributes do not affect the logical digest.
- The semantic executable bit does affect the digest.
- Hard links are read as regular files; hard-link identity is not preserved.
- Socket, FIFO, block, and character device entries are unsupported and block takeover/deployment.

The external digest string is always algorithm- and schema-qualified:

```text
sha256-bundle-v1:<64 lowercase hexadecimal characters>
```

# Bundle safety caps

The default policy is maximum depth 64, 10,000 entries, 1 GiB total regular-file bytes, and 256 MiB for one file. Metadata traversal tracks these values before hashing or copying; as soon as a cap is exceeded, further content reads and all mutation are blocked. Import/takeover never partially succeeds. An advanced device setting may raise the policy, which forces rescan and replanning. Every plan records the caps and observed depth/count/byte statistics; commit revalidates them, and its disk estimate includes staging, Snapshot, and rollback space.

Persistent object and manifest times are RFC3339 UTC strings; the SQLite projection uses UTC Unix milliseconds. Elapsed and timeout measurement uses a process monotonic clock.

# Stable-read behavior

For each regular file and symbolic link, capture metadata before and after reading its payload. Link targets are captured only after the metadata-only cap traversal, then re-read and compared before hashing or link-safety validation. If file ID, size, modification time, type, executable mode, or raw link target changes, return `UnstableInput`; debounce and retry the whole bundle rather than persisting a mixed digest. A bounded retry failure remains visible as unverified.

The same production implementation is used for:

- scan observations;
- takeover staging verification;
- immutable object verification;
- Vault working-state reconciliation;
- Managed Copy post-commit and drift verification.

# Implemented M0-006 takeover use

Takeover now copies an externally observed Bundle with the object store's exact no-follow primitive, re-hashes the copy, publishes or verifies the immutable content-addressed baseline, and stages working and Managed Copy content only from that verified object. The active Vault path is a UUID container with exactly one sealed Bundle subpath; generic operation verification rejects extra siblings, a link in place of that Bundle, or a digest mismatch. Selected originals are protected by Snapshot relations to verified objects plus their exact before fingerprints before any replacement.

# Symlink policy inside a Skill

Scanning records symlink entries without following them. Takeover and deployment permit a symlink only when all of the following hold:

1. its target is relative;
2. lexical resolution from the link parent remains inside the bundle;
3. staging-time canonical resolution, when the target exists, remains inside the staged bundle;
4. it does not form a cycle when validating the bundle;
5. copying preserves the link itself, not target bytes.

Absolute, escaping, cyclic, and broken internal symlinks remain visible in an external observation but block takeover with an explicit path error. This is format/filesystem validation, not the full M1 security audit.

The digest payload can preserve a non-UTF-8 raw link target on macOS, but takeover validation cannot safely resolve it and blocks it as `UnsupportedName`. A lexical link target that resolves to the Bundle root is also blocked rather than treated as a deployable internal entry.

# Object store

Objects are immutable directory trees:

```text
.manager/objects/sha256-bundle-v1/<first-two-hex>/<remaining-hex>/
├── object.json
└── bundle/
    ├── SKILL.md
    └── ...
```

Publication algorithm:

1. Copy the validated source to operation-owned staging without following links.
2. Recompute the canonical digest from staged content.
3. Write `object.json` with schema, digest, entry count, byte count, and creation time.
4. Flush files and staging directory.
5. If the key already exists, verify its metadata/digest and reuse it.
6. Otherwise atomically rename staging to the content-addressed destination.
7. Remove owner-write permission as protection against accidents; verification remains the trust mechanism.

No object is considered published merely because a directory with its name exists.

`object.json` schema version `1` contains `digest`, `entryCount`, `byteCount`, and RFC3339 UTC `createdAt`. Publication enforces the Bundle caps again while copying, independently of the earlier validation pass, and limits each read to the observed file length plus one byte so same-user growth races cannot cause unbounded staging writes. A crash after rename but before permissions are hardened may leave a writable object; the key, manifest, and canonical re-hash—not permission bits—remain the reuse trust decision.

# Working versions and revisions

The working Bundle is editable ordinary content. A Revision is an immutable relation from a Skill to an object and a reason, such as:

- `TakeoverBaseline`;
- `PreOperationSnapshot`;
- `TrashCheckpoint`;
- `UndoCheckpoint`.

M0 does not model remote upstream revisions. External edits update the working digest and deployment health but do not overwrite prior objects.

# Retention and garbage collection

References come from Skill baselines, Snapshots, protected Operations, and Trash entries. Object GC is allowed only when:

- the object has no live reference in rebuilt/verified metadata;
- no unresolved journal refers to it;
- its retention deadline has passed;
- a complete reference verification pass succeeded;
- deletion is constrained to its exact, validated object path.

GC first moves the object to an internal pending-delete area and records Activity; physical deletion occurs in a later verified pass. If the index is unhealthy, GC is disabled rather than guessing.

# Golden-vector requirements

Tests freeze digests for fixtures covering:

- deterministic ordering independent of file creation order;
- hidden and empty entries;
- executable-bit changes;
- Unicode UTF-8 names;
- contained symlinks;
- path, bytes, and entry-type changes;
- rejected non-UTF-8 and special entries.

Changing any vector requires a new hash schema, a migration design, and an accepted Wiki update.

# Related concepts

- [Vault and SQLite](vault-and-sqlite.md)
- [Identity and state](../domain/identity-and-state.md)
- [Filesystem safety](../security/filesystem-safety.md)
- [Testing and acceptance](../quality/testing-and-acceptance.md)
