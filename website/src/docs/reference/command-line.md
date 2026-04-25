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

### `--quiet` {#arg-quiet}

Alias: `-q`.

Suppresses the structured stdout report that every command emits — tables in plain
mode, the JSON document in `--format json` mode. Errors, warnings, and progress
spinners continue to surface on stderr.

Quiet is opt-in and orthogonal to [`--format`](#arg-format). Use it when calling
ocx as a step in a larger pipeline that only cares about the exit code, or when
chaining commands where intermediate output would clutter logs.

```shell
# Pre-warm the project store in CI without dumping a table per package.
ocx --quiet pull
```

The same toggle is available via the [`OCX_QUIET`][env-ocx-quiet] environment
variable; the flag wins when both are set.

### `--jobs` {#arg-jobs}

Caps the number of root packages pulled in parallel. Applies to every command
that fans out through `pull_all` — `install`, `pull`, `package pull`, `exec`
(when it auto-installs missing tools), and the env-composition path of `env`.

The cap acts on the **outer dispatch only**: transitive dependencies and OCI
layer extraction stay unbounded so a child pull never deadlocks waiting for a
permit held by its own ancestor. Singleflight dedup and per-package file locks
already protect the registry against duplicate work.

| Value | Meaning |
|-------|---------|
| (unset) | Unbounded. Every root package spawns immediately — legacy behavior. |
| `0` | Use all logical cores (matches [GNU `parallel -j 0`][gnu-parallel-j0]). |
| `N > 0` | Cap at `N` concurrent root pulls. |
| Negative | Rejected at parse time. |

OCX intentionally diverges from Cargo on `--jobs 0`: GNU Parallel's "saturate
this machine" convention is more useful in CI matrices where the runner has a
variable CPU count and the user wants the cap computed for them.

The same value can be set persistently via [`OCX_JOBS`][env-ocx-jobs]. The CLI
flag wins when both are set.

```shell
# Cap parallelism on a constrained runner.
ocx --jobs 2 install cmake:3.28 ripgrep:14
```

### `--color` {#arg-color}

Controls when to use <Tooltip term="ANSI colors">Escape sequences defined by ECMA-48 / ISO 6429, supported by virtually all modern terminals.</Tooltip> in output.

- `auto` (default): Enable colors when stdout is a terminal and
  [`NO_COLOR`][env-no-color] is not set.
- `always`: Always emit color codes, even when piped.
- `never`: Disable all color output.

The `--color` flag takes the highest precedence over all
color-related environment variables ([`NO_COLOR`][env-no-color],
[`CLICOLOR`][env-clicolor], [`CLICOLOR_FORCE`][env-clicolor-force]).

### `--project` {#arg-project}

Path to the project-level `ocx.toml` (project-tier toolchain config).

When set, OCX reads this file as the project tier and skips the CWD walk entirely. Any filename is accepted (not just `ocx.toml`), which is useful for fixtures and integration tests.

The same override can be set persistently via the [`OCX_PROJECT_FILE`][env-project-file] environment variable. To disable project-file discovery entirely — including the `OCX_PROJECT_FILE` variable but not an explicit `--project` flag — set [`OCX_NO_PROJECT`][env-no-project]`=1`.

**Symlink policy:** Paths supplied via `--project` or `OCX_PROJECT_FILE` are trusted and followed through symlinks. Paths discovered by the CWD walk reject symlinks to prevent directory-traversal redirection.

**Error cases:** A missing explicit path exits with code 79 ([`NotFound`][exit-codes]). A path that exists but cannot be read (permission denied, not a regular file) exits with code 74 ([`IoError`][exit-codes]).

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
| Object store (default) | _(none)_ | `~/.ocx/objects/…/{digest}/content` |
| Candidate symlink | `--candidate` | `~/.ocx/symlinks/…/candidates/{tag}/content` |
| Current symlink | `--current` | `~/.ocx/symlinks/…/current/content` |

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
Packages are auto-installed if not already available locally (unless [`--offline`](#arg-offline) is set). If a package declares [dependencies][ug-dependencies], their environment variables are applied in [topological order][ug-deps-env] before the package's own variables.

The command composes its tool set from two sources: project-tier groups selected with `--group` (resolved via the digest-pinned `ocx.lock`) and explicit positional packages (resolved via the index, tag-style). Right-most positional binding wins over both group entries and earlier positionals. At least one of `--group` or a positional `<PACKAGE>` must be supplied.

**Usage**

```shell
ocx exec [OPTIONS] [[name=]PACKAGE...] -- <COMMAND> [ARGS...]
```

**Arguments**

- `[name=]PACKAGE`: Package identifier (zero or more). The optional `name=` prefix overrides the binding name targeted by the override; without it, the binding name is inferred from the identifier's repository basename (`cmake:3.29` → `cmake`, `ghcr.io/acme/foo:1` → `foo`). Optional when `--group` is supplied; otherwise required.
- `<COMMAND>`: The command to execute within the package environment.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

- `-p`, `--platform`: Specify the platforms to use.
- `-i`, `--interactive`: Run in interactive mode, forwarding stdin to the child process.
- `--clean`: Start with a clean environment containing only the package-defined variables, instead of inheriting the current shell environment.
- `-g`, `--group <NAME>`: Select tool group(s) from the project [`ocx.lock`](../user-guide.md#project-toolchain-lock). Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table. Requires an `ocx.toml` in the working directory or its parents; when supplied, the lock-staleness gate fires and a stale lock exits 65.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Command exited successfully (`exec` propagates the wrapped command's exit code). |
| _N_ | Wrapped command exited with code _N_ — `exec` forwards the child status verbatim. |
| 64 | Usage error: empty `--group` segment, no group or package supplied, unknown group name, or `--group` without an `ocx.toml` in scope. |
| 65 | `ocx.lock` is stale (declaration_hash mismatch with `ocx.toml`); run [`ocx lock`](#lock). |
| 78 | `--group` was selected but `ocx.lock` was not found at the expected path; run [`ocx lock`](#lock) to create it. |

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

### `generate` {#generate}

Parent command for project-tier generators. Each subcommand writes a single configuration file in the current directory based on the project's `ocx.toml`.

**Usage**

```shell
ocx generate <SUBCOMMAND> [OPTIONS]
```

**Options**

- `-h`, `--help`: Print help information.

The only subcommand today is [`direnv`](#generate-direnv); additional generators (e.g. `gitignore`, `editorconfig`) may land in future releases.

#### `direnv` {#generate-direnv}

Writes a `.envrc` file in the current directory that wires [`ocx shell-hook`](#shell-hook) into [direnv](https://direnv.net/). After running `ocx generate direnv`, run `direnv allow` in the same directory to activate the hook. The generated `.envrc` watches `ocx.toml` and `ocx.lock`, so direnv re-runs the hook whenever either file changes.

**Usage**

```shell
ocx generate direnv [OPTIONS]
```

**Options**

- `--force`: Overwrite an existing `.envrc` in the current directory. Without this flag, an existing file causes the command to exit with a `ConfigError` (78) and leave the file untouched.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `.envrc` written successfully. |
| 74 | I/O error writing `.envrc`. |
| 78 | `.envrc` already exists and `--force` was not given. |

### `hook-env` {#hook-env}

Kept at the top level (not under `ocx shell …`) for minimal invocation cost — the prompt-hook runs `hook-env` on every prompt, where startup latency adds up.

Stateful prompt-hook entry point. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, walks the actually-installed default-group tools, and emits shell export lines only when the resolved set has changed since the last invocation. The fingerprint that drives the fast path lives in the `_OCX_APPLIED` environment variable (a `v1:<sha256-hex>` token); when the fingerprint matches the previous invocation's value, the command exits 0 with no output.

The command is invoked from the shell's prompt-hook mechanism — see [`ocx shell init`](#shell-init) for the per-shell wiring snippet. It never contacts the network and never installs missing tools; tools that are declared in the lock but not present in the object store produce a one-line stderr note ("# ocx: cmake not installed; run `ocx pull` to fetch") and are skipped.

**Usage**

```shell
ocx hook-env [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, fast path, or fresh export emitted). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

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

### `lock` {#lock}

Resolves every tool tag in the nearest `ocx.toml` to a pinned OCI manifest digest and writes the result to `ocx.lock` next to it. The command is fully transactional — either every tool resolves successfully and the file is rewritten atomically, or nothing is written and the previous `ocx.lock` survives unchanged.

The lock carries a `declaration_hash` over the canonicalized [RFC 8785 JCS](https://www.rfc-editor.org/rfc/rfc8785) of `ocx.toml`. Downstream commands ([`ocx pull`](#pull), [`ocx exec`](#exec)) consult this hash to detect when the lock is stale relative to the source declaration. When the resolved content of every tool is unchanged between two `ocx lock` runs, the file's `generated_at` timestamp is preserved verbatim — the byte-stable output keeps version-control diffs minimal.

After a successful write, the command checks whether the project's `.gitattributes` declares `ocx.lock merge=union` and emits a one-line stderr advisory when it does not, helping prevent merge conflicts on team projects.

**Usage**

```shell
ocx lock [OPTIONS]
```

**Options**

- `-g`, `--group <NAME>`: Restrict resolution to the named group(s). Repeatable and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects the top-level `[tools]` table. When omitted, every `[tools]` and `[group.*]` entry is resolved.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | `ocx.lock` written (or preserved if content was unchanged). |
| 64 | Missing `ocx.toml`, unknown `--group` name, or empty comma segment. |
| 69 | Registry unreachable while resolving advisory tags. |
| 74 | I/O error writing `ocx.lock`. |
| 78 | Existing `ocx.lock` is malformed (parse error) or `ocx.toml` schema-invalid. |
| 79 | Tag unresolvable during resolution (package not found in registry after retries). |
| 80 | Authentication failure against the registry. |

### `pull` {#pull}

Pre-warms the [object store][fs-objects] from the project `ocx.lock` without
creating [install symlinks][fs-symlinks]. Distinct from
[`package pull`](#package-pull): this is the **project-tier** entry point — every
tool comes from the digest-pinned lock, never from the index — making it the
recommended primitive for reproducible CI setups.

`ocx pull` is read-only on `ocx.lock`. Re-resolution lives in `ocx update`;
rewriting from the config lives in `ocx lock`.

**Usage**

```shell
ocx pull [OPTIONS]
```

**Options**

- `-g`, `--group <NAME>`: Restrict the pull to one or more named groups. Repeatable
  and comma-separated (`-g ci,lint -g release`). The reserved name `default` selects
  the top-level `[tools]` table. When omitted, every entry from the lock is pulled.
- `--dry-run`: Print which locked tools are already cached vs. would be fetched,
  then exit without writing to the store.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (or empty group filter — nothing to pull). |
| 64 | Missing `ocx.toml`, unknown `--group` name, or empty comma segment. |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 78 | `ocx.toml` present but `ocx.lock` is missing — run `ocx lock` first. |

#### Dry-run preview {#pull-dry-run}

`ocx pull --dry-run` resolves each locked tool through the local index
(cache-first, like the real pull does) and reports whether it is already in the
store. The store is never modified. Combine with [`--offline`](#arg-offline) to
forbid the cache-miss network probe entirely.

```shell
$ ocx pull --dry-run
Package                         Status       Path
localhost:5000/cmake@sha256:... cached       /home/me/.ocx/packages/.../content
localhost:5000/ripgrep@sha256:..would-fetch  -
```

The staleness gate fires ahead of the dry-run branch, so a stale lock still
exits 65 — the preview is not a way to bypass `declaration_hash` validation.
The output respects [`--format json`](#arg-format) and [`--quiet`](#arg-quiet).

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

### `shell-hook` {#shell-hook}

Kept at the top level (not under `ocx shell …`) for minimal invocation cost — direnv runs `shell-hook` on every directory change, where startup latency adds up.

Stateless export generator for the project toolchain. Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, looks up every default-group tool in the local object store, and prints shell-specific export lines for the resolved environment. Unlike [`hook-env`](#hook-env), this command does not consult or update `_OCX_APPLIED` — it emits a fresh export block on every invocation, leaving the diffing/caching to the caller (typically [direnv](https://direnv.net/)).

The command never contacts the network and never installs missing tools. Tools missing from the object store produce a one-line stderr note and are skipped; a stale lock produces a stderr warning but the stale digests are still used. When no project `ocx.toml` is found in scope, the command exits 0 with no output.

**Usage**

```shell
ocx shell-hook [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Success (no project, or exports emitted). |
| 65 | `ocx.lock` is stale (declaration_hash mismatch — run `ocx lock`). |
| 74 | I/O error during resolution. |
| 78 | Parse error reading `ocx.toml` or `ocx.lock`. |

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

#### `init` {#shell-init}

Prints a shell-specific init snippet that wires [`ocx hook-env`](#hook-env) into the shell's prompt cycle. The snippet is a pure code generator — it never reads the project, never touches disk, and never contacts the network. Source the output once from your shell rc (or pipe directly via `>>`), and every subsequent prompt re-evaluates the project toolchain.

The exact mechanism differs per shell: Bash inserts into `PROMPT_COMMAND`, Zsh registers a `precmd` hook via `add-zsh-hook`, Fish defines a function `--on-event fish_prompt`, and Nushell writes a closure into `$env.config.hooks.pre_prompt` (saved to `$env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu`). All four are idempotent — sourcing the snippet repeatedly does not duplicate the hook.

**Usage**

```shell
ocx shell init <SHELL>
```

**Arguments**

- `<SHELL>`: Target shell. Required. Accepts `bash`, `zsh`, `fish`, `nushell`. Other recognized shells (`ash`, `ksh`, `dash`, `elvish`, `powershell`, `batch`) print a `# ocx shell init does not yet ship a prompt-hook snippet for this shell` placeholder.

**Options**

- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Snippet printed (or placeholder comment for unsupported shells — always 0; the command is pure code generation). |

#### `profile` {#shell-profile}

Manage the shell profile — packages whose environment variables are loaded into every new shell session.

The profile is stored in `$OCX_HOME/profile.json`. The env file (sourced by your shell profile) calls `ocx --offline shell profile load` at startup to resolve all profiled packages and emit shell exports.

::: warning Deprecated in v1; removed in v2
The `shell profile add`, `remove`, `list`, and `load` subcommands are deprecated in this release and will be removed in v2. Use the project-tier `ocx.toml` plus [`ocx shell init`](#shell-init) instead — see the [project toolchain user guide](../user-guide.md#project-toolchain) for the migration path. [`shell profile generate`](#shell-profile-generate) survives v2 as a one-shot file generator.
:::

##### `add` {#shell-profile-add}

::: warning Deprecated
Use [`ocx.toml`](../user-guide.md#project-toolchain-toml) plus [`ocx shell init`](#shell-init) (project-level) or [`shell profile generate`](#shell-profile-generate) (file-based). Removed in v2.
:::

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

::: warning Deprecated
Use [`ocx.toml`](../user-guide.md#project-toolchain-toml) plus [`ocx shell init`](#shell-init) (project-level) or [`shell profile generate`](#shell-profile-generate) (file-based). Removed in v2.
:::

Remove one or more packages from the shell profile.

**Usage**

```shell
ocx shell profile remove <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to remove from the profile.

This does not uninstall the package — it only removes it from the profile manifest.

##### `list` {#shell-profile-list}

::: warning Deprecated
Use [`ocx.toml`](../user-guide.md#project-toolchain-toml) plus [`ocx shell init`](#shell-init) (project-level) or [`shell profile generate`](#shell-profile-generate) (file-based). Removed in v2.
:::

List all packages in the shell profile with their status.

**Usage**

```shell
ocx shell profile list
```

Shows each profiled package with its resolution mode (`candidate`, `current`, or `content`), status (`active` or `broken`), and resolved path.

##### `load` {#shell-profile-load}

::: warning Deprecated
Use [`ocx.toml`](../user-guide.md#project-toolchain-toml) plus [`ocx shell init`](#shell-init) (project-level) or [`shell profile generate`](#shell-profile-generate) (file-based). Removed in v2.

Unlike `add`, `remove`, and `list`, this subcommand does not emit a runtime deprecation warning — it is invoked from shell init files where stderr output would appear on every new prompt.
:::

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

##### `generate` {#shell-profile-generate}

File-generating variant of [`shell profile load`](#shell-profile-load). Emits the same export lines but writes them to a file the user sources once from their shell rc, rather than re-running `eval` on every shell startup. Defaults to `$OCX_HOME/init.<shell>` (e.g. `init.bash`); pass `--output <PATH>` to override or `--output -` to write to stdout.

This is the only `shell profile` subcommand that survives the v2 breaking release — `add`, `remove`, `list`, and `load` are removed in v2 in favor of the project-tier toolchain (see [`ocx shell init`](#shell-init) and the [project toolchain user guide](../user-guide.md#project-toolchain)).

**Usage**

```shell
ocx shell profile generate [OPTIONS]
```

**Options**

- `-s`, `--shell <SHELL>`: Shell to generate exports for. Auto-detected if not specified.
- `-o`, `--output <PATH>`: Output path. Defaults to `$OCX_HOME/init.<shell>`. Use `-` to write to stdout.
- `-h`, `--help`: Print help information.

**Exit codes**

| Code | Meaning |
|------|---------|
| 0 | Init file written (or printed to stdout when `-o -`). |
| 74 | I/O error writing the init file. |

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

For project-tier setups driven by `ocx.lock`, use [`pull`](#pull) instead — it consumes the
lockfile directly and ignores the index.
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
[gnu-parallel-j0]: https://www.gnu.org/software/parallel/parallel.html

<!-- environment -->
[env-no-color]: ./environment.md#external-no-color
[env-clicolor]: ./environment.md#external-clicolor
[env-clicolor-force]: ./environment.md#external-clicolor-force
[env-ocx-index]: ./environment.md#ocx-index
[env-no-config]: ./environment.md#ocx-no-config
[env-config-file]: ./environment.md#ocx-config-file
[env-project-file]: ./environment.md#ocx-project-file
[env-no-project]: ./environment.md#ocx-no-project
[env-ocx-quiet]: ./environment.md#ocx-quiet
[env-ocx-jobs]: ./environment.md#ocx-jobs

<!-- reference -->
[config-ref]: ./configuration.md

<!-- internal -->
[exit-codes]: #exit-codes
[fs-objects]: ../user-guide.md#file-structure-objects
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
[fs-index]: ../user-guide.md#file-structure-index
[ug-dependencies]: ../user-guide.md#dependencies
[ug-deps-env]: ../user-guide.md#dependencies-environment
