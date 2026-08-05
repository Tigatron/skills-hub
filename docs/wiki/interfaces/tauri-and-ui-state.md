---
type: Interface Contract
title: Tauri and UI State Contract
description: Defines typed commands, read models, events, query invalidation, errors, and frontend authority boundaries.
status: accepted
tags: [skills-hub, m0, tauri, react, api]
requirements: [VLT-01, SCN-05, IMP-02, IMP-03, DPL-05, DPL-10, DEL-01]
timestamp: 2026-07-23T00:00:00Z
---

# Boundary rule

Tauri commands expose use cases and read models, not repositories or filesystem primitives. Rust returns final ownership, health, blockers, and outcomes. TypeScript treats IDs and digests as opaque strings and never reconstructs target paths or domain state.

Rust is the interface source. Generate TypeScript DTOs and Tauri command bindings, preferring a Specta/Tauri Specta compatibility spike in `M0-001`; if blocked, fall back only to generated DTOs plus a centralized typed invoke wrapper. Generated files are committed and CI drift-checked. UI code never scatters raw `invoke` calls. A generated-contract diff is reviewed like an API change.

# Implemented M0-004 slice

Generated bindings now expose `scan_start`, `scan_get`, `scan_cancel`, and `library_list` over Rust-owned scan and external-Library DTOs. `scan-progress` updates transient progress and `domain-invalidated` identifies `scan`/`library` scopes that must be refetched after durable changes. The Library query supports bounded pagination, filtering, stable grouped observations, visible location errors, and authoritative next actions. The remaining provisional command groups and read models below stay planned until their owning tasks complete.

# Implemented M0-005 slice

The Rust-internal Operation kernel now supplies immutable ID-plus-digest execution, explicit terminal outcomes, replay-safe results, and a stable serializable error envelope with recovery actions. It deliberately does not expose the provisional `operation_*` command group below: product intent DTOs, Tauri/runtime wiring, progress/terminal events, and startup recovery action driving remain M0-006/M0-008 work. The M0-005 startup surface classifies journal evidence only.

# Implemented M0-006 slice

Generated bindings now expose `takeover_keep_external`, `takeover_plan`, `operation_execute`, `operation_cancel`, `operation_get`, `skill_get`, and `skill_preview_file`. Planning accepts Observation IDs, an explicit takeover choice, selected Observation IDs, and requested modes; Rust derives and seals every path and authority record. Execution accepts only Operation ID plus plan digest, while Operation and Skill-detail responses return Rust-owned outcomes, ownership, source/deployment/observation paths, conflicts, allowed actions, and bounded safe text previews.

# M0-007 implementation evidence

Generated bindings add `target_register_fixture`, `targets_list`, `deployment_plan`, `undeploy_plan`, `deployment_verify`, and bounded/filterable `deployments_list`. Deployment mutation intents contain only Skill/Target/Deployment IDs, requested mode, or explicit undeploy resolution; the fixture registration command is the sole directory-selection seam. The existing `operation_execute`, `operation_get`, and `operation_cancel` commands dispatch persisted takeover, deploy, and undeploy Operations by kind while preserving the ID-plus-digest execution contract. Health, drift direction, explanations, actions, disabled reasons, and terminal outcomes are Rust-owned DTO fields. Activity listing and startup recovery action execution are delivered in M0-008; operation progress events remain later UI work.

# M0-008 implementation evidence

Generated bindings add `batch_deployment_plan`, `deployment_undo_plan`, `startup_recovery_run`, `startup_recovery_status`, `activity_list`, and `activity_detail`. Batch mutation input is one Skill ID plus 2–20 Target ID/requested-mode choices; inverse input is only the completed Operation ID. Generic execute/get dispatch now returns distinct typed single or batch Operation views. Startup recovery evidence and bounded Activity list/detail—including typed actual path/mode, failure step/code, plan/journal links, and recovery references—remain Rust-authoritative. Startup recovery runs while opening the configured Vault and blocks later mutation/scan service access if a nonterminal Operation remains unresolved.


# M0-009 implementation evidence

`vault_initialize` and `vault_status` complete the first-run Vault seam. The thin-slice renderer consumes generated commands for scan, library, takeover, deployment, undeploy, and Activity through TanStack Query. Bootstrap reports `vaultInitialized` / `vaultPath`. Events invalidate queries; the UI never optimistically changes ownership or declares Operation success.


# M0-009 backend bootstrap evidence

Generated bindings add `vault_status` and `vault_initialize`. Status reports the active canonical Vault path, default path, and startup-recovery completion without creating files. Initialization accepts an optional absolute selected directory (or uses the default), validates it through the existing Vault safety contract, installs scan/takeover/deployment/Activity services into the live runtime, and runs startup recovery before those services become available. A second initialization in the same process is rejected. `bootstrap_get_state` includes Vault initialization/path fields so first render can branch without inferring filesystem state.

# Command groups

Names are provisional but their responsibility is stable:

## Bootstrap and settings

```text
bootstrap_get_state() -> BootstrapState
vault_status() -> VaultStatusView
vault_initialize(InitializeVaultRequest) -> VaultSummary
vault_plan_relocate(RelocateVaultRequest) -> OperationPlanView
vault_verify() -> JobRef
settings_get() -> SettingsView
settings_update(UpdateSettingsRequest) -> SettingsView
adapters_list() -> Vec<AdapterView>
targets_list() -> Vec<TargetView>
```

## Workspace and scanning

```text
workspace_roots_list() -> Vec<WorkspaceRootView>
workspace_root_add(SelectedDirectoryRequest) -> WorkspaceRootView
workspace_root_update(UpdateWorkspaceRootRequest) -> WorkspaceRootView
workspace_root_remove(WorkspaceRootId) -> OperationPlanView | RemovalResult
project_add(SelectedDirectoryRequest) -> ProjectView
scan_start(ScanRequest) -> JobRef
scan_cancel(JobId) -> CancelResult
scan_get(JobId) -> ScanRunView
```

