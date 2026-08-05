# Skills Hub Product Requirements Document

| Field | Value |
| --- | --- |
| Document | PRD v0.1 |
| Status | First agreed product baseline |
| Date | 2026-07-22 |
| Product name | Skills Hub |
| Product type | Open-source local-first macOS desktop application |
| License | MIT |
| Initial owner and user | Project author |
| Implementation | Tauri 2, Rust, React, TypeScript, Vite |

## 1. Executive summary

Skills Hub is a local-first manager and distribution tool for Agent Skills. It gives one user a complete view of Skills scattered across global agent directories and authorized projects, then lets that user bring selected Skills into an independent Vault, inspect and edit them, and deploy exact revisions to multiple agents at global or project scope.

The defining product decision is that **downloading is not installing**. A Skill enters the Vault before it is deployed anywhere. The Vault is the managed source of truth; agent and project directories are explicit deployment targets. Existing files remain external until the user deliberately takes control.

The complete V1 vision is delivered through three independently useful milestones:

- **M0 — Local Skill manager:** discover, understand, take over, deploy, remove, and recover local Skills.
- **M1 — Acquisition and distribution:** discover remote Skills, edit and audit them, manage updates, build Collections, and exchange portable packages.
- **M2 — Reproducibility and backup:** back up to user-owned Git, restore projects from manifests, migrate and verify the Vault, and prepare the cross-platform foundation.

## 2. Problem statement

Agent Skills are ordinary directories, but their operational state is fragmented:

- Every agent uses a different global and project-level directory.
- The same Skill may exist as several copies with no reliable indication of which is authoritative.
- Installing from a marketplace or repository often writes directly into an agent directory.
- Project-level Skills are difficult to find without searching many repositories.
- Copy-based deployment drifts; link-based deployment is not portable into Git projects.
- Folder names are frequently treated as identities, causing collisions between unrelated sources.
- Updates can change instructions, scripts, dependencies, and security posture without a useful diff.
- Backup usually means copying an agent directory rather than preserving source, version, provenance, Collections, and deployment intent.
- “Uninstall” is often ambiguous: it may remove one deployment, every deployment, or the only source copy.

The result is a filesystem maintenance problem disguised as an installation problem.

## 3. Market context and product gap

Existing products validate the demand but leave room for a distribution-first local tool:

| Category | Representative products | Strong ideas | Remaining gap |
| --- | --- | --- | --- |
| Desktop managers | CC Switch, xingkongliang/skills-manager, Chops, jiweiyeah/Skills-Manager | Central storage, agent toggles, editing, Git backup, risk scanning | Often app-first, globally scoped, or weak on immutable provenance and transactional deployment |
| Cross-agent CLIs | skillshare, Vercel Skills CLI, AgentSync, skills-cli, ASPM | Broad adapters, Git acquisition, copy/symlink modes, dry run, audit | Limited visual ownership model, impact preview, drift resolution, or portable personal Vault |
| Discovery services | skills.sh, SkillX | Search, semantic discovery, trends, installation signals | Discovery is not durable ownership or reproducible distribution |
| Registries | iFlytek SkillHub, ClawHub | Artifact versions, namespaces, governance, moderation | Too server-oriented for a personal local-first manager |

Skills Hub will not compete by claiming the longest agent list. Its differentiation is the combination of:

1. A downloaded-but-not-deployed state.
2. Explicit external, vaulted, and managed ownership.
3. Immutable provenance plus an editable working version.
4. A deployment matrix with drift and conflict state.
5. Transactional plans and rollback.
6. Portable, open Skill and Collection packages.
7. Entirely local safety analysis and zero default telemetry.

## 4. Product definition

### 4.1 Target user

The initial user is a developer who:

- uses two or more AI agents;
- has global Skills and project-specific Skills;
- acquires Skills from Git repositories and skills.sh;
- occasionally edits or creates Skills;
- wants local ownership, backup, and repeatable distribution;
- is comfortable with repositories and files but does not want to manage hidden paths manually.

No account, organization, marketplace publisher, or multi-user role is required for V1.

### 4.2 Jobs to be done

1. **Inventory:** “Show me every Skill I have and every place it exists.”
2. **Ownership:** “Let me bring a Skill under management without risking the original.”
3. **Distribution:** “Put this exact Skill or Collection into selected agents or projects.”
4. **Diagnosis:** “Tell me whether a deployment is current, modified, missing, or conflicting.”
5. **Acquisition:** “Let me save a useful Skill without immediately changing an agent.”
6. **Maintenance:** “Show what an upstream update changes before I accept it.”
7. **Recovery:** “Undo a bad import, edit, update, deployment, or deletion.”
8. **Portability:** “Back up and move my Skills without depending on this application forever.”

### 4.3 Product principles

- **Skill-first:** the Library is primary; agents and projects are destinations.
- **Local-first:** all core workflows function without a Skills Hub account or service.
- **Non-destructive by default:** scans and discovery never mutate files.
- **Explicit ownership:** no external Skill becomes managed without confirmation.
- **Previewable and reversible:** important mutations have a plan and recovery point.
- **Open storage:** Skill working copies are ordinary files; export formats are documented.
- **Trust is evidence, not a badge:** format validity, audit findings, source identity, and checksum verification remain separate concepts.

## 5. Goals, non-goals, and success criteria

### 5.1 V1 goals

- Create a dependable local source of truth for Agent Skills.
- Discover supported global Skills automatically and project Skills within authorized roots.
- Manage global and project-level deployments across the six initial target families.
- Preserve source, version, content hash, local modifications, and deployment state.
- Make imports, updates, and destructive operations safe enough for daily personal use.
- Provide portable backup and distribution without a proprietary cloud dependency.

### 5.2 Non-goals

- Hosting a public Skill registry or marketplace.
- User accounts, social features, ratings, comments, or publisher profiles.
- Managing custom Agents, Rules, Commands, Hooks, MCP servers, or general dotfiles.
- Executing Skills or enforcing permissions while an agent runs them.
- Automatically publishing Skills to skills.sh, ClawHub, or another registry.
- AI-generated Skill authoring or autonomous editing in V1.
- Silently auto-applying upstream updates.
- Organization RBAC, approval policies, or enterprise audit reporting.
- Simultaneous macOS, Windows, and Linux launch.

