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

## Deploying Packages



## Mirrors

[docker-credential]: https://github.com/keirlawson/docker_credential
