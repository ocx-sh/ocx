State: handed-off → loop · Updated: 2026-09-28
Ratified: 2026-09-28 → loop
Confidence: ratified by owner (explicit yes); research vintages 2026-09-27 (research_code_comment_density.md), 2026-09-28 (recon, prior-art, community, archaeology lanes)

# Discussion: code-docs cleanup — comment density, ADR references, log-string tests

## Problem

Owner ask (verbatim): "use the /code-docs-cleanup and new code docs rules to
refactor our code documentation and adr references. You should reach 1:4-1:6
ratio. You may also want to reduce tests that alert specific string in log
outputs."

Prod Rust comment:code is ~0.5–1.2 per crate (census `--report`, 2026-09-28;
0.98 by the classifier in `.claude/artifacts/research_code_comment_density.md`).
Reference Rust repos cluster around 0.20 (1:5).

## Requirements

- Apply `.claude/rules/code-docs.md` (+ `.claude/rules/code-docs/*.md`) through
  the `code-docs-cleanup` skill procedure across `crates/**/*.rs`.
- 1:4–1:6 (0.17–0.25) is the **expected, reported outcome**, never a gate or a
  done-criterion that overrides LEN-04: no guard is deleted to hit a number.
  Per-crate ratio reported from `comment_census.py --report` before/after.
- ADR references: rationale prose moves out to (or already lives in) the ADR;
  code keeps at most one file-qualified pointer (`adr_x.md § Y`) where a
  non-obvious line needs it. Plan, WP, C-/S-/DX-, review-round IDs deleted.
- Tests asserting on log-output text are rewritten to assert behaviour (exit
  code, state, user-facing error), or deleted where the behaviour is already
  covered; log messages become free to reword. User-facing error-message
  asserts (e.g. `test/tests/test_exit_codes.py`) are *not* log asserts.
- `#N` issue refs: kept only where the line is a workaround tied to a
  still-open issue, written `owner/repo#N`; refs to closed/landed work deleted.
- Dangling artifact pointers deleted or re-pointed (3 of 16 dead at HEAD:
  `design_spec_ocx_python.md`, `plan_lazy_package_loading.md`, `plan_self_setup.md`).
- Adopt the checks **first**: `linkage_check.py ids --discover` with a
  verdict per prefix, commit `.code-docs-length.json` / `.code-docs-linkage.json`
  baselines, wire census `--check` + linkage `ids`/`pointers` into the uncached
  T0 lint tier (`task test:lint:structure` / `test/lint/`). Each cleanup WP
  lowers baselines; density cannot regrow.
- The ratchet also covers **test code**: Rust test modules/files and the
  Python acceptance suite (`test/**/*.py`, `--lang python`), baselined at
  current counts so test comments cannot regrow either. Gated only, not
  cleaned (see Out of scope).
- Every guard keeps constraint + consequence (GRD-01, LEN-05/06);
  `cleanup_check.py --base <branch base>` (CLN-01) runs per cleanup WP.
- Doc comments on clap / schemars types render into `--help` and JSON Schema
  (interface text): rationale moves out per `code-docs/surfaces.md`, and any
  change to rendered text is reviewed as a CLI-surface change.
- Work sliced per crate (heaviest first: `ocx_package_manager`, `ocx_config`,
  `ocx_index`, `ocx_announce`, `ocx_oci`, `ocx_sign`, …), file-disjoint for
  parallel worktrees.

## Out of scope

- *Cleaning* Rust test code comments/test names (already ~0.29) and Python
  acceptance tests (except log-string asserts) — they are ratcheted, not
  trimmed; taskfiles, scripts, website, `external/**`.
- `.claude/**` rule/skill text and ADR contents (ADRs only receive moved
  rationale where none exists yet).
- Any behaviour change in Rust code; `CHANGELOG.md`.

## Open questions

- [NEEDS CLARIFICATION: do the security crates (`ocx_oci`, `ocx_sign`,
  `ocx_trust`, `ocx_config`) get an opus-tier guard review per WP?]
  Recommended: yes — per CLAUDE.md model policy, guard loss there is a
  security regression.
- [NEEDS CLARIFICATION: moved rationale with no existing ADR home — new ADR
  section or research artifact?]
  Recommended: append to the nearest existing ADR; mint no new ADR for a
  comment move.

- [NEEDS CLARIFICATION: does `comment_census.py` separate prod/test for
  Rust and read `test/**/*.py` correctly? A quick `--report --scope test`
  returned the prod table unchanged.]
  Recommended: verify at adoption; if the census cannot scope test code,
  baseline test paths via a separate `--root`/config run rather than patching
  the vendored checks.

## Verification

- `comment_census.py --report` before/after per crate, recorded in the PR
  body; target band 0.17–0.25 reported, not gated.
- New T0 lint gates green and shown red on a planted regression (census
  `--check`, linkage `ids`, `pointers`) — for prod Rust, Rust test code
  and the Python suite each.
- `cleanup_check.py --base origin/main` clean (no flagged guard removal
  without replacement).
- `rust:doc:ratchet` not raised (no new broken intra-doc links).
- `task verify` green at finalize; `--help` snapshot / schema diffs reviewed.

## Related

- `.claude/rules/code-docs.md`, `.claude/rules/code-docs/{guards,length,linkage,routing,surfaces}.md`, `checks/`
- `.claude/skills/code-docs-cleanup/SKILL.md`
- `.claude/artifacts/research_code_comment_density.md` (2026-09-27 benchmark)
- `test/tests/test_logging.py` (only clear log-line assert found by recon)

## Research

- Recon 2026-09-28: no `.code-docs*.json` baselines, no task/CI wiring of the
  checks — rule installed, not adopted. ~5.1k comment-anchored ID/ref hits in
  360 files; `#N` issue refs dominate over ADR/plan IDs. Heaviest files:
  `crates/ocx_package_manager/src/tasks/render_toolchain.rs`, `composer.rs`,
  `activation.rs`, `crates/ocx_index/src/ocx_index.rs`, `crates/ocx_config/src/{lib,env}.rs`.
  No Rust `tracing_test`/`logs_contain` usage.
- Prior art 2026-09-28: OSS comment density averages ~18.7% (Arafat & Riehle);
  no study shows a ratio *target* beats content rules; deleted "why" comments
  have reintroduced guarded bugs; log-text asserts are brittle, structured /
  behavioural asserts preferred.
- Community 2026-09-28: AI-grown codebases over-comment; team pattern = drop
  restatement, keep non-obvious constraints; `missing_docs` would red on `///`
  deletion (OCX enforces none — checked). Log-text asserts: delete once
  outcome coverage exists (streamarr-server#424 pruned 32).
- Archaeology 2026-09-28: one prior cleanup (43379e95c, narration only); no
  guard-loss regression in history; `test_logging.py` never churned by a
  log reword; 3/16 artifact pointers in code dangle.