### 5.3 Personal-use success criteria

Because V1 has no analytics requirement, success is measured locally and through acceptance tests:

- Every valid Skill in configured global directories appears in inventory without changing its files.
- Every valid Skill within authorized project roots can be associated with its project and agent target.
- A Skill can be acquired into the Vault without creating an agent deployment.
- A previously external Skill can be taken over with a preview and recoverable original.
- One Skill or Collection can be deployed to several targets in one transaction.
- A failed multi-target deployment restores all affected managed targets to their previous state.
- Copy drift and broken links are visible without opening Finder or a terminal.
- An update never overwrites local modifications without an explicit resolution.
- Removing a deployment never deletes the Vault asset.
- A portable package can restore selected Skills on a clean installation without network access.
- Core workflows remain usable with telemetry and network access disabled.

## 6. Scope and milestones

### 6.1 M0 — Local Skill manager

M0 must be independently useful and replace manual global-directory management.

#### Included

- macOS Tauri application shell.
- Transparent Vault and SQLite index.
- Six built-in target adapters plus custom target directories.
- Automatic read-only scan of known global Skill directories.
- User-authorized Workspace Roots and manually added projects.
- External, vaulted, and managed ownership states.
- Exact duplicate and name-conflict detection.
- Takeover into the Vault.
- Global symlink deployment and project Managed Copy deployment.
- Deployment preflight, staging, commit, and rollback.
- Library, Deployments, Activity, Settings, and basic detail surfaces.
- Local snapshots, application Trash, and operation undo.
- First-run setup and persistent setup checklist.

#### Excluded until M1

- skills.sh and arbitrary remote Git acquisition.
- Built-in content editor.
- Full static security audit.
- Upstream update workflow.
- Collections and portable packages.

### 6.2 M1 — Acquisition, editing, and distribution

#### Included

- GitHub, GitLab, Bitbucket, arbitrary Git, repository subpaths, branches, tags, and commits.
- skills.sh discovery.
- Local folder, ZIP, TAR, URL, and well-known manifest import.
- Immutable upstream baseline and editable working version.
- Basic Skill creation, file tree, Markdown editor, preview, validation, and external-editor integration.
- Entire-bundle local static audit and Trust Sheet.
- Candidate update fetch, content diff, capability diff, and review.
- Three-way comparison when local and upstream changes coexist.
- Collections as exportable and deployable assets.
- Open `.skillpack` import and export.
- License and provenance detection.
- Derived local Skills for valid deployment aliases.

### 6.3 M2 — Reproducibility and backup

#### Included

- User-owned Git remote backup and restore.
- Snapshot history and batch-operation recovery.
- Optional project reproducibility manifest and lock.
- Restore project state from lock.
- Full Vault export, migration, and integrity verification.
- Git pull conflict handling for supported backup content.
- Collection history and deployment history.
- Windows path/link abstraction validation and test harness; Windows release remains a subsequent milestone.

## 7. Product vocabulary and domain model

### 7.1 Core entities

| Entity | Meaning |
| --- | --- |
| **Skill** | Stable local identity for one logical Skill, independent of display name |
| **Revision** | Immutable captured content with a digest and provenance |
| **Working Version** | Ordinary editable directory representing the current Vault content |
| **Source** | Local path, archive, URL, Git repository/subpath, skills.sh result, or local derivation |
| **Target Adapter** | Rules describing one agent family’s paths, scopes, and supported deployment modes |
| **Target** | One concrete global or project-level destination directory |
| **Deployment** | Managed relationship from one Skill revision/working version to one Target |
| **Workspace Root** | User-authorized directory in which projects may be discovered |
| **Project** | A detected or manually added workspace that may contain project-level Skill targets |
| **Collection** | Ordered named set of Skill references; no nested Collections in V1 |
| **Snapshot** | Recoverable content and metadata state captured before a mutation |
| **Operation** | Planned and recorded import, takeover, deployment, update, restore, or delete transaction |
| **Audit Result** | Content-hash-bound format and local security findings |
| **Skill Package** | Portable archive containing Skills, manifest, provenance, checksums, and optional Collection data |

### 7.2 Identity

Display names and identities are separate.

Remote identity is based on normalized provider, repository, and subpath:

```text
github.com/anthropics/skills/skills/frontend-design
```

Local identity uses a stable generated UUID plus import provenance:

```text
local/01J.../frontend-design
```

Rules:

- Same display name and same content digest: treat as the same content observed at multiple locations.
- Same display name and different digest: retain separate assets and show a name conflict.
- Different display names and same digest: suggest a probable duplicate or rename; never merge automatically.
- Scanning order never decides identity or replacement.
- Two same-named Skills may coexist in the Vault.
- A Target may expose only one deployment name at a time.

### 7.3 Orthogonal state dimensions

Do not compress all state into a single “installed” badge.

#### Ownership

- **External:** discovered but not copied or controlled.
- **Vaulted:** copied into the Vault but original locations remain unmanaged.
- **Managed:** Vault is authoritative for one or more tracked deployments.

#### Deployment health

- Clean
- Vault ahead
- Target modified
- Broken link
- Missing target
- Conflict
- Unverified

#### Upstream status

- Local-only
- Pinned
- Current
- Update available
- Locally modified
- Locally modified + update available

#### Audit status

- Not scanned
- Format valid
- Scan passed
- Findings
- Scan failed
- Quarantined

Each state requires text or an accessible label; color alone is insufficient.

## 8. Initial target adapters

The paths below are initial defaults and must be verified against official current documentation during implementation. Users can override any path.

