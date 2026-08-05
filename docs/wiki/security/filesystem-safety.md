---
type: Threat Model
title: M0 Filesystem Safety
description: Defines authorization boundaries, path validation, link policy, TOCTOU mitigations, safe cleanup, and residual risks.
status: accepted
tags: [skills-hub, m0, security, filesystem]
requirements: [SCN-04, IMP-04, IMP-06, IMP-07, DPL-05, DPL-06, DPL-07, DPL-08, DEL-04, DEL-05]
timestamp: 2026-07-23T00:00:00Z
---

# Scope

This threat model covers local filesystem discovery, Vault storage, deployment, deletion, and recovery on macOS. It assumes Skills may contain hostile names, links, bytes, or instructions. Skills Hub never executes Skill code.

M0 does not claim to sandbox an agent after deployment and does not implement the M1 full static content audit. Filesystem validation answers “can this Bundle be handled without escaping authorized paths?”, not “is this Skill safe to execute?”

# Implemented M0-005 slice

The generic Operation executor now enforces the mutation subset of this contract. Before each backup, final, rollback-aside, and backup-restore rename it revalidates root containment, sealed parent identity, source fingerprint, and destination fingerprint, then uses descriptor-relative no-replace rename. Exact journal-recorded cleanup additionally requires containment, operation/kind marker, durable file identity, and content proof; failed proof preserves the artifact and records the failure. Product-specific target authorization and action construction remain M0-006/M0-007, and M0-008 remains responsible for startup action execution.

# Trust boundaries

| Boundary | Trust decision |
| --- | --- |
| Built-in global root | May be scanned read-only; mutation only through a concrete registered Target. |
| User-selected Workspace | May be traversed read-only under configured limits; discovered projects are not automatically mutation targets. |
| Manual project/custom target | Selection authorizes the concrete role/path after Rust validation. |
| External Skill Bundle | Untrusted input; no links followed for scan/hash, unsupported entries block takeover. |
| Vault working content | User-owned and editable, but must be revalidated before every mutation. |
| `.manager` internals | Application-controlled; still verify identity and containment before cleanup. |
| Frontend/IPC arguments | Untrusted data; domain IDs and enums only, never direct mutation authority. |

# Authorized path derivation

Mutation commands accept Skill, Target, deployment, and Operation IDs. Rust derives final paths from registered records and adapter-safe relative components.

`deployment_name` and all Bundle names must be safe UTF-8 components. Reject root/prefix separators, `.`, `..`, NUL/control/dangerous characters, non-UTF-8 names, and names that collide under a documented NFC plus target-appropriate case-fold key. Preserve accepted spelling for display; never auto-rename, merge, or choose last writer. Unsafe external observations remain visible, but takeover/deploy is blocked.

The path validator:

1. performs lexical component validation without filesystem access;
2. finds the nearest existing ancestor for a future path;
3. canonicalizes and identifies that ancestor;
4. verifies it remains inside the registered root and is not an unexpected symlink component;
5. appends only previously validated missing components;
6. captures parent volume/file identity for precondition checks.

Calling `canonicalize` only on the final nonexistent destination is insufficient and is not the policy.

# Symlink rules

- Use `symlink_metadata` to distinguish absence, link, and broken link.
- Do not follow directory symlinks during Workspace traversal.
- Unknown global target symlinks outside the scan root are recorded but not read.
- The only scan exception is an exact registered managed link to the expected Vault path.
- Internal Bundle links must be relative, contained, acyclic, and preserved as links.
- Deployment links are absolute in M0 so sibling staging/rename does not change their meaning.
- Before deleting/replacing a link, compare raw link target and entry identity; never recursively delete its resolved target.

# Mutation safety

