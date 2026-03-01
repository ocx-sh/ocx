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

### `OCX_AUTH_<REGISTRY>_TYPE` {#ocx-auth-registry-type}

The authentication type for the registry.

Valid values are:

- `basic`
- `bearer`
- `anonymous`

### `OCX_AUTH_<REGISTRY>_USER` {#ocx-auth-registry-user}

Configures the username for the registry when using `basic` authentication.
The corresponding password should be configured using the `OCX_AUTH_<REGISTRY>_TOKEN` variable.

This value is ignored if the `OCX_AUTH_<REGISTRY>_TYPE` is not set to `basic`.

### `OCX_AUTH_<REGISTRY>_TOKEN` {#ocx-auth-registry-token}

Configures the access token for the registry.
For `basic` authentication, this value will be used as the password.

This value is ignored if the `OCX_AUTH_<REGISTRY>_TYPE` is not set to `bearer` or `basic`.

### `OCX_DEFAULT_REGISTRY` {#ocx-default-registry}

The default registry to use when no registry is specified in a package reference on the [command line](./command-line.md).
If not set, OCX will default to `ocx.sh`.

::: warning
Thus this variable is mostly intended for testing.
It is recommended to specify the registry explicitly or use [mirrors](../user-guide.md#mirrors).
:::

### `OCX_HOME` {#ocx-home}

The root directory for all OCX data — the [object store][fs-objects], [local index][fs-index], and [install symlinks][fs-installs].
If not set, defaults to `~/.ocx`.

```sh
export OCX_HOME="/opt/ocx"
```

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
You can set this variable to the same values as the [`--log-level`](command-line.md#arg-log-level) command line option (e.g. `warn`, `info`, etc.).
If [`--log-level`](command-line.md#arg-log-level) is specified, it will take precedence over this environment variable.
For more information on log levels, see the [command line reference](command-line.md#arg-log-level).

### `OCX_LOG_CONSOLE` {#ocx-log-console}

Similar to [`OCX_LOG`](#ocx-log), but specifically for configuring the log level of messages emitted to the console.
If `OCX_LOG_CONSOLE` is set, it will take precedence over [`OCX_LOG`](#ocx-log) for console messages.

### `OCX_OFFLINE` {#ocx-offline}

When set to a [truthy value](#truthy-values), OCX will run in offline mode, which will not attempt to fetch any remote information.
The command line option [`--offline`](command-line#arg-offline) takes precedence over this variable.

### `OCX_REMOTE` {#ocx-remote}

When set to a [truthy value](#truthy-values), OCX will use the remote index by default instead of the local index.
The command line option [`--remote`](command-line#remote) takes precedence over this variable.

## External {#external}

### `DOCKER_CONFIG` {#external-docker-config}

The location of the docker configuration file.

### `RUST_LOG` {#external-rust-log}

A fallback for configuring the log level of OCX and its dependencies.
If [`OCX_LOG`](#ocx-log) is not set, OCX will respect the log level configured via `RUST_LOG`.
The format for this variable is the same as for [`OCX_LOG`](#ocx-log).

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-objects
[fs-index]: ../user-guide.md#file-structure-index
[fs-installs]: ../user-guide.md#file-structure-installs