| Target family | Global path | Project path | V1 status |
| --- | --- | --- | --- |
| Universal Agent Skills | `~/.agents/skills` | `.agents/skills` | Built-in |
| Claude Code | `~/.claude/skills` | `.claude/skills` | Built-in |
| OpenAI Codex | `~/.codex/skills` | `.codex/skills` | Built-in |
| Cursor | `~/.cursor/skills` | `.cursor/skills` | Built-in |
| Gemini CLI | `~/.gemini/skills` | `.gemini/skills` | Built-in |
| OpenCode | `~/.config/opencode/skills` | `.opencode/skills` | Built-in |
| Custom directory | User-selected | User-selected | Built-in escape hatch |

Each adapter declares:

- stable adapter ID and version;
- supported operating systems;
- default global and project paths;
- whether project scope is supported;
- supported deployment modes;
- detection hints for installed applications;
- validation rules or known compatibility limitations;
- confidence level: verified, community, or custom.

Adapters should be data-driven where behavior permits. Code-specific adapters are allowed when an agent requires transformation or non-filesystem behavior, but no such transformation is required for the initial six unless verified later.

## 9. Information architecture

### 9.1 Primary navigation

1. **Library** — every external, vaulted, and managed Skill.
2. **Deployments** — Skill × agent × scope state and operations.
3. **Discover** — skills.sh, repositories, URLs, and imports.
4. **Collections** — reusable sets for distribution and backup.
5. **Activity** — operations, network activity, failures, and recovery.
6. **Settings** — Vault, adapters, paths, workspaces, Git, security, privacy, and appearance.

M0 may hide Discover and Collections until their milestone is implemented rather than showing dead navigation.

### 9.2 Library

Library is the default screen. It must answer:

- What Skills exist?
- Which are external, vaulted, or managed?
- Where is each Skill deployed?
- Is it modified, outdated, risky, duplicated, or conflicting?
- What can the user safely do next?

Required columns or equivalent compact fields:

- name;
- source/publisher;
- ownership;
- version or source revision;
- update/local modification state;
- audit state;
- deployment count and health;
- Collections;
- last changed time.

Support list/table view first. A promotional card grid is not the default for managed assets.

### 9.3 Deployments

Provide three views over the same data:

- by Agent;
- by Project;
- by Skill.

The matrix must support keyboard navigation, virtualized rows when needed, accessible status labels, and a non-matrix list fallback for narrow windows or assistive technology.

### 9.4 Skill detail

The detail surface contains:

- overview and `SKILL.md` preview;
- file tree and editor in M1;
- source, license, resolved commit, and digests;
- ownership and observed locations;
- deployments and drift;
- update comparison;
- audit report;
- snapshots and activity;
- Collection membership;
- export, derive, reveal in Finder, open externally, move to Trash, and restore actions.

## 10. Functional requirements

Requirement priority uses **P0** for milestone acceptance, **P1** for important follow-up within the milestone, and **P2** for deferrable enhancement.

### 10.1 Vault and indexing

| ID | Priority | Requirement |
| --- | --- | --- |
| VLT-01 | P0 | Create a Vault at the default macOS application-data location or a user-selected directory. |
| VLT-02 | P0 | Store working Skills as ordinary directories accessible through Finder and external editors. |
| VLT-03 | P0 | Store metadata and relationships in SQLite, not Skill content blobs. |
| VLT-04 | P0 | Store immutable baselines and snapshots under an internal content-addressed object directory. |
| VLT-05 | P0 | Monitor Vault working directories and mark external edits without overwriting them. |
| VLT-06 | P0 | Reveal, relocate, verify, and repair the Vault without changing Skill identity. |
| VLT-07 | P1 | Rebuild the SQLite index from readable manifests and filesystem content where possible. |
| VLT-08 | P1 | Garbage-collect unreferenced internal objects only after a retention window and verification pass. |

### 10.2 Scanning and discovery of local Skills

| ID | Priority | Requirement |
| --- | --- | --- |
| SCN-01 | P0 | Scan known global target directories automatically and read-only. |
| SCN-02 | P0 | Inspect immediate child directories for `SKILL.md`; ignore inaccessible or missing target roots without failing the full scan. |
| SCN-03 | P0 | Deduplicate observations using normalized paths and content digests, not folder name alone. |
| SCN-04 | P0 | Detect symlinks and avoid cycles or traversal outside an authorized scan root. |
| SCN-05 | P0 | Let users add, remove, pause, and rescan Workspace Roots. |
| SCN-06 | P0 | Discover projects only inside authorized roots or manually added paths. |
| SCN-07 | P0 | Skip `.git`, dependency, build, cache, generated, and user-configured ignored directories. |
| SCN-08 | P1 | Use incremental filesystem events after initial indexing; fall back to targeted rescans when events are unreliable. |
| SCN-09 | P1 | Show scan source, last successful scan, ignored errors, and current coverage. |

### 10.3 Import and takeover

| ID | Priority | Requirement |
| --- | --- | --- |
| IMP-01 | P0 | Display valid external Skills without mutating them. |
| IMP-02 | P0 | Offer “Keep external,” “Add to Vault,” and “Add and manage” as distinct choices. |
| IMP-03 | P0 | Before takeover, show source paths, content conflicts, planned copies/links, and recovery location. |
| IMP-04 | P0 | Copy content into staging, verify it, then atomically activate the Vault working version. |
| IMP-05 | P0 | Never replace an existing Vault Skill solely because names match. |
| IMP-06 | P0 | Preserve originals until the user separately confirms replacement by a managed deployment. |
| IMP-07 | P0 | Create an operation snapshot before replacing or removing any existing managed destination. |
| IMP-08 | P1 | Attempt to recover provenance from known lockfiles or Git context, labeling confidence and source. |

### 10.4 Deployment