- Acquire the per-Vault mutation lock before revalidating a confirmed plan.
- Capture path kind, raw link target, volume/file ID, digest/fingerprint, and ownership in plan preconditions.
- Persist the final parent directory identity in the sealed plan and recheck that identity plus the destination immediately before every backup, stage-to-final, rollback-aside, and backup-restore rename.
- Create staging/backup siblings with unpredictable operation-scoped names and exclusive creation.
- Stage on the destination filesystem to make each final rename local and atomic where supported.
- Never replace an unmanaged entry unless the exact entry is the subject of an explicit takeover plan.
- Never report success before final path verification and metadata finalization.

These checks reduce time-of-check/time-of-use races. A hostile process running as the same user can still race filesystem operations; M0 responds to detected identity changes by stopping and preserving versions rather than claiming perfect isolation.

# Copy and entry rules

- Open regular files for read without executing them.
- Copy exact bytes and the semantic executable bit; no package manager, hook, or script is invoked.
- Reject sockets, FIFOs, block/character devices, non-UTF-8 names, and escaping/broken links for takeover/deployment.
- Recompute staged/final digests instead of trusting source metadata.
- Limit one Operation's total copied bytes/file count to configurable safety caps derived during planning; exceeding a cap is a blocker, not partial success.
- Detect unstable input when metadata changes during read and retry only with a bounded debounce.

# Delete and cleanup rules

- External unmanaged Skills have no normal direct-delete command.
- Undeploy removes only an exact registered managed target after drift review.
- Permanent delete is valid only for a Skill already in application Trash and after secondary confirmation.
- Broad `remove_dir_all` is allowed only on a verified application-owned staging/object/Trash root, never on a user-selected target root.
- Cleanup requires the exact journal-recorded artifact path, containment, matching operation-and-kind marker, and usable fingerprint/file identity including content proof before recursive removal. A marker alone is never authority; forged markers and replaced inode/content are preserved as recovery evidence.
- A cleanup failure is logged and left for targeted retry; it never expands deletion scope.

# Vault and target nesting

Reject configurations where:

- the Vault is inside a global/project/custom target;
- any target is inside Vault `.manager`;
- a Workspace Root would cause recursive scanning of Vault internals without a deliberate separate choice;
- a relocation destination aliases the source by canonical/file identity.

The Vault's visible `skills` directory may be inside an authorized Workspace only if the scanner excludes the Vault ID explicitly and reports that exclusion.

# Durability and lock safety

- One active process owns a per-Vault advisory lock; stale lock metadata is not itself proof that no process owns the lock.
- Journal/manifest updates use temp-write, file fsync, atomic rename, and parent fsync.
- SQLite uses WAL and full synchronization at critical operation boundaries.
- Startup resolves non-terminal journals before watchers or mutations.
- Unknown crash state becomes `RecoveryRequired`; automatic recovery never discards the only identifiable copy.

# Privacy and network

M0 core workflows make no remote source or audit requests and work with network access disabled. Paths and Skill names stay local. Optional application-update behavior, if enabled later, is isolated from Skill operations and recorded according to the PRD.

# Residual risks

- Same-user malicious processes can create races between checks; identity rechecks limit but cannot eliminate this on arbitrary filesystems.
- Network/removable filesystems may not provide reliable atomic rename, file IDs, events, or fsync semantics. Preflight marks unsupported reliability and blocks mutation by default in M0.
- A syntactically valid Skill can still contain dangerous instructions/scripts; that is an explicit M1 audit concern.
- Absolute deployment symlinks break if the Vault is moved outside the relocation Operation.
- Filesystem permissions do not make content-addressed objects cryptographically immutable; periodic digest verification is required.

# Security verification

- Property-test path normalization, component rejection, and containment.
- Test case/Unicode collisions and nonexistent descendants.
- Test broken/retargeted/cyclic/escaping links.
- Test special files and unstable reads.
- Race parent/destination replacement around failpoints and require stale/recovery outcomes.
- Audit every cleanup call site to prove exact application ownership.

# Related concepts

- [Bundle hashing](../storage/bundle-hashing-and-objects.md)
- [Transaction execution](../workflows/transaction-execution.md)
- [Testing and acceptance](../quality/testing-and-acceptance.md)
