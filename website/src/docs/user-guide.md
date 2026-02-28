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

Most package managers store everything in a single mutable tree — installing a new version silently replaces the old one, breaking anything that referenced the old path. They also hit the network on every operation, making offline use and reproducible CI awkward.

### Storage

ocx separates concerns into three independent stores under `~/.ocx/`:

<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="objects/" icon="📦">
      <Description>immutable, content-addressed binary store</Description>
    </Node>
    <Node name="index/" icon="🗂️">
      <Description>local mirror of registry metadata — no binaries</Description>
    </Node>
    <Node name="installs/" icon="🔀">
      <Description>stable symlinks safe to embed in shell profiles and configs</Description>
    </Node>
  </Node>
</Tree>

Each store has a single responsibility. Upgrading a package updates symlinks in `installs/` and may add a new object to `objects/`, but never touches `index/`. The stores can be understood — and reasoned about — one at a time.

#### Objects {#file-structure-objects}

When ocx installs a package, the actual files land in `objects/`. The critical design decision: the storage path is derived from the content's <Tooltip term="SHA-256 digest">A 64-character hex fingerprint of the exact bytes in a file or archive. The same bytes always produce the same digest; any change — even a single bit — produces a completely different one. Used here as a content address: the path is uniquely determined by what the files contain.</Tooltip>, not from the package name or tag.

<Tree>
  <Node name="~/.ocx/objects/" icon="📦" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}/" icon="📁" open-icon="📂" open>
        <Node name="sha256:abc123…/" icon="📁" open-icon="📂" open>
          <Description>one directory per unique package build</Description>
          <Node name="content/" icon="📂">
            <Description>package files — binaries, libraries, headers</Description>
          </Node>
          <Node name="metadata.json" icon="📋">
            <Description>declared env vars, supported platforms</Description>
          </Node>
          <Node name="refs/" icon="🔗">
            <Description>back-references to install symlinks — guards against deletion</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

This <Tooltip term="content-addressed layout">A storage scheme where every item's address is derived from a cryptographic hash of its contents. The same bytes always map to the same address — accidental duplication is impossible and silent corruption is detectable.</Tooltip> has two important consequences:

- **Automatic deduplication.** If `cmake:3.28` and `cmake:latest` resolve to the same binary build, they share one directory on disk. Storage is proportional to the number of *distinct builds*, not the number of tags that reference them.
- **Immutability.** A path under `sha256:…/` never changes its contents. This makes objects safe to reference directly from scripts or cache layers that require a known, stable binary.