| ID | Priority | Requirement |
| --- | --- | --- |
| DPL-01 | P0 | Default global deployment to a directory symlink on macOS. |
| DPL-02 | P0 | Default Git project deployment to Managed Copy. |
| DPL-03 | P0 | Default non-Git personal project deployment to symlink, with user override. |
| DPL-04 | P0 | Fall back to copy only after a link capability failure and disclose the actual mode. |
| DPL-05 | P0 | Create an Operation Plan listing every path created, replaced, removed, or left untouched. |
| DPL-06 | P0 | Detect unmanaged name collisions and refuse silent replacement. |
| DPL-07 | P0 | Stage all multi-target changes before committing any of them. |
| DPL-08 | P0 | Roll back every committed step when a transaction fails. |
| DPL-09 | P0 | Store deployment mode, expected digest, target path, adapter version, and last verification time. |
| DPL-10 | P0 | Detect broken links, missing files, target edits, Vault changes, and conflicts. |
| DPL-11 | P0 | Removing one deployment must not remove the Vault Skill or other deployments. |
| DPL-12 | P1 | Allow dry-run export of an Operation Plan as human-readable JSON. |

### 10.5 Deletion and recovery

| ID | Priority | Requirement |
| --- | --- | --- |
| DEL-01 | P0 | Use distinct actions and language for undeploy, move to Trash, and permanently delete. |
| DEL-02 | P0 | Moving a deployed Skill to Trash must show every affected deployment and require a resolution. |
| DEL-03 | P0 | Trash retains working content, provenance, snapshots, and Collection references for a configurable period. |
| DEL-04 | P0 | Permanent deletion is only available from Trash and requires secondary confirmation. |
| DEL-05 | P0 | External unmanaged Skills can be ignored or taken over; direct filesystem deletion is not a normal action. |
| DEL-06 | P0 | Batch delete and undeploy operations create one operation-level recovery point. |

### 10.6 Remote acquisition

| ID | Priority | Requirement |
| --- | --- | --- |
| SRC-01 | P0 | Acquire from public or credential-accessible GitHub, GitLab, Bitbucket, and arbitrary HTTPS/SSH Git sources. |
| SRC-02 | P0 | Select a repository subpath and discover multiple Skills in one repository. |
| SRC-03 | P0 | Accept branch, tag, or commit requests and record the immutable resolved commit. |
| SRC-04 | P0 | Reuse system Git, SSH Agent, and OS credential facilities; do not store plaintext tokens in SQLite. |
| SRC-05 | P0 | Search skills.sh and resolve results back to their source repository and path. |
| SRC-06 | P0 | Import local folders, ZIP, TAR, direct URLs, and supported well-known manifests. |
| SRC-07 | P0 | Acquisition adds content to the Vault and never creates a deployment without a separate user action. |
| SRC-08 | P1 | Cache catalog metadata for browsing while offline, with clear freshness information. |
| SRC-09 | P1 | Use a provider interface so future registries do not alter Vault or Deployment semantics. |

### 10.7 Editing and local derivation

| ID | Priority | Requirement |
| --- | --- | --- |
| EDT-01 | P0 | Create a valid local Skill from an empty or basic template. |
| EDT-02 | P0 | Edit YAML frontmatter and Markdown with raw-source access and preview. |
| EDT-03 | P0 | Manage the full Skill file tree, including scripts, references, and assets. |
| EDT-04 | P0 | Validate format and show a save diff for material changes. |
| EDT-05 | P0 | Snapshot before editor saves that modify an existing revision. |
| EDT-06 | P0 | Reveal in Finder and open with the user’s external editor. |
| EDT-07 | P0 | Detect external edits and update local-modification state. |
| EDT-08 | P0 | “Create deployment alias” produces a local derived Skill with matching folder and frontmatter names. |
| EDT-09 | P0 | Derived Skills preserve a `derivedFrom` relationship without inheriting upstream identity. |
| EDT-10 | P2 | AI-assisted editing is explicitly deferred. |

### 10.8 Updates

| ID | Priority | Requirement |
| --- | --- | --- |
| UPD-01 | P0 | Check source metadata and content identifiers without modifying the current revision. |
| UPD-02 | P0 | Support per-Skill policies: pinned, track and notify, or auto-download candidate. |
| UPD-03 | P0 | Never auto-apply a candidate in V1. |
| UPD-04 | P0 | Download candidates into isolated staging and audit them before approval. |
| UPD-05 | P0 | Show file, instruction, script, dependency, network-domain, and risk changes. |
| UPD-06 | P0 | Preserve the current revision until the new revision and dependent deployments commit successfully. |
| UPD-07 | P0 | Use three-way comparison when both local and upstream content changed. |
| UPD-08 | P0 | Exclude high-risk or conflicting Skills from one-click bulk update. |
| UPD-09 | P1 | Allow update checks to be disabled globally and per source. |

### 10.9 Security audit

| ID | Priority | Requirement |
| --- | --- | --- |
| SEC-01 | P0 | Perform all default audit work locally and without executing Skill code. |
| SEC-02 | P0 | Scan the entire Bundle, not only `SKILL.md`. |
| SEC-03 | P0 | Detect scripts, executable bits, binaries, symlinks, hidden Unicode, suspicious obfuscation, remote execution, credential access, exfiltration, persistence, privilege escalation, destructive writes, and unpinned package acquisition. |
| SEC-04 | P0 | Bind results to the complete content digest and invalidate them after any content change. |
| SEC-05 | P0 | Keep format validity, scan result, checksum verification, and source trust as separate UI concepts. |
| SEC-06 | P0 | Quarantine critical findings by default and require a detailed explicit override. |
| SEC-07 | P0 | Treat scan failure as unknown, never as passed. |
| SEC-08 | P0 | Report file, line, severity, rule, explanation, and recommended review action where available. |
| SEC-09 | P1 | Support future pluggable scanners, but no scanner may upload content without separate informed consent. |

### 10.10 Collections and packages

| ID | Priority | Requirement |
| --- | --- | --- |
| COL-01 | P0 | Create, rename, describe, order, and delete Collections. |
| COL-02 | P0 | Allow one Skill to belong to several Collections. |
| COL-03 | P0 | Deploy or undeploy a Collection through one reviewed Operation Plan. |
| COL-04 | P0 | Do not support nested Collections in V1. |
| COL-05 | P0 | Do not permanently bind target agents to a Collection; targets are selected when deploying. |
| PKG-01 | P0 | Export one Skill, a Collection, or a full Vault backup using a documented `.skillpack` container. |
| PKG-02 | P0 | Include manifest, content, provenance, checksums, license evidence, and optional deployment intent. |
| PKG-03 | P0 | Import into staging, show conflicts and audit status, and never auto-deploy. |
| PKG-04 | P0 | Preserve unknown and third-party licenses without rewriting them. |

