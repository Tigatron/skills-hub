# Skills Hub Technical Wiki

This directory is the OKF v0.1 knowledge bundle for Skills Hub technical design and implementation planning. The M0 technical baseline is accepted; product behavior remains governed by [PRD v0.1](../PRD-v0.1.md).

## Recommended reading paths

- **Understand the system:** [Architecture](architecture/index.md) → [Domain](domain/index.md) → the relevant workflow.
- **Implement a task:** [M0 plans](plans/index.md) → the task entry → its linked design pages → [Traceability](traceability.md).
- **Review safety:** [Mutation transactions](workflows/transaction-execution.md) → [Filesystem safety](security/filesystem-safety.md) → [Testing](quality/testing-and-acceptance.md).
- **Build UI or IPC:** [Application boundary](interfaces/tauri-and-ui-state.md) → [Domain state](domain/identity-and-state.md).

## Knowledge areas

- [Architecture](architecture/index.md) - system context, ownership boundaries, runtime modules, and dependency rules.
- [Domain](domain/index.md) - identities, ownership, deployment health, Operations, snapshots, Activity, and Trash semantics.
- [Storage](storage/index.md) - transparent Vault layout, SQLite index, manifests, canonical hashing, and immutable objects.
- [Workflows](workflows/index.md) - scanning, takeover, deployment, journaling, rollback, and reconciliation.
- [Interfaces](interfaces/index.md) - target adapter contract and typed Tauri/UI boundary.
- [Security](security/index.md) - M0 filesystem threat model and path-safety contract.
- [Quality](quality/index.md) - test strategy, performance, accessibility, and M0 acceptance verification.
- [Implementation plans](plans/index.md) - M0 delivery sequence and executable task breakdown.

## Cross-cutting references

- [M0 traceability matrix](traceability.md) - PRD requirement → design concept → implementation task → acceptance coverage.
- [Bundle update log](log.md) - chronological documentation changes.
- [Product strategy](../../PRODUCT.md) - product purpose, personality, and design principles.
- [PRD v0.1](../PRD-v0.1.md) - agreed scope and requirements.

## Bundle status

The M0 architecture, domain, storage, workflow, interface, security, quality, and traceability contracts are `accepted`. Tasks `M0-001`–`M0-014` are complete with recorded implementation evidence after the M0-014 acceptance rework; `M0-015`–`M0-017` remain planned.
