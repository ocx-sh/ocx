# OCX CLI Commands Reference

AI-friendly reference for all `ocx` CLI commands, their flags, task methods, and workflow patterns.
**Read this file whenever a task involves the CLI interface, user workflows, or command behavior.**

---

## Global Flags (all commands)

| Flag | Env Var | Default | Purpose |
|------|---------|---------|---------|
| `--remote` | `OCX_REMOTE` | false | Use remote index instead of local |
| `--offline` | `OCX_OFFLINE` | false | Disable all network access |
| `--format plain\|json` | — | plain | Output format |
| `--index PATH` | `OCX_INDEX` | — | Override local index directory |
| `-l/--log-level` | — | — | Tracing level (trace/debug/info/warn/error) |

---

## Package Lifecycle Commands

### `ocx install PACKAGES...`

Downloads and installs packages from the registry.

| Flag | Purpose |
|------|---------|
| `-s/--select` | Also set `current` symlink after install |
| `-p/--platform` | Comma-separated platforms (e.g., `linux/amd64,darwin/arm64`) |

- Calls `manager.install_all(packages, platforms, candidate=true, select)`
- Always creates a `candidates/{tag}` symlink
- Auto-detects platform if `-p` not given (current platform + `any`)
- Returns `Vec<InstallInfo>` → reported via `api::data::install::Installs`
- Report: "Package | Version | Path"

---

### `ocx find PACKAGES...`

Resolves already-installed packages to their paths. **Does not download.**

| Flag | Purpose |
|------|---------|
| `--candidate` | Resolve via `candidates/{tag}` symlink (stable path) |
| `--current` | Resolve via `current` symlink (floating) |
| `-p/--platform` | Platform filter |

- Default: calls `manager.find_all()` → returns object store path
- `--candidate`/`--current`: calls `manager.find_symlink_all(kind)`
- **No auto-install** — fails with `PackageNotFound` if absent
- Returns `Vec<InstallInfo>` → reported via `api::data::paths::Paths`
- Report: "Package | Path"

---

### `ocx select PACKAGES...`

Sets the `current` symlink for already-installed packages.

| Flag | Purpose |
|------|---------|
| `-p/--platform` | Platform filter |

- Calls `manager.find_all()` then `ReferenceManager::link(current_path, content)`
- **No auto-install** — package must be installed first
- Returns `Vec<InstallInfo>` → reported via `api::data::install::Installs`
- Report: "Package | Version | Path" (path = current symlink)

---

### `ocx deselect PACKAGES...`

Removes the `current` symlink. Package remains installed (candidate preserved).

- Calls `manager.deselect_all(&packages)`
- Returns `Vec<Option<PathBuf>>` (`None` = symlink was already absent)
- Report: "Package | Status | Path" (status: `removed` or `absent`)

---

### `ocx uninstall PACKAGES...`

Removes the `candidates/{tag}` symlink.

| Flag | Purpose |
|------|---------|
| `-d/--deselect` | Also remove `current` symlink |
| `--purge` | Delete the object from disk if no remaining refs |

- Calls `manager.uninstall_all(&packages, deselect, purge)`
- `--purge` removes the content-addressed object only if `refs/` is empty
- Returns `Vec<Option<UninstallResult>>` (`None` = candidate was already absent)
- Report: "Package | Status | Path" (status: `removed`, `purged`, or `absent`)

---

### `ocx clean`

Removes all unreferenced objects (empty `refs/` directory) and stale temp dirs.

| Flag | Purpose |
|------|---------|
| `--dry-run` | Preview without deleting |

- Calls `manager.clean(dry_run)`
- Report: lists of removed objects/temp dirs with type column

---

## Environment & Execution Commands

### `ocx env PACKAGES...`

Prints the resolved environment variables for one or more packages.

| Flag | Purpose |
|------|---------|
| `-p/--platform` | Platform filter (num_args: 0..) |
| `--candidate` | Resolve via candidate symlink |
| `--current` | Resolve via current symlink |

- Default: calls `manager.find_or_install_all()` — **auto-installs** if missing
- `--candidate`/`--current`: calls `manager.find_symlink_all()` — no auto-install
- Uses `package::metadata::env::exporter::Exporter` to resolve vars
- Report: "Key | Value | Type" (type: `constant` or `path`)
- JSON: `{ "entries": [{ "key", "value", "type" }] }`

---

### `ocx exec PACKAGES... -- COMMAND [ARGS]`

Runs a command with the package environment injected.