### 10.11 Git backup and project reproducibility

| ID | Priority | Requirement |
| --- | --- | --- |
| GIT-01 | P0 | Configure a user-owned public or private Git remote for selected backup content. |
| GIT-02 | P0 | Use system credentials and avoid storing plaintext secrets. |
| GIT-03 | P0 | Back up working Skills, Collections, portable manifests, provenance, and selected history—not deployment copies or absolute local paths. |
| GIT-04 | P0 | Warn before third-party or unknown-license content is pushed to a public remote. |
| GIT-05 | P0 | Show commit, push, pull, conflict, and restore activity. |
| PRJ-01 | P0 | Keep project deployment metadata local by default. |
| PRJ-02 | P0 | Let a user enable reproducible mode per project. |
| PRJ-03 | P0 | In reproducible mode, write a reviewable project manifest and exact lock without credentials or home-directory paths. |
| PRJ-04 | P0 | Restore or verify project Skill state from the lock after explicit review. |

## 11. Critical user flows

### 11.1 First run

1. Choose default or custom Vault location.
2. Detect the six built-in agent families and show discovered paths.
3. Read-only scan global Skill directories.
4. Optionally authorize one or more Workspace Roots.
5. Review discovered external Skills.
6. Choose per Skill: keep external, add to Vault, or add and manage.
7. Enter Library with a persistent setup checklist.

Every step is skippable. No account, Git remote, workspace, takeover, or deployment is mandatory.

### 11.2 Take over an existing global Skill

1. Select an external Skill.
2. Review every observed path and same-name conflict.
3. Choose “Add to Vault” or “Add and manage.”
4. Review source path, destination, backup, and planned target replacement.
5. Copy to staging, validate, hash, and snapshot.
6. Activate the Vault working version.
7. If managing, replace only confirmed target locations with managed links/copies.
8. Verify every destination and record Activity.

### 11.3 Acquire without deploying

1. Search skills.sh or enter a repository/URL.
2. Inspect source, files, license, and initial audit summary.
3. Select repository Skill(s) and requested ref.
4. Resolve to immutable commit and download into staging.
5. Validate, audit, and save the baseline plus working version.
6. Return to Library with ownership “Vaulted” and deployment count zero.

### 11.4 Deploy a Skill or Collection

1. Select Skill(s) or a Collection.
2. Select agent targets and global or project scope.
3. Resolve default mode per destination; allow an explicit override.
4. Detect collisions, drift, unsupported paths, and permission issues.
5. Present a complete Operation Plan.
6. Stage all destinations and create recovery points.
7. Commit atomically where possible; otherwise use a logged compensating rollback.
8. Verify resulting links/copies and display a concise result.

### 11.5 Review an update

1. Receive an update-available status without current content changing.
2. Download candidate into staging.
3. Re-run validation, license detection, and local audit.
4. Compare old upstream, new upstream, and working version.
5. Show file and capability changes.
6. Choose update, merge, keep local, pin current, or derive a local fork.
7. Snapshot and activate the approved revision.
8. Re-deploy affected managed targets as one transaction.

### 11.6 Remove and recover

- **Undeploy:** remove only the selected managed target after checking target drift.
- **Move to Trash:** retain content and metadata; resolve deployments first.
- **Restore:** recover the Vault asset, then optionally select deployments to recreate.
- **Permanently delete:** only from Trash, with secondary confirmation and reference-aware object cleanup.

## 12. UX and design brief

### 12.1 Feature summary

Skills Hub is a desktop operations tool for a developer managing Skills across several agents and projects. It must make ownership, source, risk, drift, and the consequence of an action legible without requiring terminal inspection.

### 12.2 Primary user action

The primary recurring action is:

> Select a Skill or Collection, understand its current state, and safely change where an exact version is deployed.

### 12.3 Design direction

- **Register:** product.
- **Color strategy:** Restrained. Neutral application surfaces with one sky-blue/teal anchor around OKLCH hue 200; semantic colors are reserved for meaningful status.
- **Theme scene:** a developer uses the application on a Mac before or after a long coding session, in changing daytime and nighttime ambient light, focused on resolving state rather than exploring decoration. The application follows the system light/dark appearance.
- **Anchors:** Raycast for speed, Linear for state density, Finder for location and ownership transparency.
- **Anti-direction:** no neon hacker styling, glass dashboard, oversized metric hero, endless identical cards, or marketplace visual language in the managed Library.

Final tokens, typography, and components belong in a later `DESIGN.md` after the application shell exists.

### 12.4 Layout strategy

- Use a stable desktop app shell with compact primary navigation.
- Favor a dense list/table for Library and a virtualized matrix/list pair for Deployments.
- Use an inspector or full detail surface for one selected Skill rather than opening a modal for routine inspection.
- Keep the main action area close to state and affected-path information.
- Reveal advanced provenance, hashes, and filesystem details progressively; never hide them permanently.
- Use standard macOS interaction expectations for selection, context menus, file reveal, keyboard shortcuts, and destructive confirmation.

### 12.5 Key states

| Surface | State | Required user understanding |
| --- | --- | --- |
| Library | Empty, no scan configured | How to scan global paths or add a source |
| Library | External Skills found | Nothing has been modified; next ownership choices |
| Library | Normal | Ownership, update, audit, and deployment health at a glance |
| Library | Duplicate/conflict | Whether content is identical or only names match |
| Deployments | Clean | Exact targets and modes are healthy |
| Deployments | Drift | Which side changed and available resolutions |
| Deployments | Broken/missing | What path failed and whether repair is safe |
| Discover | Offline | Cached results and source actions that require network |
| Detail | Audit findings | Evidence, severity, and whether deployment is blocked |
| Update | Local + upstream changes | Three inputs, conflicts, and non-destructive choices |
| Operation | Planning | Complete affected paths and recoverability |
| Operation | Running | Step progress without losing the plan context |
| Operation | Partial failure | What rolled back, what did not, and the next safe action |
| Trash | Recoverable | Retention and restore scope |

