# Plan — Decouple action-pin updates from the mirror drift guard

## Status
- State: in-progress
- Branch: feat/mirror-pipeline-describe
- Owner: AI
- Started: 2026-06-07

## Problem

Generated mirror workflows SHA-pin third-party actions, but the surface is owned
by the `ocx-mirror` binary via `verify-generated.yml` (`pipeline generate ci
--check`). Two consequences:

1. **Mirror repos**: a Renovate/Dependabot bump of `actions/checkout` in a
   committed `mirror.yml` re-renders the *old* baked pin → drift red. Renovate
   can never land an action bump downstream.
2. **Source repo**: Dependabot's `github-actions` ecosystem only scans
   `.github/**`; the baked templates under `crates/ocx_mirror/.../templates/*.yml`
   are invisible, so their pins rot. (Live proof: PR #146 bumps `.github/` only.)

Also: `ocx-sh/setup-ocx@v1` floats while every third-party action is SHA-pinned —
an undocumented asymmetry (`subsystem-ci.md §5` says "pin every action").

## Decisions (user, 2026-06-07)

- **B**: drift guard normalizes `uses:` refs — mirror repos own pins, guard
  polices logic + action identity only.
- **Renovate**: full migrate `.github/dependabot.yml` → `renovate.json`, incl. a
  customManager regex bumping the baked-template pins.
- **Pin setup-ocx too**: seed `ocx-sh/setup-ocx@8cd2f38eed07c9b79aaed22e61f9ba6b36e967a3  # v1.2.2`
  (v1 == v1.2.2 confirmed via gh api).

## Work breakdown

| # | Part | Surface |
|---|------|---------|
| 1 | Normalize `uses:` refs in `check_drift` (+ unit tests: tolerate bumped pin, still trip on logic/action-name change) | `crates/ocx_mirror/src/command/pipeline/generate/ci.rs` |
| 2 | SHA-pin `setup-ocx` in 3 templates; update 3 test assertions (602/940/1041) | `templates/{workflow,describe,verify-generated}.yml`, `ci.rs` |
| 3 | `renovate.json` (cargo/npm/actions groups + release.yml ignore + customManager); delete `dependabot.yml` | repo root, `.github/` |
| 4 | Docs/rules: `subsystem-mirror.md` R4, `subsystem-ci.md §5`, `rules.md` auto-load table (dependabot.yml→renovate.json), structural test `.claude/tests/test_ai_config.py`; website note if applicable | `.claude/**`, `website/**` |

## Verification

- `task rust:verify` (unit tests incl. new normalization tests)
- `task claude:tests` (AI-config structural drift)
- relevant pytest: `test/tests/test_mirror_pipeline.py`
- Renovate config validated against current docs (background research agent)
- Adversarial review workflow on the diff before finalize

## Notes

- Normalization: strip `@<ref>` through EOL on `uses: owner/action` lines; keep
  `owner/action` so swapping actions still trips drift. Apply to both on-disk and
  rendered content before compare.
- Renovate customManager: `datasource=github-tags`, keep SHA + `# vX` comment in
  sync. Exact syntax pending background research (managerFilePatterns vs
  fileMatch, named groups, autoReplaceStringTemplate).
