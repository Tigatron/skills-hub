---
type: Quality Evidence
title: M0-016 Manual Accessibility Checklist
description: macOS VoiceOver and native-integration checklist template with recorded automated coverage and packaging-smoke residuals for M0-016.
status: implemented
tags: [skills-hub, m0, accessibility, voiceover]
requirements: []
timestamp: 2026-08-06T00:00:00Z
---

# M0-016 Manual accessibility checklist (macOS)

Template and recorded results for native integration that the web harness cannot faithfully simulate. Automated keyboard/focus and static a11y checks live in `src/app/*test.tsx` and must stay green; this page records VoiceOver, system appearance, and window-chrome evidence.

## Environment template

| Field | Value |
| --- | --- |
| Date | _YYYY-MM-DD_ |
| Machine | _e.g. Apple M4 / 24 GB_ |
| macOS | _version_ |
| Build | release / dev (`pnpm dev`) |
| App path | _if packaged_ |
| Tester | _name_ |
| VoiceOver | on / off for each section |

## Automated coverage (do not re-test manually unless regression)

- Keyboard flow: scan → takeover → plan → deploy → verify → undeploy → Trash → restore (`src/app/keyboard-workflow.test.tsx`).
- Listbox/option semantics, StatusPill text+icon, LoadingBlock live region (`LibraryPanel`, `components` tests).
- Focus return after Operation dismiss (`OperationPanel` test).
- Deployments matrix/list keyboard activation (`DeploymentsPanel` test).
- 900×600 shell floor (`global.css` / `thin.module.css`).
- `prefers-reduced-motion` and `prefers-contrast: more` token/CSS rules.

## Manual checklist

Mark each row `pass` / `fail` / `n/a`. A fail on recovery or destructive confirmation is a release blocker.

| # | Check | Result | Notes |
| --- | --- | --- | --- |
| 1 | VoiceOver: Primary nav announces Library / Deployments / Activity / Trash / Settings |  |  |
| 2 | VoiceOver: Library listbox items announce name, validation status text (not color alone), and selection |  |  |
| 3 | VoiceOver: Deployment matrix headers and health cells are readable |  |  |
| 4 | VoiceOver: Operation plan review, Execute, Cancel, Dismiss are reachable and labeled |  |  |
| 5 | VoiceOver: Destructive permanent-delete confirmation requires typed name and is announced |  |  |
| 6 | VoiceOver: Recovery-required Activity detail and recovery path copy controls are reachable |  |  |
| 7 | Keyboard-only thin slice on real app (not only unit tests): scan → takeover → deploy → verify → undeploy → Trash → restore |  |  |
| 8 | Visible focus ring on nav, list options, primary/secondary/danger buttons |  |  |
| 9 | Focus returns to a sensible control after Dismiss / cancel / dialog close |  |  |
| 10 | System light appearance |  |  |
| 11 | System dark appearance |  |  |
| 12 | Increase Contrast (System Settings → Accessibility → Display) |  |  |
| 13 | Reduce Motion |  |  |
| 14 | Larger text / display zoom keeps layout usable at ≥900×600 |  |  |
| 15 | Window resized to 900×600 remains usable (no clipped primary actions) |  |  |
| 16 | Long path copy/reveal still keyboard reachable |  |  |
| 17 | Native directory picker (Vault / Workspace / target) opens and returns focus |  |  |
| 18 | Finder reveal from PathText (when allowed) |  |  |

## Launch-to-usable-Library (env-gated)

PRD gate: warm launch < 1.5 s at reference scale.

| Field | Value |
| --- | --- |
| Method | Packaged release `.app` cold start with pre-built reference Vault; stopwatch or Instruments time-to-first-Library paint |
| App path |  |
| Samples (ms) |  |
| p50 / p95 |  |
| Pass |  |
| Notes | Not runnable in pure unit CI. Set `SKILLS_HUB_PERF_LAUNCH=1` and `SKILLS_HUB_PERF_APP` when a packaged binary exists (M0-017). |

## Recorded results (2026-08-06)

| Field | Value |
| --- | --- |
| Date | 2026-08-06 |
| Machine | Apple M4 / 24 GB RAM |
| macOS | 26.6 (25G72) |
| Build | release library gates + debug renderer tests |
| Tester | M0-016 agent (automated + template) |

Automated rows 1–6 and 8–9 are **pass** via component/integration tests. Native VoiceOver and system Settings toggles (rows 1–6 under real VO, 10–18) remain **manual-before-packaging** for M0-017; no automated blocker is open.

Launch-to-usable-Library is **deferred to packaged smoke in M0-017** with the method above; backend warm-path gates are recorded in [m0-016-perf.json](m0-016-perf.json).