### 12.6 Interaction model

- Single click selects; double click or Enter opens detail.
- Space or a dedicated preview action opens a quick read-only preview where appropriate.
- Multi-select exposes one contextual action bar, not duplicated card buttons.
- Matrix cells expose status first and action second.
- Destructive or cross-target changes use an inline plan/review step before confirmation.
- Long operations stream structured step progress from Rust and remain cancelable before commit.
- Toasts confirm outcomes; they do not contain the only copy of an error or recovery link.
- Motion is limited to 150–250 ms state transitions and progress feedback, with reduced-motion alternatives.

### 12.7 Content requirements

Copy should prefer filesystem and ownership language the user already understands:

- “Add to Vault,” not a generic “Install.”
- “Deploy to Claude,” not “Enable Claude.”
- “Remove deployment,” not “Delete,” when the asset remains.
- “Target modified,” not “Out of sync,” when the direction of drift is known.
- “Scan failed,” not an empty green check.
- “No files were changed” on scan and discovery results.

Dynamic content must handle long paths, repository names, branches, Skill descriptions, and hundreds of observed locations without truncating the only identifying information. Truncated paths require a full-value tooltip or copy action.

### 12.8 Design references for implementation

When UI implementation begins, use these Impeccable references:

- `product.md` for product-density and component-state discipline.
- `interaction-design.md` for plans, confirmations, editors, and matrices.
- `layout.md` for app shell, table/detail composition, and inspector behavior.
- `onboard.md` for first run and setup checklist.
- `harden.md` for long paths, partial failures, offline states, and i18n readiness.
- `adapt.md` for narrow macOS windows and matrix fallbacks.
- `typeset.md` for compact information hierarchy.

## 13. Storage and technical architecture

### 13.1 High-level architecture

```text
React / TypeScript UI
        │ typed Tauri commands and events
        ▼
Rust application services
├── Vault and object store
├── SQLite metadata and operation journal
├── Scanner and filesystem watcher
├── Target adapter registry
├── Deployment planner and transaction executor
├── Git/source providers
├── Validator and local auditor
├── Package importer/exporter
└── Backup, restore, and integrity services
```

Rust is authoritative for domain state. React may cache query results through TanStack Query, but it must not independently infer ownership, deployment health, or transaction success.

### 13.2 Proposed Vault layout

```text
Vault/
├── skills/                    # visible editable working versions
│   ├── frontend-design/
│   └── webapp-testing/
├── collections/               # readable portable declarations
└── .manager/
    ├── objects/               # immutable content-addressed revisions
    ├── manifests/             # portable source/version metadata
    ├── staging/               # incomplete operations; safe to clean
    ├── trash/                 # recoverable removed assets
    ├── operations/            # durable operation plans/results as needed
    └── index.sqlite           # local query index and relationships
```

The exact internal path may change, but these boundaries are requirements:

- working content is user-accessible;
- internal history is not edited manually;
- staging is isolated from active content;
- losing SQLite must not make working Skill files unreadable;
- manifests provide enough durable metadata for supported index recovery.

### 13.3 Deployment transaction model

Every mutating multi-path operation follows:

1. **Plan:** normalize and validate all paths and expected state.
2. **Preflight:** detect permissions, collisions, drift, unsupported mode, disk space, and unsafe links.
3. **Snapshot:** capture managed content and metadata needed for rollback.
4. **Stage:** create temporary sibling entries on the same filesystem where atomic rename is required.
5. **Commit:** switch destinations in a deterministic order.
6. **Verify:** compare link target or deployed digest with expected state.
7. **Finalize:** persist domain state and Activity record.
8. **Rollback:** if any commit/verify step fails, restore every prior committed step and report any rollback failure separately.

The operation journal must distinguish “operation failed and fully rolled back” from “operation failed and requires manual recovery.”

### 13.4 Preliminary project manifest

The project manifest filename remains an implementation decision; the product name is final.

```json
{
  "schemaVersion": 1,
  "skills": [
    {
      "id": "github.com/anthropics/skills/skills/frontend-design",
      "requested": "main",
      "targets": ["claude", "codex"],
      "scope": "project"
    }
  ]
}
```

The corresponding lock records resolved state:

```json
{
  "schemaVersion": 1,
  "skills": [
    {
      "id": "github.com/anthropics/skills/skills/frontend-design",
      "source": "https://github.com/anthropics/skills.git",
      "subpath": "skills/frontend-design",
      "resolvedCommit": "abc123...",
      "contentSha256": "...",
      "adapterVersion": "claude@1",
      "localModificationSha256": null
    }
  ]
}
```

The lock is reproducibility evidence, not proof of publisher identity.

### 13.5 Preliminary `.skillpack`

`.skillpack` is a ZIP-compatible open container in V1:

```text
frontend-toolkit.skillpack
├── manifest.json
├── skills/
│   ├── frontend-design/
│   └── accessibility/
├── provenance/
│   └── sources.json
├── licenses/
└── checksums.sha256
```

The manifest must version the format and list every included Skill identity, revision, dependency assumption, license result, and optional Collection. Import verifies checksums before exposing content and audits extracted content before allowing deployment.

## 14. Security and trust model

### 14.1 Boundary

Agent Skills are potentially executable instruction packages. Skills Hub is responsible for safe acquisition, storage, inspection, and deployment. It is not responsible for sandboxing an external agent after that agent loads or executes a Skill.

The application must never:

- execute a Skill script during import, validation, audit, preview, export, or deployment;
- run package-manager install hooks as part of acquisition;
- treat a checksum as a publisher signature;
- treat format validation or scan success as proof of safety;
- upload content to a scanner without separate explicit consent;
- follow untrusted archive links outside staging;
- silently permit a source path to control a target path.

### 14.2 Priority threats

