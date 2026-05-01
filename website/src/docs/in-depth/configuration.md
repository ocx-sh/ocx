---
outline: deep
---
# Configuration

Most package managers couple configuration to the system they're installed on. Homebrew bakes its prefix into the binary; apt scatters config under `/etc/apt`; pip relies on per-Python-version site files. Each works for its scope but breaks when you need the *same* OCX install to behave differently on a developer laptop, a CI runner, and an air-gapped Docker image.

OCX takes the opposite approach. Configuration is a layered chain of optional TOML files. The binary ships sensible defaults; every layer above is opt-in; explicit paths win over ambient ones; environment variables and CLI flags always win over any file. This page explains *how* that chain is assembled and *why* each tier exists. The strict API surface — every key, every path, every error string — lives at the [Configuration reference][config-ref].

## Why Configuration Files Exist {#why}

OCX works without any config file using compiled-in defaults. Most users never write one. The file format only earns its keep when:

- A team needs every machine to default to a private registry (`registry.company.com`) without exporting an env var on every shell.
- A CI image needs to be self-describing — the registry default sits in a checked-in `config.toml` that humans can read and review.
- A portable OCX install (a zipped `$OCX_HOME` carried between machines) needs settings that travel with the data, not with the host OS.

Each of these is a different *scope*, and each is served by a different tier in the discovery chain.

## The Three Discovery Tiers {#tiers}

OCX looks for config files in three tiers. None has to exist; missing files are silently skipped. The three were chosen so that **scope of effect** matches **scope of file location** — a setting written into a system tier affects every user on the machine; a setting in a user tier affects only that user; a setting in `$OCX_HOME` travels with the OCX install.

### System tier {#tier-system}

`/etc/ocx/config.toml` is for machine-wide defaults set by sysadmins, Dockerfiles, or provisioning tools. It is the only tier the OS itself considers privileged — locked down by file permissions in production environments. The most common use is baking a private default registry into a base image:

```dockerfile
RUN mkdir -p /etc/ocx
COPY config.toml /etc/ocx/config.toml
```

### User tier {#tier-user}

