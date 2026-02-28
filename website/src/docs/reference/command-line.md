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

See [path resolution modes](#path-resolution) for how the `current` symlink is used downstream.

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

### `package` {#package}

#### `create` {#package-create}

Bundles a local directory into a compressed package archive (`.tar.xz`) ready for publishing.

**Usage**

```shell
ocx package create [OPTIONS] <PATH>
```

**Arguments**

- `<PATH>`: Path to the directory to bundle.

**Options**

- `-i`, `--identifier <IDENTIFIER>`: Package identifier, used to infer the output filename when `--output` is a directory.
- `-p`, `--platform <PLATFORM>`: Platform of the package, used to infer the output filename.
- `-o`, `--output <PATH>`: Output file or directory. If a directory is given, the filename is inferred from the identifier and platform.
- `-f`, `--force`: Overwrite the output file if it already exists.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the output bundle.
- `-l`, `--compression-level <LEVEL>`: Compression level (`fast`, `default`, `best`). Default: `default`.
- `-h`, `--help`: Print help information.

#### `push` {#package-push}

Publishes a package bundle to the registry. A [candidate symlink](../user-guide.md#path-resolution) is created locally after a successful push.

**Usage**

```shell
ocx package push [OPTIONS] <IDENTIFIER> <CONTENT>
```

**Arguments**

- `<IDENTIFIER>`: Package identifier including the tag, e.g. `cmake:3.28.1+20260216120000`.
- `<CONTENT>`: Path to the package bundle (`.tar.xz`) to publish.

**Options**

- `-p`, `--platform <PLATFORM>`: Target platform of the package (required).
- `-c`, `--cascade`: Cascade rolling releases. When set, pushing `cmake:3.28.1+20260216120000` automatically re-points the rolling ancestors (`cmake:3.28.1`, `cmake:3.28`, `cmake:3`, and `cmake:latest` if applicable) to the new build — only if this is genuinely the latest at each specificity level. See [tag cascades](../user-guide.md#versioning-cascade).
- `-n`, `--new`: Declare this as a new package that does not exist in the registry yet. Skips the pre-push tag listing that is otherwise used for cascade resolution.
- `-m`, `--metadata <PATH>`: Path to the metadata file. If omitted, ocx looks for a sidecar file next to the bundle.
- `-h`, `--help`: Print help information.
