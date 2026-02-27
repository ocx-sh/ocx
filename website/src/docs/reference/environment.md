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

### `OCX_OFFLINE` {#ocx-offline}

When set to a [truthy value](#truthy-values), OCX will run in offline mode, which will not attempt to fetch any remote information.
The command line option [`--offline`](command-line#arg-offline) takes precedence over this variable.

### `OCX_REMOTE` {#ocx-remote}

When set to a [truthy value](#truthy-values), OCX will use the remote index by default instead of the local index.
The command line option [`--remote`](command-line#remote) takes precedence over this variable.

## External {#external}

### `DOCKER_CONFIG` {#external-docker-config}

The location of the docker configuration file.
