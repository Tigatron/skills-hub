---
type: Quality Evidence
title: M0-017 VoiceOver and native a11y residual
description: Live VoiceOver pass status for the packaged M0 build; records environment blockers honestly.
status: implemented
tags: [skills-hub, m0, accessibility, voiceover]
requirements: []
timestamp: 2026-08-06T00:00:00Z
---

# M0-017 VoiceOver / native accessibility residual

Template source: [m0-016-manual-a11y.md](m0-016-manual-a11y.md).

## Environment

| Field | Value |
| --- | --- |
| Date | 2026-08-06 |
| Machine | Apple M4 / 24 GB |
| macOS | 26.6 |
| Build | packaged release `.app` (`com.terrylan.skillshub` 0.1.0, ad-hoc) |
| Tester | M0-017 agent session |

## Automated coverage (still green)

- `src/app/keyboard-workflow.test.tsx` — scan → takeover → plan → deploy; Deployments verify/undeploy; Trash restore (3/3 pass in M0-017 acceptance run).
- Listbox/option semantics, StatusPill text+icon, LoadingBlock live region, focus return after Operation dismiss (component suites under `pnpm check`).
- Reduced-motion / increased-contrast CSS and 900×600 floor retained from M0-016.

## Live VoiceOver checklist

Interactive VoiceOver (VO keys + spoken output) **could not be fully executed** in this agent environment: no reliable hands-on control of the system VoiceOver rotor, spoken-output capture, or human confirmation of announcement wording on the packaged WebView. Faking a pass would violate the honesty rule.

| # | Check | Result | Notes |
| --- | --- | --- | --- |
| 1 | VO: Primary nav announces Library / Deployments / Activity / Trash / Settings | **blocked** | Requires live VO session on packaged app |
| 2 | VO: Library listbox name + validation status text | **blocked** | Same |
| 3 | VO: Deployment matrix headers/health | **blocked** | Same |
| 4 | VO: Operation plan Execute/Cancel/Dismiss | **blocked** | Same |
| 5 | VO: Permanent-delete typed confirmation | **blocked** | Same |
| 6 | VO: Recovery-required Activity detail | **blocked** | Same |
| 7 | Keyboard-only thin slice on real app | **partial** | Domain thin-slice + keyboard unit tests pass; full GUI keyboard path not agent-driven |
| 8 | Visible focus ring | **pass (automated)** | Component/CSS coverage |
| 9 | Focus return after dismiss/cancel | **pass (automated)** | OperationPanel test |
| 10–18 | System appearance / contrast / motion / picker / Finder | **not re-run** | No regression expected from packaging-only task; still manual |

## Release decision

- **Not a silent pass.** Criterion 12 closes on: automated keyboard workflow + static a11y component coverage + documented VO residual.
- A human VO sign-off on the packaged `.app` remains recommended before any external distribution beyond ad-hoc developer use.
- No recovery or destructive confirmation was found inaccessible in automated coverage; those remain release blockers if a human VO pass finds them missing.

## How a human completes the residual

1. Build: `bash scripts/m0-package.sh`
2. Open `src-tauri/target/release/bundle/macos/Skills Hub.app` (ad-hoc; do not disable Gatekeeper—use a dev machine that already trusts local builds, or wait for notarization).
3. Enable VoiceOver; walk rows 1–6 and 10–18 in [m0-016-manual-a11y.md](m0-016-manual-a11y.md).
4. Append results to this file.