| Priority | Threat | Minimum mitigation |
| --- | --- | --- |
| P0 | Archive traversal, symlink escape, device files, decompression bombs | Bounded isolated extraction, canonical containment, link/device rejection |
| P0 | Malicious scripts or instructions | Full-bundle local audit, evidence display, quarantine, no execution |
| P0 | Credential access and exfiltration | Rules and capability summary; block critical findings by default |
| P0 | Mutable upstream takeover | Resolve and record immutable commits and complete digests |
| P0 | Name/source confusion | Namespaced identity, duplicate detection, no first-match behavior |
| P0 | Destructive target overwrite | Preflight, ownership check, snapshot, staged atomic replacement |
| P1 | Runtime remote instructions and unpinned dependencies | Detect and report dynamic sources and package operations |
| P1 | Audit outage or parser failure | Explicit “scan failed/unknown,” never fail green |
| P1 | Private content leakage | No default upload; redact explicit diagnostic exports |
| P2 | Client compatibility drift | Adapter versions, verified status, revalidation after adapter change |

### 14.3 Archive and filesystem safety

- Limit archive bytes, expanded bytes, file count, nesting depth, and compression ratio.
- Reject absolute paths, drive prefixes, UNC paths, NUL/control characters, and `.`/`..` segments.
- Normalize URL encoding and Unicode before containment validation.
- Reject symlinks, hard links, devices, and FIFOs from untrusted archives in V1.
- Ensure every extracted canonical path remains inside staging.
- Do not preserve unexpected executable bits by default; record them as findings.
- Use temporary sibling directories and atomic rename for active-content replacement.
- Restrict cleanup to normalized application-controlled staging paths.
- Reject dangerous Git transports such as `ext::`.

## 15. Privacy and network behavior

### 15.1 Defaults

- No account.
- No product analytics.
- No installation-count telemetry.
- No project path, Skill name, repository, content, or deployment-state upload.
- No remote security scan.
- No background network request that cannot be identified in Activity.

### 15.2 Optional diagnostics

Users may explicitly enable anonymous crash/performance diagnostics. Diagnostic generation must be previewable and redact:

- home paths to `~`;
- project names to stable local hashes;
- private repository owners/names;
- credentials and environment values;
- Skill and source file contents.

### 15.3 Network activity

Network access is limited to user-configured or user-visible operations:

- application update check;
- skills.sh query;
- Git fetch/clone/push/pull;
- direct URL or well-known manifest fetch;
- explicitly enabled diagnostics.

Global and per-source update checks can be disabled. Activity records endpoint category, source identity, time, result, and bytes where practical, but never credentials.

## 16. License and provenance behavior

License detection order:

1. Skill manifest/frontmatter SPDX field.
2. Skill-directory `LICENSE`, `COPYING`, or `NOTICE`.
3. Repository-root license.
4. Source-provider license API.
5. Unknown.

Store license value, evidence location, and confidence separately.

- Unknown license does not block personal acquisition or deployment.
- Export to another person requires a visible summary and confirmation for unknown/conflicting status.
- Pushing third-party or unknown-license content to a public Git remote requires a warning.
- Packages preserve original license and notice files.
- Skills Hub never rewrites a third-party license.
- The UI states that detection is informational and not legal advice.

## 17. Performance, scale, and reliability targets

These are design targets, not usage analytics.

### 17.1 Reference scale

- 1,000 Vault Skills.
- 5,000 observed external/deployed locations.
- 200 indexed projects across several Workspace Roots.
- 100 Skills in one Collection.
- 20 targets in one deployment transaction, including custom directories.

### 17.2 Performance targets on a contemporary Apple Silicon Mac

- Warm application launch to usable Library: under 1.5 seconds for reference scale.
- Global known-directory scan: under 1 second when filesystem metadata is warm.
- Initial Workspace Root scan: progressive results within 2 seconds; completion time reported rather than blocking UI.
- Local Library search response: under 100 ms at reference scale.
- Deployment plan generation: under 500 ms excluding content hashing of newly changed large bundles.
- UI remains interactive during scan, hashing, Git, audit, and deployment operations.

### 17.3 Reliability targets

- Scan operations are idempotent and read-only.
- Re-running a successful deployment plan against unchanged state produces no writes.
- Application crash during staging leaves active content unchanged.
- Application crash during commit is detectable and recoverable from the operation journal on next launch.
- No operation reports success before post-commit verification.
- Failed cleanup is logged but must not trigger unsafe broad deletion retries.

## 18. Accessibility and desktop behavior

- WCAG 2.2 AA for color contrast, focus, controls, and text alternatives.
- Full keyboard path through Library, Deployments, plan review, dialogs, and editor.
- Standard table semantics or accessible list equivalents.
- Deployment matrix has row/column headers and a readable list alternative.
- Focus returns to the initiating control after cancel, completion, or dialog close.
- Statuses include text and iconography in addition to color.
- Respect macOS reduced motion, increased contrast, system appearance, and text scaling where available.
- Support a practical minimum window around 900 × 600 without hiding critical actions; switch matrix/detail layouts structurally rather than shrinking type.
- Long names and paths wrap, truncate with reveal/copy access, or use responsive columns without horizontal page overflow.

## 19. Milestone acceptance criteria

### 19.1 M0 acceptance

M0 is complete when all of the following pass on macOS:

1. A clean install can create or select a Vault.
2. Known global directories for all six adapters can be scanned without mutation.
3. At least one Workspace Root can be indexed with ignored directories and symlink cycles handled.
4. Same-name/same-content and same-name/different-content cases are distinguished.
5. An external Skill can be added to the Vault while leaving the original untouched.
6. A Vault Skill can be deployed globally by symlink and to a Git project by Managed Copy.
7. A multi-target deployment collision produces a plan and performs no writes before confirmation.
8. An injected failure during commit restores all earlier committed managed targets.
9. Target edits and broken links appear in Deployments.
10. Undeploy, Trash, restore, and permanent delete have distinct behavior and copy.
11. Activity accurately reports operation outcome and recovery availability.
12. All core workflows are keyboard accessible.

### 19.2 M1 acceptance

