# Architecture

* [M0 system context](system-context.md) - component boundaries, system-wide invariants, lifecycle, and milestone boundary.
* [Rust runtime and module boundaries](runtime-and-modules.md) - module ownership, dependency direction, concurrency, and selected foundational libraries.

## Related areas

* [Domain](../domain/index.md) - states and invariants enforced by the architecture.
* [Interfaces](../interfaces/index.md) - contracts crossing the Rust/UI and adapter boundaries.
* [Filesystem safety](../security/filesystem-safety.md) - security constraints on infrastructure code.
