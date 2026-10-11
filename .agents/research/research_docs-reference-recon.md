# Research: How OCX reference pages are produced today

## Metadata

**Date:** 2026-10-11
**Domain:** cli
**Triggered by:** `.agents/discussions/docs-next-level.md`
**Expires:** 2026-12-11 (codebase recon — re-verify before acting)

## Direct Answer

- All reference pages hand-written except `website/src/docs/reference/dependencies.md`
  (SBOM, `sbom:generate:page`).
- Structural lints against `crates/ocx_schema/tests/golden/cli.json`:
  `test/lint/test_doc_command_reference.py` (anchor + Options row per command/flag;
  floors 70/280/12) and `test/lint/test_env_reference_coverage.py` (env headings both
  ways, floor 50). Nothing binds `configuration.md`, `metadata.md`, `env-composition.md`.
- Lookup share (estimate): command-line ~40%, configuration ~25%, environment ~25%.
- Explanation sections: `configuration.md` "Project Configuration — ocx.toml"
  (:1890-2125, overlaps `in-depth/project.md`), "Future Config Keys" (:2145, speculative),
  "Discovery and Merge Precedence"/"Kill switch" (duplicate `in-depth/configuration.md`),
  `[[trust.policy]]` (~414 lines), `[shell]` (~352 lines). `environment.md` "Truthy
  Values", "Shell Activation Files", `__OCX_ENV_STATE`/`__OCX_ENV_PWD` (defers to
  `in-depth/shell-integration.md`), ~15 repeated `apply_ocx_config` paragraphs.
  `command-line.md` `--format`/json-documents overlaps `machine-interface.md`; `exec`
  restates `in-depth/project.md#running`.
- **The rule already exists, unenforced:** `.claude/rules/docs-quality/page-types.md`
  DOC-TYPE-04 (reference = description only), -05 (opinion only in explanation), -18
  (item set from code, proven by test), -19 (split past ~15 items). Only 2 of 46 pages
  declare `doc_type`; no ocx task runs `doc_declaration.py` / `page_type.py`.
- Site: VitePress 2.0.0-alpha.16, 46 pages under `docs/`, 15 code-groups in 9 files,
  21 Vue components. Docs readers: lychee, `test_doc_binding.py`,
  `test_doc_project_toolchain.py`, `test/doc_scripts/cast.bzl`, `test/BUILD.bazel:301`.
- Migration: `../ocx-website/.agents/plans/plan_phase3-pilots.md` excludes ocx docs
  (line 28). Theme 0.1.1; peers astro ^7, starlight 0.42.x. Port pins theme by git SHA
  (`pnpm add 'github:ocx-sh/website#<sha>&path:/packages/theme'` + `allowBuilds`);
  D13: no consumer merges while pinned to a SHA not on website `main`.
