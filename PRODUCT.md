# Skills Hub

## Register

product

## Users

Skills Hub is initially built for one person: a developer who regularly uses several AI coding agents, accumulates Skills from many repositories, and works across multiple projects. They need to know which Skills exist, which copy is authoritative, where each Skill is deployed, whether deployed content has drifted, and whether an update is trustworthy.

The primary job is not discovering the largest possible catalog. It is safely owning and operating a personal Skill library without manually inspecting hidden directories or copying folders between agents.

## Product Purpose

Skills Hub is a local-first desktop library and distribution manager for Agent Skills. It discovers Skills from supported agents and authorized projects, brings selected Skills into a user-owned Vault, and deploys exact revisions to global or project-level targets with previews, provenance, drift detection, snapshots, and rollback.

The product succeeds when it replaces manual filesystem management while preserving user ownership. Downloading a Skill must not imply installing it. Scanning must never imply taking control. Deployment must be explainable and reversible.

## Brand Personality

**Restrained, trustworthy, efficient.**

The interface should feel calm under uncertainty, precise around destructive operations, and fast during routine work. It should help a technical user understand state without making them learn an invented package-management vocabulary.

Reference anchors:

- **Raycast** for native-feeling speed, keyboard efficiency, and compact actions.
- **Linear** for dense but legible state presentation and consistent interaction vocabulary.
- **Finder** for transparent file ownership, locations, and familiar filesystem concepts.

## Anti-references

- A neon “hacker tool” with terminal decoration used as personality.
- A marketplace made from an endless grid of identical promotional cards.
- An agent-first switcher where the same Skill appears as unrelated copies under separate product tabs.
- A dashboard dominated by vanity counts rather than actionable state.
- A proprietary package format or opaque database that prevents users from accessing their own files.
- One-click automation that silently overwrites, deletes, uploads, or deploys content.

## Design Principles

1. **The Skill is the asset.** Agents and projects are deployment destinations, not the primary navigation model.
2. **Show ownership before action.** Clearly distinguish external, vaulted, and managed content before offering mutation controls.
3. **Preview the consequence.** Imports, updates, deployments, and deletions expose their affected paths, conflicts, and recovery plan first.
4. **Prefer reversible operations.** Stage changes, snapshot important state, commit atomically, and make rollback obvious.
5. **Keep local files ownable.** Working Skills remain ordinary directories; metadata augments the filesystem rather than replacing it.
6. **State beats decoration.** Color, icons, and motion communicate selection, risk, drift, progress, and success—not atmosphere.

## Accessibility & Inclusion

- Target WCAG 2.2 AA for application surfaces.
- Support complete keyboard navigation, visible focus, and familiar macOS shortcuts.
- Never encode ownership, update, security, or deployment state using color alone.
- Respect reduced-motion and increased-contrast preferences.
- Use readable labels and status text alongside icons in dense tables and the deployment matrix.
- Ensure long Skill names, paths, repository names, translated copy, and large collections do not break layouts.
- Follow the macOS system light or dark appearance; neither theme is secondary.
