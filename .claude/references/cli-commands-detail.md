# OCX CLI Commands — Detailed Reference

Per-command documentation with flags, task methods, and workflow examples.
For the quick-reference index, see `.claude/rules/cli-commands.md`.

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
- Report: "Key | Type | Value" (type: `constant` or `path`)
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
| `--platforms` | Show platforms for the specified tag (or `latest`) |
| `--variants` | List unique variant names from tags |

- Calls `default_index.list_tags(&identifier)`
- `--platforms` fetches manifest for the specified tag (or `latest`), not all tags
- Report: "Package | Tag", "Package | Platform", or "Package | Variant"

---

### `ocx index update PACKAGES...`

Syncs the local index from the remote registry.

- **Tagged identifier** (e.g., `cmake:3.28`): fetches only that tag's digest and manifest — no tag listing. Merges into existing `tags/{repo}.json`.
- **Bare identifier** (e.g., `cmake`): fetches all tags (existing behavior).
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
| `-j/--threads` | Compression threads (0 = auto-detect, 1 = single-threaded). Default: 0 (all cores, capped at 16) |

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

### `ocx package describe IDENTIFIER`

Pushes package description metadata (title, description, keywords, README, logo) to the registry.

| Flag | Purpose |
|------|---------|
| `--readme PATH` | Path to a README markdown file |
| `--logo PATH` | Path to a logo image (PNG or SVG) |
| `--title TITLE` | Short display title |
| `--description TEXT` | One-line summary |
| `--keywords LIST` | Comma-separated search keywords |

- At least one metadata option must be provided
- Identifier is repository only; tag is ignored

---

### `ocx package info IDENTIFIER`

Displays description metadata for a package from the registry.

| Flag | Purpose |
|------|---------|
| `--save-readme PATH` | Save the README to a file or directory |
| `--save-logo PATH` | Save the logo to a file or directory |

- Identifier is repository only

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
