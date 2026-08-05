# Skills Hub

Local-first library and distribution manager for Agent Skills.

The repository is implementing the accepted M0 design in [`docs/wiki`](docs/wiki/index.md).
M0 begins with a Tauri 2 desktop shell, a Rust-authored typed IPC boundary, and a small React
bootstrap screen backed by the real Rust runtime.

## Prerequisites

- macOS 14 or newer
- Xcode Command Line Tools
- Node.js 24.12
- pnpm 10.28
- Rust 1.89 with `rustfmt` and `clippy`

## Commands

```sh
pnpm install
pnpm bindings:generate
pnpm dev
```

Quality checks:

```sh
pnpm check
```

Build the renderer or native app separately:

```sh
pnpm build:renderer
pnpm build
```

M0 artifacts are local/ad-hoc builds. They are not Developer ID signed or notarized.
