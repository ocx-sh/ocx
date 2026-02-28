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

ocx stores everything under a single root directory, by default `~/.ocx`.
The root is split into three independent stores, each with a distinct role:

```
~/.ocx/
├── objects/                       ← content-addressed binary store (immutable)
│   └── {registry}/
│       └── {repo}/
│           └── sha256/{a}/{b}/{c}/
│               ├── content/       ← package files (executables, headers, …)
│               ├── metadata.json  ← package metadata
│               └── refs/          ← back-references to install symlinks
│
├── index/                         ← cached OCI index (no binaries)
│   └── {registry}/
│       ├── tags/
│       │   └── {repo}.json        ← { "3.28": "sha256:abc…" }
│       └── objects/
│           └── sha256/{a}/{b}/{c}.json  ← cached manifest
│
└── installs/                      ← stable symlinks for tools and shell profiles
    └── {registry}/
        └── {repo}/
            ├── current            → objects/…/{selected}/content
            └── candidates/
                └── {tag}          → objects/…/{digest}/content
```

**objects** stores the actual package binaries.
Paths are sharded by the first three segments of the digest hex string to keep individual directories small.
Each object directory is self-contained: `content/` holds the package files, `metadata.json` describes the package, and `refs/` records back-references to every install symlink that currently points here (enabling safe garbage collection).

**index** mirrors remote OCI registry metadata locally — tags and manifests, but no binaries.
It lets ocx resolve tags and check versions without hitting the network.
`ocx index update` refreshes it; `ocx --remote` bypasses it and queries the registry directly.

**installs** provides stable paths that are safe to embed in shell profiles, IDE configs, or build files.
Unlike object-store paths (which change with every digest update), install symlink paths remain constant — only their targets are silently re-pointed when you upgrade or select a different version.

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
