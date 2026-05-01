---
layout: doc
outline: deep
---
# Environment

## Primer

### Truthy Values {#truthy-values}

Some OCX options can be configured using environment variables instead of command line flags.
For example, the `--offline` flag can be set by configuring the `OCX_OFFLINE` environment variable to a truthy value.
Truthy values are case-insensitive and include:

- `1`
- `y`
- `yes`
- `on`
- `true`

for enabling an option, and

- `0`
- `n`
- `no`
- `off`
- `false`

for disabling an option.

## Internal

::: info Presentation flags do not propagate
The presentation flags `--log-level`, `--format`, and `--color` are CLI-only by design — they have no `OCX_*` counterpart and never propagate from a parent ocx into a subprocess (such as a generated [entrypoint launcher][entrypoints-ref]). Carrying them through env would leak ocx's own logging, JSON output, or ANSI color choices into the launcher's child stream. Only resolution-affecting policy (binary path, offline, remote, config file, index) propagates.
:::

### `OCX_AUTH_<REGISTRY>_TYPE` {#ocx-auth-registry-type}

The authentication type for the registry.

Valid values are:

- `basic`
- `token` (or `bearer`)
- `anonymous`

### `OCX_AUTH_<REGISTRY>_USER` {#ocx-auth-registry-user}

Configures the username for the registry when using `basic` authentication.
The corresponding password should be configured using the `OCX_AUTH_<REGISTRY>_TOKEN` variable.

This value is ignored if the `OCX_AUTH_<REGISTRY>_TYPE` is not set to `basic`.

### `OCX_AUTH_<REGISTRY>_TOKEN` {#ocx-auth-registry-token}

Configures the access token for the registry.
For `basic` authentication, this value will be used as the password.

This value is ignored if the `OCX_AUTH_<REGISTRY>_TYPE` is not set to `bearer` or `basic`.

### `OCX_BINARY_PIN` {#ocx-binary-pin}

Absolute path to an `ocx` executable. Set automatically by the running ocx on every subprocess it spawns so child ocx invocations — most importantly, the inner `ocx launcher exec` call inside a generated [entrypoint launcher][entrypoints-ref] — pin to the same binary that installed the package, instead of falling back to whatever `$PATH` happens to resolve at the launcher site.

```sh
export OCX_BINARY_PIN=/usr/local/bin/ocx
```

When `OCX_BINARY_PIN` is unset, generated launchers fall back to `$PATH`-resolved `ocx`. Set it manually only when running a launcher outside an outer ocx invocation and you want to pin a specific binary (typical use: a wrapper that records arguments, or a release-candidate binary tested side-by-side with the installed one).

### `OCX_CONFIG` {#ocx-config}

