# Skills Hub Repository Instructions

## Product boundary

- Read `PRODUCT.md` for product principles and `docs/PRD-v0.1.md` for agreed product behavior.
- M0 is a local-only macOS Skill manager. Do not pull M1/M2 features such as remote acquisition, editing, security auditing, Collections, packages, Git backup, or project lockfiles into M0 unless the PRD is deliberately revised.
- Rust owns domain truth, filesystem mutation, deployment health, and operation outcomes. React may cache read models but must not independently infer those states.
- Scanning is read-only. Every filesystem mutation requires a reviewed Operation Plan, verified preconditions, a recovery point where needed, post-write verification, and a durable outcome record.
- Skill working content stays in ordinary files. SQLite stores indexes, relationships, and operation metadata, never Skill content blobs.

## Technical documentation is an OKF LLM Wiki

All technical designs and implementation plans MUST be maintained as the Open Knowledge Format (OKF) v0.1 bundle rooted at `docs/wiki/`. Follow the upstream specification at <https://github.com/GoogleCloudPlatform/knowledge-catalog/blob/main/okf/SPEC.md>.

### Bundle rules

- Each non-reserved Markdown file under `docs/wiki/` represents exactly one concept and starts with parseable YAML frontmatter.
- Every concept frontmatter MUST contain a non-empty `type`. Use `title`, `description`, `status`, `tags`, `requirements`, and `timestamp` when applicable.
- The concept ID is its stable bundle-relative file path without `.md`. Do not casually move or rename a page; update every incoming link when a move is necessary.
- `index.md` and `log.md` are reserved OKF files. An `index.md` is a concise, one-level catalog for progressive disclosure; `log.md` records dated documentation changes newest first.
- Use normal Markdown links to connect related concepts. Prefer links over copying the same decision into several pages.
- Keep one source of truth for each contract. A summary page should point to the owning concept rather than restating its details.
- New pages require an entry in the nearest `index.md`. Removed or superseded pages require repaired links and a `log.md` entry.
- Authored links must resolve even though an OKF consumer is required to tolerate broken links.

### Concept status

- `draft`: incomplete exploration; not safe to implement.
- `proposed`: complete enough for review; implementation may expose unresolved issues.
- `accepted`: approved implementation contract.
- `implemented`: reflects verified current behavior.
- `superseded`: retained for history and linked to its replacement.

Do not mark a concept `accepted` or `implemented` merely because an LLM wrote it. Acceptance is a product or engineering decision; implementation status requires checking the code and tests.

### LLM reading protocol

1. Start at `docs/wiki/index.md` instead of loading the entire Wiki.
2. Read only the indexes and concept pages relevant to the task, then follow their explicit links.
3. For implementation work, also read the linked PRD requirements and task entry.
4. Treat the Wiki as intended design and verify claims about implemented behavior against the code.
5. If code, PRD, and Wiki disagree, do not silently choose one. Preserve safe behavior, identify the mismatch, and update or escalate the appropriate source of truth.

### Design page expectations

A technical concept page should state, when relevant:

- scope and non-goals;
- decisions and rationale;
- invariants and ownership boundaries;
- data or interface contracts;
- state transitions and failure modes;
- security and recovery behavior;
- verification strategy;
- links to related concepts and PRD requirement IDs.

Implementation task pages must include a stable Task ID, PRD requirement IDs, linked design concepts, dependencies, deliverables, explicit exclusions, acceptance conditions, automated tests, risks/recovery, and parallelization notes.

### Keeping the Wiki current

- Update affected concept pages in the same change that alters an accepted architecture, schema, state machine, interface, filesystem contract, or task scope.
- Update `docs/wiki/traceability.md` when requirement or task ownership changes.
- Update the nearest indexes only when membership or summaries change; update `docs/wiki/log.md` for meaningful bundle changes.
- Record rationale and consequences, not an edit transcript.
- Never replace the linked Wiki with one monolithic `TECHNICAL_DESIGN.md`.
