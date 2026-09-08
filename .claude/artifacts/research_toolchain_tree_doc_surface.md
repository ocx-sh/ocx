# Research: Toolchain Tree Rename — Documentation & Cast Surface Enumeration

## Metadata

**Date:** 2026-09-07
**Domain:** packaging / cli / testing
**Triggered by:** rendered toolchain tree layout change — `{root}/{.gitignore, bin/, <group>/<entry>/}` → `{root}/{.gitignore, active -> shells/default/, links/<group>/<entry>/, shells/<name>/bin/}`
**Expires:** on merge of the tree-rename implementation PR (superseded by the actual diff at that point)

## Summary

Exhaustive, read-only enumeration of every documentation, cast, generator-script, and
Rust-source-comment site that spells or diagrams the toolchain tree's on-disk shape. No
files were edited; this is an input to the doc-writer and to the implementation review, not
a patch.

**Totals per surface:**

| Surface | Count |
|---|---|
| Website doc pages (`.md`) carrying live path prose or diagrams | 17 distinct sites across 11 files |
| `.cast` recordings embedding the old tree in recorded terminal output | 9 / 39 |
| `test/doc_scripts/*.sh` generator scripts with a literal old-path assertion | 2 (drive 1 of the 9 casts) |
| Rust source files with `///`/`//!`/documentation-bearing `//` comments naming the shape | 25 files, 112 lines |
| Generated JSON schemas mirroring those Rust comments | 4 files — no direct fix needed (see Section 6) |
| Design-invariant accuracy issue found | 1 (`configuration.md:1670-1684` — see Section 1) |

## Section 1 — The `configuration.md:1670-1684` invariant

**File:** `website/src/docs/reference/configuration.md:1670-1684`

```
1670  Three names are reserved, and the namespace differs:
1672  | Reserved | Namespace | Why |
1674  | `bin` | group name **and** binding name | the rendered toolchain's launcher directory, a sibling of every `<group>/` |
1680  `bin` is reserved on both namespaces so a per-group `<group>/bin/` layout stays
      available later without a break in the tree's shape. Its refusal names the
      scope it was found in:
1683  error: [group] name 'bin' is reserved; `bin` is reserved for a future per-group launcher directory
```

**The invariant asserted:** `bin` is reserved as both a group name and a binding name for
one stated reason — today it is "a sibling of every `<group>/`" at the toolchain root
(`{home}/bin/`, `{home}/{group}/`). The reservation is framed as sufficient, by itself, to
let a future per-group `<group>/bin/` sub-layout ship later "without a break in the tree's
shape."

**Correction to my own prior analysis, per team-lead review of the source:** I previously
claimed `links`, `shells`, and `active` would also need reserving as group names under the
new layout. **That claim is wrong.** Group and binding names become path components of
`links/<group>/<entry>` — one level *below* the tree's own directory names, not siblings of
them. A group literally named `links` renders at `links/links/<entry>` and collides with
nothing; the same holds for a group named `shells` or `active`. Retracted.

**What remains true, and is a real accuracy issue:** the two things the paragraph actually
states are both falsified by the rename, independent of the retracted claim above:

1. The stated rationale — "`bin` is... a sibling of every `<group>/`" — is no longer true.
   `bin/` moves under `shells/<name>/bin/` and is no longer a root-level sibling of anything
   `<group>` names.
2. The forward-looking promise — reserving `bin` lets a future per-group `<group>/bin/`
   layout "stay available... without a break in the tree's shape" — is moot, because the
   tree's shape is breaking now, by a different route (top-level restructuring) than the one
   this paragraph anticipated. The shipped refusal error string (line 1683) also still reads
   "reserved for a future per-group launcher directory," a forward reference to a layout this
   rename does not deliver and whose continued relevance under the new shape is now an open
   question, not a documentation typo.

This section needs a rewrite of the rationale, not just a path substitution — flag for
whoever owns the config-parsing implementation before the doc-writer edits prose here, since
the paragraph's claim about *why* the reservation exists no longer parses under the new tree.

