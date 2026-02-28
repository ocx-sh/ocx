---
layout: doc
outline: deep
---
# Command Line

## General Options

### `--offline` {#arg-offline}

When set, ocx will run in offline mode, which will not attempt to fetch any remote information.
If any command requires information that is not already available locally, it will fail with an error.

### `--remote` {#arg-remote}

When set, ocx will use the remote index by default instead of the local index.
Combining this flag with [`--offline`](#arg-offline) will most likely result in an error.

## Path resolution modes {#path-resolution}

Several commands resolve package content to a filesystem path that is embedded in their output.
The path you receive determines how stable that output is across future package updates.

| Mode | Flag | Path used | Auto-install | Use case |
|---|---|---|---|---|
| Object store *(default)* | *(none)* | `~/.ocx/objects/…/<digest>/content` | yes (online) | CI, scripts, one-shot queries |
| Candidate symlink | `--candidate` | `~/.ocx/installs/…/candidates/<tag>/content` | **no** | Pinning to a specific tag in editor or IDE config |
| Current symlink | `--current` | `~/.ocx/installs/…/current/content` | **no** | "Always selected" path in shell profiles or IDE settings |

**Object store** paths are content-addressed and change whenever the package digest changes (i.e. on every update).
They are precise and self-verifying but unsuitable for static configuration.

**Candidate** and **Current** paths go through symlinks maintained by `ocx install`.
Because the symlink path itself does not change, any tool that hardcodes the path continues to work after the package is updated — only the symlink target is re-pointed by the next install.

- `--candidate` requires the package to have been installed at least once (`ocx install <pkg>`).
  A digest component in the identifier is rejected; use a tag-only identifier.
- `--current` requires the package to have been explicitly selected (`ocx install --select <pkg>`),
  mirroring the `update-alternatives` model on Linux.
  `current` is not automatically the latest installed version — it only moves when `--select` is used.
  A digest component in the identifier is rejected.

Both symlink modes fail immediately with an actionable error if the required symlink is absent.
They never attempt to install or select a package as a side effect.

::: tip Example — stable path for VSCode `settings.json`
```jsonc
{
  // This path survives `ocx install --select clangd` without any manual edits.
  "clangd.path": "/home/user/.ocx/installs/ocx.sh/clangd/current/content/bin/clangd"
}
```

Set it up once:
```shell
# Install and select a version
ocx install --select clangd:18

# Inspect the stable path
ocx env --current clangd
```
:::

## Commands

### `exec` {#exec}

Executes a command within the environment of one or more packages.
Packages are auto-installed if not already available locally (unless `--offline` is set).

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
- `-h`, `--help`: Print help information.

### `env` {#env}

Print the resolved environment variables for one or more packages.

Plain format outputs an aligned table with `Key`, `Value`, and `Type` columns.
JSON format (`--json`) outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.

In the default mode, packages are auto-installed if not already available locally.
With `--candidate` or `--current`, no auto-install is performed — see [path resolution modes](#path-resolution).

**Usage**

```shell
ocx env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`: Resolve env paths via the candidate symlink. Mutually exclusive with `--current`. See [path resolution modes](#path-resolution).
- `--current`: Resolve env paths via the current-selected symlink. Mutually exclusive with `--candidate`. See [path resolution modes](#path-resolution).
- `-h`, `--help`: Print help information.

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS]
```

Lists available package catalogs from the remote index.

**Options**

- `--with-tags`: Include package tags in the output.
  This will slow down the command significantly, as it requires fetching additional information from the remote index for each package.

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Updates the local index by fetching the latest information from the remote index for the specified packages.

**Arguments**

- `<PACKAGE>`: Package identifiers to update in the local index for.

### `install` {#install}

Downloads and installs one or more packages into the local object store.

**Usage**

```shell
ocx install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--select`: After installing, update the `current` symlink for each package to point to the newly installed version. Required before using `ocx env --current` or `ocx shell env --current`.
- `-h`, `--help`: Print help information.

### `shell` {#shell}

#### `env` {#shell-env}

Generate shell export statements for the environment variables declared by one or more packages.
Intended to be evaluated by the shell:

```shell
eval "$(ocx shell env mypackage)"
```

In the default mode this command does not auto-install packages — if a package is not already available locally it will fail with an error.
With `--candidate` or `--current`, no auto-install is performed either — see [path resolution modes](#path-resolution).

**Usage**

```shell
ocx shell env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to generate environment variable export statements for.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--shell <SHELL>`: Shell dialect to emit (`bash`, `zsh`, `fish`). Auto-detected by default.
- `--candidate`: Resolve env paths via the candidate symlink. Mutually exclusive with `--current`. See [path resolution modes](#path-resolution).
- `--current`: Resolve env paths via the current-selected symlink. Mutually exclusive with `--candidate`. See [path resolution modes](#path-resolution).
- `-h`, `--help`: Print help information.

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Options**

- `--shell <SHELL>`: The shell to generate the completions for (e.g., `bash`, `zsh`, `fish`).
  By default, ocx will attempt to auto-detect the shell and generate the appropriate completions.
