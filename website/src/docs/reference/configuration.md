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
| 5 | [`[managed]`](#keys-managed) snapshot | Local, identity-gated; see [Precedence and snapshot](#keys-managed-precedence) |
| 6 | [`OCX_CONFIG`][env-config] | Layered on top of the discovered chain and the managed snapshot |
| 7 | [`--config`][arg-config] `FILE` | Layered on top of [`OCX_CONFIG`][env-config] |
| 8 | Environment variables (`OCX_*`) | Always win over any config file |
| 9 (highest) | CLI flags | Per-invocation; always win |

### Merge rules {#precedence-merge}

- **Scalars**: the nearest (highest-precedence) value wins.
- **Tables** (e.g. [`[registries.<name>]`](#keys-registries)): merged key-by-key across tiers; inner keys use nearest-wins.
- **Layering**: every file is loaded and merged in order. Explicit paths do not replace the discovered tiers.

### Kill switch {#precedence-kill-switch}

[`OCX_NO_CONFIG`][env-no-config]`=1` skips the **discovered chain** (tiers 2–4) and the [`[managed]`](#keys-managed) snapshot (tier 5) — hermetic means hermetic, so the [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] env-override read is suppressed along with the candidate itself. Explicit paths ([`--config`][arg-config], [`OCX_CONFIG`][env-config]) still load.

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

#### System-locked {#keys-registry-system-lock}

When `[registry]` is declared at the system scope (`/etc/ocx/config.toml`), it is locked **unconditionally** — unlike [`[patches]`'s system-required posture](#keys-patches-scopes), there is no `required` field to gate the lock on. A bare `[registry] default = "..."` at system scope is enough: no lower-precedence config-file tier (user, [`$OCX_HOME`][env-ocx-home], [`OCX_CONFIG`][env-config], [`--config`][arg-config], or a [`[managed]`](#keys-managed) payload) can change `default` once the system tier sets it.

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
`url` and `index` are defined in v1. The `[registries.<name>]` table is reserved for per-registry settings — future fields (`insecure`, `location` rewrite, `timeout`, auth) will slot into the same entry without breaking existing configs. Unknown fields inside an entry are rejected (typo protection); unknown top-level sections are silently ignored (forward compatibility).
:::

#### `index` {#keys-registries-index}

**Type**: string

Selects the resolution protocol for this namespace. An entry that sets `index` resolves through the [ocx-index protocol][in-depth-indices-public] (root document → observation object → platform selection) against that base URL; an entry without `index` — or no entry at all — resolves as a plain OCI registry. There is exactly one resolution protocol per namespace: OCX never falls back from the index protocol to plain OCI tags, or the reverse.

```toml
[registries."ocx.sh"]
index = "https://index.ocx.sh"
```

`index` needs no `<dialect>+` URL-scheme prefix, because OCX has exactly one index wire dialect — the field's presence is the kind marker, the same convention [Cargo][cargo-registries] uses for its own `[registries.NAME] index = "…"`. `index` is a second, independent field on the same entry as [`url`](#keys-registries-url): a `[registries.<name>]` entry may declare a hostname alias, an index URL, or both.

::: info Why the resolved physical pointer uses `oci://`, never `http(s)`
A [derived index's][in-depth-indices-dispatch] local root document — the file `ocx index update` writes under `$OCX_HOME/index/<source>/p/<ns>/<pkg>.json` — records the package's resolved physical location as `oci://<host>/<repository>`, not `http://` or `https://`. That scheme marks the reference *kind* — "an OCI registry repository" — not a transport to dial. Transport is a host-side decision: it comes from a [`[mirrors]`](#keys-mirrors) entry's own scheme for that host, or the plain-HTTP allowance in [`OCX_INSECURE_REGISTRIES`][env-insecure-registries]. If the pointer itself carried `http://` or `https://` instead, a publisher able to write that shared identity data could force every consumer resolving it down to plaintext — a scheme belongs to the operator who configures the host, never to data that travels with a package's identity.
:::

#### System-locked {#keys-registries-system-lock}

Each `[registries.<name>]` entry declared at the system scope is locked the same way as [`[registry]`](#keys-registry-system-lock) — unconditionally, per entry, covering both `url` and `index`. This closes an indirection a bare `[registry]` lock would leave open: without it, a lower tier could leave a locked `[registry] default = "company"` alone and instead redirect `[registries.company] url` to a different host, changing where the locked default actually resolves. Locking the named entry itself closes that path.

### `[mirrors]` {#keys-mirrors}

A mirror replaces the network endpoint for one host — but a host can serve two different kinds of traffic. **Registry** traffic is the OCI `/v2` distribution API (manifests, layers). **Index** traffic is the plain-HTTPS static files an [ocx-index source][in-depth-indices-public] serves (`config.json`, `c/`, `p/`). The two usually live on different hosts entirely — `ghcr.io` serves registry traffic for a package, `index.ocx.sh` serves index traffic for that same package's version pointer — so `[mirrors]` is keyed by whichever host is actually being redirected, and each entry states which role(s) the redirect covers.

```toml
[mirrors]
"ghcr.io" = "https://company.jfrog.io/ghcr-remote"                     # both roles → one host
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }      # index role only
"registry-1.docker.io" = { registry = "http://mirror.local:5000" }     # registry role only
```

A **plain string** value redirects both roles for that host — the common case, where one corporate proxy fronts everything a host serves. An **object** `{ registry?, index? }` splits per role: `registry` redirects `/v2` distribution traffic, `index` redirects the index static-file tree. A role field left out of the object means no redirect for that role — there is no fallthrough to the other form.

This is a **source-replacement model**: once a role is configured for a host, all matching read traffic for that host goes to the mirror. There is no origin fallback. An unreachable mirror is a hard error — in firewall-controlled networks, falling back to the open internet would silently defeat the point.

#### Value shape {#keys-mirrors-value}

**Type**: string, or an object with optional `registry` and `index` string fields  
**Required at startup**: an entry with an empty string, or an object where every present field is empty, is a hard error when OCX resolves the mirror map — same enforcement point as the [`[registries]`](#keys-registries) v1 scope.  
**Overridden by**: [`OCX_MIRRORS`][env-mirrors] — per-host, per-role; a role set in `OCX_MIRRORS` wins over the same role from the config entry

Each role's value is `scheme://host[/repo-key-prefix]`. For the **registry** role, OCX builds the full pull path as `<mirror-host>/<prefix>/<upstream-repo>`:

```toml
# Artifactory path-based routing (repository-path method):
# ghcr.io/owner/tool:1.2  →  company.jfrog.io/ghcr-remote/owner/tool:1.2
[mirrors]
"ghcr.io" = "https://company.jfrog.io/ghcr-remote"

# Subdomain / host-only form (empty prefix):
# ghcr.io/owner/tool:1.2  →  ghcr-remote.company.jfrog.io/owner/tool:1.2
[mirrors]
"ghcr.io" = "https://ghcr-remote.company.jfrog.io"
```

**Artifactory note.** The registry-role value is the Docker/OCI *pull* path: `<host>/<repo-key>`. This is not the Artifactory admin REST path (`/artifactory/api/docker/<repo-key>`) — that path is for administrative operations and is not a valid Docker pull URL. The pull path is what you would use with `docker pull` or `oras pull`.

**[Nexus][nexus-docs] 3.83+ path-based routing** uses the same `<host>/<repo-key>` shape as Artifactory — the repo-key alone, without any prefix:

```toml
# Nexus Repository 3.83+ path-based routing (repo-key only, no /repository/ prefix):
# ghcr.io/owner/tool:1.2  →  nexus.corp/docker-proxy/owner/tool:1.2
[mirrors]
"ghcr.io" = "https://nexus.corp/docker-proxy"
```

::: warning Nexus legacy form
The legacy `/repository/<name>` URL form (e.g. `https://nexus.corp/repository/docker-proxy`) is **not** used with Nexus 3.83+ path routing. Use the repo-key alone as the path prefix, matching the Artifactory convention above.
:::

Older Nexus deployments expose each repository on a per-repository port. Those use the host-only mirror form (`https://nexus.corp:8082` — no path prefix).

**Harbor** follows the same `<host>/<project-name>/<image>` shape for its project-level proxy caches.

**Docker Hub `library/` images.** OCX appends the repository path verbatim and does not expand Docker Hub short names. For Docker Hub official images, use the fully-qualified form (`docker.io/library/alpine`) so the mirror URL resolves to `<mirror>/<prefix>/library/alpine`.

**Index role.** The same `scheme://host[/path-prefix]` shape applies to `index`, and OCX contacts it for every root, observation-object, and catalog fetch a resolved namespace's [ocx-index protocol][in-depth-indices-public] makes — content is still verified by SHA-256 against the digest recorded in the fetched object, so the mirror changes only where bytes come from, never whether they are trusted.

**Same-host co-serving.** The two roles are path-disjoint (`/v2` versus `config.json`/`c/`/`p/`), so an object entry can point both roles at the same host without collision if a deployment ever serves both from one proxy.

**Scheme default.** When a role's value has no `scheme://` prefix (e.g., `"nexus.corp/docker-proxy"`), OCX defaults to `https`. Explicit `https://` is recommended for clarity.

**Plain-HTTP mirrors.** A role value starting with `http://` requires the mirror host to be listed in [`OCX_INSECURE_REGISTRIES`][env-insecure-registries] — the same gate applies to both the registry and index roles. If the mirror host is absent, OCX exits at startup with an actionable error naming the variable and the mirror host — it does not silently downgrade TLS. The check runs before any network activity.

::: info Typo protection
`[mirrors]` values parse against a named shape — a string, or an object with only `registry`/`index` fields — with per-field errors rather than an opaque "did not match any variant" message. A typo such as `{ registr = "..." }` is a parse error naming the unrecognized field, not a silent no-op.
:::

#### System-locked {#keys-mirrors-system-lock}

A `[mirrors]` entry declared at the system scope locks unconditionally, **per role** — the same enforcement as [`[registry]`](#keys-registry-system-lock), narrowed to whichever role(s) the system-scope value covers. A plain-string system entry locks both roles for that host; an object entry with only `index` set locks the index role and leaves the registry role open to a lower tier — a corporate policy can pin where index traffic goes while leaving OCI mirror choice to the project. A lower-precedence tier cannot add, change, or remove a role the system tier already locked for a host; other roles for that host, and hosts the system tier did not mention, still resolve through ordinary merge.

#### Merge behavior {#keys-mirrors-merge}

`[mirrors]` entries merge **field-wise** across config tiers, not whole-entry: OCX normalizes every value — string or object — to its two roles before merging, so a higher-precedence tier that sets only the `index` role for a host leaves a lower tier's `registry` role for that host untouched, and vice versa. A higher-precedence plain-string entry sets both roles and so overrides both, same as before.

[`OCX_MIRRORS`][env-mirrors] overrides on the same per-host, per-role basis: a role present in a host's `OCX_MIRRORS` entry replaces the config entry for that role only; roles and hosts absent from `OCX_MIRRORS` still come from `[mirrors]`.

#### Auth {#keys-mirrors-auth}

Credentials are resolved against the **mirror** host, not the upstream. Configure them with `OCX_AUTH_<mirror_slug>_*` or via [`docker login`][docker-login] against the mirror host. The upstream's credentials are never consulted on the read path. Static-file index endpoints have no OCI token flow, so there is no equivalent auth mechanism for the index role today — this is deferred until a deployment needs authenticated access to a mirrored index.

#### Interactions {#keys-mirrors-interactions}

| Concern | Behavior |
|---------|----------|
| `[registry] default` / `OCX_DEFAULT_REGISTRY` | Default injection runs before mirror rewrite. A bare identifier expanded to the default registry is then mirrored if that registry has a `[mirrors]` entry. |
| `--offline` | No network activity at all; mirrors are not consulted. |
| `--remote` | Mutable lookups (tag list, tag→digest resolution) hit the **mirror**, not the origin. |
| `ocx.lock` | Stores canonical upstream coordinates and per-platform leaf digests — not the mirror host. A lock made behind a mirror is valid on a machine with direct egress, and vice versa. |
| `push` | Push is not mirror-redirected. The canonical upstream host is contacted. Remote/proxy repositories are read-only; redirecting push would fail confusingly. |
| `ocx index catalog` / `ocx index update` | Against a namespace resolving through the [ocx-index protocol][in-depth-indices-public], every root, observation-object, and catalog fetch honors that host's **index** role only — unrelated to the same host's `registry` role, if any. Against a plain OCI registry mirror, the catalog lists only repositories a proxy-type mirror has cached — a registry-side constraint, not an OCX behavior. |

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
**Required**: no — omitting `registry` (or the whole `[patches]` section) simply leaves the patch tier inactive. Only a *present-but-empty* `registry = ""` is a hard error at config resolve time — same footgun-guard as an empty [`[mirrors]` `url`](#keys-mirrors-url).  
**Overridden by**: [`OCX_PATCHES`][env-ocx-patches] (JSON wire format forwarded to subprocesses)

The OCI registry root that hosts patch descriptors. The global descriptor (`__ocx.patch`
at the reserved `global` repository, e.g. `<registry>/global:__ocx.patch`) applies to
all packages; per-package descriptors live at sub-paths computed from the `path` template.

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

The expanded path always produces a non-empty sub-path. The reserved `global` repository
name is the fixed location of the global descriptor and must not be used as a per-package path.

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
higher-precedence config tier (`$OCX_HOME` scope > user scope > system scope) overrides
fields field-by-field.

**System-required posture.** When `[patches]` is declared at the system scope
(`/etc/ocx/config.toml`) with `required = true` — or with no `required` line, which
defaults to `true` — the tier is locked as **system-required**. A system-required tier
cannot be redirected, suppressed, or flipped to fail-open by any higher-precedence tier,
including `OCX_PATCHES` or per-package `no-patches`. This is the fail-closed enforcement
point for corporate CA distribution.

An explicit `required = false` in the system config is NOT locked; a higher-precedence
tier may still override it.

#### Per-package opt-out {#keys-patches-no-patches}

A project can opt a specific base package out of the user-scope or project-scope patch
tier by adding a `[package."<id>"]` table with `no-patches = true` to `ocx.toml`:

```toml
[package."ocx.sh/cmake:3.28"]
no-patches = true
```

The match is by canonical `registry/repository` — tag and digest are stripped, so the
opt-out is version-independent: it follows every tag of `ocx.sh/cmake`, not just `3.28`.

A system-required tier is never skipped by `no-patches`, regardless of which surface below
resolved the opt-out.

**Where the opt-out is honored.** The opt-out is a project-toolchain concern: it only takes
effect where a project's `ocx.toml` is directly in scope. That covers three commands —
[`ocx run`][cmd-run], [`ocx env`][cmd-env-root], and [`ocx direnv export`][cmd-direnv-export] —
each of which reads the project config and composes the environment itself.

A fourth surface reaches the opt-out indirectly: a tool spawned by `ocx run` that re-enters
ocx through its own generated launcher (`ocx launcher exec`). `ocx run` forwards the opt-out
to that child process over [`OCX_PATCHES`][env-ocx-patches] — including, for each opted-out
base actually resolved that run, its content digest, since a launcher resolves its base via a
synthetic content-addressed identifier with no real `registry/repository` to match against.

A **direct** launcher invocation — one not spawned by an `ocx run` that forwarded the
opt-out, for example a generated launcher run standalone, or reached through the OCI-tier
[`ocx package exec`][cmd-package-exec] — has no forwarded opt-out to decode and does not
honor `no-patches`. It composes the same companion overlay [`ocx package env`][cmd-package-env]
would for the same base.

See [Patch Opt-Out Scope][env-composition-patch-opt-out] for the full forwarding mechanics.

### `[managed]` section {#keys-managed}

The `[managed]` tier is a **seed pointer**, not the settings themselves. It names an
operator-published OCX package whose content is a plain `config.toml` — typically
`[mirrors]`, a `[patches]` pointer, and a default `[registry]` — synced into local state
and merged above the user config on every invocation. Where `[mirrors]` and `[patches]`
are configured by hand on every machine, `[managed]` lets an operator publish one
package (via [`ocx config push`][cmd-config-push]) and have every workstation and CI
runner converge on it.

Unknown fields inside `[managed]` are ignored, matching every other section — fleet
forward-compatibility: a seed written for a newer ocx must not break older binaries
reading the same file. The cost is that a typo'd key silently no-ops;
[`ocx config update --check`][cmd-config-update] surfaces the tier's effective state for
diagnosis.

```toml
[managed]
source   = "internal.company.com/ocx-config:user"
required = true
refresh  = "notify"
interval = "1d"
```

This block is normally written by [`ocx config setup`][cmd-config-setup] (or
[`ocx self setup --managed-config <ref>`][cmd-self-setup], which runs the same adoption)
rather than hand-edited — both re-serialize the same four fields with their
resolved values. Bootstrapping this way performs a synchronous fetch before the fence is
written, so a network failure leaves no partial seed. See the
[managed-configuration walkthrough][user-guide-managed-config] for the full onboarding
flow.

#### `source` {#keys-managed-source}

**Type**: string  
**Required**: yes, at resolve time — omitting `source` (or the whole `[managed]` section) leaves the tier inactive. A present-but-empty `source = ""` is a hard error, the same footgun guard as [`[patches]` `registry`](#keys-patches-registry) and [`[mirrors]` `url`](#keys-mirrors-url).  
**Overridden by**: [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] — invocation-only, never written back to the seed

The OCI reference for the managed-config package: `<registry>/<repository>[:<tag>][@<digest>]`, parsed with the same [`Identifier`](#keys-registry-default) grammar as any other package reference. A registry-less `source` resolves against the **built-in** default registry (`ocx.sh`), never a configured `[registry] default` — the managed tier's trust root can not be redirected by the very config it is about to replace. Use a fully qualified reference in corporate seeds.

A `source` pinned by digest (`…@sha256:<hex>`) binds the tier to that exact content: the [`required` gate](#keys-managed-required) accepts only a snapshot carrying that digest, so a drifted registry (or a `config update <VERSION>` to anything else) fails closed until the seed pin is updated.

#### `required` {#keys-managed-required}

**Type**: boolean  
**Default**: `true`

Fail posture when no local snapshot matches `source`.

| Value | Behavior |
|-------|----------|
| `true` (default) | Every command fails closed with `SnapshotRequired` (exit 78) until [`ocx config update`][cmd-config-update] (or [`ocx config setup`][cmd-config-setup] / `ocx self setup --managed-config`) syncs a matching snapshot. Identical online and offline — the gate is on local disk state, not network reachability. |
| `false` | The tier contributes nothing until synced. A throttle-gated stderr hint is printed instead of failing (no per-invocation warning). |

#### `refresh` {#keys-managed-refresh}

**Type**: string (`"apply"` \| `"notify"` \| `"manual"`)  
**Default**: `"notify"`

Background refresh posture, checked at most once per [`interval`](#keys-managed-interval). [`ocx config update`][cmd-config-update] always bypasses this — it is explicit user intent, mirroring [`ocx self update`][cmd-self-update].

| Value | Behavior |
|-------|----------|
| `apply` | Drift against the registry silently triggers a full fetch, persist, and snapshot swap. |
| `notify` (default) | Drift prints a stderr advisory ("run `ocx config update`"); content is not fetched by the tick. |
| `manual` | The background tick is skipped entirely; only an explicit [`ocx config update`][cmd-config-update] refreshes the snapshot. |

[`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] kills the background tick regardless of `refresh`; an explicit `ocx config update` still works.

**Activation conditions.** The tick this posture governs only runs when *all* of the following hold: stderr is a terminal, the process is not running inside CI (`CI` unset), the invocation is not offline ([`--offline`][arg-offline]/[`OCX_OFFLINE`][env-offline]), the tier is not paused ([`ocx config update --pause`][cmd-config-update]), and the [`interval`](#keys-managed-interval) throttle window has elapsed. Any one of those failing skips the tick outright — so `refresh = "apply"` never auto-converges a CI runner or another headless host; those hosts converge only through an explicit [`ocx config update`][cmd-config-update].

#### `interval` {#keys-managed-interval}

**Type**: string, `\d+[smhd]?` (bare digits = seconds)  
**Default**: `"1d"`

Minimum spacing between background refresh probes. Governs only the automatic tick — [`ocx config update`][cmd-config-update] always bypasses it. `interval = "0"` (or `"0s"`) disables the throttle: the tick probes the registry on every eligible invocation instead of waiting out a window.

#### Precedence and snapshot {#keys-managed-precedence}

The managed tier folds in as priority 5 in the [precedence table](#precedence) — after the [`$OCX_HOME` config tier](#file-locations) and below [`OCX_CONFIG`][env-config]/[`--config`][arg-config]. Resolution reads a local snapshot only; no network access happens during ordinary config loading.

The snapshot lives at `$OCX_HOME/state/managed-config/snapshot.json` and is written only by [`ocx config update`][cmd-config-update], [`ocx config setup`][cmd-config-setup], or `ocx self setup --managed-config`. It records the source it was fetched from, the tag it tracked at that moment, the package's top-level manifest digest (the tier's drift identity), the fetch timestamp, and the payload text.

Before folding it in, OCX identity-gates the snapshot against the effective `source` (env override, then seed): the snapshot must come from the **same registry and repository**, and — when the seed pins a digest — carry exactly that digest. Tags float within a repository: a snapshot synced with `ocx config update user-1.4.1` still satisfies a seed tracking `:user`, which is what makes per-host version pins and rollbacks safe under a fleet-wide floating tag. A cross-repository or pin-violating snapshot is treated as entirely absent, regardless of `required`; this closes a CI cache-poisoning path where a stale `$OCX_HOME` carries a snapshot fetched for a different `source`.

A content-bearing pause file (`$OCX_HOME/state/managed-config/pause.json`, written by [`ocx config update --pause`][cmd-config-update]) sits beside the snapshot: while in force it short-circuits the background tick — and nothing else. Expired or corrupt pause files read as absent.

#### One-hop rule {#keys-managed-one-hop}

A `[managed]` section inside the fetched payload itself is stripped before merge, with a warning — the tier that fetched a payload can never be redirected or loosened by that same payload. Every other section in the payload (`[mirrors]`, `[patches]`, `[registry]`, …) merges normally.

#### System-lock interaction {#keys-managed-system-lock}

`[managed]` merges through the same [`Config::merge`](#precedence-merge) fold as every other tier, so a system-scope lock on [`[registry]`](#keys-registry-system-lock), [`[registries.<name>]`](#keys-registries-system-lock), or [`[mirrors]`](#keys-mirrors-system-lock) is never overridable by a managed payload — the lock applies before the managed tier's content is folded in, the same as it applies to any lower tier. `[managed]` also carries its own lock: a system-scope `[managed]` declaration with `required = true` (the default) is itself non-overridable by any lower tier, mirroring [`[patches]`'s system-required posture](#keys-patches-scopes).

## Environment Variable Override Table {#env-overrides}

This table shows which OCX environment variables map to config file fields. Variables not listed here have no config equivalent.

| Environment Variable | Config Equivalent | Notes |
|---------------------|-------------------|-------|
| [`OCX_DEFAULT_REGISTRY`][env-default-registry] | `[registry] default` | Env var wins when both are set |
| [`OCX_MIRRORS`][env-mirrors] | `[mirrors]` | Env var wins per host, per role when both are set; roles/hosts absent from env var still come from config |
| [`OCX_PATCHES`][env-ocx-patches] | `[patches] registry` / `path` / `required` | Forwarded JSON wire format; overrides the config-file tier on process boundaries |
| [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] | `[managed] source` | Invocation-only override, never written back; `=""` is treated as unset |
| [`OCX_HOME`][env-ocx-home] | None | Determines where config is loaded from; cannot be in a config file |
| [`OCX_CONFIG`][env-config] | None | Meta-variable pointing at the config file itself |
| [`OCX_NO_CONFIG`][env-no-config] | None | Kill switch; also suppresses the [`[managed]`](#keys-managed) snapshot candidate and the `OCX_MANAGED_CONFIG` env-override read |
| [`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] | None | Kill switch for the [`[managed]`](#keys-managed) background refresh tick only; explicit `ocx config update` still works |
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

OCX publishes JSON Schemas for every config, project, and patch file at stable URLs. IDEs and language servers ([taplo][taplo], [yaml-language-server][yaml-ls], VS Code, Zed) consume them for autocompletion, hover docs, and validation.

| File | Schema URL |
|------|------------|
| `config.toml` (any tier) | [`https://ocx.sh/schemas/config/v1.json`][schema-config] |
| `ocx.toml` (project) | [`https://ocx.sh/schemas/project/v1.json`][schema-project] |
| `ocx.lock` (project lock — machine-generated) | [`https://ocx.sh/schemas/project-lock/v2.json`][schema-project-lock] |
| `metadata.json` (package) | [`https://ocx.sh/schemas/metadata/v1.json`][schema-metadata] |
| Patch descriptor (`ocx patch publish --descriptor`) | [`https://ocx.sh/schemas/patch/v1.json`][schema-patch] |

`ocx init` writes a `#:schema https://ocx.sh/schemas/project/v1.json` directive on the first line of every generated `ocx.toml`, so [taplo][taplo]-aware editors pick the schema up automatically with no extra wiring. To opt other files in by hand, prepend the same directive at the top of the file. A patch descriptor is plain JSON, so add a `"$schema": "https://ocx.sh/schemas/patch/v1.json"` key to get the same autocompletion and validation while authoring it. The `project-lock` schema carries a top-level `$comment` flagging it as machine-generated — never hand-edit `ocx.lock`; rerun [`ocx lock`][cmd-lock] instead.

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
[schema-patch]: https://ocx.sh/schemas/patch/v1.json

<!-- in-depth -->
[config-indepth]: ../in-depth/configuration.md
[in-depth-indices-public]: ../in-depth/indices.md#public-index
[in-depth-indices-dispatch]: ../in-depth/indices.md#local-dispatch

<!-- commands -->
[arg-config]: ./command-line.md#arg-config
[arg-offline]: ./command-line.md#arg-offline
[cmd-lock]: ./command-line.md#lock
[cmd-run]: ./command-line.md#run
[cmd-env-root]: ./command-line.md#env-root
[cmd-direnv-export]: ./command-line.md#direnv-export
[cmd-package-exec]: ./command-line.md#package-exec
[cmd-package-env]: ./command-line.md#package-env
[cmd-config-setup]: ./command-line.md#config-setup
[cmd-self-setup]: ./command-line.md#self-setup
[cmd-self-update]: ./command-line.md#self-update
[cmd-config-update]: ./command-line.md#config-update
[cmd-config-push]: ./command-line.md#config-push

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
[env-ocx-managed-config]: ./environment.md#ocx-managed-config
[env-ocx-no-config-refresh]: ./environment.md#ocx-no-config-refresh

<!-- user guide -->
[user-guide-managed-config]: ../user-guide.md#managed-config

<!-- env composition -->
[env-composition-patch-opt-out]: ./env-composition.md#patch-opt-out-scope

<!-- patches user guide -->
[patches-user-guide]: ../user-guide/patches.md
[env-no-update-check]: ./environment.md#ocx-no-update-check
[env-no-modify-path]: ./environment.md#ocx-no-modify-path
[env-ocx-binary-pin]: ./environment.md#ocx-binary-pin
[xdg-basedir]: ./environment.md#external-xdg-config-home