| Flag | Purpose |
|------|---------|
| `-i/--interactive` | Keep stdin open |
| `--clean` | Start with clean environment (no ambient env) |
| `-p/--platform` | Platform filter (num_args: 0..) |

- Packages specified before `--`, command after
- Calls `manager.find_or_install_all()` — **auto-installs** if missing
- Spawns child with `tokio::process::Command` + resolved env
- Exit code propagated from child process

---

### `ocx shell env PACKAGES...`

Prints shell-specific `export KEY=value` lines (for `eval` in shell init scripts).

| Flag | Purpose |
|------|---------|
| `-s/--shell` | Shell name (or auto-detect from `$SHELL`) |
| `-p/--platform` | Platform filter (num_args: 0..) |
| `--candidate` / `--current` | Symlink resolution mode |

- Default: calls `manager.find_all()` — **no auto-install** (unlike `env`)
- Emits raw shell export lines, not a report table

---

### `ocx shell completion`

Generates shell completion scripts.

| Flag | Purpose |
|------|---------|
| `--shell` | bash/zsh/fish/powershell/elvish (or auto-detect) |

---

### `ocx shell profile add PACKAGES...`

Adds packages to the shell profile manifest (`$OCX_HOME/profile.json`).

| Flag | Purpose |
|------|---------|
| `--candidate` | Pin to `candidates/{tag}` symlink (requires tag in identifier) |
| `--current` | Use `current` symlink (default, floating pointer) |

- Validates symlink exists before adding (suggests `ocx install --select` if missing)
- Duplicate adds update mode in place (idempotent)
- Saves manifest atomically (temp + rename)

---

### `ocx shell profile remove PACKAGES...`

Removes packages from the shell profile.

- Removes matching entries from `profile.json`
- Does not uninstall the package

---

### `ocx shell profile list`

Lists all profiled packages with their status.

- Checks each entry's symlink: `Active` (exists) or `Broken` (missing)
- Report: "Package | Mode | Status | Path"
- JSON: array of `{ package, mode, status, path }` objects

---

### `ocx shell profile load`

Outputs shell export statements for all profiled packages.

| Flag | Purpose |
|------|---------|
| `-s/--shell` | Shell to generate exports for (or auto-detect) |

- Reads `profile.json`, resolves each entry's symlink, reads metadata
- Emits shell-specific `export` lines via `ProfileBuilder`
- Silently skips broken entries (no error output)
- Called from the env file via `eval "$(ocx --offline shell profile load)"`

---

## Index Commands

### `ocx index catalog`

Lists all known repositories in the registry.

| Flag | Purpose |
|------|---------|
| `--with-tags` | Also fetch tags for each repository |

- Calls `default_index.list_repositories(registry)` (respects `--remote`/`--offline`)
- Report: "Repository" or "Repository | Tags"
- JSON: array or keyed object

---

### `ocx index list PACKAGES...`

Lists available tags for packages.

| Flag | Purpose |
|------|---------|
| `--with-platforms` | Include platform info per tag (fetches manifests) |

- Calls `default_index.list_tags(&identifier)`
- `--with-platforms` also calls `default_index.fetch_manifest()` per tag (parallel)
- Report: "Package | Tags" or "Package | Platform | Tags"

---

### `ocx index update PACKAGES...`

Syncs the local index from the remote registry.

- Calls `local_index_mut().update(&remote_index, &identifier)` per package (parallel)
- Updates `$OCX_HOME/index/{registry}/tags/{repo}.json` and manifest cache
- No output except progress spans

---

## Package Commands

### `ocx package pull PACKAGES...`

Downloads packages into the local object store without creating install symlinks.

| Flag | Purpose |
|------|---------|
| `-p/--platform` | Comma-separated platforms |

- Calls `manager.install_all(packages, platforms, candidate=false, select=false)`
- **No symlinks created** — only populates the object store
- Returns `Vec<InstallInfo>` → reported via `api::data::paths::Paths` (content paths)
- Report: "Package | Path" (path = object store content directory)
- Designed for CI environments where reproducibility matters and symlink management is unnecessary

---

### `ocx package create PATH`

Bundles a directory into a distributable archive.

| Flag | Purpose |
|------|---------|
| `--identifier IDENTIFIER` | Optional, used for output filename inference |
| `--platform PLATFORM` | Optional, used for filename inference |
| `-o/--output FILE\|DIR` | Output path or directory |
| `--force` | Overwrite if output exists |
| `-m/--metadata PATH` | Metadata file (inferred if not provided) |
| `-l/--compression-level` | `fast`, `balanced` (default), `best`, `none`, `xz`, `gz` |

