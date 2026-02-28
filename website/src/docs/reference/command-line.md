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

## Commands

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
- `-h`, `--help`: Print help information.

### `env` {#env}

Print the resolved environment variables for one or more packages.

Plain format outputs an aligned table with `Key`, `Type` and `Value` columns.
JSON format (`--json`) outputs `{"entries": [{"key": "…", "value": "…", "type": "constant"|"path"}, …]}`.

In the default mode, packages are auto-installed if not already available locally.
With `--candidate` or `--current`, no auto-install is performed — see [path resolution modes](../user-guide.md#path-resolution).

**Usage**

```shell
ocx env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to resolve the environment for.

**Options**

- `-p`, `--platform`: Target platforms to consider when resolving packages.
- `--candidate`: Resolve env paths via the candidate symlink. Mutually exclusive with `--current`.
- `--current`: Resolve env paths via the current-selected symlink. Mutually exclusive with `--candidate`.
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

Installs packages into the local object store and create a [candidate symlink](../user-guide.md#path-resolution) for each package, making them available for use by other commands.

```shell
ocx install [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.

**Options**

- `-p`, `--platform`: Target platforms to consider.
- `-s`, `--select`: After installing, update the [current symlink](../user-guide.md#path-resolution) for each package to point to the newly installed version. Required before using `ocx env --current` or `ocx shell env --current`.
- `-h`, `--help`: Print help information.

### `shell` {#shell}

#### `env` {#shell-env}

Generate shell export statements for the environment variables declared by one or more packages.
Intended to be evaluated by the shell:

```shell
eval "$(ocx shell env mypackage)"
```

In the default mode this command does not auto-install packages — if a package is not already available locally it will fail with an error.
With `--candidate` or `--current`, no auto-install is performed either — see [path resolution modes](../user-guide.md#path-resolution).

**Usage**

```shell
ocx shell env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to generate environment variable export statements for.

**Options**

- `-p`, `--platform`: Target platforms to consider. Auto-detected by default.
- `-s`, `--shell <SHELL>`: Shell dialect to emit. Auto-detected by default.
- `--candidate`: Resolve env paths via the candidate symlink. Mutually exclusive with `--current`.
- `--current`: Resolve env paths via the current-selected symlink. Mutually exclusive with `--candidate`.

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Options**

- `--shell <SHELL>`: The shell to generate the completions for (e.g., `bash`, `zsh`, `fish`).
  By default, ocx will attempt to auto-detect the shell and generate the appropriate completions.
