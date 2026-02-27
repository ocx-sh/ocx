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

## Commands

### `exec` {#exec}

Executes a command within the environment of one or more packages.
This allows you to run commands with the dependencies and environment variables provided by the specified packages.

**Usage**

```shell
ocx exec [OPTIONS] <PACKAGE>... -- <COMMAND> [ARGS...]
```

**Arguments**

- `<PACKAGE>`: Package identifiers to install.
- `<COMMAND>`: The command to execute within the environment of the specified packages.
- `[ARGS...]`: Arguments to pass to the command.

**Options**

- `-p`, `--platform`: Specify the platforms to use.
- `-i`, `--interactive`: Run in interactive mode, forwarding stdin to the child process.
- `-h`, `--help`: Print help information.

Execute a command within the environment of a package.

### `index` {#index}

#### `catalog` {#index-catalog}

```bash
ocx index catalog [OPTIONS]
```

Lists available package catalogs from the remote index.

**Options*

- `--with-tags`: Include package tags in the output.
  This will slow down the command significantly, as it requires fetching additional information from the remote index for each package.

#### `update` {#index-update}

```bash
ocx index update <PACKAGE>...
```

Updates the local index by fetching the latest information from the remote index for the specified packages.

**Arguments**

- `<PACKAGE>`: Package identifiers to update in the local index for.

### `shell` {#shell}

#### `env` {#shell-env}

Generate shell commands to set environment variables for a specified packages.

::: warning
This command will not install any packages or fetch any information from the remote index.
If the specified packages are not already available locally, this command will fail with an error.
:::

**Usage**

```shell
ocx shell env [OPTIONS] <PACKAGE>...
```

**Arguments**

- `<PACKAGE>`: Package identifiers to generate environment variable commands for.

**Options**

- `--shell <SHELL>`: The shell to generate the completions for (e.g., `bash`, `zsh`, `fish`).
  By default, ocx will attempt to auto-detect the shell and generate the appropriate completions.

#### `completion` {#shell-completion}

Generate shell completion scripts for ocx.

**Options**

- `--shell <SHELL>`: The shell to generate the completions for (e.g., `bash`, `zsh`, `fish`).
  By default, ocx will attempt to auto-detect the shell and generate the appropriate completions.


