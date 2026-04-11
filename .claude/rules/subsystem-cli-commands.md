---
paths:
  - crates/ocx_cli/src/command/**
---

# OCX CLI Commands — Quick Reference

Concise index of all `ocx` CLI commands. User-facing per-command documentation lives in [`website/src/docs/reference/command-line.md`](../../website/src/docs/reference/command-line.md). Implementation lives under `crates/ocx_cli/src/command/` — read the source for return types, internal call sites, and report column formats.

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
| `deps PKGS...` | Show dependency tree/flat/why | No | `--flat`, `--why`, `--depth`, `-p` |
| `uninstall PKGS...` | Remove candidate symlink | No | `-d/--deselect`, `--purge` |
| `clean` | GC unreferenced objects | No | `--dry-run` |
| `env PKGS...` | Print resolved env vars | **Yes** | `--candidate`, `--current`, `-p` |
| `exec PKGS... -- CMD` | Run command with package env | **Yes** | `-i`, `--clean`, `-p` |
| `shell env PKGS...` | Shell-specific export lines | No | `-s/--shell`, `-p`, `--candidate/--current` |
| `shell completion` | Generate completions | No | `--shell` |
| `shell profile add PKGS...` | Add to shell profile manifest | No | `--candidate`, `--current` |
| `shell profile remove PKGS...` | Remove from shell profile | No | — |
| `shell profile list` | List profiled packages | No | — |
| `shell profile load` | Output profile export lines | No | `-s/--shell` |
| `index catalog` | List known repositories | No | `--with-tags` |
| `index list PKGS...` | List tags for packages | No | `--platforms`, `--variants` |
| `index update PKGS...` | Sync local index from remote | No | — |
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
| `--candidate` | `$OCX_HOME/installs/.../candidates/{tag}` | **Yes** | No | `find --candidate`, `env --candidate` |
| `--current` | `$OCX_HOME/installs/.../current` | **Yes** | No | `find --current`, `env --current` |

Use `--candidate` or `--current` when embedding paths in configs, IDE settings, or shell profiles.

---

## Semantics & Gotchas

Design intent that isn't visible from a flag table — read this before changing CLI behavior in these areas.

- **`index update <pkg>`**: a tagged identifier (`cmake:3.28`) fetches only that tag's digest and manifest and merges into the existing `tags/{repo}.json`. A bare identifier (`cmake`) fetches all tags. The two modes are intentional — tagged mode keeps offline indexes minimal and reproducible.
- **`deps`**: tree view marks repeated subtrees with `(*)` and does not re-expand them. Flat view (`--flat`) emits topological evaluation order — the same order `exec` and `env` use to layer environment variables. Why view (`--why`) traces all paths from roots to a target by registry/repository (tag is ignored when matching).
- **`package push -p/--platform` is required.** Multi-platform manifests are assembled by repeated single-platform pushes; there is no auto-detect path here on purpose.
- **`package describe` / `package info`**: identifier is repository only, tag is ignored. `describe` requires at least one of `--readme`, `--logo`, `--title`, `--description`, `--keywords` — no-op invocation is rejected, not silently accepted.
- **`shell profile load`**: silently skips broken entries (no error output). It is designed to be invoked from a shell init file as `eval "$(ocx --offline shell profile load)"` — `--offline` is essential because the env file runs on every shell startup and must not touch the network.
- **`env` vs `shell env`**: `env` auto-installs missing packages (`find_or_install_all`); `shell env` does not (`find_all`). The split exists because `shell env` is wired into shell init paths where surprise downloads would be hostile.
