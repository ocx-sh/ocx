# Renaming a page `.md` to `.mdx`

A page needs `.mdx` only to use a component. Do the rename as its own pure-rename commit (`chore(docs):`), no content change, so the diff proves the page did not move.

1. `git mv <page>.md <page>.mdx`.
2. Comment syntax: `<!-- ... -->` becomes `{/* ... */}` (the `doc_type` / `doc_tier` lines and link-group comments).
3. Heading ids: `{#id}` becomes `\{#id\}`.
4. Includes: `<<< @/_scripts/x.sh{sh}` becomes `\<\<\< @/_scripts/x.sh\{sh\}`.
5. One-line `<details><summary>...</summary>` is split over two lines; MDX rejects the one-line form.
6. Inbound link targets: every `...page.md` link in other pages becomes `...page.mdx` (grep the old file name across `website/src/content/docs`).
7. Generated reference pages: the link strings in `crates/ocx_schema/src/docs/render.rs`, then `task website:reference:generate` (`task website:reference:check` must pass).
8. The page inventory row in `.claude/artifacts/docs_ia.md` (old path to new path).
9. Lint tests that name page paths: `git grep -n "<page>.md" -- test website crates` (`test/lint/test_deprecated_spellings.py`, `test/lint/test_doc_project_toolchain.py` did).
10. Never add an `old-urls.txt` line or a redirect: the URL does not change.
11. Check: built text, ids and links identical before and after (`task website:build`, `task website:check`).
