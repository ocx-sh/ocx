# Research: Doc-Script Display-Render Layer + Region Unification

> Date: 2026-05-17 · Tier: high · Author: swarm-plan (worker-researcher, persisted by orchestrator)
> Grounds: ADR `adr_tested_doc_command_mechanism.md` Decision D/G,
> `design_spec_doc_command_scripts.md` §1.3/§6b. Companion to
> `research_vitepress_transclusion_cast_cost.md`.
> Drives: `plan_doc_display_render.md`.

## Problem

Published `test/doc_scripts/*.sh` are copied **verbatim** → `website/src/_scripts/<slug>.sh`,
transcluded `<<< @/_scripts/<slug>.sh{sh}` (whole file). Reader sees shebang +
`# state:`/`# doc:`/`# title:`/`# description:` header + `set -euo pipefail` +
literal `$PKG_CMAKE` (never expanded — runtime-only under provisioned StateProvider).

## Axis 1 — VitePress 2.0-alpha region syntax (DEFINITIVE)

Source: `vuejs/vitepress` `src/node/markdown/plugins/snippet.ts` `findRegion()`; CHANGELOG; GH #4625.

- Shell/`#` region regex: **start `#\s*#?region`**, **end `#\s*#?endregion`**.
  Therefore the design-spec marker **`# region cast` / `# endregion cast`
  already satisfies the VitePress shell region grammar with zero syntax change.**
- `<<< @/file.sh#cast{sh}` is valid — region name + `{lang}` coexist. Order:
  `path#region{lang opts}[title]`.
- Marker lines themselves are **excluded** from rendered output.
- 2.0-alpha.4: end marker no longer needs the region name (`# endregion` ok).
  No region-syntax break 1.x→2.0-alpha. alpha.17 `<!-- @include -->` break does
  NOT affect `<<<`.
