---
layout: doc
outline: deep
---
# Configuration

API reference for OCX configuration files. For the rationale behind the tier model, the merge philosophy, and worked examples, see the [Configuration in-depth page][config-indepth].

Config files are in [TOML][toml] format and are optional. OCX works without any config file using compiled-in defaults.

## File Locations {#file-locations}

| Tier | Path | Purpose |
|------|------|---------|
| System | `/etc/ocx/config.toml` | Machine-wide defaults |
| User (Linux) | [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` or `~/.config/ocx/config.toml` | Per-user defaults |
| User (macOS) | `~/Library/Application Support/ocx/config.toml` | Per-user defaults; `XDG_CONFIG_HOME` not consulted |
| OCX home | [`$OCX_HOME`][env-ocx-home]`/config.toml` (default: `~/.ocx/config.toml`) | Co-located with data; survives a zip-and-move of [`$OCX_HOME`][env-ocx-home] |

Missing files are silently skipped.

### Explicit additions {#file-locations-explicit}

Two mechanisms add a file *on top of* the discovery chain — they do not replace it. Missing files are an error in this case (explicit paths must exist).

- **[`--config`][arg-config] `FILE`** — CLI flag, before subcommand
- **[`OCX_CONFIG`][env-config]`=/path/to/file.toml`** — environment variable

When both are set, [`--config`][arg-config] layers on top of [`OCX_CONFIG`][env-config]. Setting [`OCX_CONFIG`][env-config] to the empty string disables an ambient value without unsetting it.

## Discovery and Merge Precedence {#precedence}

Settings are resolved lowest-to-highest. Higher-precedence sources override lower ones.

| Priority | Source | Notes |
|----------|--------|-------|
| 1 (lowest) | Compiled defaults | Built into the OCX binary |
| 2 | System config — `/etc/ocx/config.toml` | Discovered tier |
| 3 | User config — [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` (Linux) or `~/Library/Application Support/ocx/config.toml` (macOS) | Discovered tier |
| 4 | OCX home config — [`$OCX_HOME`][env-ocx-home]`/config.toml` | Discovered tier |
| 5 | [`OCX_CONFIG`][env-config] | Layered on top of discovered tiers |
| 6 | [`--config`][arg-config] `FILE` | Layered on top of [`OCX_CONFIG`][env-config] |
| 7 | Environment variables (`OCX_*`) | Always win over any config file |
| 8 (highest) | CLI flags | Per-invocation; always win |

### Merge rules {#precedence-merge}

- **Scalars**: the nearest (highest-precedence) value wins.
- **Tables** (e.g. [`[registries.<name>]`](#keys-registries)): merged key-by-key across tiers; inner keys use nearest-wins.
- **Layering**: every file is loaded and merged in order. Explicit paths do not replace the discovered tiers.

### Kill switch {#precedence-kill-switch}

[`OCX_NO_CONFIG`][env-no-config]`=1` skips the **discovered chain only** (tiers 2–4). Explicit paths ([`--config`][arg-config], [`OCX_CONFIG`][env-config]) still load.

| Goal | Invocation |
|------|-----------|
| Default | _(no flags)_ |
| Layer override on ambient | [`--config`][arg-config] `extra.toml` |
| Hermetic with a specific file | [`OCX_NO_CONFIG`][env-no-config]`=1 --config ci.toml` |
| Hermetic, no files | [`OCX_NO_CONFIG`][env-no-config]`=1` |

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

::: info v1 scope
Only `url` is defined in v1. The `[registries.<name>]` table is reserved for per-registry settings — future fields (`insecure`, `location` rewrite, `timeout`, auth) will slot into the same entry without breaking existing configs. Unknown fields inside an entry are rejected (typo protection); unknown top-level sections are silently ignored (forward compatibility).
:::

## Environment Variable Override Table {#env-overrides}

This table shows which OCX environment variables map to config file fields. Variables not listed here have no config equivalent.

| Environment Variable | Config Equivalent | Notes |
|---------------------|-------------------|-------|
| [`OCX_DEFAULT_REGISTRY`][env-default-registry] | `[registry] default` | Env var wins when both are set |
| [`OCX_HOME`][env-ocx-home] | None | Determines where config is loaded from; cannot be in a config file |
| [`OCX_CONFIG`][env-config] | None | Meta-variable pointing at the config file itself |
| [`OCX_NO_CONFIG`][env-no-config] | None | Kill switch; cannot be represented in a config file by definition |
| [`OCX_OFFLINE`][env-offline] | None | Per-invocation mode, not a persistent setting |
| [`OCX_REMOTE`][env-remote] | None | Per-invocation debugging mode, not a persistent setting |
| [`OCX_BINARY_PIN`][env-ocx-binary-pin] | None | Subprocess-only: set automatically by ocx on every spawn so child ocx invocations pin to the same binary |
| [`OCX_INSECURE_REGISTRIES`][env-insecure-registries] | None (deferred) | Will move to a per-entry `insecure` field under [`[registries.<name>]`](#keys-registries) once the flag is implemented; the env var remains the source of truth today |
| [`OCX_NO_UPDATE_CHECK`][env-no-update-check] | None | CI-only concern; env var is sufficient |
| [`OCX_NO_MODIFY_PATH`][env-no-modify-path] | None | Install-time concern; env var is sufficient |

[`OCX_OFFLINE`][env-offline] and [`OCX_REMOTE`][env-remote] are intentionally absent from the config file. Both are per-invocation modes — a persistent `offline = true` would silently break `ocx install` on a fresh setup.

## Error Reference {#errors}

Literal sizes in the examples below reflect the current 64 KiB safety cap (`MAX_CONFIG_SIZE` in the loader source). Angle-bracket placeholders such as `<SIZE>` stand in for runtime values that depend on the offending file.

| Error | Cause | Resolution |
|-------|-------|-----------|
| `error: config file not found: /path/to/file.toml (check --config or OCX_CONFIG)` | [`--config`][arg-config] or [`OCX_CONFIG`][env-config] points to a non-existent file | Check the path; unlike the three discovery tiers, explicit paths must exist. To disable an ambient [`OCX_CONFIG`][env-config] without unsetting it, set it to the empty string. |
| `error: config file /path/to/file.toml exceeds maximum allowed size (<SIZE> bytes > 65536 bytes); OCX config files are typically under 1 KiB — did you point at the wrong file` | A config file is larger than the 64 KiB safety cap | The hint usually explains it — a `--config` flag or `OCX_CONFIG` env var pointed at a non-config file (e.g. an archive or binary). |
| `error: invalid TOML at /path/to/file.toml: ...` | TOML syntax error in the config file | Fix the TOML syntax error at the indicated location |
| `error: failed to read config file /path/to/file.toml: ...` | The file exists but cannot be read — permission denied, the path is a directory, or another I/O failure | Check file permissions; [`--config`][arg-config] and [`OCX_CONFIG`][env-config] must point to a regular, readable file. |

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

A CWD-walk for a project-level `ocx.toml` is planned. The file name is deliberately different from `config.toml` so the data-directory tier and project tier are never confused. When it lands, it will sit between `$OCX_HOME/config.toml` and `OCX_CONFIG` in the precedence order.
:::

<!-- external -->
[toml]: https://toml.io/
[cargo-registries]: https://doc.rust-lang.org/cargo/reference/registries.html

<!-- in-depth -->
[config-indepth]: ../in-depth/configuration.md

<!-- commands -->
[arg-config]: ./command-line.md#arg-config

<!-- environment -->
[env-ocx-home]: ./environment.md#ocx-home
[env-default-registry]: ./environment.md#ocx-default-registry
[env-config]: ./environment.md#ocx-config
[env-no-config]: ./environment.md#ocx-no-config
[env-offline]: ./environment.md#ocx-offline
[env-remote]: ./environment.md#ocx-remote
[env-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-no-update-check]: ./environment.md#ocx-no-update-check
[env-no-modify-path]: ./environment.md#ocx-no-modify-path
[env-ocx-binary-pin]: ./environment.md#ocx-binary-pin
[xdg-basedir]: ./environment.md#external-xdg-config-home
