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
| `--remote` | `OCX_REMOTE` | false | Use remote index instead of local |
| `--offline` | `OCX_OFFLINE` | false | Disable all network access |
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
| `deps PKGS...` | Show dependency tree/flat/why | No | `--flat`, `--why`, `--depth`, `-p`, `--mode` |
| `uninstall PKGS...` | Remove candidate symlink | No | `-d/--deselect`, `--purge` |
| `clean` | GC unreferenced objects | No | `--dry-run` |
| `env PKGS...` | Print resolved env vars | **Yes** | `--candidate`, `--current`, `-p`, `--mode` |
| `exec PKGS... -- CMD` | Run command with package env | **Yes** | `-i`, `--clean`, `-p`, `--mode` |
| `shell env PKGS...` | Shell-specific export lines | No | `-s/--shell`, `-p`, `--candidate/--current`, `--mode` |
| `shell completion` | Generate completions | No | `--shell` |
| `shell profile add PKGS...` | Add to shell profile manifest | No | `--candidate`, `--current` |
| `shell profile remove PKGS...` | Remove from shell profile | No | — |
| `shell profile list` | List profiled packages | No | — |
| `shell profile load` | Output profile export lines | No | `-s/--shell`, `--mode` |
| `index catalog` | List known repositories | No | `--tags` |
| `index list PKGS...` | List tags for packages | No | `--platforms`, `--variants` |
| `index update PKGS...` | Sync local index from remote | No | — |
| `package pull PKGS...` | Download to object store only | N/A (is pull) | `-p` |
| `package create PATH` | Bundle directory into archive | No | `-o`, `-m`, `-l`, `-j`, `--force` |
| `package push ID CONTENT` | Publish archive to registry | No | `-c/--cascade`, `-n`, `-m`, `-p` |
| `package describe ID` | Push description metadata | No | `--readme`, `--logo`, `--title` |
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

- **`index update <pkg>`**: tagged identifier (`cmake:3.28`) fetches only that tag's digest + manifest, merges into existing `tags/{repo}.json`. Bare identifier (`cmake`) fetches all tags. Two modes intentional — tagged mode keep offline indexes minimal + reproducible.
- **`deps`**: tree view marks repeated subtrees with `(*)`, no re-expand. Flat view (`--flat`) emits topological evaluation order — same order `exec` and `env` use to layer env vars. Why view (`--why`) traces all paths from roots to target by registry/repository (tag ignored when matching).
- **`package push -p/--platform` required.** Multi-platform manifests assembled by repeated single-platform pushes; no auto-detect path on purpose.
- **`package describe` / `package info`**: identifier is repository only, tag ignored. `describe` requires at least one of `--readme`, `--logo`, `--title`, `--description`, `--keywords` — no-op invocation rejected, not silently accepted.
- **`shell profile load`**: silently skips broken entries (no error output). Designed for shell init file as `eval "$(ocx --offline shell profile load)"` — `--offline` essential because env file runs every shell startup, must not touch network.
- **`env` vs `shell env`**: `env` auto-installs missing packages (`find_or_install_all`); `shell env` does not (`find_all`). Split exists because `shell env` wired into shell init paths where surprise downloads hostile.
- **`--self` flag** (shared by `exec`, `env`, `shell env`, `shell profile load`, `ci export`, `deps`): selects the private surface — emits vars where `has_private()` is true (`private` and `public`). Default (off) selects the interface surface — emits vars where `has_interface()` is true (`public` and `interface`). See `subsystem-cli.md` Cross-Cutting section.
- **`exec` identifier form**: `ocx exec` accepts only bare OCI identifiers (e.g. `node:20`); identifiers resolve through the index and auto-install when missing. The former `file://` URI form was removed (generated launchers re-enter via `ocx launcher exec` instead). The `oci://` scheme is not parsed by `oci::Identifier::from_str`.
- **`launcher exec` internal subcommand**: hidden from `--help` (`#[command(hide = true)]`). Wire ABI is `ocx launcher exec '<pkg-root>' -- <argv0> [args...]`. Forces `self_view=true` internally. Validates `pkg-root`: must be absolute, canonical inside `$OCX_HOME/packages/`, and contain `metadata.json`. Exits 64 (UsageError) on any validation failure.
- **Entrypoint collision behavior**: Within a single package, duplicate entrypoint names are rejected at deserialization (`EntrypointError::DuplicateName`). Cross-package collisions (two currently-selected packages with the same entrypoint name on the interface surface) are detected by `composer.rs` at compose time and surface as `PackageErrorKind::EntrypointCollision { name, owners }` (exit code `DataError` = 65). Entries that do not enter the active surface (e.g., `private` entries when composing the interface surface) are excluded from collision detection — they cannot collide at runtime under that surface.