::: info Similar to the Nix store and Git objects
The [Nix package manager](https://nixos.org/) stores every package at `/nix/store/{hash}-name/` using the same principle: the path is a function of the content. This is what makes Nix derivations reproducible across machines — the same hash always means the same files. Git's internal object store (`.git/objects/`) works identically. ocx applies this model to OCI-distributed binaries.
:::

**Garbage collection via `refs/`.** The `refs/` subdirectory inside each object tracks every install symlink that currently points to it. `ocx clean` only removes objects whose `refs/` is empty — anything still referenced by an active install is never touched.

::: details How back-references work
When `ocx install cmake:3.28` creates the symlink `installs/…/cmake/candidates/3.28 → objects/…/sha256:abc123…/content`, it simultaneously writes a corresponding back-reference entry inside `objects/…/sha256:abc123…/refs/`. Removing the symlink via `ocx uninstall` removes that back-reference entry. `ocx clean` then applies a simple rule: if `refs/` is empty, the object is orphaned and can be safely deleted. No reference counting, no graph traversal — just check whether the directory is empty.
:::

*Commands: [`ocx install`](./reference/command-line.md#install) adds objects; `ocx uninstall --purge` removes a specific one; `ocx clean` removes all unreferenced objects.*

#### Index {#file-structure-index}

When you run `ocx install cmake:3.28`, how does ocx know which binary to fetch? It looks up the tag `3.28` in the index and finds the corresponding <Tooltip term="digest">The content fingerprint stored in the OCI registry manifest. A tag like `3.28` is just a human-readable alias; the digest is the canonical, immutable identifier that pinpoints the exact binary build behind that tag at index-update time.</Tooltip>. The index is a local copy of that mapping — no network required.

<Tree>
  <Node name="~/.ocx/index/" icon="🗂️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="tags/" icon="📁" open-icon="📂" open>
        <Node name="{repo}.json" icon="📄">
          <Description>{ "3.28": "sha256:abc…", "3.30": "sha256:def…" }</Description>
        </Node>
      </Node>
      <Node name="objects/" icon="📁" open-icon="📂">
        <Node name="sha256:abc123….json" icon="📄">
          <Description>cached OCI manifest — platform list, layer references</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

The index is a *snapshot*: it reflects the state of the remote registry at the last time you refreshed it. That snapshot has two benefits:

- **Offline installs.** If the index is populated and the binary is already in the object store, `ocx install cmake:3.28` works with no network access at all — tag resolution is a local file read.
- **Reproducibility.** A CI runner that does not update the index gets the same binary on every run, regardless of what the registry serves today. Tags in OCI registries are mutable; your local snapshot is not.

::: info Similar to APT's package lists
`apt-get update` downloads package metadata from configured sources and caches it in `/var/lib/apt/lists/`. All subsequent `apt-get install` calls resolve packages from that local snapshot — the network is only involved during an explicit refresh, not on every install. `ocx index update <package>` is the per-package equivalent: you control when the snapshot changes, and the rest of the time you work from the local cache.
:::

`ocx index update <package>` refreshes the index for a specific package. The global flag [`--remote`](./reference/command-line.md#arg-remote) skips the local index entirely and queries the registry directly for a single command — useful for a one-off check without updating the persistent snapshot.

*Commands: [`ocx index catalog`](./reference/command-line.md#index-catalog), [`ocx index update`](./reference/command-line.md#index-update); flag: [`--remote`](./reference/command-line.md#arg-remote)*

#### Installs {#file-structure-installs}

Object-store paths embed the digest: `~/.ocx/objects/ocx.sh/cmake/sha256:abc123…/content`. That path changes on every upgrade. You cannot put it in a shell profile, an IDE config, or a build file and expect it to still work next month.

`installs/` solves this with <Tooltip term="stable symlinks">A symbolic link (symlink) is a filesystem entry that points to another path. The link's own path never changes — only its target can be updated. Tools that reference the link path continue to work transparently after the target is re-pointed to a new version.</Tooltip> whose paths never change — only their targets are re-pointed when you install or select a new version.

<Tree>
  <Node name="~/.ocx/installs/" icon="🔀" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}/" icon="📁" open-icon="📂" open>
        <Node name="current" icon="➡️">
          <Description>active version — set by ocx select</Description>
        </Node>
        <Node name="candidates/" icon="📁" open-icon="📂" open>
          <Node name="3.28" icon="➡️">
            <Description>pinned install — created by ocx install cmake:3.28</Description>
          </Node>
          <Node name="3.30" icon="➡️">
            <Description>pinned install — created by ocx install cmake:3.30</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

There are two symlink levels, for two different use cases:

**`candidates/{tag}`** — pinned to a specific version. Created by `ocx install` and pointed at the exact digest that tag resolved to at install time. You can have cmake 3.28 and 3.30 installed side by side; both candidates coexist until you explicitly uninstall one. Even if the registry later re-pushes the `3.28` tag with a different binary, your candidate still points to the build you originally installed.

**`current`** — a floating pointer to whichever candidate you last declared active. Set by [`ocx select`](./reference/command-line.md#select) (or `ocx install --select` in one step). It is never updated automatically — not when you install a newer version, not when you update the index. This is intentional: tools referencing `current` should only change behavior when *you* decide they should.

::: info Inspired by SDKMAN and Homebrew
[SDKMAN](https://sdkman.io/) (the Java SDK manager) uses the same two-level pattern: `~/.sdkman/candidates/{tool}/{version}/` for pinned installs and a `current` symlink updated by `sdk default {version}`. [Homebrew](https://brew.sh/) does the same with its `Cellar/{formula}/{version}/` store and a stable `opt/{formula}` symlink pointing at the active version. Linux's `update-alternatives` is the system-level equivalent, managing tools like `java` and `python3` via a layer of stable symlinks in `/etc/alternatives/`.
:::

::: tip Stable paths in IDE settings and shell profiles
Because `current` never changes its own path, you reference it once and forget it:

```shell
# Install cmake and make it the active version in one step
ocx install --select cmake:3.30
```

```jsonc
// .vscode/settings.json — path survives every future upgrade
{ "cmake.cmakePath": "~/.ocx/installs/ocx.sh/cmake/current/content/bin/cmake" }
```

```sh
# ~/.bashrc — always resolves to the selected version
export PATH="$HOME/.ocx/installs/ocx.sh/cmake/current/content/bin:$PATH"
```

When you later run `ocx install --select cmake:3.32`, `current` is re-pointed. Your IDE and shell pick up the new version automatically — no config changes needed.
:::

*Commands: [`ocx install`](./reference/command-line.md#install), [`ocx select`](./reference/command-line.md#select); `ocx deselect`, `ocx uninstall`*

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