## Section 2 — Complete checklist (rows 1-44; row 38-41 is a single merged row deferring to Section 5)

| # | File | Line | Exact quoted text (trimmed) | Kind | Fix |
|---|---|---|---|---|---|
| 1 | `website/src/docs/in-depth/storage.md` | 253-278 | `<Tree>` component: `{home}/` → `.gitignore`, `bin/` (with `cmake`/`cmake.exe`/`cmake.exec` children), `{group}/{entry}` | ascii-tree | rewrite |
| 2 | `website/src/docs/in-depth/storage.md` | 280 | "`bin/` holds the trampolines that make `activate = \"bin\"` work: one `PATH` entry... covers the default group only" | prose-mechanism | rewrite |
| 3 | `website/src/docs/in-depth/storage.md` | 282 | "`<group>/<entry>` is the link lane. Each entry is a directory link to a package root..." | path-literal | rewrite |
| 4 | `website/src/docs/in-depth/storage.md` | 284-286 | link-repair/link-lane prose ("Every composing emitter repairs this lane...") | prose-mechanism | rewrite |
| 5 | `website/src/docs/in-depth/storage.md` | 288 | "A toolchain link takes no `refs/symlinks/` back-reference..." | prose-mechanism | verify-only (still true of `links/<group>/<entry>`, just re-anchor prose) |
| 6 | `website/src/docs/in-depth/storage.md` | 290-294 | "Where a project's tree lives" — `<project>/.ocx/toolchain/`, `<root>/<project-key>/toolchain/` | path-literal | verify-only (top-level home path unaffected; only what's *inside* changes — cross-check) |
| 7 | `website/src/docs/in-depth/storage.md` | 296-307 | render-stamp section: "the `bin/` entry set with each file's identity, and the default group's links" | prose-mechanism | rewrite |
| 8 | `website/src/docs/in-depth/storage.md` | 28-29 | `<Node name="toolchain/">` in the top-level `~/.ocx` tree, description "the global tier's rendered toolchain tree" | ascii-tree | verify-only (no sub-path spelled, likely fine as-is) |
| 9 | `website/src/docs/reference/command-line.md` | 1966-1978 | "Toolchain render" table: `bin/<name>`, `<group>/<entry>/`, `.gitignore` | path-literal | rewrite |
| 10 | `website/src/docs/reference/command-line.md` | 585 | "`<home>/toolchain/` holds `bin/` trampolines and `<group>/<entry>` links" (in `ocx clean`'s scope note) | path-literal | rewrite |
| 11 | `website/src/docs/reference/command-line.md` | 2340-2346 | session-PATH registration: "`$OCX_HOME/toolchain/bin` — the global toolchain's trampolines" | path-literal | rewrite |
| 12 | `website/src/docs/reference/command-line.md` | 2596-2598 | shell-start PATH prepends: `<OCX_HOME>/toolchain/bin` ahead of the symlinks bin dir | path-literal | rewrite |
| 13 | `website/src/docs/reference/command-line.md` | 461, 496 | link-lane degrade prose: "`<group>/<entry>` link is absent... degrades the whole composition" | prose-mechanism | rewrite |
| 14 | `website/src/docs/reference/env-composition.md` | 128-135 | activate×pinned matrix rows: `<home>/toolchain/bin`, `<group>/<entry>` links | ascii-tree/table | rewrite |
| 15 | `website/src/docs/reference/env-composition.md` | 139, 151, 157 | "`$OCX_HOME/toolchain/bin`... on `PATH` in every row" / render-stamp gate / PATH ordering list | path-literal | rewrite |
| 16 | `website/src/docs/reference/env-composition.md` | 163-171 | "A trampoline composes at call time" — `<home>/toolchain/bin/` code fence | path-literal + code-example | rewrite (path in prose; fence body itself is location-independent, verify-only) |
| 17 | `website/src/docs/reference/env-composition.md` | 184 | "`bin` mode the interactive shell has `toolchain/bin` prepended" | path-literal | rewrite |
| 18 | `website/src/docs/reference/configuration.md` | 1656 | "become path components of the rendered toolchain tree — `<home>/<group>/<entry>/`" | path-literal | rewrite |
| 19 | `website/src/docs/reference/configuration.md` | 1670-1684 | reserved-names table + prose | prose-mechanism | **see Section 1 — design question, not a mechanical rewrite** |
| 20 | `website/src/docs/reference/configuration.md` | 1736, 1760 | `activate="bin"` doc: "Only `<home>/toolchain/bin` goes on `PATH`" / "`$OCX_HOME/toolchain/bin` is a session-level directory" | path-literal | rewrite |
| 21 | `website/src/docs/reference/environment.md` | 719-720, 838, 853-885 | `OCX_TOOLCHAIN_DIR`/`OCX_TOOLCHAIN_ACTIVATE`: "`$OCX_HOME/toolchain/bin`", "moves where the `<group>/<entry>` links and `bin/` trampolines live" | path-literal | rewrite |
| 22 | `website/src/docs/user-guide.md` | 45, 105-106 | session-PATH bullet + shell snippet: `~/.ocx/toolchain/bin` | path-literal | rewrite |
| 23 | `website/src/docs/user-guide.md` | 383-419 | "Tools on PATH, nothing else composed" — `activate = "bin"`, `<home>/toolchain/bin/cmake`, embeds `toolchain-bin-mode.cast` | prose-mechanism + cast-output | rewrite (cast in Section 3) |
| 24 | `website/src/docs/user-guide.md` | 397-399 | "`$OCX_HOME/toolchain/bin` on `PATH` and no global environment envelope" | path-literal | rewrite |
| 25 | `website/src/docs/user-guide.md` | 421-450 | "Reach what no profile reaches" — session-path registration, `toolchain_home + "/bin"` jq recipe | path-literal | rewrite |
| 26 | `website/src/docs/user-guide.md` | 551-569 | "Keep rendered toolchains out of the checkout" — `<project>/.ocx/toolchain/` fills with "launcher trampolines and directory links" | prose-mechanism | rewrite |
| 27 | `website/src/docs/user-guide.md` | 637-639 | embeds `toolchain-activation.cast` | cast-output | re-record (Section 3) |
| 28 | `website/src/docs/user-guide.md` | 682-684 | "resolve a tool through the toolchain's rendered `<group>/<entry>` link" (reproducibility caveat) | path-literal | rewrite |
| 29 | `website/src/docs/in-depth/shell-integration.md` | 65 | "the ocx installation's `bin` directory and `$OCX_HOME/toolchain/bin`" | path-literal | rewrite |
| 30 | `website/src/docs/in-depth/shell-integration.md` | 80-95 | "the project's `<home>/toolchain/bin` (`bin` mode only)"; render-stamp gate | path-literal | rewrite |
| 31 | `website/src/docs/in-depth/shell-integration.md` | 175, 226-230 | `ocx shell state` sample: `toolchain: /work/acme/api/.ocx/toolchain`; `toolchain_home` field doc | path-literal | verify-only (field name/value unaffected; interior shape prose may need rewording) |
| 32 | `website/src/docs/in-depth/shell-integration.md` | 232 | embeds `toolchain-state.cast` | cast-output | re-record (Section 3) |
| 33 | `website/src/docs/in-depth/shell-integration.md` | 18, 20, 22, 119 | embeds `adding-a-package.cast`, `cd-into-project.cast`, `cd-out-of-project.cast`, `inert-to-consented.cast` | cast-output | re-record (Section 3) |
| 34 | `website/src/docs/installation.md` | 59 | "toolchain bin first: `~/.ocx/toolchain/bin`, then `~/.ocx/symlinks/...`" | path-literal | rewrite |
| 35 | `website/src/docs/authoring/entry-points.md` | 34 | "leaves `bin/` private" — package's own `bin/`, a different concept | prose-mechanism | verify-only — confirm not conflated |
| 36 | `website/src/docs/reference/command-line.md` | 447, 688 | embeds `reference/command-line/pinned.cast` (twice; shared with `user-guide.md`) | cast-output | re-record (Section 3) |
| 37 | `website/src/docs/in-depth/lazy-loading.md` | 16 | embeds `lazy-loading/lifecycle.cast` | cast-output | re-record (Section 3) |
| 38-41 | Rust source, 25 files | — | doc-comment / module-doc prose | doc-comment | rewrite — full enumeration in Section 5, supersedes an earlier undercounted summary (13 files / 99 lines) I sent by message before this file existed |
| 42 | `test/doc_scripts/user-guide__toolchain-bin-mode.sh` | 17 | `export PATH="$PWD/.ocx/toolchain/bin:$PATH"` | cast-generator | rewrite (drives cast in Section 3) |
| 43 | `test/doc_scripts/user-guide__bin-mode-render.sh` | 14-15 | `[[ -x .ocx/toolchain/bin/cmake ]]` / error message naming the same path | cast-generator | rewrite |
| 44 | `website/src/public/schemas/{config,project,reports,execution-record}/v1.json` | — | mirrored path prose in `"description"` fields | doc-comment (generated) | no direct fix — see Section 6 |

## Section 3 — Casts requiring re-recording (9/9)

Confirmed by grepping recorded event bytes inside each `.cast` file's JSON, not just
filenames or directory names — 9 of the 39 total casts under `website/src/public/casts/`
hit.

| Cast file | Generating script | Embedding doc page(s) |
|---|---|---|
| `casts/in-depth/shell-integration/adding-a-package.cast` | `test/doc_scripts/shell-integration__adding-a-package.sh` | `in-depth/shell-integration.md:18` |
| `casts/in-depth/shell-integration/cd-into-project.cast` | `test/doc_scripts/shell-integration__cd-into-project.sh` | `in-depth/shell-integration.md:20` |
| `casts/in-depth/shell-integration/cd-out-of-project.cast` | `test/doc_scripts/shell-integration__cd-out-of-project.sh` | `in-depth/shell-integration.md:22` |
| `casts/in-depth/shell-integration/inert-to-consented.cast` | `test/doc_scripts/shell-integration__inert-to-consented.sh` | `in-depth/shell-integration.md:119` |
| `casts/in-depth/shell-integration/toolchain-state.cast` | `test/doc_scripts/shell-integration__toolchain-state.sh` | `in-depth/shell-integration.md:232` |
| `casts/user-guide/toolchain-activation.cast` | `test/doc_scripts/user-guide__toolchain-activation.sh` | `user-guide.md:639` |
| `casts/user-guide/toolchain-bin-mode.cast` | `test/doc_scripts/user-guide__toolchain-bin-mode.sh` (literal `export PATH="$PWD/.ocx/toolchain/bin:$PATH"` at line 17; sibling assertion script `user-guide__bin-mode-render.sh:14-15`) | `user-guide.md:419` |
| `casts/reference/command-line/pinned.cast` | `test/doc_scripts/command-line__pinned.sh` | `reference/command-line.md:447` **and** `user-guide.md:688` (same cast, embedded twice) |
| `casts/lazy-loading/lifecycle.cast` | `test/doc_scripts/lazy-loading__lifecycle.sh` | `in-depth/lazy-loading.md:16` |

**Caveat:** `website/src/_scripts/**/*.sh` is a separate, non-symlinked copy of
`test/doc_scripts/` — confirmed by a byte-level diff on `toolchain-bin-mode.sh`, which
differs between the two trees. Fix `test/doc_scripts/` first; the regeneration path that is
supposed to refresh the `website/src/_scripts/` copy was not identified in this pass and
should be confirmed before assuming it self-heals.

## Section 4 — `activate = "bin"` (config value) adjacent to `bin/` (directory)

Corruption-risk sites — places where a careless find/replace on the literal token `bin`
would corrupt the *unchanged* config-value spelling `activate = "bin"` while trying to fix
the *changed* directory path:

- `website/src/docs/user-guide.md:383-419` — "Tools on PATH, nothing else composed" section;
  nearly every sentence interleaves the config value and the directory.
- `website/src/docs/reference/configuration.md:1730-1760` — `activate` key reference; line
  1736 reads `Only \`<home>/toolchain/bin\` goes on \`PATH\`` directly under the enum table
  that defines `"bin"` as a value.
- `website/src/docs/reference/env-composition.md:128-165` — the activate×pinned matrix has
  `bin` as a row label in column 1 and `<home>/toolchain/bin` as literal cell content in
  column 3 of the same table row.
- `website/src/docs/reference/configuration.md:1670-1684` — worst case, see Section 1: the
  config value's reservation and the directory's shape are asserted in the same paragraph.

## Section 5 — Rust source doc-comment sites (full enumeration: 25 files, 112 lines)

**Correction to the "13 files / 99 lines" figure I gave in an earlier message** (sent before
this file existed): that count only searched `///` item docs. It missed `//!` module docs —
the highest-value sites, being top-of-file architecture comments — and one plain `//` line
that documents the same rationale inline. Corrected figure: **25 files, 112
documentation-bearing lines** (101 `///` item docs + 10 `//!` module docs + 1 plain `//`
line in `ocx_shim/src/main.rs:143`). A separate ~60-line bucket of test-fixture string
literals and inline `//` implementation comments across roughly 15 of these same files is
code/test-owned, not documentation, and is not enumerated below.

| File | Lines | What it asserts |
|---|---|---|
| `crates/ocx_lib/src/file_structure/toolchain_store.rs` | 12(`//!`), 63(`//!`), 168, 302, 456, 621, 984 | Canonical Rust-side tree spec — `//!` module doc includes `└── <group>/<entry>/    directory link (junction on Windows) to a package root`, mirroring `storage.md`'s `<Tree>` 1:1 |
| `crates/ocx_lib/src/file_structure.rs` | 57, 92 | Module-level doc: "launcher trampolines under `bin/` and one `<group>/<entry>` directory..." — sibling canonical spec to the above |
| `crates/ocx_lib/src/package_manager/tasks/render_toolchain.rs` | 4(`//!`), 13(`//!` ascii-tree), 108(`//!`), 242, 269, 295, 314, 415, 645, 687, 1394, 1504, 1833, 2107, 2187, 2539, 3059, 3956, 4758, 4989, 5781, 6563, 6640, 7420, 7563 | The renderer module itself — most load-bearing file for the actual code change; module doc at top plus 22 item docs on link-writing, repair, pruning |
| `crates/ocx_lib/src/activation.rs` | 275, 280, 283, 1489, 2389, 2425, 2441, 2815, 2909, 3665, 4312 | Per-prompt PATH-ordering contract; lines 2909/3665 embed a planted-file attack scenario naming `.ocx/toolchain/bin/cmake` — security-relevant |
| `crates/ocx_cli/src/command/self_group/activate.rs` | 667, 672, 923, 965, 1698, 1709, 3869, 3870, 4253 | CLI-side mirror of the same PATH-ordering contract (`ocx self activate`) |
| `crates/ocx_lib/src/package_manager/composer.rs` | 718, 1128, 1139, 1151, 1159, 1193, 7856, 7949, 8300, 8343 | Env-composition link-following: `<home>/toolchain/<group>/<entry>` and `<home>/<group>/<entry>` spellings |
| `crates/ocx_lib/src/package_manager/launcher/body.rs` | 20(`//!`), 170, 216, 308, 1167 | Trampoline-body module doc: "a rendered `<home>/toolchain/bin/<name>` gets"; collision-resolution order |
| `crates/ocx_lib/src/setup/session_path.rs` | 7(`//!`), 9(`//!`), 271, 624 | Module doc: "`$OCX_HOME/toolchain/bin` and the ocx install `bin` directory... never reaches a session PATH by ocx's own hand" |
| `crates/ocx_lib/src/env.rs` | 176, 191, 421, 5190, 5736 | `pinned`-flag doc; "resolution-affecting" note on `<home>/toolchain/<group>/<entry>` |
| `crates/ocx_lib/src/activate.rs` | 9(`//!`), 11(`//!`), 57, 147 | The `Activate`/`Pinned` enum's own canonical doc — source of the schema mirror in Section 6 |
| `crates/ocx_lib/src/setup.rs` | 424, 437, 1023 | Session-PATH registration ordering ("`$OCX_HOME/toolchain/bin` leads") |
| `crates/ocx_lib/src/record/execution_record.rs` | 1028, 1096, 1888 | Execution-record doc: `<home>/toolchain/<group>/<entry>/…` on PATH, `which::which_in` note |
| `crates/ocx_lib/src/project/config.rs` | 182, 198 | `ocx.toml` `activate`/`pinned` key docs |
| `crates/ocx_lib/src/file_structure/state_store.rs` | 339 | Render-stamp JSON key format `"<group>/<entry>"` → digest root |
| `crates/ocx_lib/src/oci/client/builder.rs` | 98 | Builder doc referencing trampoline construction |
| `crates/ocx_lib/src/package_manager/mutate.rs` | 257 | "Render `<home>/toolchain/` as a whole-compose pass" |
| `crates/ocx_shim/src/core.rs` | 205, 385, 400, 602 | Shim's own resolution-order/security doc: "write access to `<home>/toolchain/bin`, which is owner-only" |
| `crates/ocx_shim/src/main.rs` | 143(plain `//`), 1322, 2583 | Line 143 is a plain `//` comment (missed on first pass), not `///`; documents the same stderr-message rationale. 1322/2583 are `///` docs on `CreateProcessW` search order |
| `crates/ocx_cli/src/app.rs` | 369, 854 | Root `--help` doc text — clap-derived, these lines are literally rendered as `ocx --help` output |
| `crates/ocx_cli/src/command.rs` | 171 | Shared command doc: "one link per `<group>/<entry>`, plus the `bin/`..." |
| `crates/ocx_cli/src/command/toolchain_exec.rs` | 506, 507 | `ocx exec` target directories doc |
| `crates/ocx_cli/src/command/self_group/setup.rs` | 91, 776 | Doc comments on session registration — plus line 803, a `--help`/error string literal (not a doc comment) naming the same path; user-visible text, flagged separately |
| `crates/ocx_cli/src/command/self_group.rs` | 33 | Windows registry-value doc |
| `crates/ocx_cli/src/options/pinned.rs` | 31, 45, 89 | `--pinned` flag semantics doc |
| `crates/ocx_cli/src/api/data/shell_state.rs` | 342 | `shell state` API doc on the `pinned` JSON field |

## Section 6 — Generated JSON schemas

`website/src/public/schemas/{config,project,reports,execution-record}/v1.json`:

| File | Generated by `task schema`? | Checked in? | Mirrored text |
|---|---|---|---|
| `website/src/public/schemas/config/v1.json` | Yes — `task schema:default` (`website/taskfile.yml:7,42` → `schema.taskfile.yml`) | No — gitignored (`website/.gitignore:19`, `src/public/schemas/`); `git ls-files` returns empty | `description` field only (`toolchain-dir` property doc) — property *name* is unchanged by this rename |
| `website/src/public/schemas/project/v1.json` | Yes, same task | No, same gitignore rule | `description` field only (`Activate::Bin` variant doc: "Put `<home>/toolchain/bin` on `PATH`...") |
| `website/src/public/schemas/reports/v1.json` | Yes, same task | No, same gitignore rule | `description` field only (`toolchain_home` field doc, duplicated at two schema roots in this file) |
| `website/src/public/schemas/execution-record/v1.json` | Yes, same task | No, same gitignore rule | `description` field only (references "a project toolchain" — generic, not path-literal) |

All four are confirmed **not load-bearing**: the mirrored text lives exclusively inside
JSON Schema `"description"` strings, sourced verbatim from the Rust `///`/`//!` doc comments
on `Activate`/`Pinned` in `crates/ocx_lib/src/activate.rs` and the `toolchain_home` /
`toolchain-dir` fields elsewhere (Section 5). The schema *property names* themselves
(`"toolchain-dir"`, `"toolchain_home"`) are untouched by this rename. **No direct edit is
needed** — fix the Rust source in Section 5, then rerun `task schema:generate` /
`task schema:default`; hand-editing these four files is wrong per `subsystem-website.md`
("Never edit generated files — build pipeline overwrites").

## Section 7 — Patterns searched

- `grep -rn "toolchain"` over `website/src/docs/**/*.md` (all 43 files enumerated, 26 hit),
  `website/src/index.md`, `website/src/public/data/catalog/packages/ocx/cli/README.md`,
  `website/.vitepress/config.mts` + `theme.index.mts` + `theme/index.mts` +
  `theme/components/*.vue` (0 hits), `website/src/_scripts/**` (2 hits), `README.md` /
  `CONTRIBUTING.md` (both false-positive: Python/uv toolchain, unrelated), `.claude/rules/**`
  (37 files, not itemized — rule prose about the abstract concept, not the tree shape),
  `.claude/artifacts/**` (~100 files hit — historical ADRs/plans, out of scope for "fix"
  since they are point-in-time record, not summarized per-file here), `.github/workflows/**`
  + `.github/actions/**` + `taskfiles/**` (all false-positive: Rust compiler toolchain /
  `rust-toolchain.toml`), `crates/**/*.rs` (101 files hit on the bare word; narrowed below).
- `grep -rn -F` (fixed-string, after an alternation-based regex silently mis-parsed by this
  host's emulated `rg` — corroborates the known proxy `rg`-emulation issue) for:
  `toolchain/bin`, `<home>/toolchain`, `<group>/<entry>`, `.ocx/toolchain`,
  `$OCX_HOME/toolchain`, `~/.ocx/toolchain`, `.gitignore` (tree-root marker).
- `grep -rn "├──\|└──"` across `website/src/docs/**` for ASCII tree diagrams — found the
  `storage.md` `<Tree>` component (Vue, not literal ASCII) plus unrelated ASCII trees in
  `command-line.md` (dependency-closure and index-layout diagnostics) and `indices.md`
  (index file layout) — ruled out as false positives by reading surrounding context.
- `grep -o "toolchain[^"\\]*"` on every `.cast` file's raw bytes (not filename) to find
  literal recorded-output path strings — 9/39 hit.
- Rust, in three passes (the first two under-scoped, corrected by the third):
  1. `grep -rn '^\s*///.*(toolchain/bin|<group>/<entry>|<home>/toolchain|toolchain\\bin)'`
     across `crates/**/*.rs`, excluding `/tests/` dirs — 101 lines across ~24 files
     (`///` item docs only; missed `//!` module docs and plain `//` comments).
  2. `grep -rn 'toolchain/bin|<group>/<entry>|<home>/toolchain' crates/ --include="*.rs" |
     grep -v '///'` to recover everything the first pass excluded — surfaced 60 additional
     lines, hand-separated into: `//!` module docs (10 lines, real documentation, added to
     the Section 5 total), one plain `//` line documenting rationale
     (`ocx_shim/src/main.rs:143`, added), and ~59 lines of test-fixture path literals /
     `format!` string bodies / inline implementation comments (excluded — code/test-owned).
  3. `git ls-files` / `git check-ignore -v` / `grep -n schema website/taskfile.yml` to
     establish JSON-schema generation and checked-in status for Section 6.
- `grep -n "bin"` filtered against `configuration.md` / `user-guide.md` /
  `env-composition.md` for the Section 4 config-value-vs-directory adjacency check.
