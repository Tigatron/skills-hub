---
type: Interface Contract
title: Target Adapter Contract
description: Defines data-driven adapter descriptors, path resolution, mode capabilities, built-in targets, and custom directory behavior.
status: accepted
tags: [skills-hub, m0, adapters, targets]
requirements: [SCN-01, SCN-05, DPL-01, DPL-02, DPL-03, DPL-04, DPL-09]
timestamp: 2026-07-23T00:00:00Z
---

# Purpose

A Target Adapter describes where one agent family reads Skills and which filesystem deployment modes it can support. It does not scan, mutate, own an Operation, or transform Skill content in M0.

# Descriptor

```rust
struct AdapterDescriptor {
    id: AdapterId,
    version: u32,
    display_name: String,
    supported_platforms: Vec<Platform>,
    global_paths: Vec<PathTemplate>,
    project_paths: Vec<SafeRelativePath>,
    supported_scopes: ScopeSet,
    supported_modes: DeploymentModeSet,
    detection_hints: Vec<DetectionHint>,
    validation_notes: Vec<CompatibilityNote>,
    confidence: AdapterConfidence,
}
```

`AdapterId` is stable across display-name changes. Increment `version` when path resolution, compatibility, or deployment semantics change enough to require revalidation.

Initial confidence values are `Verified`, `Community`, and `Custom`. Confidence describes path-contract evidence, not Skill safety.

# Generic filesystem behavior

All six initial families use one generic filesystem adapter implementation:

- expand known macOS global paths;
- derive project target roots from safe relative paths;
- inspect immediate Skill child directories;
- support directory symlink and Managed Copy where declared;
- validate deployment names and Bundle shape;
- return compatibility notes for planning/UI.

Introduce code-specific adapter behavior only after verified agent requirements demand transformation or non-filesystem steps. No such behavior is assumed in M0.

# Initial descriptors

These PRD defaults are implementation inputs that `M0-011` must verify against current official sources:

| Adapter ID | Global path | Project path | M0 modes |
| --- | --- | --- | --- |
| `universal-agent-skills` | `~/.agents/skills` | `.agents/skills` | Symlink, Managed Copy, Copy fallback |
| `claude-code` | `~/.claude/skills` | `.claude/skills` | Symlink, Managed Copy, Copy fallback |
| `openai-codex` | `~/.agents/skills` | `.agents/skills` | Symlink, Managed Copy, Copy fallback |
| `cursor` | `~/.cursor/skills` | `.cursor/skills` | Symlink, Managed Copy, Copy fallback |
| `gemini-cli` | `~/.gemini/skills` | `.gemini/skills` | Symlink, Managed Copy, Copy fallback |
| `opencode` | `~/.config/opencode/skills` | `.opencode/skills` | Symlink, Managed Copy, Copy fallback |

`M0-011` verified the descriptors on 2026-08-05 against current official documentation:

| Adapter | Official source | Verification caveat |
| --- | --- | --- |
| Universal Agent Skills | [Agent Skills client implementation](https://agentskills.io/client-implementation/adding-skills-support) | `.agents/skills` is an interoperability convention; the Skill format specification does not mandate installation paths. |
| Claude Code | [Claude Code Skills](https://code.claude.com/docs/en/skills) | Supports personal/project paths, directory symlinks, and additional nested/project precedence behavior. |
| OpenAI Codex | [Codex Skills](https://developers.openai.com/codex/skills/) | Current official local paths are `.agents/skills`, not the earlier PRD `.codex/skills` draft; repository discovery also scans ancestors. |
| Cursor | [Cursor Agent Skills](https://cursor.com/docs/context/skills) | Also reads `.agents`, Claude, and Codex compatibility paths and supports recursive/nested discovery. |
| Gemini CLI | [Gemini CLI Agent Skills](https://geminicli.com/docs/cli/skills/) | Also reads `.agents/skills`, with that alias taking precedence within a scope. |
| OpenCode | [OpenCode Agent Skills](https://opencode.ai/docs/skills) | Also reads Claude/`.agents` compatibility paths and walks project ancestors to the Git worktree. |

A path override keeps the product usable if upstream conventions change. Descriptor versions must be incremented when path, compatibility, or deployment semantics require renewed validation.

# Path expansion

- Expand `~` from the current user's home directory in Rust; never accept shell expansion output.
- Path templates are data, not format strings evaluated by a shell.
- Project paths are validated relative paths with no root, prefix, empty, `.` or `..` component.
- A target root is a registered domain object with display and canonical identity, not a transient string.
- Missing default roots are valid scan coverage states and are not created during scanning.

# Target and mode resolution

A Target is one concrete adapter/scope/root tuple. The adapter declares supported modes; the planner selects the product default:

- global → symlink;
- Git project → Managed Copy;
- non-Git personal project → symlink;
- explicit user override → requested supported mode;
- proven link capability failure → proposed Copy fallback requiring renewed confirmation.

Adapters do not silently decide fallback during execution.

The M0-007 vertical slice established these rules through a versioned Universal fixture. M0-011 broadened the same generic path to all six production descriptors and custom targets. Registration records stable adapter/scope/root/project identity plus custom display/mode and selected-root filesystem identity; planning reopens that authority, probes write/rename/link behavior, and seals the result. A proven unsupported link produces a new Managed Copy plan with a fallback reason, while `Unknown` blocks.

# Custom target directories

Custom targets use the same generic filesystem behavior but require explicit directory selection. The user supplies:

- display name;
- concrete selected root;
- global or project scope label;
- supported/allowed mode preference;
- optional project association.

Rust validates and records canonical identity. If the selected directory is moved/replaced or authorization identity changes, mutation is blocked until the user reselects it. Custom targets cannot bypass containment, collision, plan, snapshot, or rollback rules.

# Overrides and detection

- Detection hints may inform setup but never authorize a path mutation.
- Users may disable a built-in adapter or override global/project roots.
- An override produces a configured Target tied to the same adapter version plus override metadata.
- Adapter version changes mark related deployments `Unverified` until revalidated; they do not rewrite paths automatically.

# Contract tests

Every descriptor fixture verifies:

- stable ID and version serialization;
- safe global expansion and project-relative path parsing;
- correct scope/mode declarations;
- missing root read-only behavior;
- path override and disabled state;
- global/project plan output for a temporary HOME/project;
- custom target containment and collision handling.

# Related concepts

- [Scanning and reconciliation](../workflows/scanning-and-reconciliation.md)
- [Takeover and deployment](../workflows/takeover-and-deployment.md)
- [Filesystem safety](../security/filesystem-safety.md)