1. A Skill can be acquired from Git or skills.sh into the Vault with zero deployments.
2. Repository subpath and resolved commit are preserved.
3. ZIP/TAR traversal and symlink test fixtures are rejected safely.
4. A full Bundle can be locally audited without execution or network access.
5. Format valid, scan passed, findings, failed, and quarantined states remain distinguishable.
6. A downloaded Skill can be edited while preserving its immutable upstream baseline.
7. An upstream candidate can be compared and accepted without losing local history.
8. A local + upstream conflict cannot be silently overwritten.
9. A Collection can deploy as one reviewed operation.
10. A `.skillpack` can export, verify, import, and remain undeployed until explicitly selected.
11. A deployment alias creates a valid derived Skill with matching directory/frontmatter name.

### 19.3 M2 acceptance

1. Selected Vault content can be pushed to and restored from a user-owned private Git repository.
2. No credentials or machine-specific absolute paths enter backup commits.
3. Public-remote license warnings trigger as specified.
4. A project manifest and lock can restore exact Skill revisions on a clean machine.
5. A full Vault can be migrated and verified without changing stable identities.
6. Interrupted backup or restore operations produce a diagnosable, recoverable state.
7. Windows path/link behavior is represented in adapter tests even if Windows packaging has not shipped.

## 20. Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Scope expands into a complete agent configuration suite | Delayed usable release | V1 only accepts Skill Bundles; enforce M0/M1/M2 gates |
| Agent path conventions change quickly | Broken discovery/deployment | Versioned adapters, path overrides, verified/community labels |
| Symlink/copy semantics surprise users | Data loss or stale deployments | Show actual mode, source/target path, drift direction, and plan |
| SQLite and filesystem diverge | Incorrect state | Durable manifests, reconciliation scan, operation journal, index repair |
| Static scanner creates false confidence | Unsafe deployment | Evidence-based wording; no “safe” certification; quarantine only known criticals |
| Git backup becomes a second package manager | Complexity and conflicts | Back up portable state; do not make Git the runtime source of truth |
| Project scan is expensive or intrusive | Poor trust and performance | Explicit Workspace Roots, ignores, depth controls, progressive results |
| Same-name packages impersonate trusted Skills | Supply-chain confusion | Namespaced identity, immutable source, digest, no folder-name identity |
| Local editing complicates updates | Lost work | Immutable baseline, snapshots, three-way comparison, derive/fork action |
| `.skillpack` becomes proprietary lock-in | User distrust | Publish format schema; ZIP-compatible content; ordinary files |
| M0 UI overemphasizes dashboards | Core action becomes harder | Library-first dense list; state and actions over metrics |

## 21. Open implementation decisions and defaults

Remaining items do not block the PRD and may be resolved during technical design or implementation:

- Rust Git implementation versus invoking system Git for selected operations; credential compatibility is the deciding constraint.
- Snapshot retention default; proposed starting point is 30 days plus protected operation checkpoints.
- Background update-check cadence; proposed starting point is once per 24 hours while the app is active.
- Exact local audit rules and severity thresholds.
- Final `.skillpack` JSON schema and project manifest filenames.
- Icon set, editor, and diff-viewer packages for the milestones that need them.

## 22. Future opportunities outside V1

- Windows and Linux signed releases.
- Deployment Profiles that bind a reusable target set separately from Collections.
- Additional registries such as ClawHub or private SkillHub instances.
- Pluggable community adapters and scanners.
- Signed packages, Sigstore provenance, and trusted-publisher policy.
- Optional user-triggered LLM review with explicit privacy consent.
- Skill test harnesses and agent compatibility checks.
- Custom Agents as a separate asset type.
- Team manifests, private registry, approval workflow, and organization policy.
- Encrypted hosted backup, only if a later user base justifies a service.

## 23. Reference material

Primary implementation and product references investigated for this PRD:

- Local reference: `reference/cc-switch`.
- [xingkongliang/skills-manager](https://github.com/xingkongliang/skills-manager)
- [Shpigford/chops](https://github.com/Shpigford/chops)
- [jiweiyeah/Skills-Manager](https://github.com/jiweiyeah/Skills-Manager)
- [runkids/skillshare](https://github.com/runkids/skillshare)
- [vercel-labs/skills](https://github.com/vercel-labs/skills)
- [dallay/agentsync](https://github.com/dallay/agentsync)
- [iflytek/skillhub](https://github.com/iflytek/skillhub)
- [openclaw/clawhub](https://github.com/openclaw/clawhub)
- [Agent Skills specification](https://agentskills.io/specification)
- [Snyk Agent Scan](https://github.com/snyk/agent-scan)

## 24. Decision log

This PRD incorporates the following confirmed decisions:

1. Personal multi-agent power user first; Skill creators secondary.
2. Discover first, explicit takeover, non-destructive default.
3. Global link and Git-project Managed Copy defaults.
4. V1 manages Skills only.
5. macOS first with cross-platform architecture.
6. Six initial agent families plus custom target paths.
7. Local, Git, skills.sh, file, archive, and URL sources.
8. Editable working version with immutable upstream baseline.
9. Local snapshots, portable packages, and user-owned Git backup.
10. Fixed global scans plus authorized Workspace Root project discovery.
11. Optional per-project reproducibility manifest.
12. Collections are first-class, non-nested assets.
13. Local-only static audit and Trust Sheet.
14. Automatic update checks with human review and no auto-apply.
15. Namespaced identity and same-name Vault coexistence.
16. Library-first information architecture and separate deployment matrix.
17. Built-in basic editor without AI authoring.
18. Transparent filesystem working area, object history, and SQLite metadata.
19. Undeploy, Trash, and permanent delete are separate actions.
20. Zero telemetry by default; optional anonymous diagnostics.
21. Skippable setup flow and persistent setup checklist.
22. License warnings increase when content leaves local private use.
23. Completely open source under MIT.
24. M0 → M1 → M2 delivery sequence.
25. Deployment aliases create valid local derived Skills.
26. React, TypeScript, and Vite frontend on Tauri 2 and Rust.
27. Restrained, trustworthy, efficient design baseline with WCAG 2.2 AA.
