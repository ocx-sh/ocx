---
layout: doc
outline: deep
---
# Command Line

## General Options

The following options are available for all commands and must be specified before the command name.

### `--log-level` {#arg-log-level}

The log level for OCX, which can be set to one of the following values:

- `off`: No logs will be emitted.
- `error`: Only error messages will be emitted.
- `warn`: Warning messages and error messages will be emitted.
  This is the default log level.
- `info`: Informational messages, warning messages, and error messages will be emitted.
- `debug`: Debug messages, informational messages, warning messages, and error messages will be emitted.
- `trace`: All messages will be emitted, including trace messages.

### `--format` {#arg-format}

When set, ocx will output information in the specified format instead of plain text.
Supported formats are:

- `plain` (default): Human-readable plain text.
- `json`: Machine-readable JSON format.

The available data depends on the command being executed.

### `--offline` {#arg-offline}

When set, ocx will run in offline mode, which will not attempt to fetch any remote information.
If any command requires information that is not already available locally, it will fail with an error.

### `--remote` {#arg-remote}

When set, ocx will use the remote index by default instead of the local index.
Combining this flag with [`--offline`](#arg-offline) will most likely result in an error.

### `--index` {#arg-index}

Override the path to the [local index][fs-index] directory for this invocation.
By default, ocx reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`).

```shell
ocx --index /path/to/bundled/index install cmake:3.28
```

This flag is intended for environments where the [local index][fs-index] is bundled alongside a
tool rather than living inside `OCX_HOME` — for example inside a [GitHub Action][github-actions-docs],
[Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features] that ships a frozen
index snapshot as part of its release.

The flag has no effect when [`--remote`](#arg-remote) is set.

The same override can be set persistently via the [`OCX_INDEX`][env-ocx-index] environment
variable. The `--index` flag takes precedence when both are set.

### `--candidate` / `--current` {#path-resolution}

The `--candidate` and `--current` flags are available on commands that resolve a package's content
path, for example [`env`](#env), [`find`](#find), or [`shell env`](#shell-env).

By default these commands use the content-addressed path in the
[object store](../user-guide.md#file-structure-objects) — a hash-derived directory that changes
whenever the package is reinstalled at a different version. Use `--candidate` or `--current` to
resolve via a [stable install symlink](../user-guide.md#path-resolution) instead, whose path never changes regardless of the underlying
object. This is useful for paths embedded in editor configs, Makefiles, or shell profiles that
should survive package updates.

| Mode | Flag | Path |
|------|------|------|
| Object store (default) | _(none)_ | `~/.ocx/objects/…/{digest}/content` |
| Candidate symlink | `--candidate` | `~/.ocx/installs/…/candidates/{tag}/content` |
| Current symlink | `--current` | `~/.ocx/installs/…/current/content` |

**Constraints**

- `--candidate`: the package must already be installed. Digest identifiers are rejected — use a tag identifier.
- `--current`: a version must be selected first (via [`select`](#select) or [`install --select`](#install)). Digest identifiers are rejected. The tag portion of the identifier is ignored — only registry and repository are used to locate the symlink.
- `--candidate` and `--current` are mutually exclusive.

## Commands

### `clean` {#clean}

Removes unreferenced objects from the local object store.

An object is unreferenced when its `refs/` directory is empty — no candidate or current symlink points to it. This happens after [`uninstall`](#uninstall) (without `--purge`) or when symlinks are removed manually.

**Usage**

```shell
ocx clean [OPTIONS]
```

**Options**

- `--dry-run`: Show what would be removed without making any changes.
- `-h`, `--help`: Print help information.

### `deselect` {#deselect}

Removes the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its [candidate symlink](../user-guide.md#path-resolution) and object-store content remain intact. To also remove the installed files, use [`uninstall`](#uninstall).

**Usage**

```shell
ocx deselect <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to deselect.

**Options**

- `-h`, `--help`: Print help information.

### `env` {#env}

Print the resolved environment variables for one or more packages.

Plain format outputs an aligned table with `Key`, `Type` and `Value` columns.
JSON format outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.

In the default mode, packages are auto-installed if not already available locally.
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

**Usage**

```shell
ocx env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `-h`, `--help`: Print help information.

### `exec` {#exec}

Executes a command within the environment of one or more packages.
Packages are auto-installed if not already available locally (unless [`--offline`](#arg-offline) is set).

**Usage**

```shell
ocx exec [OPTIONS] <PACKAGE>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<PACKAGE>`: Package identifiers to run within.
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

- `-p`, `--platform`: Specify the platforms to use.
- `-i`, `--interactive`: Run in interactive mode, forwarding stdin to the child process.
- `--clean`: Start with a clean environment containing only the package-defined variables, instead of inheriting the current shell environment.
- `-h`, `--help`: Print help information.

### `find` {#find}

Resolves one or more packages and prints their content directory paths.

Resolves the content directory for each package. See [Path Resolution](#path-resolution) for the difference between the default object-store path and the stable symlink modes.

No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx find [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve.

**Options**

- `-p`, `--platform`: Platforms to consider when resolving. Defaults to the current platform. Ignored when `--candidate` or `--current` is set.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `-h`, `--help`: Print help information.

::: tip
Use `--json` with `jq` to embed the path in a script:

```shell
cmake_root=$(ocx find --candidate --json cmake:3.28 | jq -r '.["cmake:3.28"]')
```
:::

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS]
```

Lists all packages available in the index. Uses the local index by default; pass [`--remote`](#arg-remote) to query the registry directly.

**Options**

- `--with-tags`: Include available tags for each package. Slower — requires fetching additional information for each package.

#### `list` {#index-list}

```bash
ocx index list [OPTIONS] <PACKAGE>...
```

Lists available tags for one or more packages.

**Arguments**

- `<PACKAGE>`: Package identifiers to list tags for.

**Options**

- `--with-platforms`: Include platform availability for each tag. Slower — requires fetching each manifest individually.
- `-h`, `--help`: Print help information.

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Updates the local index by fetching the latest information from the remote index for the specified packages.

When a tagged identifier is used (e.g., `cmake:3.28`), only that single tag's digest and manifest are fetched — the remote tag listing is skipped entirely. This is ideal for lockfile workflows where the local index should contain only explicitly requested tags. When a bare identifier is used (e.g., `cmake`), all tags are fetched as before.

**Arguments**

- `<PACKAGE>`: Package identifiers to update in the local index for. Include a tag to update only that tag; omit the tag to update all tags.

### `info` {#info}

Prints build information: the ocx version, supported platforms, and the detected shell.

**Usage**

```shell
ocx info
```

### `install` {#install}

Downloads and installs one or more packages into the local object store.

Installs packages into the [object store](../user-guide.md#file-structure-objects) and creates a [candidate symlink](../user-guide.md#path-resolution) for each package, making them available for use by other commands.

**Usage**

```shell
ocx install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--select`: After installing, update the [current symlink](../user-guide.md#path-resolution) for each package to point to the newly installed version. Required before using `ocx env --current` or `ocx shell env --current`.
- `-h`, `--help`: Print help information.

### `select` {#select}

Selects one or more packages as the current version by updating the [current symlink](../user-guide.md#path-resolution).

Each package is resolved using the [selected index](../user-guide.md#indices-selected).
No downloading is performed — the package must already be installed.

**Usage**

```shell
ocx select [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to select.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `-h`, `--help`: Print help information.

::: tip
`ocx install --select` installs and selects in one step.
:::

See [path resolution modes](../user-guide.md#path-resolution) for how the `current` symlink is used downstream.

### `shell` {#shell}

#### `env` {#shell-env}

Generate shell export statements for the environment variables declared by one or more packages.
Intended to be evaluated by the shell:

```shell
eval "$(ocx shell env mypackage)"
```

This command does not auto-install packages — if a package is not already available locally it will fail with an error.
See [Path Resolution](#path-resolution) for the `--candidate` and `--current` modes.

**Usage**

```shell
ocx shell env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to generate environment variable export statements for.

**Options**

- `-p`, `--platform`: Target platforms to consider. Auto-detected by default.
- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Usage**

```shell
ocx shell completion [OPTIONS]
```

**Options**

- `--shell <SHELL>`: The shell to generate the completions for (e.g., `bash`, `zsh`, `fish`).
  By default, ocx will attempt to auto-detect the shell and generate the appropriate completions.

#### `profile` {#shell-profile}

Manage the shell profile — packages whose environment variables are loaded into every new shell session.

The profile is stored in `$OCX_HOME/profile.json`. The env file (sourced by your shell profile) calls `ocx --offline shell profile load` at startup to resolve all profiled packages and emit shell exports.

##### `add` {#shell-profile-add}

Add one or more packages to the shell profile.

**Usage**

```shell
ocx shell profile add [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to add to the profile.

**Options**

- `--candidate`: Resolve via the `candidates/{tag}` symlink (default, pinned). Requires a tag in the identifier.
- `--current`: Resolve via the `current` symlink (floating pointer set by `ocx select`).
- `--content`: Resolve via the content-addressed object store path. The path changes whenever the package is reinstalled at a different version.

Packages that are not yet installed will be auto-installed. For `--current` mode, install with `--select` first to create the `current` symlink (`ocx install --select <pkg>`).

##### `remove` {#shell-profile-remove}

Remove one or more packages from the shell profile.

**Usage**

```shell
ocx shell profile remove <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to remove from the profile.

This does not uninstall the package — it only removes it from the profile manifest.

##### `list` {#shell-profile-list}

List all packages in the shell profile with their status.

**Usage**

```shell
ocx shell profile list
```

Shows each profiled package with its resolution mode (`candidate`, `current`, or `content`), status (`active` or `broken`), and resolved path.

##### `load` {#shell-profile-load}

Output shell export statements for all profiled packages.

**Usage**

```shell
ocx shell profile load [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell to generate exports for. Auto-detected if not specified.

Reads `$OCX_HOME/profile.json` and emits shell-specific export lines for each package's declared environment variables. Broken entries are silently skipped. This command is intended to be called from the env file via `eval`:

```shell
eval "$(ocx --offline shell profile load)"
```

### `uninstall` {#uninstall}

Removes the installed candidate for one or more packages.

Removes the [candidate symlink](../user-guide.md#path-resolution) and its back-reference. Object-store content is preserved unless `--purge` is given. To also remove the current symlink, pass `--deselect` or run [`deselect`](#deselect) separately. To remove all unreferenced objects at once, use [`clean`](#clean).

**Usage**

```shell
ocx uninstall [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to uninstall.

**Options**

- `-d`, `--deselect`: Also remove the [current symlink](../user-guide.md#path-resolution). Equivalent to running `ocx deselect` after uninstall.
- `--purge`: Delete the object from the store when no other references remain after uninstall.
- `-h`, `--help`: Print help information.

### `version` {#version}

Prints the ocx version number.

**Usage**

```shell
ocx version
```

### `ci` {#ci}

#### `export` {#ci-export}

Exports the environment variables declared by one or more packages directly into a CI system's
runtime files. For [GitHub Actions][github-actions-docs], this appends path entries to
`$GITHUB_PATH` and other variables to `$GITHUB_ENV`.

The CI flavor is auto-detected from the environment (e.g. `GITHUB_ACTIONS=true`) but can be
overridden with `--flavor`.

This command does not auto-install packages — if a package is not already available locally it
will fail with an error. In CI workflows, use [`package pull`](#package-pull) before `ci export`.

**Usage**

```shell
ocx ci export [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to export the environment for.

**Options**

- `--flavor <FLAVOR>`: CI system to target. Currently supported: `github-actions`. Auto-detected if omitted.
- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`, `--current`: Path resolution mode — see [Path Resolution](#path-resolution).
- `-h`, `--help`: Print help information.

::: tip
Pair with [`package pull`](#package-pull) for a minimal CI setup:

```shell
ocx package pull cmake:3.28
ocx ci export cmake:3.28
```
:::

### `package` {#package}

#### `create` {#package-create}

Bundles a local directory into a compressed package archive ready for publishing.

**Usage**

```shell
ocx package create [OPTIONS] <PATH>
```

**Arguments**

- `<PATH>`: Path to the directory to bundle.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier, used to infer the output filename when `--output` is a directory.
- `-p`, `--platform <PLATFORM>`: Platform of the package, used to infer the output filename.
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform. The file extension controls the compression algorithm: `.tar.xz` (LZMA, default) or `.tar.gz` (Gzip).
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to a metadata file to bundle with the package. When provided, it is copied as a sidecar file next to the output archive. If omitted, no metadata sidecar is written.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`. Applies to whichever algorithm is selected.
- `-h`, `--help`: Print help information.

#### `pull` {#package-pull}

Downloads packages into the local [object store][fs-objects] without creating
[install symlinks][fs-installs].

Unlike [`install`](#install), this command only populates the content-addressed object store — no
candidate or current symlinks are created. This is the recommended primitive for CI environments
where reproducibility matters and symlink management is unnecessary.

**Usage**

```shell
ocx package pull [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to pull.

**Options**

- `-p`, `--platform`: Target platforms to consider. Defaults to the current platform.
- `-h`, `--help`: Print help information.

::: tip
`package pull` reports the content-addressed object store path for each package — the same
digest-derived path that [`find`](#find) and [`exec`](#exec) resolve to. Two pulls of the same
digest are safe to run concurrently.
:::

#### `push` {#package-push}

Publishes a package bundle to the registry. A [candidate symlink](../user-guide.md#path-resolution) is created locally after a successful push.

**Usage**

```shell
ocx package push [OPTIONS] <IDENTIFIER> <CONTENT>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier including the tag, e.g. `cmake:3.28.1_20260216120000`.
- `<CONTENT>`: Path to the package bundle (`.tar.xz`) to publish.

**Options**

- `-p`, `--platform <PLATFORM>`: Target platform of the package (required).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `cmake:3.28.1_20260216120000` automatically re-points the rolling ancestors (`cmake:3.28.1`, `cmake:3.28`, `cmake:3`, and `cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the bundle.
- `-h`, `--help`: Print help information.

<!-- external -->
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/

<!-- environment -->
[env-ocx-index]: ./environment.md#ocx-index

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-objects
[fs-installs]: ../user-guide.md#file-structure-installs
[fs-index]: ../user-guide.md#file-structure-index