The user tier follows the OS convention for per-user app configuration. On Linux, OCX honors the [XDG Base Directory specification][xdg-basedir] — `$XDG_CONFIG_HOME/ocx/config.toml`, falling back to `~/.config/ocx/config.toml`. On macOS, [Apple's directory conventions][apple-dirs] put config at `~/Library/Application Support/ocx/config.toml`; `XDG_CONFIG_HOME` is *not* consulted on macOS even when set, matching how every other native macOS tool behaves.

The user tier sits separately from the OCX *data* directory (`~/.ocx/`) by design. Settings live with the user; data lives where the user pointed `$OCX_HOME`. They scale independently — a user can blow away their data directory without losing config preferences, and vice versa.

### OCX home tier {#tier-ocx-home}

`$OCX_HOME/config.toml` (default `~/.ocx/config.toml`) is co-located with the OCX data directory. This is the *only* tier that moves with the data when you relocate `$OCX_HOME`. It is the right home for settings that must survive a zip-and-move of an entire OCX install — for example, a portable OCX bundle carried between machines, or an air-gapped install where the data and config travel together.

The system and user tiers, by contrast, live under OS-specific locations that do not travel with the data. A portable install that wrote to the user tier on machine A would silently lose those settings the moment you copied `$OCX_HOME` to machine B.

## Explicit Additions {#explicit}

Two mechanisms add an extra file *on top of* the discovery chain — they do not replace it. This supports the common "refine ambient config with a targeted override" use case:

- **`--config FILE`** — CLI flag, passed before the subcommand. Right for one-off overrides.
- **`OCX_CONFIG=/path/to/file.toml`** — environment variable. Right for CI and Docker where env vars are more practical than CLI flags.

When set, the specified file layers at the top of the file-tier chain. If the file is missing, that is an error — explicit paths must exist, because they represent deliberate intent, not ambient discovery. Both can coexist; when both are set, `--config` layers on top of `OCX_CONFIG`.

To disable an ambient `OCX_CONFIG` for a single invocation without unsetting it (common when it is exported from a shell profile), set it to the empty string:

```sh
OCX_CONFIG= ocx install cmake:3.28
```

Empty is the escape hatch — `OCX_CONFIG` set to empty string is treated as unset, not as an error.

## Discovery and Merge Precedence {#precedence}

Settings are resolved lowest-to-highest. Higher-precedence sources override lower ones. This is the same shape as [Cargo's config][cargo-config] and [uv's config][uv-config] — start with conservative defaults, layer the user's intent on top.

The full precedence stack runs from compiled defaults at the bottom up through CLI flags at the top. The exact order, with each tier's path, lives in the [precedence table in the reference][config-precedence]. The principles that drove the order:

- **Explicit beats ambient.** A `--config` flag the user just typed beats a `~/.config/ocx/config.toml` they wrote months ago.
- **Per-invocation beats persistent.** Env vars and CLI flags are per-invocation; they always win over any file.
- **Inner tiers beat outer tiers.** A setting in `$OCX_HOME/config.toml` (which travels with the data) wins over one in `/etc/ocx/config.toml` (which is machine-global).

### Merge rules {#merge-rules}

- **Scalars** (strings): the nearest (highest-precedence) value wins.
- **Tables** (e.g. `[registries.<name>]`): merged key-by-key across tiers; inner keys use nearest-wins.
- **Layering**: every file in the chain is loaded in precedence order and merged. Explicit paths (`OCX_CONFIG`, `--config`) do not replace the discovered tiers — they layer on top of them.

This means a `[registries.company]` entry defined in `/etc/ocx/config.toml` and a `[registries.private]` entry defined in `~/.config/ocx/config.toml` both end up in the resolved config — the user's file does not erase the system file's entry. Each layer contributes; only conflicting *values* on the same key are resolved by precedence.

## The Kill Switch {#kill-switch}

`OCX_NO_CONFIG=1` skips the **discovered chain only** — the system, user, and `$OCX_HOME` tiers. Explicit paths (`--config` and `OCX_CONFIG`) still load, because they represent deliberate intent rather than ambient environment.

This separation gives you all four common modes from two orthogonal primitives:

| Goal | Invocation |
|------|-----------|
| Default: use ambient config | _(no flags)_ |
| Layer an override on ambient config | `--config extra.toml` |
| Hermetic with a specific file | `OCX_NO_CONFIG=1 --config ci.toml` |
| Hermetic, no files at all | `OCX_NO_CONFIG=1` |

::: tip CI reproducibility
Set `OCX_NO_CONFIG=1` in CI environments where you need to guarantee that no ambient config file on the runner silently changes behavior. Pair it with `--config` or `OCX_CONFIG` if you still need to provide a checked-in CI config.
:::

## Worked Examples {#examples}

### Private default registry {#examples-private-registry}

Teams hosting packages on an internal or private registry can set it as the default so bare identifiers resolve there:

```toml
# ~/.ocx/config.toml
[registry]
default = "registry.company.com"
```

Any package without an explicit registry prefix — `cmake:3.28`, `myapp:1.0` — resolves to `registry.company.com`. Whether to put this in the user tier (`~/.config/ocx/config.toml`) or the OCX home tier (`~/.ocx/config.toml`) depends on whether the setting should travel with the data when `$OCX_HOME` is relocated.

### Docker image with system-wide config {#examples-docker}

Provision the OCX default registry in a Dockerfile so all users on the image share the same default:

```dockerfile
RUN mkdir -p /etc/ocx
COPY config.toml /etc/ocx/config.toml
```

```toml
# config.toml (mounted to /etc/ocx/config.toml)
[registry]
default = "registry.company.com"
```

The system tier is the natural home here — every user inside the container shares the default, and the file lives at a path the build pipeline already controls.

### CI with explicit config file {#examples-ci}

In CI, pair `OCX_CONFIG` with `OCX_NO_CONFIG=1` to guarantee a hermetic run — the ambient discovery chain is suppressed, and only the checked-in CI config loads:

::: code-group
```yaml [GitHub Actions]
env:
  OCX_NO_CONFIG: "1"
  OCX_CONFIG: ${{ github.workspace }}/.ocx-ci.toml
```

```toml [.ocx-ci.toml]
[registry]
default = "ghcr.io"
```
:::

Alternatively, skip the file entirely and rely on env vars:

```yaml
env:
  OCX_NO_CONFIG: "1"
  OCX_DEFAULT_REGISTRY: "ghcr.io"
```

Either form is hermetic. Choose the file form when humans on the team need to read what the CI run does without leaving the repository; choose the env-var form when the CI configuration already lives in the workflow file.

::: warning Always set the registry explicitly in automation
For scripts, CI pipelines, and programmatic tools, include the registry in every package identifier (e.g., `ghcr.io/cmake:3.28`) rather than relying on the config default. Explicit identifiers are immune to ambient config and survive across machines and environments.
:::

## See Also

- [Configuration reference][config-ref] — every key, type, default, error string
- [Environment variable reference][env-ref] — the `OCX_*` variables that override config keys
- [Command-line `--config` flag][arg-config] — invocation-level explicit config

<!-- external -->
[xdg-basedir]: https://specifications.freedesktop.org/basedir-spec/latest/
[apple-dirs]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html
[cargo-config]: https://doc.rust-lang.org/cargo/reference/config.html
[uv-config]: https://docs.astral.sh/uv/configuration/files/

<!-- reference -->
[config-ref]: ../reference/configuration.md
[config-precedence]: ../reference/configuration.md#precedence
[env-ref]: ../reference/environment.md
[arg-config]: ../reference/command-line.md#arg-config
