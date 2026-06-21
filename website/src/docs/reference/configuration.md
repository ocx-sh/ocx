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

### `[mirrors."<host>"]` {#keys-mirrors}

A mirror replaces the network endpoint for one upstream registry host. OCX appends the upstream repository path verbatim after the mirror's path prefix and contacts only the mirror — the upstream origin is never contacted on the read path.

This is a **source-replacement model**: if a mirror is configured for a host, all read traffic for that host goes to the mirror. There is no origin fallback. A mirror that is unreachable is a hard error — in firewall-controlled networks, fallback to the open internet would silently defeat the point.

```toml
[mirrors."ghcr.io"]
url = "https://company.jfrog.io/ghcr-remote"

[mirrors."docker.io"]
url = "https://company.jfrog.io/dockerhub-remote"
```

#### `url` {#keys-mirrors-url}

**Type**: string  
**Required at startup**: a missing or empty `url` is a hard error when OCX resolves the mirror map — same enforcement point as the [`[registries]`](#keys-registries) v1 scope.  
**Overridden by**: [`OCX_MIRRORS`][env-mirrors] — per-host key wins when both are set

The mirror endpoint: `scheme://host[/repo-key-prefix]`. OCX builds the full pull path as `<mirror-host>/<prefix>/<upstream-repo>`.

```toml
# Artifactory path-based routing (repository-path method):
# ghcr.io/owner/tool:1.2  →  company.jfrog.io/ghcr-remote/owner/tool:1.2
[mirrors."ghcr.io"]
url = "https://company.jfrog.io/ghcr-remote"

# Subdomain / host-only form (empty prefix):
# ghcr.io/owner/tool:1.2  →  ghcr-remote.company.jfrog.io/owner/tool:1.2
[mirrors."ghcr.io"]
url = "https://ghcr-remote.company.jfrog.io"
```

**Artifactory note.** The `url` is the Docker/OCI *pull* path: `<host>/<repo-key>`. This is not the Artifactory admin REST path (`/artifactory/api/docker/<repo-key>`) — that path is for administrative operations and is not a valid Docker pull URL. The pull path is what you would use with `docker pull` or `oras pull`.

**[Nexus][nexus-docs] 3.83+ path-based routing** uses the same `<host>/<repo-key>` shape as Artifactory — the repo-key alone, without any prefix:

```toml
# Nexus Repository 3.83+ path-based routing (repo-key only, no /repository/ prefix):
# ghcr.io/owner/tool:1.2  →  nexus.corp/docker-proxy/owner/tool:1.2
[mirrors."ghcr.io"]
url = "https://nexus.corp/docker-proxy"
```

::: warning Nexus legacy form
The legacy `/repository/<name>` URL form (e.g. `https://nexus.corp/repository/docker-proxy`) is **not** used with Nexus 3.83+ path routing. Use the repo-key alone as the path prefix, matching the Artifactory convention above.
:::

Older Nexus deployments expose each repository on a per-repository port. Those use the host-only mirror form (`https://nexus.corp:8082` — no path prefix).

**Harbor** follows the same `<host>/<project-name>/<image>` shape for its project-level proxy caches.

**Docker Hub `library/` images.** OCX appends the repository path verbatim and does not expand Docker Hub short names. For Docker Hub official images, use the fully-qualified form (`docker.io/library/alpine`) so the mirror URL resolves to `<mirror>/<prefix>/library/alpine`.

**Scheme default.** When `url` has no `scheme://` prefix (e.g., `"nexus.corp/docker-proxy"`), OCX defaults to `https`. Explicit `https://` is recommended for clarity.

**Plain-HTTP mirrors.** A `url` starting with `http://` requires the mirror host to be listed in [`OCX_INSECURE_REGISTRIES`][env-insecure-registries]. If the mirror host is absent, OCX exits at startup with an actionable error naming the variable and the mirror host — it does not silently downgrade TLS. The check runs before any network activity.

::: info Typo protection
`[mirrors."<host>"]` uses `deny_unknown_fields` — a typo such as `urll = "..."` is a TOML parse error, not a silent no-op. This matches the `[registries.<name>]` behavior.
:::

#### Merge behavior {#keys-mirrors-merge}

`[mirrors."<host>"]` entries are merged key-by-key across config tiers, following the same nearest-wins rule as [`[registries.<name>]`](#keys-registries). A higher-precedence tier that sets `[mirrors."ghcr.io"]` replaces the lower-tier entry for that host; hosts not mentioned in the higher tier are untouched.

[`OCX_MIRRORS`][env-mirrors] overrides on a per-host basis: a host key present in `OCX_MIRRORS` replaces the config entry for that host; hosts absent from `OCX_MIRRORS` still come from `[mirrors]`.

#### Auth {#keys-mirrors-auth}

Credentials are resolved against the **mirror** host, not the upstream. Configure them with `OCX_AUTH_<mirror_slug>_*` or via [`docker login`][docker-login] against the mirror host. The upstream's credentials are never consulted on the read path.

#### Interactions {#keys-mirrors-interactions}

| Concern | Behavior |
|---------|----------|
| `[registry] default` / `OCX_DEFAULT_REGISTRY` | Default injection runs before mirror rewrite. A bare identifier expanded to the default registry is then mirrored if that registry has a `[mirrors]` entry. |
| `--offline` | No network activity at all; mirrors are not consulted. |
| `--remote` | Mutable lookups (tag list, tag→digest resolution) hit the **mirror**, not the origin. |
| `ocx.lock` | Stores canonical upstream coordinates and per-platform leaf digests — not the mirror host. A lock made behind a mirror is valid on a machine with direct egress, and vice versa. |
| `push` | Push is not mirror-redirected. The canonical upstream host is contacted. Remote/proxy repositories are read-only; redirecting push would fail confusingly. |
| `ocx index catalog` | Against a proxy-type mirror, the catalog lists only repositories the proxy has cached. This is a registry-side constraint, not an OCX behavior. |

## Environment Variable Override Table {#env-overrides}

This table shows which OCX environment variables map to config file fields. Variables not listed here have no config equivalent.

| Environment Variable | Config Equivalent | Notes |
|---------------------|-------------------|-------|
| [`OCX_DEFAULT_REGISTRY`][env-default-registry] | `[registry] default` | Env var wins when both are set |
| [`OCX_MIRRORS`][env-mirrors] | `[mirrors."<host>"] url` | Env var wins per-host key when both are set; hosts absent from env var still come from config |
| [`OCX_PATCHES`][env-ocx-patches] | `[patches] registry` / `path` / `required` | Forwarded JSON wire format; overrides the config-file tier on process boundaries |
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

## JSON Schemas {#schemas}

OCX publishes JSON Schemas for every config and project file at stable URLs. IDEs and language servers ([taplo][taplo], [yaml-language-server][yaml-ls], VS Code, Zed) consume them for autocompletion, hover docs, and validation.

| File | Schema URL |
|------|------------|
| `config.toml` (any tier) | [`https://ocx.sh/schemas/config/v1.json`][schema-config] |
| `ocx.toml` (project) | [`https://ocx.sh/schemas/project/v1.json`][schema-project] |
| `ocx.lock` (project lock — machine-generated) | [`https://ocx.sh/schemas/project-lock/v2.json`][schema-project-lock] |
| `metadata.json` (package) | [`https://ocx.sh/schemas/metadata/v1.json`][schema-metadata] |

`ocx init` writes a `#:schema https://ocx.sh/schemas/project/v1.json` directive on the first line of every generated `ocx.toml`, so [taplo][taplo]-aware editors pick the schema up automatically with no extra wiring. To opt other files in by hand, prepend the same directive at the top of the file. The `project-lock` schema carries a top-level `$comment` flagging it as machine-generated — never hand-edit `ocx.lock`; rerun [`ocx lock`][cmd-lock] instead.

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

### `[patches]` section {#keys-patches}

The `[patches]` tier points at an operator-controlled OCI registry that hosts
[patch descriptors][patches-user-guide]. Descriptors map glob patterns over package
identifiers to **companion packages** — small packages that carry site-specific
environment overlays (CA bundles, proxy endpoint variables, license-server hints). At
exec time OCX composes matched companions' `interface` environment entries on top of the
base package's entries without modifying the base package.

The `[patches]` tier is the execution-environment twin of `[mirrors]`: `[mirrors]`
adapts where bytes come from; `[patches]` adapts what environment a tool runs in. Both
are opt-in and configured here.

```toml
[patches]
registry = "registry.corp.example/ocx-patches"
path     = "{registry}/{repository}"
required = true
```

#### `registry` {#keys-patches-registry}

**Type**: string  
**Required**: yes — absent or empty is a hard error at config resolve time.  
**Overridden by**: [`OCX_PATCHES`][env-ocx-patches] (JSON wire format forwarded to subprocesses)

The OCI registry root that hosts patch descriptors. The global descriptor (`__ocx.patch`
at the registry root) applies to all packages; per-package descriptors live at
sub-paths computed from the `path` template.

```toml
[patches]
registry = "registry.corp.example/ocx-patches"
```

#### `path` {#keys-patches-path}

**Type**: string  
**Default**: `{registry}/{repository}`

Template for per-package patch repository paths. Two placeholder tokens are substituted
at runtime:

| Token | Expands to |
|-------|-----------|
| `{registry}` | Slugified registry host of the base package (e.g. `ocx.sh` stays `ocx.sh`; `localhost:5000` becomes `localhost_5000`) |
| `{repository}` | Repository path of the base package verbatim (e.g. `java` for `ocx.sh/java:21`) |

The default `{registry}/{repository}` is suitable for most setups. Customise only if
the patch registry lays out sub-paths differently:

```toml
[patches]
registry = "registry.corp.example/ocx-patches"
path     = "bases/{repository}"
```

The expanded path always produces a non-empty sub-path. The registry root is reserved
for the global descriptor.

#### `required` {#keys-patches-required}

**Type**: boolean  
**Default**: `true`

Fail posture when a matched companion package is unavailable.

| Value | Behavior |
|-------|----------|
| `true` (default) | Execution aborts if a matched companion cannot be resolved. Use for security-critical companions (CA bundles, proxy config) where running without the companion is unsafe. |
| `false` | OCX logs a warning and continues. Use for non-security companions (metrics endpoints, license server hints). |

#### Scopes and merge {#keys-patches-scopes}

The `[patches]` section follows the same multi-tier merge as `[mirrors]`. A
higher-precedence config tier (user scope > `$OCX_HOME` scope > system scope) overrides
fields field-by-field.

**System-required posture.** When `[patches]` is declared at the system scope
(`/etc/ocx/config.toml`) with `required = true` — or with no `required` line, which
defaults to `true` — the tier is locked as **system-required**. A system-required tier
cannot be redirected, suppressed, or flipped to fail-open by any lower-precedence tier,
including `OCX_PATCHES` or per-package `no-patches`. This is the fail-closed enforcement
point for corporate CA distribution.

An explicit `required = false` in the system config is NOT locked; a lower-precedence
tier may still override it.

#### Per-package opt-out {#keys-patches-no-patches}

A project can opt a specific base package out of the user-scope or project-scope patch
tier by adding a `[package."<id>"]` table with `no-patches = true` to `ocx.toml`:

```toml
[package."ocx.sh/cmake:3.28"]
no-patches = true
```

A system-required tier is never skipped by `no-patches`.

### `[clean]` section {#future-clean}

Retention policy configuration will live under `[clean]`. Deferred to the retention policy feature.

### Project-level `ocx.toml` {#future-project}

A project-level `ocx.toml` is now shipped — see the [Project Toolchain section in the user guide](../user-guide.md#project-toolchain) for the schema, locking model, and activation hooks. The file name is deliberately different from `config.toml` so the data-directory tier and project tier are never confused: `ocx.toml` is loaded by a distinct API and never participates in the ambient config chain described above.
:::

<!-- external -->
[toml]: https://toml.io/
[cargo-registries]: https://doc.rust-lang.org/cargo/reference/registries.html
[taplo]: https://taplo.tamasfe.dev/
[yaml-ls]: https://github.com/redhat-developer/yaml-language-server
[nexus-docs]: https://help.sonatype.com/en/proxy-repository.html
[docker-login]: https://docs.docker.com/reference/cli/docker/login/

<!-- schemas -->
[schema-config]: https://ocx.sh/schemas/config/v1.json
[schema-project]: https://ocx.sh/schemas/project/v1.json
[schema-project-lock]: https://ocx.sh/schemas/project-lock/v2.json
[schema-metadata]: https://ocx.sh/schemas/metadata/v1.json

<!-- in-depth -->
[config-indepth]: ../in-depth/configuration.md

<!-- commands -->
[arg-config]: ./command-line.md#arg-config
[cmd-lock]: ./command-line.md#lock

<!-- environment -->
[env-ocx-home]: ./environment.md#ocx-home
[env-default-registry]: ./environment.md#ocx-default-registry
[env-config]: ./environment.md#ocx-config
[env-no-config]: ./environment.md#ocx-no-config
[env-offline]: ./environment.md#ocx-offline
[env-remote]: ./environment.md#ocx-remote
[env-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-mirrors]: ./environment.md#ocx-mirrors
[env-ocx-patches]: ./environment.md#ocx-patches

<!-- patches user guide -->
[patches-user-guide]: ../user-guide/patches.md
[env-no-update-check]: ./environment.md#ocx-no-update-check
[env-no-modify-path]: ./environment.md#ocx-no-modify-path
[env-ocx-binary-pin]: ./environment.md#ocx-binary-pin
[xdg-basedir]: ./environment.md#external-xdg-config-home