Path to an extra [configuration file][config-ref] to load. The file layers on top of the discovered tier chain (system, user, `$OCX_HOME/config.toml`) at highest file-tier precedence — it does not replace them. Use it to refine ambient config without rewriting it, or combine with [`OCX_NO_CONFIG`](#ocx-no-config) for a fully hermetic load.

Equivalent to the `--config` CLI flag, but injectable via environment — the intended use is CI and Docker setups where the env is controlled but the command line is not.

```sh
export OCX_CONFIG=/etc/ocx/ci.toml
```

If both `OCX_CONFIG` and `--config` are set, both load — `--config` sits at the highest file-tier precedence and wins on conflicting scalars. Missing files produce a clear error with the path.

**Escape hatch**: setting this to the empty string (`OCX_CONFIG=`) is treated as unset, not as an error. Useful when the variable is exported from a shell profile and you want to disable it for a single invocation without unsetting it.

### `OCX_DEFAULT_REGISTRY` {#ocx-default-registry}

The default registry to use when no registry is specified in a package reference on the [command line][cmd-ref].
Overrides the `[registry] default` key in the [configuration file][config-ref].
If neither is set, OCX uses `ocx.sh`.

::: warning
This variable is mostly intended for testing.
It is recommended to specify the registry explicitly in the package reference.
:::

### `OCX_HOME` {#ocx-home}

The root directory for all OCX data — the [object store][fs-objects], [local index][fs-index], and [install symlinks][fs-symlinks].
If not set, defaults to `~/.ocx`.

```sh
export OCX_HOME="/opt/ocx"
```

OCX also discovers a configuration file at `$OCX_HOME/config.toml` — see the [OCX home tier in the Configuration in-depth page][config-home-tier].

### `OCX_INDEX` {#ocx-index}

Override the path to the [local index][fs-index] directory.
By default, OCX reads the local index from `$OCX_HOME/index/` (typically `~/.ocx/index/`).

```sh
export OCX_INDEX="/path/to/bundled/index"
```

This variable is intended for environments where the index snapshot is bundled alongside a tool
rather than stored in [`OCX_HOME`](#ocx-home) — for example inside a [GitHub Action][github-actions-docs],
[Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features].

The command line option [`--index`][arg-index] takes precedence over this variable.
This variable has no effect when [`--remote`][arg-remote] or [`OCX_REMOTE`][env-ocx-remote] is set.

### `OCX_INSECURE_REGISTRIES` {#ocx-insecure-registries}

A comma-separated list of registry hostnames (with optional port) that should be contacted over plain HTTP instead of HTTPS.

```sh
export OCX_INSECURE_REGISTRIES="localhost:5000,registry.local:8080"
```

::: warning
This variable disables TLS for the listed registries. Only use it for local development registries that do not support HTTPS.
:::

### `OCX_LOG` {#ocx-log}

The log level for OCX.
You can set this variable to the same values as the [`--log-level`][arg-log-level] command line option (e.g. `warn`, `info`, etc.).
If [`--log-level`][arg-log-level] is specified, it will take precedence over this environment variable.
For more information on log levels, see the [command line reference][arg-log-level].

### `OCX_LOG_CONSOLE` {#ocx-log-console}

Similar to [`OCX_LOG`](#ocx-log), but specifically for configuring the log level of messages emitted to the console.
If `OCX_LOG_CONSOLE` is set, it will take precedence over [`OCX_LOG`](#ocx-log) for console messages.

### `OCX_NO_CONFIG` {#ocx-no-config}

When set to a [truthy value](#truthy-values), OCX skips the **discovered** [configuration][config-ref] chain — no system, user, or `$OCX_HOME/config.toml` is loaded. Explicit paths supplied via [`--config`][arg-config] or [`OCX_CONFIG`](#ocx-config) still load, because they represent deliberate intent rather than ambient environment.

Use this for CI reproducibility: locked workflows should ignore any ambient config that might leak in from the runner image or a mounted home directory.

Combined with an explicit path, this is the canonical hermetic pattern:

```sh
OCX_NO_CONFIG=1 ocx --config /ci/ocx.toml install cmake:3.28
```

`OCX_NO_CONFIG` is available only as an environment variable. A `--no-config` CLI flag would duplicate surface without solving a new problem: the hermetic-CI use case is best expressed via env vars, which are how CI systems already inject policy. A flag would require callers to both export the env var and pass the flag in every per-command invocation — two sources of truth for the same intent.

### `OCX_NO_UPDATE_CHECK` {#ocx-no-update-check}

When set to a [truthy value](#truthy-values), OCX will not check the remote registry for newer versions on CLI startup.
By default, OCX prints a notice to stderr if a newer version is available in the remote registry.

The update check is also automatically suppressed when:
- `CI` is set to a truthy value
- [`OCX_OFFLINE`](#ocx-offline) is set to a truthy value (or `--offline` flag)
- stderr is not a terminal (e.g., piped or redirected)
- the command is `version`, `info`, or `shell completion`

### `OCX_NO_MODIFY_PATH` {#ocx-no-modify-path}

When set to a [truthy value](#truthy-values), the install scripts (`install.sh` and `install.ps1`) will skip modifying shell profile files.
Use this in CI environments or when you manage your `PATH` manually.

The command line option `--no-modify-path` on the install scripts has the same effect.

### `OCX_NO_CODESIGN` {#ocx-no-codesign}

When set to a [truthy value](#truthy-values), OCX will skip ad-hoc code signing of macOS binaries after installation.
By default, OCX automatically applies ad-hoc code signatures to extracted [Mach-O][mach-o] binaries on macOS,
which is required for execution on Apple Silicon.
See the [FAQ][faq-codesign] for details on why this is necessary and how it works.

This variable has no effect on non-macOS systems.

### `OCX_OFFLINE` {#ocx-offline}

When set to a [truthy value](#truthy-values), OCX will run in offline mode, which will not attempt to fetch any remote information.
The command line option [`--offline`][arg-offline] takes precedence over this variable.

### `OCX_REMOTE` {#ocx-remote}

When set to a [truthy value](#truthy-values), tag and catalog lookups query the registry directly,
bypassing the local tag store. Digest-addressed blob reads still use the local cache with
write-through to `$OCX_HOME/blobs/`. Only `$OCX_HOME/tags/` is not updated.

Equivalent to passing the [`--remote`][arg-remote] flag on every invocation.

## External {#external}

### `CI` {#external-ci}

When set to a [truthy value](#truthy-values), OCX suppresses the update check on startup.
Most CI systems (GitHub Actions, GitLab CI, Travis, etc.) set this automatically.

### `GITHUB_ACTIONS` {#external-github-actions}

Set to `true` by [GitHub Actions][github-actions-docs] runners. Used by
[`ocx ci export`][cmd-ci-export] to auto-detect the CI flavor. When detected,
the command writes environment variable exports to the files specified by
[`GITHUB_PATH`](#external-github-path) and [`GITHUB_ENV`](#external-github-env).

### `GITHUB_PATH` {#external-github-path}

Set by [GitHub Actions][github-actions-docs] to a file path.
[`ocx ci export`][cmd-ci-export] appends `PATH` entries to this file, making
them available in subsequent workflow steps.

### `GITHUB_ENV` {#external-github-env}

Set by [GitHub Actions][github-actions-docs] to a file path.
[`ocx ci export`][cmd-ci-export] appends non-`PATH` environment variables to
this file using `KEY=value` syntax (or [heredoc delimiters][github-multiline-env]
for multiline values).

### `DOCKER_CONFIG` {#external-docker-config}

The location of the Docker configuration directory. Read by the Docker credential helper that ocx delegates to when resolving registry credentials from `~/.docker/config.json`.

### `XDG_CONFIG_HOME` {#external-xdg-config-home}

User-level configuration base directory, defined by the [XDG Base Directory Specification][xdg-basedir]. On Linux, OCX uses it to locate the user-tier [configuration file][config-ref]: the user tier is `$XDG_CONFIG_HOME/ocx/config.toml`, falling back to `~/.config/ocx/config.toml` when the variable is unset.

On macOS, `XDG_CONFIG_HOME` is not consulted. The user-tier path is `~/Library/Application Support/ocx/config.toml`, following [Apple's directory conventions][apple-dirs-env]. Use the [OCX home tier][config-home-tier] (`$OCX_HOME/config.toml`, default `~/.ocx/config.toml`) if you want a platform-neutral user config path.

OCX does not write anything to these directories — the config loader is read-only. This follows the convention for CLI tools that need user-level configuration separate from data (`~/.ocx/`).

### `NO_COLOR` {#external-no-color}

When set to any non-empty value, disables ANSI color output.
This is a <Tooltip term="cross-tool convention">Defined at <a href="https://no-color.org/">no-color.org</a>. Adopted by hundreds of CLI tools across all ecosystems.</Tooltip> for respecting user color preferences.
The [`--color`][arg-color] flag takes precedence.

### `CLICOLOR` {#external-clicolor}

When set to `0`, disables color output.
Part of the <Tooltip term="CLICOLOR convention">Defined at <a href="https://bixense.com/clicolors/">bixense.com/clicolors</a>. Originated in macOS/BSD tooling.</Tooltip>.

### `CLICOLOR_FORCE` {#external-clicolor-force}

When set to a non-zero value, forces color output even when stdout is not a terminal.
Overrides [`CLICOLOR`](#external-clicolor) but is itself overridden by [`NO_COLOR`](#external-no-color).

### `RUST_LOG` {#external-rust-log}

A fallback for configuring the log level of OCX and its dependencies.
If [`OCX_LOG`](#ocx-log) is not set, OCX will respect the log level configured via `RUST_LOG`.
The format for this variable is the same as for [`OCX_LOG`](#ocx-log).

<!-- external -->
[mach-o]: https://en.wikipedia.org/wiki/Mach-O
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-multiline-env]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions#multiline-strings
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[xdg-basedir]: https://specifications.freedesktop.org/basedir-spec/latest/
[apple-dirs-env]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html

<!-- commands -->
[cmd-ref]: command-line.md
[cmd-ci-export]: command-line.md#ci-export
[arg-color]: command-line.md#arg-color
[arg-config]: command-line.md#arg-config
[arg-index]: command-line.md#arg-index
[arg-log-level]: command-line.md#arg-log-level
[arg-offline]: command-line.md#arg-offline
[arg-remote]: command-line.md#arg-remote

<!-- in-depth -->
[exec-modes-ref]: ../in-depth/environments.md#visibility-views
[entrypoints-ref]: ../in-depth/entry-points.md

<!-- reference -->
[config-ref]: ./configuration.md
[config-home-tier]: ../in-depth/configuration.md#tier-ocx-home

<!-- environment -->
[env-ocx-remote]: #ocx-remote

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-objects
[fs-index]: ../user-guide.md#file-structure-index
[fs-symlinks]: ../user-guide.md#file-structure-symlinks
[faq-codesign]: ../faq.md#macos-codesign
