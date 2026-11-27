# DOC2 evidence (run 2): records, user pages and AI config

Branch `hex/bazel-rbe-rules-ocx--DOC2`, base `333a5264`. No V-row is proven here. C-143 is a review row, so each step's red is a search that lists missing or old text, and its green is the same search coming back clean.

## Step 1: records (C-143, DP-44), commit `89d9855a`

- Red: a fixed-string grep for each § 14 replacement text in its target record. 33 evidence lines were checked. 29 were already present. Four were missing:
  - `adr_toolchain_activation.md` C-079 (the v1 and v2 `active` target).
  - Two `adr_windows_exe_shim.md` sentences about reading `.shim` and dispatching on `.shimref`. Each lacked the appended "This describes the launchers already on disk…".
  - C-RO-01 and C-RO-14 of `system_design_vendor_rules_ocx.md` (DP-44).
- Green: the same greps hit after the edit. Every amendment that replaces text no longer finds its old text.
- C-RO-01 now admits `ocx --project <scratch>/ocx.toml update --no-pull …` and `ocx --index <dir> index update <identifier>`, but only inside the generated update program. C-RO-14 admits the same two forms, and only in the update program's template files. The RU step must make `scripts/check_ocx_interface.py` match.

## Step 2: user pages (DP-47), commit `6a104183`

- Red: the DP-47 search over `website/src/content/docs/` found 154 hits, 127 of them outside generated pages. The old vendored-layout names were among them, in `guides/vendor.mdx` and `reference/vendor-manifest.md`.
- Green: 77 hits remain. 75 are current terms (`trampoline`, `ocx clean`, `shells/default-<hex>`) and 2 are history notes, in `explanation/storage.mdx:152` and `explanation/toolchain-tree.mdx:57`. None is an old name.
- 24 pages were edited. No generated page was edited.
- Checks:
  - `test/lint/test_reference_shape.py`, `test_docs_quality_gate.py` and `test_doc_binding.py`: 55 passed.
  - `website/tests/shell-session.test.mjs` and `product-name.test.mjs`: 7 passed.
  - `prose.py` found no new sentence over 25 words.
- Not run: `website:build`, `website:lint:links`, and the node tests that need `@ocx-sh/theme`. There is no `node_modules` and no network.

## Step 3: AI config, commit carrying this file

- Edited: `subsystem-file-structure.md`, `subsystem-package-manager.md`, `subsystem-ci.md`, `arch-principles.md` and `product-context.md`.
  - In `subsystem-ci.md` § Windows Shim Build, the `launcher_blob_check.sh` case list now follows the feature branch. `classic-pinned-ocx` is gone, the record-kind cases are listed, and the per-target floor is stated. The section also has a `junction-proof` row (coordinator request after CI2).
- Red: the grep for old launcher names over `.claude/rules`, `.claude/skills`, `.claude/agents` and `AGENTS.md` found 27 hits. Green: in the five files, old names remain only in statements that older releases' files are never written.
- Checks: `.claude/tests` (`test_ai_config.py`, `test_hooks.py`, `test_workflows.py`) gave 678 passed, 21 skipped, 19 failed, both before and after. The 19 failures are the same test ids in both runs, all in the release and prune tests of `test_workflows.py`. `task claude:tests` itself was not run (uv needs network). The three files ran with the main checkout's venv.

## Left for other pipelines (outside DOC2's files)

- `website/src/data/glossary.json` (generates `reference/glossary.md`): `launcher`, `native-launcher` (no longer Windows-only) and `shim` (now a record of kind `deferred`) need rewording.
- Generated pages that I2 regenerates:
  - `reference/environment.md` needs the `__OCX_LAUNCHER_DEPTH` row.
  - `reference/command-line/package-consuming.md:40,459` still says `entrypoints/`.
  - `reference/command-line/commands.md:298,359,398,462` still says `shells/default/bin`.
  - In `reference/exit-codes.md`, the `launcher_path_not_utf8` and `launcher_unsafe_character` rows need review.
  - The `ocx clean` description lacks the `launchers` kind.
- The doc-script includes `website/src/_scripts/entry-points/*.sh` and `lazy-loading/*.sh` may print `entrypoints/` paths.
- Launcher stderr wording on the pages is generic ("exit 78 and names what it tried"). Re-read it once LA lands.
- AI config files outside the set that name old launcher files:
  - `AGENTS.md:85`: the `ocx_env_apply` row says "`.direct` launcher sidecar decoder".
  - `.claude/skills/docs/references/vocabulary.md:10`: names `render_stamp.json`.
  - `.claude/rules/subsystem-cli-commands.md:77`: the `entrypoints/` wording.
