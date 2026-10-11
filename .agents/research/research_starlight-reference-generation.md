# Research: Generating CLI/env/config reference in Starlight

## Metadata

**Date:** 2026-10-11
**Domain:** web
**Triggered by:** `.agents/discussions/docs-next-level.md`
**Expires:** 2027-01-11

## Direct Answer

- `cli.json` is golden-only (`crates/ocx_schema/tests/golden/cli.json`,
  `crates/ocx_schema/src/main.rs:15`), not published. It carries command `summary`, one
  `help` per arg, env `summary` = first doc line only (`crates/ocx_schema/src/cli.rs:41-102,536`).
  No exit codes, long help or examples. A generator needs a richer projection (full
  `ocx_env` doc, long help, per-command exit codes) or direct Rust access.
- Options: (a) Astro content loader (object loader; `docs` collection with a custom
  loader undocumented; sidebar `autogenerate` only for `src/content/docs`), (b) a
  Rust/task/Bazel step writing Markdown (precedent: schemas genrule, SBOM page),
  (c) hand-written + lint (exists for env: `test/lint/test_env_reference_coverage.py`).
- Pagefind indexes built HTML regardless of source. Anchors: Astro 7 github-slugger,
  no native `{#id}`; 447 explicit anchors across reference pages. Default slug of
  `OCX_HOME` would be `ocx_home`, not `ocx-home` (inferred).
- No Starlight plugin for CLI schemas or JSON Schema (starlight-openapi, -typedoc only;
  cli-schema.org draft spec, not Starlight).

## Leads

- Sätteri vs unified for `{#id}` in `.md`/`.mdx`.
- How much of the 447 anchors a `cli.json` + `ocx_env` join covers.
- Whether Starlight 0.42 `docs` collection accepts a non-`docsLoader` loader.
