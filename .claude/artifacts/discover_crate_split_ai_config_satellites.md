# Discover: AI-config satellites for the crate split

Read first: `.claude/artifacts/adr_crate_split_workspace.md` § "AI-config layout"
(`:627`), § "Stability tiers and the ecosystem contract" (`:406`), § "Phase 3 —
satellite pointer bumps" (`:782`); `.claude/artifacts/system_design_crate_workspace.md`
§ 1 (`:33`). All repo-tracked-file claims below verified read-only at `evelynn`
HEAD `d8750fd7`. `/usr/sbin/git` for git, `/usr/bin/grep` for grep (the shell's
`grep`/rtk wrapper gave false positives on substring matches during this scan —
every count below is `/usr/bin/grep`-verified). No files edited.

---

## Part 1 — AI-config surfaces naming a crate path (this repo)

### 1. `.claude/rules/*.md` with `crates/…` in `paths:` frontmatter

14 files (matches ADR's count):

| File | `paths:` list (verbatim) |
|---|---|
| `arch-principles.md:3-4` | `crates/**/*.rs`, `external/**/*.rs` |
| `quality-cli-help.md:3` | `crates/ocx_cli/src/**` |
| `subsystem-cli-api.md:3-4` | `crates/ocx_cli/src/api/**`, `crates/ocx_cli/src/command/**` |
| `subsystem-cli-commands.md:3` | `crates/ocx_cli/src/command/**` |
| `subsystem-cli.md:3` | `crates/ocx_cli/src/**` |
| `subsystem-deps.md:3-6` | `Cargo.toml`, `crates/*/Cargo.toml`, `deny.toml`, `.licenserc.toml` |
| `subsystem-file-structure.md:3-6` | `crates/ocx_lib/src/file_structure/**`, `crates/ocx_lib/src/file_structure.rs`, `crates/ocx_lib/src/reference_manager.rs`, `crates/ocx_lib/src/symlink.rs` |
| `subsystem-metadata-schema.md:3-5` | `crates/ocx_lib/src/package/metadata/**`, `crates/ocx_schema/**`, `website/src/docs/reference/metadata.md` |
| `subsystem-oci.md:3-5` | `crates/ocx_lib/src/oci/**`, `external/rust-oci-client/**`, `external/sigstore-rs/**` |
| `subsystem-package-manager.md:3-4` | `crates/ocx_lib/src/package_manager/**`, `crates/ocx_lib/src/package_manager.rs` |
| `subsystem-package.md:3-4` | `crates/ocx_lib/src/package/**`, `crates/ocx_lib/src/package.rs` |
| `subsystem-script.md:3` | `crates/ocx_lib/src/script/**` |
| `workflow-bugfix.md:3-8` | `"crates/**"`, `"test/**"`, `"website/**"`, `".claude/**"`, `"Cargo.toml"`, `"Cargo.lock"` |
| `workflow-refactor.md:3-8` | `"crates/**"`, `"test/**"`, `"website/**"`, `".claude/**"`, `"Cargo.toml"`, `"Cargo.lock"` |

**Body-text occurrences of `crates/ocx_lib` / `ocx_lib::`** (count per file,
`quality-cli-help.md` = 0, `arch-principles.md`/`workflow-bugfix.md`/
`workflow-refactor.md` = 0):

| File | Count | Lines |
|---|---|---|
| `subsystem-cli.md` | 7 | 65, 242, 275, 336, 393, 401 (+ line 53 with `ocx_lib::setup::apply_managed_config`) |
| `subsystem-file-structure.md` | 5 | 3–6 (frontmatter), 52 |
| `subsystem-metadata-schema.md` | 5 | 3, 10, 18, 24, 55 |
| `subsystem-script.md` | 4 | 3, 8, 32, 85 |
| `subsystem-cli-api.md` | 3 | 33, 56, 67 |
| `subsystem-package-manager.md` | 3 | 3, 4, 9 |
| `subsystem-package.md` | 3 | 3, 4, 9 |
| `subsystem-cli-commands.md` | 2 | 134, 176 |
| `subsystem-oci.md` | 2 | 3, 10 |
| `subsystem-deps.md` | 1 | 85 |

Verbatim body lines (subsystem files), each naming `ocx_lib::<module>` a
per-crate agent must re-target:

- `subsystem-cli-api.md:33` — "Output is split across three `ocx_lib::cli` structs."
- `subsystem-cli-api.md:56` — "`ocx_lib::ci::CiFlavor::export`"
- `subsystem-cli-api.md:67` — "`emit_lines` wraps `ocx_lib::shell::Shell::export_path` / `export_constant`."
- `subsystem-cli-commands.md:134` — "`OCX_NO_MODIFY_PATH` is read through `ocx_lib::env::flag` + `BooleanString`"
- `subsystem-cli-commands.md:176` — "shares `ocx_lib::setup::apply_managed_config`"
- `subsystem-cli.md:53` — "Both call `ocx_lib::setup::apply_managed_config`"
- `subsystem-cli.md:65` — "Read through `ocx_lib::env::flag` + `BooleanString`."
- `subsystem-cli.md:242` — "Library half lives in `ocx_lib::ci` (`CiFlavor`, …"
- `subsystem-cli.md:275` — "(`ocx_lib::ci::annotations`) — …"
- `subsystem-cli.md:336` — "one shared ladder, `ocx_lib::ladder::Ladder<T>`"
- `subsystem-cli.md:393` — "`ocx_lib::env::keys::CREDENTIAL_KEYS` carries the same notes"
- `subsystem-cli.md:401` — "add it to `ocx_lib::env::keys::CREDENTIAL_KEYS`"
- `subsystem-file-structure.md:52` — "These modules sit at `crates/ocx_lib/src/` root — consumed across subsystems."
- `subsystem-metadata-schema.md:10,18,24` — `crates/ocx_lib/src/package/metadata/` (format location, table, change-trigger)
- `subsystem-metadata-schema.md:55` — "`ProjectEnv`** (`crates/ocx_lib/src/project/env.rs`)"
- `subsystem-oci.md:10` — "OCI registry client, index management, identifiers, platform matching at `crates/ocx_lib/src/oci/`."
- `subsystem-package-manager.md:9` — "Task impls at `crates/ocx_lib/src/package_manager/`."
- `subsystem-package.md:9` — "…at `crates/ocx_lib/src/package/`."
- `subsystem-script.md:8,32,85` — `crates/ocx_lib/src/script/` (rule scope, per-file convention, import-path ban)
- `subsystem-deps.md:85` — "the engine-isolation test (`crates/ocx_lib/src/script` firewall) before merge."

### 2. `.claude/rules.md` catalog — rows naming `crates/…` / `ocx_lib`

**"By subsystem" table** (`:76-82`, verbatim):
```
| OCI registry/index | [subsystem-oci.md](./rules/subsystem-oci.md) | `crates/ocx_lib/src/oci/**`, `external/rust-oci-client/**`, `external/sigstore-rs/**` |
| Storage/symlinks | [subsystem-file-structure.md](./rules/subsystem-file-structure.md) | `crates/ocx_lib/src/file_structure/**` |
| Package metadata | [subsystem-package.md](./rules/subsystem-package.md) | `crates/ocx_lib/src/package/**` |
| Package manager | [subsystem-package-manager.md](./rules/subsystem-package-manager.md) | `crates/ocx_lib/src/package_manager/**` |
| CLI commands/API | [subsystem-cli.md](./rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
| Script host API | [subsystem-script.md](./rules/subsystem-script.md) | `crates/ocx_lib/src/script/**` |
```
**"By auto-load path" table** (`:92-103`, verbatim rows naming `crates/`):
```
| `**/*.rs` | … (+ [arch-principles.md](./rules/arch-principles.md) under `crates/**`, `external/**`) |
| `Cargo.toml`, `crates/*/Cargo.toml`, `deny.toml`, `.licenserc.toml` | [subsystem-deps.md](./rules/subsystem-deps.md) |
| `crates/ocx_lib/src/oci/**`, `external/rust-oci-client/**`, `external/sigstore-rs/**` | + [subsystem-oci.md](./rules/subsystem-oci.md) |
| `crates/ocx_lib/src/file_structure/**`, `file_structure.rs`, `reference_manager.rs`, `symlink.rs` | + [subsystem-file-structure.md](./rules/subsystem-file-structure.md) |
| `crates/ocx_lib/src/script/**`, `test/tests/test_package_test_script.py` | + [subsystem-script.md](./rules/subsystem-script.md) |
| `crates/ocx_lib/src/package/**`, `package.rs` | + [subsystem-package.md](./rules/subsystem-package.md) |
| `crates/ocx_lib/src/package_manager/**`, `package_manager.rs` | + [subsystem-package-manager.md](./rules/subsystem-package-manager.md) |
| `crates/ocx_lib/src/package/metadata/**`, `crates/ocx_schema/**` | + [subsystem-metadata-schema.md](./rules/subsystem-metadata-schema.md) |
| `crates/ocx_cli/src/**` | + [subsystem-cli.md](./rules/subsystem-cli.md), [quality-cli-help.md](./rules/quality-cli-help.md) |
| `crates/ocx_cli/src/api/**`, `command/**` | + [subsystem-cli-api.md](./rules/subsystem-cli-api.md), [subsystem-cli-commands.md](./rules/subsystem-cli-commands.md) |
```
**"Declared Path-Scope Overlaps" table** (`:144`, verbatim): `| \`subsystem-cli-api.md\` + \`subsystem-cli-commands.md\` | \`crates/ocx_cli/src/command/**\` |`

17 total `crates/`/`ocx_lib` occurrences in `.claude/rules.md` (`/usr/bin/grep -c`).

### 3. `CLAUDE.md` — Architecture, Stability tiers, line count

**Line count: 154** (budget 200). Structural test: `.claude/tests/test_ai_config.py::TestClaudeMd::test_line_budget` (`:407`).

**§ "Architecture"** (`:70-86`, verbatim):
> ## Architecture
>
> Four crates: `crates/ocx_lib` (core), `crates/ocx_cli` (thin CLI, pkg `ocx`), `crates/ocx_schema` (build-only JSON schema), `crates/ocx_shim` (Windows launcher shim). The mirror tool lives in its own repo: [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) (vendors ocx as submodule). Rust 2024, resolver v3. Three deps patched to submodules under `external/`: `oci-client` (`rust-oci-client`), `docker_credential`, `sigstore` (`sigstore-rs`).
>
> Subsystem rules auto-load on path match. Read relevant one before work on that area:
>
> | Subsystem | Rule | Scope |
> |-----------|------|-------|
> | OCI registry/index | [subsystem-oci.md](./.claude/rules/subsystem-oci.md) | `crates/ocx_lib/src/oci/**` |
> | Storage/symlinks | [subsystem-file-structure.md](./.claude/rules/subsystem-file-structure.md) | `crates/ocx_lib/src/file_structure/**` |
> | Package metadata | [subsystem-package.md](./.claude/rules/subsystem-package.md) | `crates/ocx_lib/src/package/**` |
> | Package manager | [subsystem-package-manager.md](./.claude/rules/subsystem-package-manager.md) | `crates/ocx_lib/src/package_manager/**` |
> | CLI commands/API | [subsystem-cli.md](./.claude/rules/subsystem-cli.md) | `crates/ocx_cli/src/**` |
> | Script host API | [subsystem-script.md](./.claude/rules/subsystem-script.md) | `crates/ocx_lib/src/script/**` |
> | Acceptance tests | [subsystem-tests.md](./.claude/rules/subsystem-tests.md) | `test/**` |
> | Website/docs | [subsystem-website.md](./.claude/rules/subsystem-website.md) | `website/**` |

**§ "Stability tiers"** (`:13-24`, verbatim — the full section, the one the ADR
requires editing to add the ecosystem tier):
> ### Stability tiers
>
> **Internal code structure has no stability at all.** Crate layout, module paths, type names, function signatures, enum shapes — all free to change. Never add a compat shim, deprecation window, re-export alias, or `_v2` name for an internal refactor. Rename in place and delete the old form as if it never existed. `ocx_lib` is not a published library; the binary is the only consumer.
>
> **Interfaces are the CLI surface and every wire/persisted format** — command and flag grammar, exit codes, package metadata, OCI manifests, `ocx.lock`, the index format, `ocx.toml`. These are real contracts: other tools and published artifacts depend on them, so a change here is a decision, not a refactor.
>
> **Even interfaces break pre-1.0.** A break is announced in the changelog and nowhere else — no migration prose in user docs, no dual-form parsing, no warning schedule. The one hard exception: already-published packages must keep resolving, so metadata and OCI manifest changes stay backward compatible on the read path.
>
> **Batched-window carve-out.** [… full paragraph on the deprecation-window mechanics and the in-flight 0.6→0.7 renames …]
>
> Practical test: if only this repo can observe the change, just make it. If a published artifact or someone's script can observe it, weigh it — then still just make it, and write the changelog line — which means the commit subject (see below), never the file.

### 4. `.claude/rules/arch-principles.md`

**Current line count: 228.**

**"Crate Layout" table** (`:11-19`, verbatim):
```
| Crate | Purpose | Dependency Direction |
|-------|---------|---------------------|
| `ocx_lib` | Core lib — stores, OCI, packages, manager | Depend nothing internal |
| `ocx_cli` | Thin CLI shell — args, context, commands, reporting | Depend `ocx_lib` |
| (mirror tool) | Moved to own repo [ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror) — vendors ocx as submodule, `ocx_lib` path dep | — |
| `ocx_schema` | JSON schema gen (build-only) | Depend `ocx_lib` |

Patched deps: `oci-client` at `external/rust-oci-client`, `docker_credential` at `external/docker_credential`, `sigstore` at `external/sigstore-rs` (local git submodules).
```

**"Long-term: split `ocx_lib`" clause** (`:27`, verbatim): `- **Long-term**: split \`ocx_lib\` into smaller, cleanly layered crates (future \`ocx-lib\` repo); plugins link foundation crates, drive operations via CLI.`

**"Known drift" line** (`:28`, verbatim): `- Known drift: \`ocx-mirror\` reaches into operational internals — migration target is CLI for operations (pending refactor).`

**"Core vs Plugin Boundary" clause** (`:22-26`, full section verbatim):
```
## Core vs Plugin Boundary (owner doctrine, 2026-07-16)

- **`ocx` core is self-contained**: complete primitive set (`ocx package *`, toolchain tier) stays in the one binary. No verb extraction to slim the binary.
- **Add-ons are `ocx-<name>` plugins** (git/cargo-style dispatch, shipped — `app/plugin_dispatch.rs`). No plugin ABI, ever.
- **Boundary is behavioral, not link-level**: package/store/registry *operations* go through the CLI surface (`ocx package create/push/test`, …) — CLI = the stable contract. Linking *vocabulary/utility* crates (version, identifier, slug, platform types) is fine.
- **Long-term**: split `ocx_lib` into smaller, cleanly layered crates (future `ocx-lib` repo); plugins link foundation crates, drive operations via CLI.
- Known drift: `ocx-mirror` reaches into operational internals — migration target is CLI for operations (pending refactor).
```
(This whole section is what the ADR replaces: the "Boundary is behavioral" bullet is superseded by the ADR's Satellite Linking Rule; "Long-term: split" is struck as delivered; "Known drift" is replaced.)

**"ADR Index" table** (`:82-98`, header + 3 sample rows verbatim, so a new
row can be added in the same format):
```
## ADR Index

| ADR | Decision |
|-----|----------|
| `adr_cascade_platform_aware_push.md` | Per-platform version filtering + index merging |
| `adr_platform_libc_os_features.md` | libc family differentiation via `os.features` + `libc.*` namespace; `can_run()` subset matcher (superseded by `adr_platform_model_unification.md` D1's `is_compatible`/`select_best`) |
| `adr_platform_model_unification.md` | Directed compatibility relation (`is_compatible`/`compatibility_score`/`select_best`, one shared helper across fresh-resolve, lock-read, authoring pinning); canonical single-grammar platform string (`os/arch[/variant][+feature[,feature...]]` | `any`); `ocx.lock` V3 (only supported version, canonical-key validation, no digest-value uniqueness); single-platform resolution + authoring (`TargetPlatforms` deleted, `patch sync` keeps the one sanctioned multi-platform fan-out) |
```

### 5. `.claude/hooks/*.py` — `crates/ocx_lib` / `ocx_lib` occurrences

No `.sh` hooks exist. `post_tool_use_tracker.py`'s `CONTEXT_REMINDERS` table
(`:31-36`, verbatim — 4 of the 6 rows name `crates/ocx_lib`):
```python
CONTEXT_REMINDERS: list[tuple[str, str, str]] = [
    ("crates/ocx_lib/src/oci/**", "subsystem-oci.md", "OCI"),
    ("crates/ocx_lib/src/file_structure/**", "subsystem-file-structure.md", "File Structure"),
    ("crates/ocx_lib/src/package/**", "subsystem-package.md", "Package"),
    ("crates/ocx_lib/src/package_manager/**", "subsystem-package-manager.md", "Package Manager"),
    ("crates/ocx_cli/src/**", "subsystem-cli.md", "CLI"),
    ("website/**", "subsystem-website.md", "Website"),
]
```
No other hook file references `ocx_lib`.

### 6. `.claude/agents/*.md`, `.claude/skills/**/*.md`, `.claude/templates/**`

Agents (11 lines across 4 files):
- `worker-architect.md:25` — `| New task method | \`crates/ocx_lib/src/package_manager/tasks/\` |`
- `worker-architect.md:27` — `| New storage path | \`crates/ocx_lib/src/file_structure/\` |`
- `worker-architect.md:28` — `| New index operation | \`crates/ocx_lib/src/oci/index/\` |`
- `worker-architect.md:29` — `| New metadata field | \`crates/ocx_lib/src/package/metadata/\` |`
- `worker-architecture-explorer.md:21` — `- \`crates/ocx_lib/src/*.rs\` — library modules`
- `worker-builder.md:39` — `Grep existing utilities in \`crates/ocx_lib/src/utility/\` + relevant modules …`
- `worker-doc-reviewer.md:26` — `| \`crates/ocx_lib/src/package_manager/**\` (changed logic) | \`user-guide.md\` | Package lifecycle sections |`
- `worker-doc-reviewer.md:27` — `| \`crates/ocx_lib/src/oci/platform*\` (new platform) | \`installation.md\`, \`user-guide.md\` | Platform tables |`
- `worker-doc-reviewer.md:28` — `| \`crates/ocx_lib/src/oci/client*\` (auth change) | \`reference/environment.md\`, \`user-guide.md\` | Auth sections |`
- `worker-doc-reviewer.md:30` — `| \`crates/ocx_lib/src/file_structure/**\` | \`user-guide.md\` | Three-store architecture section |`

Skills (1 file):
- `.claude/skills/meta-validate-context/SKILL.md:46` — `ls crates/ocx_lib/src/module_name/`
- `.claude/skills/meta-validate-context/SKILL.md:49` — `grep -rn "^pub struct\|^pub enum\|^pub trait" crates/ocx_lib/src/subsystem/ | grep -v test`

Templates (1 file):
- `.claude/templates/artifacts/bugfix_plan.template.md:101` — `| [test_name] | \`crates/ocx_lib/src/[module]/mod.rs\` | [What test check — target root cause] |`

### 7. `.claude/tests/test_ai_config.py` — tests naming `crates/`, `ocx_lib`, line budget, worktree table, subsystem/catalog parity

2024 lines total.

| Line | Test | Assertion |
|---|---|---|
| 128 | `TestShareableQualityRules::test_shareable_rules_no_ocx_leak` | Shareable `quality-*.md` files must contain none of `_OCX_FORBIDDEN_STRINGS` (`:104-115`), which includes literal `"ocx_lib"` and `"crates/ocx"` |
| 348 | `TestRuleGlobs::test_all_rule_globs_match_files` | Every `paths:` glob in `.claude/rules/*.md` (except `quality-*` and `repository:`-tagged shareables) must `glob.glob()`-match >=1 file; this is the red/green gate the ADR's "Glob re-scope rule" cites at `:329` in its own text (line drifted to 348 in the current tree — cite the test name, not the line, per `feedback_anchor_rulings_by_symbol_not_line`) |
| 382 | `TestRuleGlobs::test_package_manager_glob_not_too_broad` | Regression test: `subsystem-package-manager.md` must not carry a bare `crates/ocx_lib/src/*.rs`-style catch-all glob (bug captured: it once matched 23 unrelated root files) |
| 407 | `TestClaudeMd::test_line_budget` | `len(claude_md_lines) <= 200` |
| 439 | `TestClaudeMd::test_worktree_count_matches_table` | The stated `**Worktrees**: N git worktrees` count must equal the table's row count |
| 654 | `TestCatalog::test_catalog_exists` | `.claude/rules.md` exists |
| 660 | `TestCatalog::test_catalog_covers_all_rules` | Every `.claude/rules/*.md` file is referenced somewhere in the catalog |
| 673 | `TestCatalog::test_catalog_references_resolve` | Every rule filename the catalog references exists on disk |
| 688 | `TestCatalog::test_claude_md_points_to_catalog` | `CLAUDE.md` links to `.claude/rules.md` |
| 795 | `TestCatalog::test_catalog_subsystem_coverage` | Every `subsystem-*.md` named in `CLAUDE.md`'s table must also appear in the catalog's "By subsystem" section (catalog may list more, never fewer) |

### 8. `website/src/docs/**`, `README.md`, `CONTRIBUTING.md`, `product-context.md`

- `website/src/docs/authoring/migration.md:10` — "…drives a download -> bundle -> push pipeline through the same `ocx_lib` publisher API the `ocx package` commands use."
- `website/src/docs/authoring/migration.md:90` — "…pushes them through the same `ocx_lib` publisher API used by `ocx package create` / `push`."
- `website/src/docs/in-depth/project.md:330` — `[composer-source]: https://github.com/ocx-sh/ocx/blob/main/crates/ocx_lib/src/…` (footnote link)
- `website/src/docs/reference/environment.md:963` — `$OCX_HOME/state/update-check/ocx_sh_ocx_cli` (a state-file *name*, not a code reference — literal string match only)
- `README.md` — 0 occurrences.
- `CONTRIBUTING.md:16` — `| \`ocx_lib\` | Core library: OCI client, file structure, package manager |`
- `CONTRIBUTING.md:17` — `| \`ocx_cli\` | Thin CLI shell using clap; produces the \`ocx\` binary |`
- `CONTRIBUTING.md:35` — `cargo nextest run -p ocx_lib <test_name>   # single test`
- `.claude/rules/product-context.md:151` — Technical Overview Workspace line (verbatim): `- **Workspace**: \`crates/ocx_lib\` (core) + \`crates/ocx_cli\` (CLI); the mirror tool lives in its own repo ([ocx-sh/ocx-mirror](https://github.com/ocx-sh/ocx-mirror))`

### 9. `.github/**`

- `.github/workflows/shell-activation.yml:34-47` — `paths:` filter lines (verbatim):
  ```
  - crates/ocx_lib/src/setup.rs
  - crates/ocx_lib/src/setup/**
  - crates/ocx_lib/src/shim.rs
  - crates/ocx_lib/src/shell.rs
  - crates/ocx_lib/src/shell/**
  - crates/ocx_lib/src/package_manager.rs
  - crates/ocx_lib/src/package_manager/**
  - crates/ocx_lib/src/oci/index.rs
  - crates/ocx_lib/src/oci/index/**
  - crates/ocx_lib/src/activation.rs
  ```
- `.github/workflows/build-windows-shims.yml` — `paths:` filters at `:21-22,35-36` (`crates/ocx_lib/src/shims/**`, `crates/ocx_lib/src/shim.rs`) plus 6 body references to the same paths in shell steps (`:171,214,225,231,276,294` — the SHIM_SHA256 ritual and blob-refresh instructions).
- `.github/workflows/verify-release-ci.yml` — **no** literal `crates/` path; its `paths:` filter (`:19-27`) uses `"**/Cargo.toml"` — crate-count-agnostic but still fires on every new manifest.
- No `crates/ocx_lib` reference elsewhere under `.github/`.

### 10. `CODEOWNERS` / `.licenserc.toml` / `deny.toml` / `.gitattributes` / `renovate.json` / dependabot

- `.github/CODEOWNERS` (67 bytes) — `* @michael-herwig` only; no path-scoped rows, nothing to re-target.
- `.licenserc.toml:7` — `includes = ["crates/**/*.rs"]` (crate-count-agnostic glob, unaffected by the split).
- `deny.toml:56` — comment only: `# crates/ocx_cli/tests/linux_self_contained.rs bans a libbz2 NEEDED entry.` (names `ocx_cli`, unaffected — that crate doesn't move).
- `.gitattributes`, `renovate.json` — no `crates/` occurrences.
- No `dependabot.yml` in this repo.

---

## Part 2 — the satellites

### ocx-mirror (`/home/mherwig/dev/ocx-mirror`)

- **State**: branch `main`, HEAD `7aad05393e931cd80d3105ce4070d777084d782c`, `git status --short` empty (clean).
- **Submodule pointer**: `git submodule status` -> ` 3538b755f55cefddced47cca6b8cf963315f91c9 external/ocx (v0.6.2)`. `git merge-base --is-ancestor 3538b755… origin/main` from `ocx-evelynn` -> **YES**, ancestor (it is this repo's own `release: v0.6.2` commit, visible in the recent-commits log).

**`Cargo.toml` (root, `ocx_mirror` package)** — key lines verbatim:
```toml
[workspace]
exclude = ["external/ocx"]

[dependencies]
ocx_lib = { path = "external/ocx/crates/ocx_lib" }
ocx_python = { path = "crates/ocx_python" }
serde_json = { version = "1.0.150", features = ["preserve_order"] }
```
No `raw_value` feature declared on `serde_json` here (only `preserve_order`);
no extra `features = [...]` set on the `ocx_lib` dependency itself (plain
path, default features).
```toml
[patch.crates-io]
oci-client = { path = "external/ocx/external/rust-oci-client" }
docker_credential = { path = "external/ocx/external/docker_credential" }
sigstore = { path = "external/ocx/external/sigstore-rs" }
```
**`crates/ocx_python/Cargo.toml`**:
```toml
[dependencies]
ocx_lib = { path = "../../external/ocx/crates/ocx_lib" }
serde_json = "1.0.150"
```
(no `preserve_order`/`raw_value` here — only the root package declares them).

**Import census** (script: scratchpad `mirror_census.py`, walks `**/*.rs`
under `ocx-mirror/{src,crates}` excluding `.git`/`target`/`external`, 248
files scanned after excluding a `.git/carve-scratch/` false-positive found in
the first pass — expands `use ocx_lib::{...}` groups incl. nested braces, and
regex-matches fully-qualified `ocx_lib::a::b::c` outside `use`/`//` lines):

- **601 raw hits -> 511 hits after excluding `.git` scratch files and
  comment-only lines**; **179 distinct fully-qualified `ocx_lib::` item
  paths**.
- Per-item target crate resolved against
  `.claude/artifacts/discover_crate_split_file_map.md` § 2's file->crate table
  (path-prefix match on the imported item's module segments), with
  hand-verified overrides (via direct `grep` on the real defining file) for
  every case where a symbol name doesn't match its containing file name —
  the mapping is file-grained, not symbol-grained, so these 12 items needed a
  direct check: the 6 `ocx_lib::file_structure::*` re-exports (all actually
  live in `index_store.rs`, not `file_structure.rs` -> `ocx_index`, not
  `ocx_store`); `ExitCode`/`ErrorCategory` (live in `cli/exit_code.rs` /
  `cli/error_category.rs` -> `ocx_exit`, not the `cli.rs`-default
  `ocx_console`); `ClassifyExitCode`/`LogLevel`/`LogSettings` (live in
  `cli/classify.rs` / `cli/log_level.rs` / `cli/log_settings.rs` -> `ocx_cli`,
  not `ocx_console`); `Error`/`Result` (root re-exports of `error.rs`, which
  the ADR dissolves — its `Error::OciClient` sites become
  `ocx_oci::client::error::ClientError` per Phase 3, everything else has no
  1:1 successor).

**Per-target-crate distinct-item counts** (179 total):

| Target crate | Distinct items | In satellite-allowed set? |
|---|---|---|
| `ocx_oci` | 61 (+1 special: `Error::OciClient` -> `ocx_oci::client::error::ClientError`) | Yes |
| `ocx_package` | 29 | Yes |
| `ocx_index` | 22 (incl. the 6 `file_structure::*` re-exports, corrected) | Yes |
| `ocx_console` | 15 | Yes |
| `ocx_util` | 15 | Yes |
| `ocx_exit` | 10 (`ExitCode` + 8 variants + `ErrorCategory`) | Yes |
| `ocx_config` | 8 (`env::var`, `env::insecure_registries`, `env::mirrors`, `env::keys::*`, `Config::default`, `resolve_mirror_map`) | Yes |
| **`ocx_announce`** | **8** (`forge::ForgeCredentials`, `ForgeKind`, `RepoCoordinate`, `WriteTransport::*`) | **No — VIOLATION** |
| **`ocx_cli`** | **3** (`ClassifyExitCode`, `LogLevel`, `LogSettings`) | **No — VIOLATION** |
| `ocx_sign` | 3 (`oci::verify::DiscoveryMethod`/`SignerCandidate`/`list_signature_candidates`) | Yes |
| **`ocx_shell`** | **2** (`ci::CiFlavor`, `ci::annotations::for_flavor`) | **No — VIOLATION** |
| `error.rs` dissolve (`Error`, `Result`) | 2 | N/A — type deleted, no successor crate |

**3 satellite-linking-rule violations, 13 items, across 3 forbidden crates**
(`ocx_announce`, `ocx_cli`, `ocx_shell` — all three explicitly named as
forbidden in the ADR's Satellite Linking Rule). Concrete site for the
sharpest one: `src/main.rs:8` imports `ClassifyExitCode` from `ocx_lib::cli`
and `src/main.rs:76` calls `error.classify().unwrap_or(ocx_lib::cli::ExitCode::ConfigError)`
— post-split, `ClassifyExitCode` lives in `ocx_cli` (contract E3, ADR Phase 1
step 1.1), which ocx-mirror may not link. This is the mirror-side follow-up
the ADR's Phase 3 doesn't itself resolve — either ocx-mirror needs its own
classification, or the rule needs a documented exception for this one trait.

**`ocx_lib::Error::OciClient` match sites** — exactly 4, all in
`src/pipeline/target_registry.rs:178,240,263,341`. Additional `ocx_lib::Result`
uses: `target_registry.rs:174,237,250,337` (4). Additional bare `ocx_lib::Error`
use: `target_registry.rs:399` (1).

**`ocx_lib::cli::ExitCode` / `ClassifyExitCode` / `try_classify`**: `ExitCode`
(qualified or via its 8 variants) appears in 10 of the 179 distinct items
above. `ClassifyExitCode`: exactly **1** use site (`main.rs:8` import +
`main.rs:76` `.classify()` call) — **not** "none" as the task's prior
assumption stated; this is now verified, not assumed. `try_classify`: **0**
hits — confirmed none.

**CI build against ocx** — `.github/workflows/verify.yml`: `smoke` job
checks out with `submodules: recursive`, runs `cargo tree -i oci-client`/`-i
sigstore` to assert `[patch.crates-io]` bound to the vendored fork path
(`external/ocx/external/{rust-oci-client,sigstore-rs}`), builds `ocx-mirror`
via `task rust:build`/`task rust:test:unit`, and separately builds
`external/ocx` itself (`cargo build --release --bin ocx`) to produce the
exact submodule-pinned `ocx` binary the `acceptance-tests` job runs against.
Has its own `taskfile.yml` + `taskfiles/` with `rust:build`, `rust:test:unit`,
`rust:format:check`, `rust:clippy:check` tasks (a `task verify`-equivalent
gate, not the same task name).

**Files needing edits for the pointer bump** — every file with an `ocx_lib`
reference: **140 files** (137 `.rs` + 3 `Cargo.toml`: root,
`crates/ocx_python/Cargo.toml`, plus one more). Heaviest:
`src/pipeline/target_registry.rs` (33), `src/pipeline/registry_copy.rs` (24),
`crates/ocx_python/src/compose.rs` (20), `src/http.rs` (18),
`src/command/package/pipeline/prepare.rs` (25).

### grimoire (`/home/mherwig/dev/grimoire`)

- Branch `main`, HEAD `ce76e2f8b88b8866f115c7ecaa980a9590ad421f`.
- **No `ocx_lib`/`ocx_*` dependency** in `Cargo.toml` — confirmed (0 hits).
- Submodules: `external/docker_credential @ 8e89cd0e…` (branch `feat/store-erase-list`),
  `external/rust-oci-client @ 7f3d0b6c8041bb9902e412dcb452aee749d2031e`
  (`ocx/integration`, tag `v0.17.0-40-g7f3d0b6`).
- **This repo's own `external/rust-oci-client` pin**: `e5ed433a7e7e9d398907161784f77727fb4b4645`
  on `ocx/drop-dead-referrers-fallback` — a **different branch and commit**
  from grimoire's `ocx/integration @ 7f3d0b6c`, confirming the ADR's framing:
  onboarding grimoire is a fork reconciliation, not a simple version bump.
- **`[patch.crates-io]`** (`Cargo.toml:106-108`, verbatim):
  ```toml
  [patch.crates-io]
  docker_credential = { path = "external/docker_credential" }
  oci-client = { path = "external/rust-oci-client" }
  ```
- Depends on `oci-client = "0.17"` (`:46`, `rustls-tls` feature only) and
  `docker_credential = "1.3"` (`:56`) today — same major/minor as `ocx_lib`
  requires (`oci-client "0.17"`, `docker_credential "1.3"`). **No `sigstore`
  dependency anywhere in `Cargo.toml`** — confirms "no signing code at all."
- `grimoire/CLAUDE.md` — **0 occurrences** of "ocx" (no ADR-relevant content
  to update there today).
