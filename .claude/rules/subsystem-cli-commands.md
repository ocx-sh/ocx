---
paths:
  - crates/ocx_cli/src/command/**
---

# OCX CLI Commands — Quick Reference

Concise index of all `ocx` CLI commands. User-facing per-command docs live in [`website/src/docs/reference/command-line.md`](../../website/src/docs/reference/command-line.md). Implementation under `crates/ocx_cli/src/command/` — read source for return types, internal call sites, report column formats.

---

## Layering: High-Level vs OCI-Tier {#layering}

The CLI surface splits into six rows. The split is firm — a command does not change contract based on `ocx.toml` presence.

| Row | Commands | Input symbols | Input | Output |
|-----|----------|---------------|-------|--------|
| **High-level read** | `pull`, `run` | Binding names (TOML keys) | `ocx.toml` + `ocx.lock` | Cache warm / child spawn |
| **Project mutators** | `add`, `remove`, `lock`, `update` | OCI identifier in → binding name written | `ocx.toml` + `ocx.lock` | Writes `ocx.toml` and/or `ocx.lock` |
| **Shell activation** | `shell hook`, `shell direnv`, `shell init` | Binding names (resolved to installed paths) | Nearest `ocx.toml` + install store | Shell export lines |
| **Bootstrap / mixed** | `init`, `info`, `version`, `shell completion` | Varies | Varies | No tier contract |
| **Low-level — registry** | `install`, `uninstall`, `package pull/push/describe/info/create`, `index update/list/catalog` | OCI identifiers | Registry + local index | Install store / index |
| **Low-level — local store** | `find`, `select`, `deselect`, `deps`, `env`, `exec`, `shell env`, `clean`, `launcher exec`, `ci export` | OCI identifiers | Install + symlink store | Reports / child spawn |

**Layer-purity rule:** `ocx run` is project-tier (binding-name semantics); `ocx exec` is OCI-tier (identifier semantics). If you have `ocx.toml`, prefer `ocx run`; if you do not, use `ocx exec`. No command silently switches contract based on CWD or filesystem state.

ADR: [`adr_cli_high_low_layering.md`](../../.claude/artifacts/adr_cli_high_low_layering.md) — rationale for the split, rejected alternatives, one-way-door commitments.

---

## Global Flags (all commands)

| Flag | Env Var | Default | Purpose |
|------|---------|---------|---------|
| `--color auto\|always\|never` | `NO_COLOR`, `CLICOLOR`, `CLICOLOR_FORCE` | auto | ANSI color output control |
| `--remote` | `OCX_REMOTE` | false | Route mutable lookups (tags, catalog, tag→manifest) to remote registry; pure queries never write local index |
| `--offline` | `OCX_OFFLINE` | false | Disable all network access; tag→digest must resolve locally or be digest-pinned |
| `--offline --remote` | both | — | Pinned-only mode: no source contact, info log fires, tag-addressed `Resolve` errors if not local |
| `--format plain\|json` | — | plain | Output format |
| `--index PATH` | `OCX_INDEX` | — | Override local index directory |
| `-l/--log-level` | — | — | Tracing level (trace/debug/info/warn/error) |

---

## Command Summary

| Command | Purpose | Auto-Install | Key Flags |
|---------|---------|-------------|-----------|
| `add IDENTIFIER` | Append binding to `ocx.toml`, update lock, install | **Yes** | `-g/--group` |
| `init` | Create minimal `ocx.toml` in current directory | No | — |
| `remove IDENTIFIER` | Drop binding from `ocx.toml`, rewrite lock, uninstall | No | — |
| `run [-g GROUP]... [NAME...] -- ARGV...` | Spawn child with project-tier composed env (binding names from `ocx.lock`) | **Yes** | `-g/--group`, `--clean`, `--self` |
| `install PKGS...` | Download and install packages | N/A (is install) | `-s/--select`, `-p/--platform` |
| `find PKGS...` | Resolve installed packages to paths | No | `--candidate`, `--current`, `-p` |
| `select PKGS...` | Set `current` symlink | No | `-p` |
| `deselect PKGS...` | Remove `current` symlink | No | — |
| `deps PKGS...` | Show dependency tree/flat/why | No | `--flat`, `--why`, `--depth`, `-p`, `--mode` |
| `uninstall PKGS...` | Remove candidate symlink | No | `-d/--deselect`, `--purge` |
| `clean` | GC unreferenced objects | No | `--dry-run`, `--force` |
| `env PKGS...` | Print resolved env vars | **Yes** | `--candidate`, `--current`, `-p`, `--self` |
| `exec PKGS... -- CMD` | Run command with package env | **Yes** | `--clean`, `-p`, `--self` |
| `shell env PKGS...` | Shell-specific export lines | No | `-s/--shell`, `-p`, `--candidate/--current`, `--self` |
| `shell completion` | Generate completions | No | `--shell` |
| `shell hook` | Stateful prompt-hook export generator (reads/updates `_OCX_APPLIED`) | No | `-s/--shell` |
| `shell direnv` | Stateless export generator for direnv `.envrc` | No | `-s/--shell` |
| `index catalog` | List known repositories | No | `--tags` |
| `index list PKGS...` | List tags for packages | No | `--platforms`, `--variants` |
| `index update PKGS...` | Sync local index from remote | No | — |
| `lock` | Resolve project tool tags to digests and write `ocx.lock` | No | `-g/--group` |
| `package pull PKGS...` | Download to object store only | N/A (is pull) | `-p` |
| `package create PATH` | Bundle directory into archive | No | `-o`, `-m`, `-l`, `-j`, `--force` |
| `package push -i ID LAYERS...` | Publish archive to registry | No | `-i/--identifier` (required), `-c/--cascade`, `-n`, `-m`, `-p`, `--format json` |
| `package describe ID` | Push description metadata | No | `--readme`, `--logo`, `--title` |
| `package test -i ID LAYERS... -- CMD` | Materialize + exec locally (no registry) | **Yes** (deps only) | `-i/--identifier` (required), `-p`, `-m`, `--keep`, `-o/--output`, `--self`, `--clean` |
| `package info ID` | Display description metadata | No | `--save-readme`, `--save-logo` |
| `ci export PKGS...` | Export env to CI system | No | `-p`, `--flavor`, `--candidate/--current`, `--mode` |
| `version` | Print version | No | — |
| `info` | Print version + platform + shell | No | — |

---

## Task Method Quick Reference

| Manager Method | Auto-Install | Symlink | Use In |
|----------------|-------------|---------|--------|
| `find_all()` | No | No | `find`, `select`, `deps` |
| `resolver().build_graph()` | No | No | `deps` |
| `find_symlink_all(kind)` | No | Yes (candidate/current) | `find --candidate`, `env --candidate` |
| `find_or_install_all()` | **Yes** | No | `env`, `exec` |
| `install_all(candidate=true)` | N/A (is install) | Creates candidate | `install` |
| `install_all(candidate=false)` | N/A (is pull) | No | `package pull` |
| `deselect_all()` | No | Removes current | `deselect` |
| `uninstall_all()` | No | Removes candidate | `uninstall` |
| `clean()` | No | — | `clean` |

---

## Path Resolution Summary

| Mode | Path | Stable? | Auto-Install? | Commands |
|------|------|---------|---------------|----------|
| Object store (default) | `$OCX_HOME/objects/.../content/` | No (digest changes) | Yes (find_or_install) or No (find) | `exec`, `env`, `find` |
| `--candidate` | `$OCX_HOME/symlinks/.../candidates/{tag}` | **Yes** | No | `find --candidate`, `env --candidate` |
| `--current` | `$OCX_HOME/symlinks/.../current` | **Yes** | No | `find --current`, `env --current` |

Use `--candidate` or `--current` when embedding paths in configs, IDE settings, or shell profiles.

---

## Semantics & Gotchas

Design intent not visible from flag table — read before changing CLI behavior here.

- **`index list <pkg>@<digest>`**: rejected with usage error. `index list` enumerates tags; a digest narrows nothing. Use `ocx package info <pkg>@<digest>` for a single artifact. Tag-only identifiers (`<pkg>:<tag>`) still work as a tag filter on the returned list.
- **`index update <pkg>`**: tagged identifier (`cmake:3.28`) fetches only that tag's digest + manifest, merges into existing `tags/{repo}.json`. Bare identifier (`cmake`) fetches all tags. Two modes intentional — tagged mode keep offline indexes minimal + reproducible. **Sole writer** of tag pointers outside install/pull (which writes via `LocalIndex::commit_tag`, gated to skip pinned-id pulls because `ocx.lock` is canonical).
- **`deps`**: tree view marks repeated subtrees with `(*)`, no re-expand. Flat view (`--flat`) emits topological evaluation order — same order `exec` and `env` use to layer env vars. Why view (`--why`) traces all paths from roots to target by registry/repository (tag ignored when matching).
- **`package push -p/--platform` required.** Multi-platform manifests assembled by repeated single-platform pushes; no auto-detect path on purpose.
- **`package describe` / `package info`**: identifier is repository only, tag ignored. `describe` requires at least one of `--readme`, `--logo`, `--title`, `--description`, `--keywords` — no-op invocation rejected, not silently accepted.
- **`env` vs `shell env`**: `env` auto-installs missing packages (`find_or_install_all`); `shell env` does not (`find_all`). Split exists because `shell env` wired into shell init paths where surprise downloads hostile.
- **`--self` flag** (shared by `exec`, `run`, `env`, `shell env`, `shell profile load`, `ci export`, `deps`): selects the private surface — emits vars where `has_private()` is true (`private` and `public`). Default (off) selects the interface surface — emits vars where `has_interface()` is true (`public` and `interface`). See `subsystem-cli.md` Cross-Cutting section.
- **`package test` tempdir lifecycle**: without `--keep` or `--output`, the temp directory is deleted on **any** exit — success and failure. `--keep` is the explicit opt-in for post-failure inspection; re-run with `--keep` to preserve. The deletion is explicit (pre-exec `drop(td_guard)`) because `child_process::exec` diverges on Unix (execvp replaces the process image) so RAII `Drop` never fires on the success path.
- **`package test --output` same-filesystem requirement**: `--output DIR` must be on the same filesystem as `$OCX_HOME/layers/` — hardlink assembly has no cross-device copy fallback. Validated via `dev()` device number comparison (Unix). Cross-fs → `IoError` (exit 74) with a message naming both paths.
- **`package test` identifier rejects `@digest`**: the digest is computed locally from the supplied layers; supplying one would conflict. `UsageError` (exit 64).
- **`--keep` + `--output` are mutually exclusive**: enforced by clap `conflicts_with`. Use one or the other; combining is a usage error.
- **`ocx run` — semantics & gotchas:**
  - `--` is mandatory. Clap rejects any invocation missing `--`; at least one argv token after `--` is also required (`num_args = 1..` on the `argv` field). Both produce exit 64 (OCX remaps clap's default exit 2 to 64 for UsageError).
  - Default scope = `[tools]` only (matches `pull` precedent). Omitting `-g` is NOT "everything" — it is "the default group". Pass `-g all` for the full toolchain.
  - `all` keyword: `-g all` expands to `[default, *named_groups_alphabetical]` at the CLI layer before `compose_tool_set` is called. `all` is a reserved keyword — `[group.all]` in `ocx.toml` is rejected at parse time (exit 78); `ocx add --group all` is rejected at mutate time (exit 64).
  - Ambiguity rule: a NAME in `-g ci -g release` that resolves to entries in both groups with *identical* identifiers is silently collapsed by `compose_tool_set` to one entry. With *different* identifiers, `compose_tool_set` returns `DuplicateToolAcrossSelectedGroups` (exit 64). The CLI NAME-filter also exits 64 when a user-supplied NAME matches more than one entry in the composed set (defense-in-depth for future sideload support).
  - Layer purity: `ocx run` never falls through to OCI-tier behavior. Missing `ocx.toml` → exit 64. Missing `ocx.lock` → exit 78. Stale `ocx.lock` → exit 65. These errors are not recoverable by supplying an OCI identifier.
  - Composition order rule: *First by group-selection order (the order of `-g` flags after `all` expansion, deduplicated); then alphabetical by binding name within each group.* This is iteration order; the composer prepends env entries, so the **last-iterated tool's PATH lands first** at runtime. Net effect: groups listed **later** in `-g` win PATH lookup. Example: `-g default,ci` → `[group.ci]`'s `bin/` lands ahead of `[tools]`' on the child's PATH.
  - PATH precedence consequence: if two groups declare *different* bindings whose installed packages both ship a binary named `cmake`, the group listed **last** in `-g` order wins. For same-binding-name conflicts (same `(group, binding)` key in two selected groups), `compose_tool_set` errors — they never silently overlap. The prepend invariant is load-bearing in `composer.rs` (reversing causes generated-launcher infinite recursion).
- **`exec` identifier form**: `ocx exec` accepts only bare OCI identifiers (e.g. `node:20`); identifiers resolve through the index and auto-install when missing. The former `file://` URI form was removed (generated launchers re-enter via `ocx launcher exec` instead). The `oci://` scheme is not parsed by `oci::Identifier::from_str`.
- **`launcher exec` internal subcommand**: hidden from `--help` (`#[command(hide = true)]`). Wire ABI is `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Forces `self_view=true` internally. Validates `pkg-root`: must be absolute, canonical inside `$OCX_HOME/packages/`, and contain `metadata.json`. Exits 64 (UsageError) on any validation failure.
- **Entrypoint collision behavior**: Within a single package, duplicate entrypoint names are rejected at deserialization (`EntrypointError::DuplicateName`). Cross-package collisions (two currently-selected packages with the same entrypoint name on the interface surface) are detected by `composer.rs` at compose time and surface as `PackageErrorKind::EntrypointCollision { name, owners }` (exit code `DataError` = 65). Entries that do not enter the active surface (e.g., `private` entries when composing the interface surface) are excluded from collision detection — they cannot collide at runtime under that surface.
- **`shell hook` vs `shell direnv`**: `shell hook` is stateful — fingerprints the *actually-installed* default-group tools and short-circuits via `_OCX_APPLIED` when unchanged; emits `unset` + new exports + new sentinel on change. Designed for prompt-hook integration where every prompt fires the command. `shell direnv` is stateless — emits exports unconditionally without consulting/updating `_OCX_APPLIED`. Designed for direnv `.envrc` integration where direnv handles diffing/unset itself. Both never touch the network and never install.
- **`package push --format json` schema (stable fields)**: `--format json` emits a single JSON object. Fields consumed by `ocx-mirror pipeline push` (treat as stable contract): `manifest_digest` (string, `sha256:<hex>`), `cascade_tags_written` (array of strings; empty when `--cascade` not set or no tags updated), `status` (`"pushed"` | `"skipped_existing"`). Never parse the plain-text output for scripting — use `--format json`.
- **`package test` exit-code passthrough invariant**: the exit code from the inner test subprocess propagates verbatim via `propagate_exit_code` helper in `pipeline.rs`. `ocx package test` itself never remaps the child exit code — if the test binary exits 42, `ocx package test` exits 42. The only OCX-level exit codes come from pre-exec failures (e.g., bundle materialize error → 65/74, usage error → 64). `ocx-mirror pipeline push` uses this invariant: a non-zero exit from `ocx package test` marks the `(V, P, C)` leg as failed in the JUnit XML.
