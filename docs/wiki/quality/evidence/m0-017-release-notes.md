---
type: Quality Evidence
title: M0-017 Release Notes
description: M0 scope, known limitations, packaging posture, and M1 boundary for the acceptance build.
status: implemented
tags: [skills-hub, m0, release]
requirements: []
timestamp: 2026-08-06T00:00:00Z
---

# Skills Hub M0 release notes (0.1.0)

## Scope

M0 is a **local-only macOS** Skill manager. A clean install can:

- create or select a transparent Vault under `~/Library/Application Support/Skills Hub/Vault`;
- scan the six supported global adapter roots without mutation;
- index authorized Workspace Roots with ignores and symlink-cycle handling;
- take external Skills into the Vault while leaving originals untouched;
- deploy by symlink (global default) and Managed Copy (Git project default);
- preview collisions and block writes until confirmation;
- recover from injected commit failures via rollback or retained recovery state;
- surface target drift and broken links in Deployments;
- undeploy, Trash, restore, and permanently delete with distinct behavior;
- show accurate Activity outcomes and recovery evidence;
- complete core workflows by keyboard.

## Package

| Field | Value |
| --- | --- |
| Product name | Skills Hub |
| Bundle ID | `com.terrylan.skillshub` |
| Version | 0.1.0 |
| Minimum macOS | 14.0 |
| Architecture | Apple Silicon (`arm64`) native |
| Default Vault | `~/Library/Application Support/Skills Hub/Vault` |
| Signing | **Ad-hoc** (`signingIdentity: "-"`). Not notarized. Not Developer ID. |
| Artifacts | `.app` and `.dmg` via `bash scripts/m0-package.sh` / `pnpm build` |

**Gatekeeper:** Do **not** disable Gatekeeper. Ad-hoc local packages are for developer machines and acceptance evidence. A future seam will add Developer ID signing and notarization; that work is out of M0.

## Network / account / telemetry

- No account, cloud sync, crash upload, or product telemetry.
- Core workflows run with network access unnecessary; product dependencies include no HTTP client crates.
- Official adapter source URLs are documentation metadata only and are not fetched at runtime in M0.

## Known limitations (honest residuals)

1. **Launch → usable Library paint:** process-start under disposable HOME is ~80–90 ms p95 on Apple M4; full WebView Library-paint timing still needs GUI automation or Instruments and is labeled a **proxy**, not a painted-Library claim. Backend reference-scale gates remain in [m0-016-perf.json](m0-016-perf.json).
2. **Live VoiceOver:** automated keyboard/a11y coverage is green; a full interactive VoiceOver pass on the packaged UI may require a human session—see [m0-017-voiceover.md](m0-017-voiceover.md).
3. **Signing / notarization / Universal Binary / Intel runtime:** not claimed. Source remains compile-checkable for `x86_64-apple-darwin` where tooling allows.
4. **Same-user hostile processes:** residual race capability is documented in the filesystem threat model; M0 detects and stops rather than claiming full sandbox isolation.
5. **DMG distribution to end users:** ad-hoc DMG is an engineering artifact, not a Gatekeeper-clean customer install path.

## M1 boundary (explicitly absent)

M0 navigation and code paths do **not** include:

- Discover / remote acquisition / skills.sh marketplace
- Collections, packages, Git backup, project lockfiles as product features
- In-app Skill editor
- Full static content security audit / quarantine / Trust Sheet
- Auto-update infrastructure
- Windows or Linux packaging

Extension seams may exist in architecture docs; they must not appear as half-built product surfaces in M0.

## How to reproduce the acceptance package

```bash
export PATH="/opt/homebrew/bin:$HOME/.local/bin:$HOME/.cargo/bin:$PATH"
eval "$(mise activate bash)"   # Node/pnpm via mise only
bash scripts/m0-package.sh
bash scripts/m0-acceptance.sh
SKILLS_HUB_PERF_LAUNCH=1 bash scripts/m0-packaged-smoke.sh
pnpm check
```

## Evidence index

- [m0-017-acceptance.md](m0-017-acceptance.md) — 12/12 criteria
- [m0-017-package-build.json](m0-017-package-build.json) — toolchain + identity
- [m0-017-packaged-smoke.md](m0-017-packaged-smoke.md) — disposable HOME smoke
- [m0-016-perf.json](m0-016-perf.json) / [m0-016-manual-a11y.md](m0-016-manual-a11y.md)
