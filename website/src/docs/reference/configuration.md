---
layout: doc
outline: deep
---
# Configuration

## Overview {#overview}

The configuration file lets you set persistent defaults for OCX without modifying shell profiles or CI secrets. It is optional — OCX works without any config file using compiled-in defaults.

Config files live outside the [object store][fs-objects]. They do not affect installed packages or resolved digests; they only influence how OCX behaves at startup (which registry to use as the default, for example). Environment variables and CLI flags always win over any config file value.

Config files are in [TOML][toml] format.

## File Locations {#file-locations}

OCX looks for config files in three tiers, each serving a different scope:

| Tier | Path | Purpose |
|------|------|---------|
| System | `/etc/ocx/config.toml` | Machine-wide defaults set by sysadmins, Dockerfiles, or provisioning tools |
| User (Linux) | [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` or `~/.config/ocx/config.toml` | Personal defaults, separate from OCX data (`~/.ocx/`) per [XDG convention][xdg-basedir] |
| User (macOS) | `~/Library/Application Support/ocx/config.toml` | macOS follows [Apple's conventions][apple-dirs]; `XDG_CONFIG_HOME` is not consulted |
| OCX home | [`$OCX_HOME`][env-ocx-home]`/config.toml` (default: `~/.ocx/config.toml`) | Co-located with the data directory; survives a zip-and-move of [`$OCX_HOME`][env-ocx-home] |

Missing files are silently skipped. None of these files need to exist.

### OCX Home Tier {#config-home-tier}

The OCX home tier — [`$OCX_HOME`][env-ocx-home]`/config.toml` (default `~/.ocx/config.toml`) — is co-located with the OCX data directory. This is the only tier that moves with the data when you relocate [`$OCX_HOME`][env-ocx-home], making it the right home for settings that must survive a zip-and-move of an entire OCX install (for example, a portable OCX bundle carried between machines). The system and user tiers, by contrast, live under OS-specific locations that do not travel with the data.

### Explicit Additions {#file-locations-explicit}

Two mechanisms add an extra file on top of the discovery chain — they do not replace it. This supports the common "refine ambient config with a targeted override" use case:

- **[`--config`][arg-config] `FILE`** — CLI flag, passed before the subcommand
- **[`OCX_CONFIG_FILE`][env-config-file]`=/path/to/file.toml`** — environment variable, useful in CI and Docker where env vars are more practical than CLI flags

When set, the specified file layers at the top of the file-tier chain (above [`$OCX_HOME`][env-ocx-home]`/config.toml`). If the file is missing, that is an error — explicit paths must exist. Both can coexist; when both are set, the [`--config`][arg-config] file layers on top of the [`OCX_CONFIG_FILE`][env-config-file] file.

To disable the ambient [`OCX_CONFIG_FILE`][env-config-file] for a single invocation without unsetting it (common when it is exported from a shell profile), set it to the empty string:

```sh
OCX_CONFIG_FILE= ocx install cmake:3.28
```

Empty is the escape hatch — [`OCX_CONFIG_FILE`][env-config-file] set to empty string is treated as unset, not as an error.

## Discovery and Merge Precedence {#precedence}

Settings are resolved lowest-to-highest. Higher-precedence sources override lower ones.

| Priority | Source | Notes |
|----------|--------|-------|
| 1 (lowest) | Compiled defaults | Built into the OCX binary |
| 2 | System config — `/etc/ocx/config.toml` | Discovered tier |
| 3 | User config — [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` (Linux) or `~/Library/Application Support/ocx/config.toml` (macOS) | Discovered tier |
| 4 | OCX home config — [`$OCX_HOME`][env-ocx-home]`/config.toml` | Discovered tier |
| 5 | [`OCX_CONFIG_FILE`][env-config-file] | Layered on top of discovered tiers |
| 6 | [`--config`][arg-config] `FILE` | Layered on top of [`OCX_CONFIG_FILE`][env-config-file] |
| 7 | Environment variables (`OCX_*`) | Always win over any config file |
| 8 (highest) | CLI flags | Per-invocation; always win |

### Merge Rules {#precedence-merge}

- **Scalars** (strings): the nearest (highest-precedence) value wins.
- **Tables** (e.g. [`[registries.<name>]`](#keys-registries)): merged key-by-key across tiers; inner keys use nearest-wins.
- **Layering**: every file in the chain is loaded in precedence order and merged. Explicit paths ([`OCX_CONFIG_FILE`][env-config-file], [`--config`][arg-config]) do not replace the discovered tiers — they layer on top of them.

### Kill Switch {#precedence-kill-switch}

[`OCX_NO_CONFIG`][env-no-config]`=1` skips the **discovered chain only** — tiers 2–4 above (system, user, and [`$OCX_HOME`][env-ocx-home]). Explicit paths ([`--config`][arg-config] and [`OCX_CONFIG_FILE`][env-config-file]) still load, because they represent deliberate intent rather than ambient environment.

This separation gives you all four common modes from two orthogonal primitives:

| Goal | Invocation |
|------|-----------|
| Default: use ambient config | _(no flags)_ |
| Layer an override on ambient config | [`--config`][arg-config] `extra.toml` |
| Hermetic with a specific file | [`OCX_NO_CONFIG`][env-no-config]`=1 --config ci.toml` |
| Hermetic, no files at all | [`OCX_NO_CONFIG`][env-no-config]`=1` |

:::tip CI reproducibility
Set [`OCX_NO_CONFIG`][env-no-config]`=1` in CI environments where you need to guarantee that no ambient config file on the runner silently changes behavior. Pair it with [`--config`][arg-config] or [`OCX_CONFIG_FILE`][env-config-file] if you still need to provide a checked-in CI config.
:::

## Configuration Keys {#keys}

### `[registry]` {#keys-registry}

Global settings for the registry subsystem.

#### `default` {#keys-registry-default}

**Type**: string  
**Default**: `"ocx.sh"`  
**Overridden by**: [`OCX_DEFAULT_REGISTRY`][env-default-registry] environment variable

The default registry used for bare package identifiers — those without an explicit registry prefix. When you write `cmake:3.28`, OCX expands it to `<default>/cmake:3.28`.

The value may be either a literal hostname (`"ghcr.io"`) or the name of a [`[registries.<name>]`](#keys-registries) entry. When it matches a named entry, OCX resolves it to that entry's `url`.

```toml
[registry]
default = "ghcr.io"
```

With this config, `ocx install cmake:3.28` resolves to `ghcr.io/cmake:3.28` instead of `ocx.sh/cmake:3.28`.

:::warning Always set the registry explicitly in automation
For scripts, CI pipelines, and programmatic tools, include the registry in every package identifier (e.g., `ghcr.io/cmake:3.28`) rather than relying on the config default. Explicit identifiers are immune to ambient config and survive across machines and environments.
:::

### `[registries.<name>]` {#keys-registries}

Per-registry settings, keyed by a friendly name. Each entry configures one registry; [`[registry] default`](#keys-registry-default) can then reference it by name rather than by hostname.

The plural form (`registries`, not `registry`) is deliberate: it mirrors [Cargo's convention][cargo-registries] and avoids a TOML collision with the singular [`[registry]`](#keys-registry) global-settings section.

#### `url` {#keys-registries-url}

**Type**: string

The actual registry hostname this entry resolves to. When `[registry] default` names this entry, OCX uses `url` as the effective default registry hostname.

```toml
[registry]
default = "company"

[registries.company]
url = "registry.company.example"

[registries.ghcr]
url = "ghcr.io"
```

With this config, `ocx install cmake:3.28` resolves to `registry.company.example/cmake:3.28`. The `ghcr` entry is defined but unused until [`[registry] default`](#keys-registry-default) references it, or until a future feature (per-registry insecure flag, location rewrite) consumes it.

:::info v1 scope
Only `url` is defined in v1. The `[registries.<name>]` table is reserved for per-registry settings — future fields (`insecure`, `location` rewrite, `timeout`, auth) will slot into the same entry without breaking existing configs. Unknown fields inside an entry are rejected (typo protection); unknown top-level sections are silently ignored (forward compatibility).
:::

## Environment Variable Override Table {#env-overrides}

This table shows which OCX environment variables map to config file fields. Variables not listed here have no config equivalent — they remain environment-variable-only.

| Environment Variable | Config Equivalent | Notes |
|---------------------|-------------------|-------|
| [`OCX_DEFAULT_REGISTRY`][env-default-registry] | `[registry] default` | Env var wins when both are set |
| [`OCX_HOME`][env-ocx-home] | None | Determines where config is loaded from; cannot be in a config file |
| [`OCX_CONFIG_FILE`][env-config-file] | None | Meta-variable pointing at the config file itself |
| [`OCX_NO_CONFIG`][env-no-config] | None | Kill switch; cannot be represented in a config file by definition |
| [`OCX_OFFLINE`][env-offline] | None | Per-invocation mode, not a persistent setting |
| [`OCX_REMOTE`][env-remote] | None | Per-invocation debugging mode, not a persistent setting |
| [`OCX_INSECURE_REGISTRIES`][env-insecure-registries] | None (deferred) | Will move to a per-entry `insecure` field under [`[registries.<name>]`](#keys-registries) once the flag is implemented; the env var remains the source of truth today |
| [`OCX_NO_UPDATE_CHECK`][env-no-update-check] | None | CI-only concern; env var is sufficient |
| [`OCX_NO_MODIFY_PATH`][env-no-modify-path] | None | Install-time concern; env var is sufficient |

[`OCX_OFFLINE`][env-offline] and [`OCX_REMOTE`][env-remote] are intentionally absent from the config file. Both are per-invocation modes — a persistent `offline = true` would silently break `ocx install` on a fresh setup.

## Example Configs {#examples}

### Private default registry {#examples-private-registry}

Teams hosting packages on an internal or private registry can set it as the default so bare identifiers resolve there:

```toml
# ~/.ocx/config.toml
[registry]
default = "registry.company.com"
```

Any package without an explicit registry prefix — `cmake:3.28`, `myapp:1.0` — resolves to `registry.company.com`.

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

### CI with explicit config file {#examples-ci}

In CI, pair [`OCX_CONFIG_FILE`][env-config-file] with [`OCX_NO_CONFIG`][env-no-config]`=1` to guarantee a hermetic run — the ambient discovery chain is suppressed, and only the checked-in CI config loads:

::: code-group
```yaml [GitHub Actions]
env:
  OCX_NO_CONFIG: "1"
  OCX_CONFIG_FILE: ${{ github.workspace }}/.ocx-ci.toml
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

## Error Reference {#errors}

Literal sizes in the examples below reflect the current 64 KiB safety cap (`MAX_CONFIG_SIZE` in the loader source). Angle-bracket placeholders such as `<SIZE>` stand in for runtime values that depend on the offending file.

| Error | Cause | Resolution |
|-------|-------|-----------|
| `error: config file not found: /path/to/file.toml (check --config or OCX_CONFIG_FILE)` | [`--config`][arg-config] or [`OCX_CONFIG_FILE`][env-config-file] points to a non-existent file | Check the path; unlike the three discovery tiers, explicit paths must exist. To disable an ambient [`OCX_CONFIG_FILE`][env-config-file] without unsetting it, set it to the empty string. |
| `error: config file /path/to/file.toml exceeds maximum allowed size (<SIZE> bytes > 65536 bytes); OCX config files are typically under 1 KiB — did you point at the wrong file` | A config file is larger than the 64 KiB safety cap | The hint usually explains it — a `--config` flag or `OCX_CONFIG_FILE` env var pointed at a non-config file (e.g. an archive or binary). |
| `error: invalid TOML at /path/to/file.toml: ...` | TOML syntax error in the config file | Fix the TOML syntax error at the indicated location |
| `error: failed to read config file /path/to/file.toml: ...` | The file exists but cannot be read — permission denied, the path is a directory, or another I/O failure | Check file permissions; [`--config`][arg-config] and [`OCX_CONFIG_FILE`][env-config-file] must point to a regular, readable file. |

## Future Config Keys {#future}

::: details Not yet implemented in v1

These sections are documented here so the format design is stable before they land. They do not exist in the current release.

### Per-registry fields beyond `url` {#future-registries-fields}

The [`[registries.<name>]`](#keys-registries) table is live in v1, but only `url` is defined. Future per-registry fields will slot in without breaking existing configs:

```toml
# Future shape (not in v1 — only `url` is implemented today):
[registries.private]
url = "registry.company.example"
insecure = false                 # per-registry TLS opt-out
location = "mirror.company.example"  # URL rewrite / mirror
```

### `[patches]` section {#future-patches}

Infrastructure patch entries will live under `[patches]`. This section is reserved for the patches feature and is ignored by the v1 loader.

### `[clean]` section {#future-clean}

Retention policy configuration will live under `[clean]`. Deferred to the retention policy feature.

### Project-level `ocx.toml` {#future-project}

A project-level `ocx.toml` is now shipped — see the [Project Toolchain section in the user guide](../user-guide.md#project-toolchain) for the schema, locking model, and activation hooks. The file name is deliberately different from `config.toml` so the data-directory tier and project tier are never confused: `ocx.toml` is loaded by a distinct API and never participates in the ambient config chain described above.
:::

<!-- external -->
[toml]: https://toml.io/
[cargo-config]: https://doc.rust-lang.org/cargo/reference/config.html
[cargo-registries]: https://doc.rust-lang.org/cargo/reference/registries.html
[uv-config]: https://docs.astral.sh/uv/configuration/files/
[apple-dirs]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html

<!-- commands -->
[arg-config]: ./command-line.md#arg-config

<!-- environment -->
[env-ocx-home]: ./environment.md#ocx-home
[env-default-registry]: ./environment.md#ocx-default-registry
[env-config-file]: ./environment.md#ocx-config-file
[env-no-config]: ./environment.md#ocx-no-config
[env-offline]: ./environment.md#ocx-offline
[env-remote]: ./environment.md#ocx-remote
[env-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-no-update-check]: ./environment.md#ocx-no-update-check
[env-no-modify-path]: ./environment.md#ocx-no-modify-path
[xdg-basedir]: ./environment.md#external-xdg-config-home

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-packages
