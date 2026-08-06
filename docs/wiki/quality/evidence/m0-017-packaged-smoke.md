---
type: Quality Evidence
title: M0-017 Packaged Smoke
description: Disposable-HOME launch of the packaged .app plus headless thin-slice (scan→takeover→deploy→undeploy).
status: implemented
tags: [skills-hub, m0, packaging, smoke]
requirements: []
timestamp: 2026-08-06T02:55:13Z
---

# M0-017 packaged-app smoke

## Environment

| Field | Value |
| --- | --- |
| Date (UTC) | 2026-08-06 |
| App | `/Users/terrylan/Development/apps/skills-hub/src-tauri/target/release/bundle/macos/Skills Hub.app` |
| Product | Skills Hub |
| Bundle ID | `com.terrylan.skillshub` |
| Version | 0.1.0 |
| Minimum macOS | 14.0 |
| Binary | /Users/terrylan/Development/apps/skills-hub/src-tauri/target/release/bundle/macos/Skills Hub.app/Contents/MacOS/skills-hub: Mach-O 64-bit executable arm64 |
| Signing | ad-hoc (see package-build evidence); never disable Gatekeeper |
| Thin-slice exit | 0 |

## First-run launch

- Disposable `HOME` via `mktemp`.
- Process start under isolated HOME: **ok**.
- Default Vault path story: `~/Library/Application Support/Skills Hub/Vault under the process HOME`.

## Thin slice

Headless domain path (same services the UI commands invoke):

```text
scan six roots (no mutation) → takeover Add to Vault → deploy symlink + Managed Copy
→ collision no-write → drift/broken verify → undeploy → Activity outcomes
```

Result: **pass** (see acceptance JSON produced alongside this run).

## Launch timing residual

| Field | Value |
| --- | --- |
| Samples (ms) | [83, 82, 96] |
| p50 / p95 | 83 / 94 |
| PRD gate | < 1500 ms warm launch to usable Library |
| Method | process-start under disposable HOME (Library paint requires GUI automation) |
| Result | proxy-pass-process-start |

Process-start is a conservative lower bound for cold process bring-up. Full Library-paint timing needs GUI automation or Instruments; when only process-start is available it is labeled as a proxy, not a false Library-paint pass.

## Commands

```bash
bash scripts/m0-package.sh
SKILLS_HUB_PERF_LAUNCH=1 bash scripts/m0-packaged-smoke.sh
```
