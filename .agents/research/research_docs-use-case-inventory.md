# Research: OCX docs use-case inventory

## Metadata

**Date:** 2026-10-11
**Domain:** cli
**Triggered by:** `.agents/discussions/docs-next-level.md`
**Expires:** 2027-01-11

## Direct Answer

40 reader goals (6 first-steps, 26 everyday, 8 integration), mapped to current pages
under `website/src/docs/`. Sources: `.claude/rules/product-context.md`, `faq.md`,
`user-guide.md`, `.claude/artifacts/analysis_issue_triage_2026-09-04.md` (live `gh`
sweep not run). Most goals are covered, but only as sections buried in the
1641-line `user-guide.md` or in `faq.md`.

Uncovered or partial clusters:
- Ecosystem consumers: Bazel `rules_ocx` (issues #191, #192), devcontainer/editor
  discovery, Python PEP 751 via `ocx_python` — none.
- Network hostility: proxy (#323), per-registry auth recipes (GHCR/ECR), non-interactive
  consent in CI (#400), offline/frozen CI cache — partial.
- Publisher CI: SBOM/provenance from release workflow (#108), bare `.gz`/`.tar.bz2`
  assets (#283, #284), pin package deps from project lock (#211) — partial/none.

Full table: the lane report (36–40 rows: goal | tier | audience | coverage | source)
is reproduced by re-running the lane prompt; seed it into `/docs-plan` user needs.
