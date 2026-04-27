---
paths:
  - crates/ocx_cli/src/command/**
---

# OCX CLI Commands — Quick Reference

Concise index of all `ocx` CLI commands. User-facing per-command docs live in [`website/src/docs/reference/command-line.md`](../../website/src/docs/reference/command-line.md). Implementation under `crates/ocx_cli/src/command/` — read source for return types, internal call sites, report column formats.

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
| `install PKGS...` | Download and install packages | N/A (is install) | `-s/--select`, `-p/--platform` |
| `find PKGS...` | Resolve installed packages to paths | No | `--candidate`, `--current`, `-p` |
| `select PKGS...` | Set `current` symlink | No | `-p` |
| `deselect PKGS...` | Remove `current` symlink | No | — |
| `deps PKGS...` | Show dependency tree/flat/why | No | `--flat`, `--why`, `--depth`, `-p` |
| `uninstall PKGS...` | Remove candidate symlink | No | `-d/--deselect`, `--purge` |
| `clean` | GC unreferenced objects | No | `--dry-run` |
| `env PKGS...` | Print resolved env vars | **Yes** | `--candidate`, `--current`, `-p` |
| `exec PKGS... -- CMD` | Run command with package env | **Yes** | `-i`, `--clean`, `-p` |
| `shell env PKGS...` | Shell-specific export lines | No | `-s/--shell`, `-p`, `--candidate/--current` |
| `shell completion` | Generate completions | No | `--shell` |
| `shell profile add PKGS...` | Add to shell profile manifest *(deprecated — see `shell profile generate` / `shell init`)* | No | `--candidate`, `--current` |
| `shell profile remove PKGS...` | Remove from shell profile *(deprecated — see `shell profile generate` / `shell init`)* | No | — |
| `shell profile list` | List profiled packages *(deprecated — see `shell profile generate` / `shell init`)* | No | — |
| `shell profile load` | Output profile export lines *(deprecated — see `shell profile generate` / `shell init`)* | No | `-s/--shell` |
| `shell profile generate` | Generate shell init file from profile | No | `-s/--shell`, `-o/--output` |
| `index catalog` | List known repositories | No | `--tags` |
| `index list PKGS...` | List tags for packages | No | `--platforms`, `--variants` |
| `index update PKGS...` | Sync local index from remote | No | — |
| `lock` | Resolve project tool tags to digests and write `ocx.lock` | No | `-g/--group` |
| `package pull PKGS...` | Download to object store only | N/A (is pull) | `-p` |
| `package create PATH` | Bundle directory into archive | No | `-o`, `-m`, `-l`, `-j`, `--force` |
| `package push ID CONTENT` | Publish archive to registry | No | `-c/--cascade`, `-n`, `-m`, `-p` |
| `package describe ID` | Push description metadata | No | `--readme`, `--logo`, `--title` |
| `package info ID` | Display description metadata | No | `--save-readme`, `--save-logo` |
| `ci export PKGS...` | Export env to CI system | No | `-p`, `--flavor`, `--candidate/--current` |
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
- **`shell profile load`**: silently skips broken entries (no error output). Designed for shell init file as `eval "$(ocx --offline shell profile load)"` — `--offline` essential because env file runs every shell startup, must not touch network.
- **`env` vs `shell env`**: `env` auto-installs missing packages (`find_or_install_all`); `shell env` does not (`find_all`). Split exists because `shell env` wired into shell init paths where surprise downloads hostile.