- Calls `package::bundle::BundleBuilder::from_path(&path)...create(&output)`
- No task method — calls library directly

---

### `ocx package push IDENTIFIER CONTENT`

Publishes a package archive to the registry.

| Flag | Purpose |
|------|---------|
| `-c/--cascade` | Auto-update major/minor/patch/latest tags |
| `-n/--new` | Skip checks for new packages |
| `-m/--metadata PATH` | Metadata file |
| `-p/--platform PLATFORM` | **(Required)** Platform of this binary |

- Calls `remote_client.push_package(package_info, content_path)`
- `--cascade` computes derived tags and calls `remote_client.copy_manifest_data()` for each
- Requires active registry credentials

---

## CI Commands

### `ocx ci export PACKAGES...`

Exports package environment variables to a CI system's runtime files.

| Flag | Purpose |
|------|---------|
| `-p/--platform` | Comma-separated platforms |
| `--flavor` | CI system (`github-actions`). Auto-detected if omitted. |
| `--candidate` | Resolve via candidate symlink |
| `--current` | Resolve via current symlink |

- Default: calls `manager.find_all()` — **no auto-install**
- `--candidate`/`--current`: calls `manager.find_symlink_all(kind)` — no auto-install
- GitHub Actions: appends PATH entries to `$GITHUB_PATH`, other vars to `$GITHUB_ENV`
- Reads `GITHUB_ACTIONS`, `GITHUB_PATH`, `GITHUB_ENV` from environment
- No report output — logs exported entries via `tracing`

---

## Info Commands

### `ocx version`

Prints the `ocx` version string.

### `ocx info`

Prints version, supported platforms, and detected shell.

---

## Task Method Quick Reference

| Manager Method | Auto-Install | Symlink | Use In |
|----------------|-------------|---------|--------|
| `find_all()` | No | No | `find`, `select` |
| `find_symlink_all(kind)` | No | Yes (candidate/current) | `find --candidate`, `env --candidate` |
| `find_or_install_all()` | **Yes** | No | `env`, `exec` |
| `install_all(candidate=true)` | N/A (is install) | Creates candidate | `install` |
| `install_all(candidate=false)` | N/A (is pull) | No | `package pull` |
| `deselect_all()` | No | Removes current | `deselect` |
| `uninstall_all()` | No | Removes candidate | `uninstall` |
| `clean()` | No | — | `clean` |

---

## Path Resolution Summary

Three modes for locating an installed package:

| Mode | Path | Stable? | Auto-Install? | Commands |
|------|------|---------|---------------|----------|
| Object store (default) | `$OCX_HOME/objects/.../content/` | No (digest changes) | Yes (find_or_install) or No (find) | `exec`, `env`, `find` |
| `--candidate` | `$OCX_HOME/installs/.../candidates/{tag}` | **Yes** | No | `find --candidate`, `env --candidate` |
| `--current` | `$OCX_HOME/installs/.../current` | **Yes** | No | `find --current`, `env --current` |

Use `--candidate` or `--current` when embedding paths in configs, IDE settings, or shell profiles — those paths never change even after updates.

---

## Common User Workflows

### Install and pin in shell profile
```sh
ocx install cmake:3.28 --select
eval "$(ocx shell env cmake:3.28 --current)"
```

### CI: pull and export environment (GitHub Actions)
```sh
ocx package pull cmake:3.28          # fetch to object store, no symlinks
ocx ci export cmake:3.28             # write to $GITHUB_PATH / $GITHUB_ENV
```

### CI: reproducible build with locked index
```sh
# Offline pull from bundled index snapshot
ocx package pull --offline cmake:3.28
ocx exec cmake:3.28 -- cmake --version
```

### Publish a new tool
```sh
ocx package create ./dist -m metadata.json -o cmake-3.29.tar.xz -p linux/amd64
ocx package push myregistry/cmake:3.29_build.1 cmake-3.29.tar.xz -p linux/amd64 --cascade
```

### Upgrade a package
```sh
ocx install cmake:3.29 --select   # installs new version and sets current
ocx uninstall cmake:3.28          # remove old candidate
```

### Switch versions temporarily
```sh
ocx select cmake:3.28             # point current to older version
# ... do work ...
ocx select cmake:3.29             # restore
```

### Clean up disk space
```sh
ocx clean --dry-run               # preview
ocx clean                         # remove all unreferenced objects
```
