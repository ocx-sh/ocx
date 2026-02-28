---
outline: deep
---
# User Guide

## Authentication {#authentication}

ocx utilizes a layered approach of different authentication methods.
Most of these are specific to the registry being accessed, allowing to configure different authentication methods for different registries.
The following authentication methods are supported and queried in the the order they are listed.
If one of the methods is successful, the rest will be ignored.

- [Environment variables](#environment-variables)
- [Docker credentials](#docker-credentials)

### Environment Variables {#authentication-environment-variables}

To configure authentication for a registry you can use environment variables.
The following examples show how to configure authentication for the registry `docker.io` using a token or a bearer token.

::: code-group
```sh [token]
export OXC_AUTH_docker_io_TYPE=token
export OXC_AUTH_docker_io_TOKEN="<token>"
```

```sh [bearer]
export OXC_AUTH_docker_io_TYPE=bearer
export OXC_AUTH_docker_io_USER="<user>"
export OXC_AUTH_docker_io_TOKEN="<token>"
```
:::

The above examples configures the following environment variables:

- [`OCX_AUTH_<REGISTRY>_TYPE`](./reference/environment.md#ocx-auth-registry-type): The type of authentication to use for the registry.
- [`OCX_AUTH_<REGISTRY>_USER`](./reference/environment.md#ocx-auth-registry-user): The username to use for authentication.
- [`OCX_AUTH_<REGISTRY>_TOKEN`](./reference/environment.md#ocx-auth-registry-token): The password to use for authentication.

::: info
The registry is normalized by replacing all non-alphanumeric characters with underscores.
For example, for the registry `docker.io`, ocx will look for the following environment variable `OCX_AUTH_docker_io_TYPE`.
:::

The user and token are used according to the authentication type specified.
If no authentication type is specified but a user or token is configured, ocx will infer the authentication type.
For example, if a user and token are configured, ocx will assume basic authentication.
If only a token is configured, ocx will assume bearer authentication.
For more information see the documentation of the [environment variables](./reference/environment.md#ocx-auth-registry-type).

### Docker Credentials {#authentication-docker-credentials}

If a docker configuration is found, ocx will attempt to use the stored credentials for authentication.
The configuration is typically stored in the file `~/.docker/config.json` and can be configured using the `docker login` command.

```sh
docker login "<registry>"
```

The docker configuration file location can be overridden by setting the [`DOCKER_CONFIG`](./reference/environment.md#external-docker-config) environment variable.

## File Structure {#file-structure}

### Path Resolution {#path-resolution}

Several commands resolve package content to a filesystem path that is embedded in their output.
The path you receive determines how stable that output is across future package updates.

| Mode | Flag | Path used | Auto-install | Use case |
|---|---|---|---|---|
| Object store *(default)* | *(none)* | `~/.ocx/objects/…/<digest>/content` | yes (online) | CI, scripts, one-shot queries |
| Candidate symlink | `--candidate` | `~/.ocx/installs/…/candidates/<tag>/content` | **no** | Pinning to a specific tag in editor or IDE config |
| Current symlink | `--current` | `~/.ocx/installs/…/current/content` | **no** | "Always selected" path in shell profiles or IDE settings |

**Object store** paths are content-addressed and change whenever the package digest changes (i.e. on every update).
They are precise and self-verifying but unsuitable for static configuration.

**Candidate** and **Current** paths go through symlinks managed by ocx.
Because the symlink path itself does not change, any tool that hardcodes the path continues to work after the package is updated — only the symlink target is re-pointed.

- `--candidate` requires the package to be installed (the candidate symlink must exist).
  A digest component in the identifier is rejected; use a tag-only identifier.
- `--current` requires a version of the package to be selected (the current symlink must exist),
  mirroring the `update-alternatives` model on Linux.
  `current` is not automatically the latest installed version — it only moves when explicitly selected.
  A digest component in the identifier is rejected.

Both symlink modes fail immediately if the required symlink is absent.
They never attempt to install or select a package as a side effect.

::: tip Example — stable path for VSCode `settings.json`
```jsonc
{
  // This path remains valid across package updates — only the symlink target changes.
  "clangd.path": "/home/user/.ocx/installs/ocx.sh/clangd/current/content/bin/clangd"
}
```

Set it up once by installing and selecting a version, then inspect the stable path:
```shell
ocx env --current clangd
```
:::

## Indices {#indices}

### Remote Index {#indices-remote}

### Local Index {#indices-local}

### Selected Index {#indices-selected}

## Mirrors {#mirrors}

[docker-credential]: https://github.com/keirlawson/docker_credential