Removing a Workspace Root removes future authorization and indexed coverage settings; it does not delete project files. If domain metadata mutation needs review, it uses a plan even though no external content is deleted.

## Library and detail

```text
library_list(LibraryQuery) -> Page<LibraryItem>
skill_get(SkillId) -> SkillDetail
skill_preview_file(SkillId, SafeBundleRelativePath) -> TextPreview
skill_reveal(SkillId) -> Result
```

File preview accepts a validated Bundle-relative path and enforces containment in Rust.

## Planning and Operations

```text
operation_plan(OperationIntent) -> OperationPlanView
operation_execute(OperationId, PlanDigest) -> OperationRef
operation_cancel(OperationId) -> CancelResult
operation_get(OperationId) -> OperationView
operation_plan_export(OperationId) -> SaveResult
operation_plan_undo(OperationId) -> OperationPlanView
```

`OperationIntent` contains domain IDs and explicit choices. It contains no caller-supplied final path. Execute cannot alter the persisted plan.

## Deployments, Activity, and Trash

```text
deployments_list(DeploymentQuery) -> Page<DeploymentItem>
activity_list(ActivityQuery) -> Page<ActivityItem>
trash_list(TrashQuery) -> Page<TrashItem>
```

Deploy, undeploy, takeover, Trash, restore, and permanent delete all enter through `operation_plan` intents.

# Core read models

## `LibraryItem`

- Skill or external observation group ID;
- display/deployment name and source summary;
- authoritative ownership and lifecycle;
- digest/validation state;
- duplicate/name-conflict summary;
- deployment count and worst/aggregate health;
- changed time and next allowed actions.

M0 omits fake update/audit/Collection values rather than returning dead placeholder states.

## `SkillDetail`

- stable identity and readable path/provenance;
- `SKILL.md` text preview and Bundle entry summary;
- observations and conflict evidence;
- deployments with individual health and drift direction;
- Snapshots, Activity, Trash/undo availability;
- allowed actions and reasons disabled.

## `OperationPlanView`

- Operation ID/digest/expiry/type;
- concise consequence summary;
- grouped affected paths with action, before/after, mode, and target;
- blockers, warnings, irreversible consequences, and recovery points;
- whether execution is currently allowed;
- machine-readable steps for details, not editable inputs.

## `DeploymentItem`

- Skill, adapter, Target/project, scope, mode, target path;
- health label, explanation, expected/current evidence, last verified time;
- allowed resolution actions.

# Events

Events report progress or tell queries to refresh; they are not durable truth:

| Event | Payload | UI action |
| --- | --- | --- |
| `scan-progress` | job ID, phase, completed/estimated units, current display path | Update transient progress; periodically refetch scan run. |
| `operation-progress` | operation ID, phase, completed/total steps, cancelable | Update operation screen; do not mark success. |
| `domain-invalidated` | revision number plus scopes/IDs | Invalidate listed TanStack queries. |
| `operation-terminal` | operation ID only | Refetch Operation, Activity, Library, and Deployments. |
| `recovery-required` | operation ID and severity | Refetch recovery state and present persistent attention UI. |

Events contain bounded summaries, not entire Library tables or Skill content.

# TanStack Query keys

```text
['bootstrap']
['settings']
['adapters']
['targets']
['workspace-roots']
['scan', jobId]
['library', normalizedFilters]
['skill', skillId]
['deployments', view, normalizedFilters]
['activity', normalizedFilters]
['trash', normalizedFilters]
['operation', operationId]
```

Mutation handlers invalidate from the Rust-provided scope. Optimistic state is limited to harmless local UI preferences; ownership, filesystem result, and health are never optimistic.

TanStack Query over generated bindings is the sole authoritative frontend server-state layer. React local state/context owns transient view state. M0 introduces neither Redux nor Zustand. Events may optimize progress presentation but always invalidate/refetch authoritative read models.

# Component foundation

M0 UI uses React Aria Components, CSS Modules, and CSS design tokens rather than a heavy visual component suite. It follows system light/dark appearance and supports reduced motion and increased contrast.

# Error envelope

```text
AppErrorView
├── code
├── title
├── message
├── retryable
├── operation_id?
├── display_path?
├── recovery_action?
└── details_token?
```

Stable code families include:

- `invalid_input`, `not_found`, `permission_denied`;
- `unsafe_path`, `unsupported_bundle`, `name_collision`;
- `stale_plan`, `operation_busy`, `cancelled`;
- `io_failure`, `database_failure`, `verification_failed`;
- `rolled_back`, `recovery_required`.

User-facing errors state what changed, what did not, and the next safe action. Internal causal chains remain in local logs and are retrieved by `details_token`; credentials/content are not embedded in the envelope.

# Directory selection

Vault, Workspace, manual project, and custom target paths originate from the native directory picker. Rust still validates selected paths, nesting, canonical identity, and intended role. A plain string typed by web content cannot authorize a mutation root.

# UI state requirements

- Library is usable while scans progress and distinguishes stale/incomplete coverage.
- Plan review remains visible while an Operation runs.
- Partial failure/recovery is a persistent page/state, not a disappearing toast.
- Buttons/actions are driven by Rust-provided capabilities and blocker reasons.
- All statuses include text/icon labels and work through keyboard and accessible list alternatives.

# Related concepts

- [System context](../architecture/system-context.md)
- [Identity and state](../domain/identity-and-state.md)
- [Operation model](../domain/operation-recovery-and-trash.md)
- [Testing and acceptance](../quality/testing-and-acceptance.md)
