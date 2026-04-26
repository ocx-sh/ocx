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

When set, ocx will run in offline mode and will not attempt to fetch any remote information.
If any command requires information that is not already available locally, it will fail with an error.

::: warning
Running `ocx --offline install <pkg>` after a bare `ocx index update <pkg>` (without a prior
online install) fails with an `OfflineManifestMissing` error that names the missing digest.
`ocx index update` writes only tag→digest pointers; it does not download manifest or layer blobs.
Run `ocx install <pkg>` online first to populate the blob cache, then offline installs will work.
:::

### `--remote` {#arg-remote}

When set, tag and catalog lookups query the registry directly, bypassing the local tag store.
Digest-addressed blob reads (manifests and layers already identified by a content digest) still
use the local cache and write newly fetched blobs through to `$OCX_HOME/blobs/`.
Only `$OCX_HOME/tags/` is not updated — the persistent local tag snapshot is left unchanged.

Combining this flag with [`--offline`](#arg-offline) will result in an error.

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

### `--color` {#arg-color}

Controls when to use <Tooltip term="ANSI colors">Escape sequences defined by ECMA-48 / ISO 6429, supported by virtually all modern terminals.</Tooltip> in output.

- `auto` (default): Enable colors when stdout is a terminal and
  [`NO_COLOR`][env-no-color] is not set.
- `always`: Always emit color codes, even when piped.
- `never`: Disable all color output.

The `--color` flag takes the highest precedence over all
color-related environment variables ([`NO_COLOR`][env-no-color],
[`CLICOLOR`][env-clicolor], [`CLICOLOR_FORCE`][env-clicolor-force]).

### `--config` {#arg-config}

Path to an extra [configuration file][config-ref] to load for this invocation.

```shell
ocx --config /path/to/config.toml install cmake:3.28
```

The file layers **on top of** the discovered tier chain — it does not replace it. Settings in the specified file win over system, user, and `$OCX_HOME/config.toml` values, but the discovered tiers still load first. To suppress the discovered chain entirely, combine with [`OCX_NO_CONFIG`][env-no-config]`=1`.

The specified file **must exist** — a missing path is an error ([exit code 79 / NotFound][exit-codes]). This is different from the three discovered tiers, which silently skip missing files.

The same override can be set persistently via [`OCX_CONFIG_FILE`][env-config-file]. When both are set, the `--config` file sits at highest file-tier precedence and wins on conflicting scalars.

See the [Configuration reference][config-ref] for the full precedence table, merge rules, and error messages.

## Exit codes {#exit-codes}

OCX exposes a stable, typed exit-code taxonomy so scripts can discriminate failures without parsing stderr.

Most package tools return 0 on success and 1 on any failure. That forces downstream scripts to either ignore the error category or grep stderr — both are fragile. A CI wrapper cannot distinguish "registry unreachable, retry in 30 seconds" from "package not found, fail the build" without parsing error text that can change.

OCX aligns with BSD [sysexits.h][sysexits-manpage] (codes 64–78) for the standard failure categories, and reserves 79–81 for OCX-specific cases. The numeric values are stable across releases — `case $?` works.

:::info
The sysexits.h convention originates in BSD Unix and is documented at [man.freebsd.org][sysexits-manpage]. It assigns semantic meaning to exit codes 64–78, leaving 79–127 free for tool-specific use. OCX occupies 79–81.
:::

| Code | Name | Mnemonic | When used | Recovery |
|------|------|----------|-----------|----------|
| 0 | Success | — | Successful completion | — |
| 1 | Failure | — | Generic failure — only when no specific code applies | Inspect stderr |
| 64 | UsageError | EX_USAGE | Bad CLI invocation: unknown flag, wrong argument count, invalid syntax | Check the command syntax |
| 65 | DataError | EX_DATAERR | Input data malformed: bad identifier, invalid digest, corrupted manifest | Validate identifiers and file contents |
| 69 | Unavailable | EX_UNAVAILABLE | Required resource unavailable: network down, registry unreachable | Retry; check network and registry URL |
| 74 | IoError | EX_IOERR | I/O error: filesystem permission denied, disk full, read/write failure | Check filesystem permissions and free space |
| 75 | TempFail | EX_TEMPFAIL | Temporary failure that may succeed on retry: rate limit, transient network | Retry with backoff |
| 77 | PermissionDenied | EX_NOPERM | Insufficient permissions: registry 403, filesystem EPERM | Refresh credentials or adjust filesystem permissions |
| 78 | ConfigError | EX_CONFIG | Configuration error: bad config file, missing required field, parse failure | Inspect the config file at the printed path |
| 79 | NotFound | OCX | Resource not found: package 404, explicit config path absent | Pin a different version or correct the path |
| 80 | AuthError | OCX | Authentication failure: registry 401, missing credentials | Refresh or set registry credentials |
| 81 | OfflineBlocked | OCX | Offline mode blocked a network operation (deliberate policy, not a fault) | Re-run online, or populate the local cache first |

Scripts can `case $?` on these stable values:

```shell
ocx install cmake:3.28
case $? in
    0)  echo "installed" ;;
    64) echo "usage error; check flags" ;;
    69) echo "registry unreachable; retry with backoff" ;;
    75) echo "temporary failure; retry later" ;;
    78) echo "bad config; inspect the config file" ;;
    79) echo "not found; pin a different version" ;;
    80) echo "auth failed; refresh credentials" ;;
    81) echo "offline mode active; re-run online" ;;
    *)  echo "unexpected failure (exit $?)"; exit 1 ;;
esac
```

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
| Object store (default) | _(none)_ | `~/.ocx/packages/…/{digest}/content` |
| Candidate symlink | `--candidate` | `~/.ocx/symlinks/…/candidates/{tag}/content` |
| Current symlink | `--current` | `~/.ocx/symlinks/…/current/content` |

`candidates/{tag}` and `current` target the **package root** (the parent of `content/`); the resolved path you see above is the same anchor with `/content` appended. Consumers that need launcher scripts traverse `…/current/entrypoints` from the same anchor, and metadata readers traverse `…/current/metadata.json`.

**Constraints**

- `--candidate`: the package must already be installed. Digest identifiers are rejected — use a tag identifier.
- `--current`: a version must be selected first (via [`select`](#select) or [`install --select`](#install)). Digest identifiers are rejected. The tag portion of the identifier is ignored — only registry and repository are used to locate the symlink.
- `--candidate` and `--current` are mutually exclusive.

## Commands

### `clean` {#clean}

Removes unreferenced objects from the local object store.

An object is unreferenced when nothing points to it — no candidate or current symlink, and no other installed package depends on it. This happens after [`uninstall`](#uninstall) (without `--purge`) or when symlinks are removed manually. When a package with [dependencies][ug-dependencies] is removed, its dependencies may become unreferenced and are cleaned up in the same pass.

::: danger
Do not run `clean` concurrently with other OCX commands. A concurrent install may reference an object that `clean` is about to remove, causing the install to fail.
:::

**Usage**

```shell
ocx clean [OPTIONS]
```

**Options**

- `--dry-run`: Show what would be removed without making any changes.
- `-h`, `--help`: Print help information.

### `deps` {#deps}

Shows the dependency tree for one or more installed packages. Operates on locally-present packages
only — no auto-install. See [Dependencies][ug-dependencies] in the user guide for
background.

**Usage**

```shell
ocx deps [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to inspect. Accepts multiple packages — when given more than one,
  the command builds the combined dependency graph (the same graph [`exec`](#exec) uses for environment
  composition).

**Options**

- `--flat`: Show the resolved evaluation order instead of the tree. This is the exact order
  [`exec`](#exec) and [`env`](#env) use for environment composition — useful for debugging
  unexpected variable values.
- `--why <DEP>`: Explain why a dependency is pulled in. Shows all paths from the given root
  packages to `<DEP>`. Mutually exclusive with `--flat`.
- `--depth <N>`: Limit tree depth. `--depth 1` shows direct dependencies only.
- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `-h`, `--help`: Print help information.

**Default output** is a logical tree showing declared dependencies. Diamond dependencies
(the same package reached via multiple paths) are marked with `(*)` and their subtree is
not expanded again:

```
myapp:1.0 (sha256:aaa1b2c3…)
├── ocx.sh/java:21 (sha256:bbb4e5f6…)
└── ocx.sh/cmake:3.28 (sha256:ccc7d8e9…)
    └── ocx.sh/gcc:13 (sha256:ddd0a1b2…)
```

**`--flat`** shows the combined evaluation order after topological sort and deduplication:

```
Package            Digest
ocx.sh/gcc:13      sha256:ddd0a1b2…
ocx.sh/cmake:3.28  sha256:ccc7d8e9…
ocx.sh/java:21     sha256:bbb4e5f6…
myapp:1.0          sha256:aaa1b2c3…
```

**`--why`** traces all paths from roots to a specific dependency:

```
myapp:1.0 → ocx.sh/cmake:3.28 → ocx.sh/gcc:13
```

### `deselect` {#deselect}

Removes the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its [candidate symlink](../user-guide.md#path-resolution) and object-store content remain intact. To also remove the installed files, use [`uninstall`](#uninstall).

When the deselected package declares [entry points](../guide/entry-points.md), the per-registry `.entrypoints-index.json` ownership row is dropped at the same time. The symlink removal is idempotent — an already-absent link is not an error.

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

If a package declares [dependencies][ug-dependencies], their environment variables are included in the output in [topological order][ug-deps-env] — dependencies before dependents.

In the default mode, packages are auto-installed if not already available locally (including transitive dependencies).
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

Each positional accepts a package reference in one of three forms:

- A bare OCI identifier (`node:20`) — equivalent to the explicit `oci://node:20`.
- An explicit `oci://<identifier>` URI — resolved through the index and auto-installed when missing (unless [`--offline`](#arg-offline) is set).
- A `file://<absolute-package-root>` URI — points at an already-installed package directory under `$OCX_HOME/packages/...`, skipping identifier resolution and the index. This is the form generated [entry point][entry-points] launchers bake; it is also the contract that lets the launcher survive an `ocx select` to a different version (the symlink target moves, the URI does not).

If a package declares [dependencies][ug-dependencies], their environment variables are applied in [topological order][ug-deps-env] before the package's own variables. Refs of either scheme can be mixed in one invocation; env entries layer in the order refs appear on the command line.

**Usage**

```shell
ocx exec [OPTIONS] <REF>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<REF>`: Package references — bare identifier, `oci://<identifier>`, or `file://<absolute-package-root>`.
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

- `-p`, `--platform`: Specify the platforms to use.
- `-i`, `--interactive`: Run in interactive mode, forwarding stdin to the child process.
- `--clean`: Start with a clean environment containing only the package-defined variables, instead of inheriting the current shell environment.
- `-h`, `--help`: Print help information.

::: warning Contract stability
The `file://` URI scheme and `ocx exec` argument shape are load-bearing for every generated launcher on disk. Changing either invalidates existing installs; both are kept stable across releases.
:::

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
Use `--format json` with `jq` to embed the path in a script:

```shell
cmake_root=$(ocx find --candidate --format json cmake:3.28 | jq -r '.["cmake:3.28"]')
```
:::

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS] [REGISTRY...]
```

Lists all packages available in the index. Uses the local index by default; pass [`--remote`](#arg-remote) to query the registry directly. Repository names are always prefixed with their registry in the output (e.g., `ocx.sh/cmake`).

**Arguments**

- `[REGISTRY...]`: Registries to query. Accepts zero or more registry hostnames. Defaults to `OCX_DEFAULT_REGISTRY` (or `ocx.sh`) when omitted.

**Options**

- `--tags`: Include available tags for each package. Slower — requires fetching additional information for each package.

#### `list` {#index-list}

```bash
ocx index list [OPTIONS] <PACKAGE>...
```

Lists available tags for one or more packages.

**Arguments**

- `<PACKAGE>`: Package identifiers to list tags for.

**Options**

- `--platforms`: Shows which platforms are available for the package. Uses the tag from the identifier, or `latest` if none specified.
- `--variants`: Lists unique variant names found in the tags.
- `-h`, `--help`: Print help information.

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Writes tag→digest pointers to `$OCX_HOME/tags/` for the specified packages by querying the
registry directly. No manifest or layer blobs are written to `$OCX_HOME/blobs/`.

When a tagged identifier is used (e.g., `cmake:3.28`), only that single tag's digest pointer is
recorded — the remote tag listing is skipped entirely. This is ideal for lockfile workflows where
the local tag store should contain only explicitly requested tags. When a bare identifier is used
(e.g., `cmake`), digest pointers for all tags are recorded.

After running `ocx index update <pkg>`, an `ocx --offline install <pkg>` will fail with
`OfflineManifestMissing` until the blob cache is populated by a prior online `ocx install <pkg>`.

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

Installs packages into the [object store](../user-guide.md#file-structure-objects) and creates a [candidate symlink](../user-guide.md#path-resolution) for each package, making them available for use by other commands. If a package declares [dependencies][ug-dependencies], all transitive dependencies are downloaded to the object store automatically — only the explicitly requested packages receive install symlinks.

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

#### Entry-point name collisions {#select-entry-point-collision}

If the package being selected declares [entry points](../guide/entry-points.md) and one of the declared names is already contributed to `$PATH` by another currently-selected package, `select` (and `install --select`) refuses to flip `entrypoints-current` and exits with a structured `EntrypointNameCollision` error reporting the conflicting name and the package that already owns it. The exit code is `65` (`DataError`); see [Exit codes](#exit-codes) for the full taxonomy. Resolve the conflict by deselecting the other package — the collision is detected before any state changes, so a failed select leaves the existing `current` and `entrypoints-current` symlinks intact.

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

For each profile entry whose package declares a non-empty `entrypoints` array and whose `entrypoints-current` symlink exists under `$OCX_HOME/symlinks/{registry}/{repo}/`, an additional `PATH` export line is emitted that prepends the symlinked entry-point directory. Entries without `entrypoints` produce only their declared environment variables; entries that have not yet been selected (no `entrypoints-current` symlink) are silently skipped, so profile load never points `$PATH` at a missing directory. See the [entry points guide](../guide/entry-points.md#path) for the full PATH-integration model.

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

- `-d`, `--deselect`: Also remove the [current symlink](../user-guide.md#path-resolution) and the `entrypoints-current` symlink (when present). Equivalent to running `ocx deselect` after uninstall — see [`deselect`](#deselect) for the full cleanup behavior.
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

Bundles a local directory into a compressed package archive ready for publishing. If the package
metadata includes [dependencies][ug-dependencies], the declared dependency graph is
validated for cycles at this stage — catching errors before the package reaches the registry.

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
- `-j`, `--threads <N>`: Number of compression threads. `0` (default) auto-detects from available CPU cores (capped at 16). `1` forces single-threaded compression. Only affects LZMA (`.tar.xz`) compression.
- `-h`, `--help`: Print help information.

#### `pull` {#package-pull}

Downloads packages into the local [object store][fs-objects] without creating
[install symlinks][fs-symlinks].

Unlike [`install`](#install), this command only populates the content-addressed object store — no
candidate or current symlinks are created. If a package declares [dependencies][ug-dependencies], all transitive dependencies are pulled into the object store as well. This is the recommended primitive for CI environments where reproducibility matters and symlink management is unnecessary.

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

Publishes a package to the registry as zero or more layers. Each layer is uploaded as an OCI blob and recorded in a single image manifest, in the order given on the command line. A zero-layer push produces a config-only OCI artifact (referrer-only / description-only manifest) and requires `--metadata`.

**Usage**

```shell
ocx package push [OPTIONS] --platform <PLATFORM> <IDENTIFIER> <LAYERS>...
```

**Arguments**

- `<IDENTIFIER>`: Package identifier including the tag, e.g. `cmake:3.28.1_20260216120000`.
- `<LAYERS>...`: Zero or more layers, in order (base layer first, top layer last). Each layer is either:
  - a path to a pre-built archive file (`.tar.gz`, `.tgz`, `.tar.xz`, or `.txz`), or
  - a digest reference of the form `sha256:<hex>.<ext>` (e.g. `sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08.tar.gz`) pointing at a layer that already exists in the target registry. The `<ext>` suffix is mandatory — OCI blob HEADs do not carry the original media type, so the publisher must declare it. Bare digests are rejected.
  - Extension aliases: `.tgz` is accepted as an alias for `.tar.gz`, and `.txz` for `.tar.xz`. The canonical forms `tar.gz` / `tar.xz` are what ocx emits internally — aliases are normalized on parse.
  - To force file interpretation of a pathological filename that happens to match the digest shape, prefix it with `./` (e.g. `./sha256:abc….tar.gz`).
  - Omitting all layers produces a config-only OCI artifact with `layers: []`, valid for referrer-only / description-only manifests. `--metadata` is required in that case.

**Options**

- `-p`, `--platform <PLATFORM>`: Target platform of the package (required).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `cmake:3.28.1_20260216120000` automatically re-points the rolling ancestors (`cmake:3.28.1`, `cmake:3.28`, `cmake:3`, and `cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the first file layer (e.g. `pkg.tar.gz` → `pkg-metadata.json`). Required when no file layers are provided (all layers are digest references, or the layer list is empty).
- `-h`, `--help`: Print help information.

::: tip Layer reuse
Digest-referenced layers are not re-uploaded — ocx only HEADs the registry to verify they exist. This is the foundation of the [layer dedup model](../user-guide.md#file-structure-layers): a base layer pushed once can be referenced from any number of subsequent packages by digest.

```shell
# Push a fresh base + tool combination
ocx package push -p linux/amd64 mytool:1.0.0 base.tar.gz tool.tar.gz

# Reuse the same base by digest in a later release.
# The digest is the full 64-char sha256 hex written verbatim —
# the ellipsis is shown here only to keep the example short.
ocx package push -p linux/amd64 mytool:1.0.1 sha256:<hex>.tar.gz newtool.tar.gz
```
:::

::: warning Bring your own archives
`ocx package push` does not bundle a directory for you. Each file layer must be a pre-built archive. Re-bundling the same content yields a non-deterministic digest (timestamps, compression entropy) and defeats layer reuse — use [`ocx package create`](#package-create) to produce a stable archive once, then push and reference it by digest from later commands.
:::

#### `describe` {#package-describe}

Pushes package description metadata (title, description, keywords, README, logo) to the registry.

**Usage**

```shell
ocx package describe [OPTIONS] <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier (repository only; tag is ignored).

**Options**

- `--readme <PATH>`: Path to a README markdown file.
- `--logo <PATH>`: Path to a logo image (PNG or SVG).
- `--title <TITLE>`: Short display title for the package catalog.
- `--description <TEXT>`: One-line summary.
- `--keywords <LIST>`: Comma-separated search keywords.
- `-h`, `--help`: Print help information.

At least one of the above metadata options must be provided.

#### `info` {#package-info}

Displays description metadata for a package from the registry.

**Usage**

```shell
ocx package info [OPTIONS] <IDENTIFIER>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier (repository only).

**Options**

- `--save-readme <PATH>`: Save the README to a file or directory.
- `--save-logo <PATH>`: Save the logo to a file or directory.
- `-h`, `--help`: Print help information.

<!-- external -->
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[sysexits-manpage]: https://man.freebsd.org/cgi/man.cgi?sysexits

<!-- environment -->
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-no-config]: ./environment.md#ocx-no-config
[env-config-file]: ./environment.md#ocx-config-file

<!-- reference -->
[config-ref]: ./configuration.md

<!-- internal -->
[entry-points]: ./metadata.md#entry-points
[exit-codes]: #exit-codes
[fs-objects]: ../user-guide.md#file-structure-objects
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
[fs-index]: ../user-guide.md#file-structure-index
[ug-dependencies]: ../user-guide.md#dependencies
[ug-deps-env]: ../user-guide.md#dependencies-environment