- **Silent-fallback trap (GH #4625, unfixed, PR #5014 open):** a `<<<` referencing
  a **missing** region returns the **entire file** — no error/warn. A region-name
  typo would dump the full script (headers, assertions, `$PKG_*`) into the page.
- Line-range form `{3,7-10}` works but is fragility-equivalent (silent wrong
  content on line shift). Named region is correct for evolving files.

**Implication:** native region selection solves header/scaffolding stripping for
**free** (no publish-time strip engine for that concern). Cast region == display
region == one marker. Must guard the silent-fallback trap on the verify path.

## Axis 2 — Prior-art: run/display projection

| Tool | Run/display split | Variable render | Applies to OCX `.sh` |
|---|---|---|---|
| rustdoc | `# ` prefix = hidden+run; others shown | none | No (bash `#` = comment no-op) |
| mdBook `{{#include f:anchor}}` | `ANCHOR:`/`ANCHOR_END:` region; non-anchor omitted | none | Yes — structurally == `# region` |
| mdBook `{{#rustdoc_include}}` | anchor shown + `#`-prefix others hidden+run | none | No (Rust target) |
| tesh | none (display = run); `...` wildcard | shell env + `...` | No (no split) |
| cram | `  $ cmd` run+display; `(re)`/`(glob)` on expected | pattern in expected spec | Partial — informs `# expect:` goldens |
| pytest-examples | `#>` inline expected; `--update-examples` write-back | none (literal; auto-update) | No (Py); write-back model informs `# expect:` |
| rust-skeptic | rustdoc `# ` prefix | none | No (Rust target) |

No prior-art tool both (a) splits display vs run **and** (b) substitutes a fixture
variable for a clean display token. That second half is OCX-specific and unsolved
by borrowing — it requires either an authoring constraint or a render pass.

## Axis 3 — OCX-specific tension (orchestrator finding, beyond researcher scope)

The researcher recommended **R1**: cast-region lines use *literal* canonical
values (`ocx package install uv:0.10`), keep `$PKG_*` only outside the region
(assertions). This makes display trivial and avoids a render pass.

**R1 conflicts with SP7 + memory `feedback`/prefix-isolation lesson.** The drift
gate runs the **whole body**, including the cast region, under the provisioned
StateProvider where repos are **UUID-prefixed for parallel isolation** (SP7;
`SetupAdapter.provision` injects `t_<8hex>_`). A literal `cmake` in an *executed*
region line does **not** resolve under `-n auto`. Doc scripts deliberately use
`$PKG_*` projected vars precisely for this (recorded lesson: "Doc scripts must use
`$PKG_*`/`$REPO_*` projected vars, never hardcoded repo names"). So R1's
literal-value approach is **infeasible for any `$PKG_*` script** without breaking
parallel-isolated drift tests — i.e. for essentially every package-touching
walkthrough.

Therefore the viable path is **R2 (publish-time render with variable
expansion)** — which is exactly the user's locked decision. The researcher's two
objections to R2 are **dissolved**, not ignored:

- *"Couples state provider → website (PT6)"* → resolved by exporting a
  `display_env` map **through the existing JSON seam** (`task
  test:doc-scripts:list`); the website never imports `test/`.
- *"Publish must not provision (SP0)"* → resolved by a **static, zero-I/O
  accessor**: the canonical short name (`cmake`, `uv`) is **static metadata
  declared in the setup/scenario definition**; only the UUID prefix is runtime.
  A new `StateProvider.display_env()` derives `{PKG_CMAKE: "cmake", …}` from
  declared names with **no `provision()`, no registry I/O** — SP0 preserved
  (must be enforced by an import/no-network test, like SP0 today).

## Recommendation (feeds the architect)

1. **Region unification = reuse `# region cast` as the single display+cast
   selector** (zero new grammar; VitePress reads it natively). Decide whether a
   generic alias (`# region doc`) is also accepted for **display-only, non-cast**
   scripts, or whether all displayed multi-step scripts simply carry a cast
   region. Prefer one marker name; avoid a second grammar unless a real
   display-only-without-cast case exists.
2. **Render layer = R2, minimal:** publish strips header/shebang/`set -euo
   pipefail` and expands `$VAR`/`${VAR}` via `display_env` from the seam.
   Region-scoped when a region marker is present (whole-body-minus-header
   otherwise). Tested file untouched (three projections: tested full body real
   env / published display / cast region replay).
3. **Seam schema extension:** add `display_env: dict[str,str]` (+ `state`,
   `cast_region`, `title`, `description` as needed) to `DocScriptExportEntry`
   and its hand-mirrored `_DocScriptExportEntry`; add a parity gate so the two
   TypedDicts cannot drift (closes the existing manual-sync coupling risk).
4. **Guard the VitePress silent-fallback trap:** extend NC2 — every `<<<
   …#region{sh}` reference must resolve to an existing region in the published
   file; fail `task verify`, not just `website:build`.
5. **Drift/equivalence gate (closes Codex Finding 1 / `project_doc_cast_two_tree_drift`):**
   a verify-path static gate asserting that for a shared `# doc:` slug the
   doc-script render and the cast region are content-equivalent (or that one
   source backs both). Resolve whether recordings scripts converge onto
   `test/doc_scripts/` (one tree) or stay separate with an equivalence gate —
   architect decision; one-tree convergence is the stronger fix.
6. **Post-hardening signal (out of scope):** pytest-examples `--update-examples`
   write-back model → a future `task test:doc-scripts:update-goldens` for
   `# expect:` golden regeneration.

## Sources

- VitePress `snippet.ts` (raw main), Markdown Extensions docs, CHANGELOG —
  region regex, alpha deltas
- GH vuejs/vitepress #4625 (+ PR #5014) — missing-region silent full-file fallback
- rustdoc doc-tests; mdBook `ANCHOR`/`rustdoc_include`; tesh; cram `(re)/(glob)`;
  pytest-examples `--update-examples`; rust-skeptic — run/display prior art
- Cross-refs: `adr_tested_doc_command_mechanism.md`,
  `design_spec_doc_command_scripts.md`, `project_doc_cast_two_tree_drift`
  (memory), SP0/SP7 in `design_spec` §3
