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
| 1 (lowest) | Compiled defaults | Built into the OCX binary: [`[registries."ocx.sh"] index`](#keys-registries-index) |
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

[`OCX_NO_CONFIG`][env-no-config]`=1` skips the **discovered chain** (tiers 2–4) and the [`[managed]`](#keys-managed) snapshot (tier 5) — hermetic means hermetic, so the [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] env-override read is suppressed along with the candidate itself. Explicit paths ([`--config`][arg-config], [`OCX_CONFIG`][env-config]) still load, and so do the compiled defaults (tier 1) — they are part of the binary, not ambient host state.

| Goal | Invocation |
|------|-----------|
| Default | _(no flags)_ |
| Layer override on ambient | [`--config`][arg-config] `extra.toml` |
| Hermetic with a specific file | [`OCX_NO_CONFIG`][env-no-config]`=1 --config ci.toml` |
| Hermetic, no files | [`OCX_NO_CONFIG`][env-no-config]`=1` |

## Configuration Keys {#keys}

### Unknown keys and sections {#unknown-keys}

**OCX ignores what it does not recognize** — an unknown top-level section, and an unknown key inside any known section, in every table below. The file still loads, and every setting this ocx does understand takes effect.

This is a deliberate trade, and the reason is the [`[managed]`](#keys-managed) tier: one `config.toml` is read by every ocx version in a fleet at once. A file written against a newer ocx has to degrade to "the parts I understand" on an older binary. Rejecting it instead would take the *whole* file out of service on every host that had not upgraded yet — one new key in a central rollout, and the mirror map and patch registry vanish fleet-wide.

The cost is that a typo silently does nothing. `[registries."ocx.sh"] indx = "..."` sets no index; `[mirrors."ghcr.io"] registr = "..."` mirrors nothing. Two things blunt it:

- A typo never *becomes* the field it resembles, and never widens anything: an unknown key cannot populate [`trusted_hosts`](#keys-registries-trusted-hosts) or any other setting.
- The [config schema][schema-config] lists every key OCX knows. Point your editor at it and a typo is flagged as you write, which is where you want to catch it.

For a managed payload, [`ocx config update --check`][cmd-config-update] shows what the tier actually resolved to. Before publishing, [`ocx config test`][cmd-config-test] runs the same lookup against a candidate file that has not been pushed yet.

**When ignoring is not enough.** Tolerance covers *added* keys. It cannot cover a change in what an existing key means or what shape its value takes — an older binary would read the new value with the old meaning. Those changes travel by tag instead: the tier's `source` is an ordinary OCI reference, so publish the incompatible payload under a new tag (`:user-2`) and leave `:user` serving the old one. Fleets move over as they upgrade; nothing has to happen in lockstep. See [Rolling out an incompatible change][user-guide-managed-config-incompatible].

### `[registry]` {#keys-registry}

Global settings for the registry subsystem.

#### `default` {#keys-registry-default}

**Type**: string  
**Default**: `"ocx.sh"`  
**Overridden by**: [`OCX_DEFAULT_REGISTRY`][env-default-registry] environment variable

The default registry used for bare package identifiers — those without an explicit registry prefix. When you write `kitware/cmake:3.28`, OCX expands it to `<default>/cmake:3.28`.

`default` is always a literal identifier prefix — the same string used as a [`[registries.<name>]`](#keys-registries) table key. OCX never dereferences it through any other field; every `[registries.<name>]` key is an identifier prefix, always.

```toml
[registry]
default = "ghcr.io"
```

#### System-locked {#keys-registry-system-lock}

When `[registry]` is declared at the system scope (`/etc/ocx/config.toml`), it is locked **unconditionally** — unlike [`[patches]`'s system-required posture](#keys-patches-scopes), there is no `required` field to gate the lock on. A bare `[registry] default = "..."` at system scope is enough: no lower-precedence config-file tier (user, [`$OCX_HOME`][env-ocx-home], [`OCX_CONFIG`][env-config], [`--config`][arg-config], or a [`[managed]`](#keys-managed) payload) can change `default` once the system tier sets it.

### `[registries.<name>]` {#keys-registries}

Per-registry settings, keyed by a friendly name. Each entry configures one registry; [`[registry] default`](#keys-registry-default) can then reference it by name rather than by hostname.

The plural form (`registries`, not `registry`) is deliberate: it mirrors [Cargo's convention][cargo-registries] and avoids a TOML collision with the singular [`[registry]`](#keys-registry) global-settings section.

::: info v1 scope
`index` and `trusted_hosts` are defined in v1. The `[registries.<name>]` table is reserved for per-registry settings — future fields (`insecure`, `location` rewrite, `timeout`, auth) will slot into the same entry without breaking existing configs. Unknown fields inside an entry are ignored, like unknown keys and sections everywhere else in the file — see [Unknown keys and sections](#unknown-keys).
:::

#### `index` {#keys-registries-index}

**Type**: string

Selects the resolution protocol for this namespace. An entry that sets `index` resolves through the [ocx-index protocol][in-depth-indices-public] (root document → OCI image index → platform selection) against that base URL; an entry without `index` — or no entry at all — resolves as a plain OCI registry. There is exactly one resolution protocol per namespace: for every name the index serves, OCX never falls back from the index protocol to plain OCI tags, or the reverse — an index that has no root for such a name fails the resolve, it does not quietly try the registry.

An index may **decline** a name outright, which is not a fallback. Its [`config.json`][in-depth-indices-declared-names] can publish a `name_segments` count declaring the shape of names it is able to hold at all; `index.ocx.sh` publishes `2`, because its root schema pins a package name to `<namespace>/<package>`. For a name of a different shape OCX still asks for the root, but reads its absence as "never claimed" rather than "not found here": that name resolves as plain OCI. An index that *does* serve a root for such a name keeps full authority over it — the served root always outranks the declaration. An index that publishes no `name_segments` declares no constraint and stays authoritative for every name in its namespace.

The [compiled-defaults tier](#precedence) ships exactly this entry, so `ocx.sh` is index-bearing out of the box:

```toml
[registries."ocx.sh"]
index = "https://index.ocx.sh"
```

Any tier above it can restate `index` with a different base URL to route `ocx.sh` through a private index. Setting it to the empty string is the off-switch: an empty base URL is not a kind marker, so the namespace resolves as a plain OCI registry.

```toml
[registries."ocx.sh"]
index = ""                    # resolve ocx.sh as a plain OCI registry
```

Pinning the namespace at a [`[mirrors]`](#keys-mirrors) **registry** endpoint is the second off-switch, and it is implicit. Such an entry suppresses the compiled-in `index` for `ocx.sh`, which then resolves as a plain OCI registry through the mirror:

```toml
[mirrors]
"ocx.sh" = "https://artifactory.corp/ocx-remote"   # compiled-in index suppressed
```

`[mirrors]` is keyed by *traffic host* and rewrites the physical location a name resolves to, so it does not follow a namespace through the index protocol — an air-gapped site that routes `ocx.sh` to its own registry would otherwise start dialling `index.ocx.sh`, a host it never allow-listed. Declaring where a namespace's traffic goes answers the question.

Two limits keep the switch from firing where it was not meant to:

- **Only a config file you control** — the compiled defaults, the discovered chain, and [`--config`][arg-config]/[`OCX_CONFIG`][env-config]. A `[mirrors]` entry arriving through the [`[managed]`](#keys-managed) tier or through [`OCX_MIRRORS`][env-mirrors] redirects traffic like any other, but cannot suppress the index: neither a remotely-published payload nor an inherited environment variable may drop a namespace off the verified resolution path and its yank gate.
- **Only the `registry` role.** The `index` role is applied keyed on the *index endpoint's* own host, so `"ocx.sh" = { index = … }` cannot redirect anything for the `ocx.sh` namespace and does not suppress. Redirecting the index endpoint itself keeps the index path, pointed at your host:

```toml
[mirrors]
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }   # index kept, served by corp
```

To keep the index path *and* pin the physical registry, name the index explicitly — a written `index` outranks the compiled-in one and is never suppressed:

```toml
[registries."ocx.sh"]
index = "https://index.ocx.sh"                     # explicit: survives the mirror entry

[mirrors]
"ocx.sh" = "https://artifactory.corp/ocx-remote"
"index.ocx.sh" = { index = "https://artifactory.corp/ocx-index" }
```

The suppression applies only to the compiled-in tier and is logged at `warn` level, naming the namespace it dropped.

`index` needs no `<dialect>+` URL-scheme prefix, because OCX has exactly one index wire dialect — the field's presence is the kind marker, the same convention [Cargo][cargo-registries] uses for its own `[registries.NAME] index = "…"`. An entry with no `index` field still resolves as plain OCI — it can still declare [`trusted_hosts`](#keys-registries-trusted-hosts) for its physical registry, `index` and `trusted_hosts` are independent fields. Omitting `index` from an entry does not clear an inherited value: tiers merge field-wise, so a `[registries."ocx.sh"]` entry that declares only `trusted_hosts` keeps the compiled-in `index`. Only an explicit `index = ""` clears it.

::: warning An index-bearing namespace has no registry fallback
The named index is the sole authority for its namespace — a yanked tag, a tampered index object, or an unreachable endpoint is a hard error, never a silent fall-through to a registry serving the same name (see [index.ocx.sh][in-depth-indices-public]). An `index.ocx.sh` outage therefore blocks `ocx.sh/…` resolution; other namespaces are unaffected.
:::

::: info Why the resolved physical pointer uses `oci://`, never `http(s)`
A [derived index's][in-depth-indices-dispatch] local root document — the file `ocx index update` writes under `$OCX_HOME/index/<source>/p/<ns>/<pkg>.json` — records the package's resolved physical location as `oci://<host>/<repository>`, not `http://` or `https://`. That scheme marks the reference *kind* — "an OCI registry repository" — not a transport to dial. Transport is a host-side decision: it comes from a [`[mirrors]`](#keys-mirrors) entry's own scheme for that host, or the plain-HTTP allowance in [`OCX_INSECURE_REGISTRIES`][env-insecure-registries]. If the pointer itself carried `http://` or `https://` instead, a publisher able to write that shared identity data could force every consumer resolving it down to plaintext — a scheme belongs to the operator who configures the host, never to data that travels with a package's identity.
:::

##### `file://` bases {#keys-registries-index-file}

`index` also accepts a `file://` base, read straight off disk with no server — the consuming half of
[serving a local index snapshot][in-depth-indices-servable]:

```toml
[registries."corp"]
index = "file:///srv/ocx-index/corp"
```

Two requirements, checked at startup rather than at first fetch — except under
[`--offline`][arg-offline], where no index source is built at all, so neither check runs and the
command exits 0:

- **Empty authority.** `file:///srv/ocx-index/corp` (three slashes) is a local path; `file://host/…`
  or `file://localhost/…` is a UNC/remote form and is refused — a `file` base never dials a network.
- **Absolute path.** A relative tail, the bare filesystem root (`file:///`), and a bare Windows drive
  designator (`file:///C:/`) are all refused rather than silently resolving against wherever `ocx`
  happened to be launched from.

A `file` base is never host-keyed, so it is **not a valid [`[mirrors]`](#keys-mirrors) override
target**: the index role there redirects a *host's* traffic, and a `file://` base has no host to key
on. Point `index` itself at the path instead of trying to reach it through `[mirrors]`.

Beyond the two startup checks, every fetch through a `file://` base carries the same guarantees the
HTTPS transport has, adapted to a filesystem:

- **Read-only.** There is no write path — nothing under this namespace's index base is ever created,
  modified, or deleted through the `file://` transport.
- **Size-bounded.** A document over the same size cap the HTTPS transport enforces is refused, not
  silently truncated — the read is bounded by bytes actually consumed, never trusted from file
  metadata.
- **Symlink-contained.** A path staged with symlinks (an `rsync`, hardlink, or symlink layout) is
  followed, but the resolved target must stay under the configured root once both are canonicalized;
  one that resolves outside it is refused rather than served.
- **Regular files only.** A directory, device node, FIFO, or anything else that is not a plain file is
  refused — including a mid-read swap that would otherwise slip one past the initial check.

Everything else about the namespace is unchanged: `file://` is still the [ocx-index protocol][in-depth-indices-public]
(root document → OCI image index → platform selection), just read from a directory tree instead of
over HTTPS, and every object is still verified against its recorded digest.

#### `trusted_hosts` {#keys-registries-trusted-hosts}

**Type**: array of strings (hostnames or CIDR blocks)

The SSRF escape hatch for this namespace's physical hosts. Before OCX dereferences an index root's `oci://<host>/<repository>` pointer into a physical registry fetch, it refuses any host that resolves to a private, loopback, link-local, or cloud-metadata address — that pointer is remote-controlled data, and a compromised or mirrored index could otherwise aim it at an internal service. A private registry legitimately lives on such an address, so listing its host or network here restores access for exactly this namespace without weakening the guard anywhere else.

Each entry is either an exact hostname or a CIDR block; a listed target skips the address check.

```toml
[registries."corp"]
index = "https://index.corp.example"
trusted_hosts = ["10.0.0.0/8", "registry.corp"]
```

The guard is default-on and needs no configuration for public registries. There is no command-line flag to widen the trust set — the exemption lives only on the config entry, so a [system-locked](#keys-registries-system-lock) entry's `trusted_hosts` cannot be broadened by a lower tier or a CLI override. A refused host exits with a configuration error that names the host and points back to `trusted_hosts`.

#### System-locked {#keys-registries-system-lock}

Each `[registries.<name>]` entry declared at the system scope is locked the same way as [`[registry]`](#keys-registry-system-lock) — unconditionally, per entry, covering both `index` and `trusted_hosts`. A lower tier cannot flip a locked entry's resolution protocol or widen its SSRF trust set.

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

**Index role.** The same `scheme://host[/path-prefix]` shape applies to `index`, and OCX contacts it for every root, index-object, and catalog fetch a resolved namespace's [ocx-index protocol][in-depth-indices-public] makes — content is still verified by SHA-256 against the digest recorded in the fetched object, so the mirror changes only where bytes come from, never whether they are trusted.

**Same-host co-serving.** The two roles are path-disjoint (`/v2` versus `config.json`/`c/`/`p/`), so an object entry can point both roles at the same host without collision if a deployment ever serves both from one proxy.

**Scheme default.** When a role's value has no `scheme://` prefix (e.g., `"nexus.corp/docker-proxy"`), OCX defaults to `https`. Explicit `https://` is recommended for clarity.

**Plain-HTTP mirrors.** A role value starting with `http://` requires the mirror host to be listed in [`OCX_INSECURE_REGISTRIES`][env-insecure-registries] — the same gate applies to both the registry and index roles. If the mirror host is absent, OCX exits at startup with an actionable error naming the variable and the mirror host — it does not silently downgrade TLS. The check runs before any network activity.

::: info Malformed values
`[mirrors]` values parse against a named shape — a string, or an object with only `registry`/`index` fields — with per-field errors rather than an opaque "did not match any variant" message. A role with a non-string value (`{ registry = 5 }`) is a parse error naming the offending host and field.

An *unrecognized* key is a different case: it is ignored, like unknown keys everywhere else in the file (see [Unknown keys and sections](#unknown-keys)). An entry left with no role OCX recognizes — `{ registr = "..." }`, or an entry declaring only a role a future ocx will understand — contributes no mirror for that host and is skipped. Nothing else in the file is affected.
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
| `ocx index catalog` / `ocx index update` | Against a namespace resolving through the [ocx-index protocol][in-depth-indices-public], every root, index-object, and catalog fetch honors that host's **index** role only — unrelated to the same host's `registry` role, if any. Against a plain OCI registry mirror, the catalog lists only repositories a proxy-type mirror has cached — a registry-side constraint, not an OCX behavior. |

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
[package."ocx.sh/kitware/cmake:3.28"]
no-patches = true
```

The match is by canonical `registry/repository` — tag and digest are stripped, so the
opt-out is version-independent: it follows every tag of `ocx.sh/kitware/cmake`, not just `3.28`.

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
package (via [`ocx config push`][cmd-config-push], previewed locally first with
[`ocx config test`][cmd-config-test]) and have every workstation and CI runner converge
on it.

Unknown fields inside `[managed]` are ignored, and so is everything unrecognized in the
payload it points at — see [Unknown keys and sections](#unknown-keys), which exists
because of this tier. A payload written against a newer ocx applies its known parts on
older fleet binaries instead of taking the whole thing out of service on them.

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

Fail posture when the tier contributes nothing.

| Value | Behavior |
|-------|----------|
| `true` (default) | Every command fails closed with `SnapshotRequired` (exit 78) until [`ocx config update`][cmd-config-update] (or [`ocx config setup`][cmd-config-setup] / `ocx self setup --managed-config`) syncs a matching snapshot. Identical online and offline — the gate is on local disk state, not network reachability. |
| `false` | The tier contributes nothing until synced. A throttle-gated stderr hint is printed instead of failing (no per-invocation warning). |

The gate is on what actually reached the merged config, not merely on a file being present. A snapshot that matches `source` but whose payload does not parse as a config applies nothing, so `required = true` fails closed on it too — with `SnapshotUnusable` (also exit 78), which names the real problem instead of reporting a snapshot that is sitting right there as absent. Under `required = false` the same state is a warning and the tier stays empty. Note this is about a *broken* payload: unknown keys and sections are not broken (see [Unknown keys and sections](#unknown-keys)) and fold normally.

#### `refresh` {#keys-managed-refresh}

**Type**: string (`"apply"` \| `"notify"` \| `"manual"`)  
**Default**: `"notify"`

Background refresh posture, checked at most once per [`interval`](#keys-managed-interval). [`ocx config update`][cmd-config-update] always bypasses this — it is explicit user intent, mirroring [`ocx self update`][cmd-self-update].

| Value | Behavior |
|-------|----------|
| `apply` | Drift against the registry silently triggers a full fetch, persist, and snapshot swap. |
| `notify` (default) | Drift prints a stderr advisory ("run `ocx config update`"); content is not fetched by the tick. |
| `manual` | The background tick is skipped entirely; only an explicit [`ocx config update`][cmd-config-update] refreshes the snapshot. |

[`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] kills the background tick regardless of `refresh`; an explicit [`ocx config update`][cmd-config-update] still works — and so does the reconciling re-sync [`ocx self setup`][cmd-self-setup] and [`ocx config setup`][cmd-config-setup] run against an already-adopted seed on every invocation. This variable governs the background tick only; use [`--offline`][arg-offline] to skip the setup-time re-sync instead.

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

`[managed]` merges through the same [`Config::merge`](#precedence-merge) fold as every other tier, so a system-scope lock on [`[registry]`](#keys-registry-system-lock), [`[registries.<name>]`](#keys-registries-system-lock), or [`[mirrors]`](#keys-mirrors-system-lock) is never overridable by a managed payload — the lock applies before the managed tier's content is folded in, the same as it applies to any lower tier. `[managed]` also carries its own lock: a system-scope `[managed]` declaration with `required = true` (the default) is itself non-overridable by any lower tier, mirroring [`[patches]`'s system-required posture](#keys-patches-scopes). [`[[trust.policy]]`](#keys-trust) locks differently, because it pools instead of replacing: a system-scope policy [governs the scopes it matches alone](#keys-trust-system-lock), so a managed payload can neither outbid it with a narrower scope nor enroll a signer alongside it — it can only pin scopes the system tier never mentions.
### `[[trust.policy]]` {#keys-trust}

[`ocx package verify`][cmd-package-verify] checks a [Sigstore][sigstore] signature's
certificate against an expected identity and OIDC issuer, supplied either as flags
(`--certificate-identity` / `--certificate-oidc-issuer`) or, once declared here, resolved
automatically for any package whose identifier falls under a policy's scope.

```toml
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "https://github.com/acme/tool/.github/workflows/release.yml@refs/heads/main"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

`[[trust.policy]]` is an array-of-tables — declare one entry per accepted signer per scope.
It is valid in every `config.toml` tier ([system, user, `$OCX_HOME`](#file-locations)) **and**
in the project `ocx.toml`. Reading it from `ocx.toml` is a deliberate exception: every other
OCI-tier command ignores `ocx.toml` entirely, but a trust policy is a security posture the
checkout owner controls, not toolchain-binding resolution. The two sources are not equal
peers, though — see [Tier precedence](#keys-trust-merge) below.

#### Fields {#keys-trust-fields}

Fields split across two levels — `scope` and `builder` are declared directly on `[[trust.policy]]`; `identity`, `identity_regexp` and `oidc_issuer` belong to its `[trust.policy.keyless]` sub-table, since identity matching and provenance-builder matching are independent checks.

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `scope` | string | yes | Package prefix this policy applies to, e.g. `"ghcr.io/acme/*"`. See [Scope matching](#keys-trust-scope). |
| `builder` | string | no | Expected SLSA provenance `builder.id` (byte-equal). Only consulted when verifying an attestation whose predicate is SLSA provenance ([`verify --attestation`][cmd-package-verify-attestations]); ignored for a plain signature or any other predicate type. A mismatch is `builder_mismatch` (exit 65). |

**`[trust.policy.keyless]`:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `identity` | string | XOR with `identity_regexp` | Exact expected certificate SAN (byte-equal). |
| `identity_regexp` | string | XOR with `identity` | Regex the certificate SAN must match in full. See [Regex identities](#keys-trust-regex). |
| `oidc_issuer` | string | yes | Exact expected OIDC issuer URL (byte-equal). No regex form in this release — issuer URLs are stable. |

Exactly one of `identity` / `identity_regexp` must be set — both present, or both absent, is a
configuration error. Unknown keys are ignored, like everywhere else in `config.toml`: a file
written for a newer ocx must still load on an older one, so a typo'd key (e.g. `scop`) is
silently dropped rather than rejected. What still fails the entry is a missing **required**
field — a policy without `scope` or `oidc_issuer` is a parse error, and one that ends up with
neither `identity` nor `identity_regexp` is rejected when the policy compiles, never silently
treated as "trust anything".

#### Scope matching {#keys-trust-scope}

A scope matches the target's canonical `registry/repository` (tag and digest stripped) on
**path-segment boundaries** — for the two safe forms below. A scope with no `*` matches the
exact package or a package directly under it: `scope = "ghcr.io/acme/tool"` matches
`ghcr.io/acme/tool` and `ghcr.io/acme/tool/plugin`, but **not** `ghcr.io/acme/tool-cli` (and
`scope = "ghcr.io/acme"` never matches `ghcr.io/acmecorp`). A trailing `/*` is the explicit
subtree glob (`scope = "ghcr.io/acme/*"` covers everything under `ghcr.io/acme/`, but not
`ghcr.io/acmecorp`). An empty scope is a catch-all.

:::warning A mid-string `*` is a substring match, not segment-bounded
Segment-boundary matching holds only for the no-wildcard and trailing-`/*` forms. A `*`
placed anywhere else globs on the literal text before it, with no `/` boundary enforced:
`scope = "ghcr.io/acme*"` matches `ghcr.io/acmecorp` and `ghcr.io/acme-evil` because it is a
plain literal-prefix (substring) match on `ghcr.io/acme`. Prefer `ghcr.io/acme/*` (or a bare
`ghcr.io/acme`) unless you specifically intend the substring behavior.
:::

When more than one policy's scope matches a target, the **longest** literal prefix wins:

```toml
[[trust.policy]]                          # literal prefix "ghcr.io/acme/" (13 chars)
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]                          # literal prefix "ghcr.io/acme/secret-tool" (24 chars)
scope = "ghcr.io/acme/secret-tool"

[trust.policy.keyless]
identity    = "release-bot@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

Verifying `ghcr.io/acme/secret-tool:1.0` only accepts `release-bot@acme.example` — the
narrower policy wins outright, and the broader `ghcr.io/acme/*` policy still governs every
other package under that prefix.

Among policies tied at the **same** winning specificity, evaluation is **ANY-of**: the
signature passes if it satisfies any one of them. This is what makes signer rotation possible
without a downtime window — declare both the old and the new identity at the same scope, and
either one verifies until the old entry is removed:

```toml
[[trust.policy]]                          # both scopes tie at "ghcr.io/acme/" (13 chars)
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "old-ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "new-ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

#### Regex identities {#keys-trust-regex}

`identity_regexp` compiles to an **anchored, full-string** match, not a substring search — a
pattern must match the entire certificate SAN, start to end. This mirrors [cosign][cosign]'s
`--certificate-identity-regexp` semantics and rules out a pattern like `acme` accidentally
matching `evil-acme-lookalike`.

```toml
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity_regexp = "^https://github\\.com/acme/.*/\\.github/workflows/release\\.yml@refs/tags/v[0-9.]+$"
oidc_issuer     = "https://token.actions.githubusercontent.com"
```

`identity_regexp` is useful when the SAN embeds a variable path component — a [GitHub
Actions][github-actions-docs] workflow SAN carries the git ref it ran on
(`…/release.yml@refs/heads/main`), so pinning one exact ref with `identity` would lock out
every other branch or tag that same workflow signs from.

#### Tier precedence: operator-authoritative, not pooled {#keys-trust-merge}

Every other section on this page [replaces](#precedence-merge) at higher-precedence tiers.
Within the `config.toml` tiers themselves, `[[trust.policy]]` is the one exception — policies
**array-append** (pool) across system, user, and `$OCX_HOME` instead of the nearest tier
winning:

```
system config.toml  →  user config.toml  →  $OCX_HOME config.toml
```

Call the pooled result of those three tiers the **operator trust set**. The project
`ocx.toml`'s policies are **not** pooled into that set — they sit behind it, at lower
priority:

- If **any** operator policy matches the target package, only the operator trust set is
  evaluated; the project `ocx.toml` is **ignored** for that package, no matter how
  specific its scope is.
- Only when **no** operator policy matches does the project `ocx.toml` apply. A project
  can therefore **add** trust for scopes the operator has not governed, but it can never
  step in front of a scope the operator already pins.

Within whichever set is chosen, [most-specific-wins + ANY-of resolution](#keys-trust-scope)
still applies — signer rotation works within the operator set, and separately within the
project set, but the two sets never mix for one target.

::: tip A project `ocx.toml` cannot weaken an operator policy
This is a deliberate security property: because the operator trust set wins outright
whenever it matches, a compromised or careless project `ocx.toml` cannot override or
narrow an operator-pinned identity by declaring a more specific scope. `ocx.toml` can only
extend trust to packages the operator has left ungoverned.
:::

#### System-locked {#keys-trust-system-lock}

Pooling makes the three `config.toml` tiers peers on storage, but not on authority. A
policy declared at the [system scope](#file-locations) is locked, unconditionally and per
entry: for every scope it matches, it **governs alone**. Entries from the user,
`$OCX_HOME`, and [managed](#keys-managed) tiers are refused for those scopes — a narrower
scope cannot take over, and an equally specific one cannot join the accepted set either.

::: code-group
```toml [/etc/ocx/config.toml]
# literal prefix "ghcr.io/acme/" — 13 chars, locked
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

```toml [a lower tier]
# literal prefix "ghcr.io/acme/tool" — 17 chars
[[trust.policy]]
scope = "ghcr.io/acme/tool"

[trust.policy.keyless]
identity    = "someone-else@example.test"
oidc_issuer = "https://token.actions.githubusercontent.com"
```
:::

Verifying `ghcr.io/acme/tool:1.0` accepts `ci@acme.example` only. Without the lock the
narrower entry would win outright by [most-specific-wins](#keys-trust-scope). With it, every
lower-tier entry matching the pinned scope is discarded — a longer literal prefix, a shorter
one (`ghcr.io/*`, say), and an exact tie at 13 characters alike.

Rotation therefore happens in the system tier: declare the outgoing and incoming identities
as two locked entries, and both are accepted for the overlap window.

```toml [/etc/ocx/config.toml]
[[trust.policy]]
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]                          # same scope, second accepted signer
scope = "ghcr.io/acme/*"

[trust.policy.keyless]
identity    = "ci-2027@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

::: warning What the lock does and does not reach
The lock is per scope, not fleet-wide. It governs the scopes its own entries match, and
nothing else: a lower tier is still free to pin any scope the system tier never mentions —
`ghcr.io/other/*` in the example above — and does so with full authority there. A lock on
`ghcr.io/acme/*` is not a statement about the rest of the registry.

Within a locked scope, though, no lower tier can **add** a signer, **narrow** the scope to
carve one package out, or **displace** the operator's identity. Whoever writes the user
tier, `$OCX_HOME`, or the [managed-config](#keys-managed) payload cannot enroll a signer
that [`ocx package verify`][cmd-package-verify] will accept there; a refused entry is
reported at debug level naming the pin that discarded it, so an operator whose policy went
nowhere is not left staring at an identity mismatch.
:::

#### No matching policy, no flags {#keys-trust-no-match}

`--certificate-identity` / `--certificate-oidc-issuer` on [`ocx package verify`][cmd-package-verify]
are optional exactly when a `[[trust.policy]]` scope matches the target. Passing both flags
always overrides any policy — an exact-match pair, unchanged from flag-only verification.
Passing neither flag with no matching scope, or passing only one of the two flags, is a usage
error. See the [`package verify` exit codes][cmd-package-verify] for the full behavior.


### `[trust.sigstore]` {#keys-trust-sigstore}

Where [`ocx package verify`][cmd-package-verify] gets its trust root when the
[Sigstore][sigstore] stack is self-hosted rather than the public good, and which
Fulcio/Rekor endpoints [`ocx package sign`][cmd-package-sign] talks to by default.

```toml
[trust.sigstore]
trusted_root = "sigstore/trusted-root.json"    # path, relative to THIS config file
fulcio_url   = "https://fulcio.corp.example"
rekor_url    = "https://rekor.corp.example"
```

Every field is optional and the whole sub-table may be absent — omitting it
reproduces public-good behaviour exactly. It is read from the `config.toml` tiers
only: the project `ocx.toml` also parses a `[trust]` section (for
[`[[trust.policy]]`](#keys-trust)), but its `sigstore` sub-table is never
consulted. A repository that could name its own Fulcio CA would be verifying its
own signatures, which is the entire trust decision.

#### Fields {#keys-trust-sigstore-fields}

| Field | Type | Description |
|-------|------|-------------|
| `trusted_root` | string | Path to a Sigstore trusted-root JSON, or a directory holding `trusted_root.json`. A **relative path resolves against the directory of the `config.toml` that declared it** — rewritten to absolute at load time, so the value means the same file regardless of the process working directory. Mutually exclusive with `trusted_root_json` |
| `trusted_root_json` | string | The trusted-root document inlined verbatim. This is the form a fleet receives — see [Publishing to a fleet](#keys-trust-sigstore-publish). Mutually exclusive with `trusted_root` |
| `fulcio_url` | string | Default [Fulcio][fulcio] base URL for `ocx package sign` / `attest` when `--fulcio-url` is omitted. Precedence: an explicit flag wins, then this field, then the public-good builtin. `ocx package push --sbom` has no `--fulcio-url` flag at all, so this field is its only override |
| `rekor_url` | string | Default [Rekor][rekor] base URL for `ocx package sign` / `verify` / `attest` / `sbom` when `--rekor-url` is omitted. Precedence: an explicit flag wins, then this field, then the public-good builtin. `ocx package push --sbom` and auto-verify expose no `--rekor-url` flag, so this field is their only override |

Setting both `trusted_root` and `trusted_root_json` is a configuration error —
exit `78`, `trust_root_load`. One trust root, one spelling.

#### Where it sits in the ladder {#keys-trust-sigstore-ladder}

Verify resolves its trust root through six rungs, first hit wins:

1. `--sigstore-trusted-root` on [`ocx package verify`][cmd-package-verify]
2. [`OCX_SIGSTORE_TRUSTED_ROOT`][env-sigstore-trusted-root]
3. `[trust.sigstore] trusted_root` / `trusted_root_json` — this section
4. `$OCX_HOME/sigstore/trusted-root.json` — a convention path, no config needed
5. The trust-root cache under `$OCX_HOME/state/trust_root/`, written by a prior online verify
6. The public-good Sigstore root, fetched over TUF

Rungs 1–3 are operator-named: a file that does not exist is an error, not a
fall-through. Rung 4 is a convention: absent falls through, but present-and-unreadable
fails. See [Self-hosted Sigstore][in-depth-self-hosted-sigstore] for choosing among them.

#### System-locked {#keys-trust-sigstore-system-lock}

Declared at the **system** scope (`/etc/ocx/config.toml`), the whole sub-table
becomes non-overridable — the user, `$OCX_HOME`, and [`[managed]`](#keys-managed)
tiers cannot replace any of its fields. The lock is per-table, not per-field: a
lower tier cannot supply a `rekor_url` alongside a system `trusted_root`.

This follows the [`[registry]`](#keys-registry) precedent rather than the
[`[[trust.policy]]`](#keys-trust-merge) one, because a scalar trust root cannot
pool: two Fulcio CAs is not a merge, it is an ambiguity. Where the sub-table is
*not* system-locked, higher tiers replace field by field — with the two trust-root
spellings coupled, so a tier switching from a path to an inline document drops the
path rather than leaving both set.

#### Publishing to a fleet {#keys-trust-sigstore-publish}

A path on the operator's disk means nothing on a consumer's, so
[`ocx config push`][cmd-config-push] reads a path-form `trusted_root` at publish
time, validates that it parses as a Sigstore trusted root, and publishes it as
`trusted_root_json`. Comments, key order and every other field survive the rewrite.

The loader enforces the other half on the consuming side:

- A path-form `trusted_root` arriving from the [`[managed]`](#keys-managed) tier is
  **ignored with a warning**. A remote payload cannot name a path on this machine.
- A `trusted_root_json` arriving from a `[managed]` source that is **not
  digest-pinned** is ignored with a warning. Otherwise the trust root arrives over
  the very channel it exists to verify; the circularity is broken by pinning the
  seed, not by policy.
- `fulcio_url` and `rekor_url` arriving from a `[managed]` source that is **not
  digest-pinned** are ignored with a warning too, for the same reason — and
  `fulcio_url` more sharply so: it names where the OIDC identity token is sent, and
  `ocx package push --sbom` has no flag to oppose a config value, so an unpinned
  payload could hand a signing identity to a server of its choosing.

## Environment Variable Override Table {#env-overrides}

This table shows which OCX environment variables map to config file fields. Variables not listed here have no config equivalent.

| Environment Variable | Config Equivalent | Notes |
|---------------------|-------------------|-------|
| [`OCX_DEFAULT_REGISTRY`][env-default-registry] | `[registry] default` | Env var wins when both are set |
| [`OCX_MIRRORS`][env-mirrors] | `[mirrors]` | Env var wins per host, per role when both are set; roles/hosts absent from env var still come from config |
| [`OCX_PATCHES`][env-ocx-patches] | `[patches] registry` / `path` / `required` | Forwarded JSON wire format; overrides the config-file tier on process boundaries |
| [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] | `[managed] source` | Invocation-only override, never written back; `=""` is treated as unset |
| [`OCX_LAZY_MODE`][env-ocx-lazy-mode] | toolchain-level `lazy-mode` in [`ocx.toml`](#project-config-toolchain-lazy) | Lowest tier of the five-level ladder — `--lazy-mode`, `[package."<id>"]`, and `[group.<name>]` all outrank both the config key and this variable; not forwarded to child processes |
| [`OCX_LAZY_REPORT`][env-ocx-lazy-report] | toolchain-level `lazy-report` in [`ocx.toml`](#project-config-toolchain-lazy) | Lowest tier of the four-level ladder; not forwarded to child processes |
| [`OCX_HOME`][env-ocx-home] | None | Determines where config is loaded from; cannot be in a config file |
| [`OCX_CONFIG`][env-config] | None | Meta-variable pointing at the config file itself |
| [`OCX_NO_CONFIG`][env-no-config] | None | Kill switch; also suppresses the [`[managed]`](#keys-managed) snapshot candidate and the `OCX_MANAGED_CONFIG` env-override read |
| [`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] | None | Kill switch for the [`[managed]`](#keys-managed) background refresh tick only; explicit `ocx config update`, and the setup-time re-sync `ocx self setup` / `ocx config setup` run against an already-adopted seed, still work |
| [`OCX_OFFLINE`][env-offline] | None | Per-invocation mode, not a persistent setting |
| [`OCX_REMOTE`][env-remote] | None | Per-invocation debugging mode, not a persistent setting |
| [`OCX_BINARY_PIN`][env-ocx-binary-pin] | None | Subprocess-only: set automatically by ocx on every spawn so child ocx invocations pin to the same binary |
| [`OCX_INSECURE_REGISTRIES`][env-insecure-registries] | None (deferred) | Will move to a per-entry `insecure` field under [`[registries.<name>]`](#keys-registries) once the flag is implemented; the env var remains the source of truth today |
| [`OCX_NO_UPDATE_CHECK`][env-no-update-check] | None | CI-only concern; env var is sufficient |
| [`OCX_NO_MODIFY_PATH`][env-no-modify-path] | None | Install-time concern; env var is sufficient |

[`OCX_OFFLINE`][env-offline] and [`OCX_REMOTE`][env-remote] are intentionally absent from the config file. Both are per-invocation modes — a persistent `offline = true` would silently break `ocx package install` on a fresh setup.

## Error Reference {#errors}

Literal sizes in the examples below reflect the current 64 KiB safety cap (`MAX_CONFIG_SIZE` in the loader source). Angle-bracket placeholders such as `<SIZE>` stand in for runtime values that depend on the offending file.

| Error | Cause | Resolution |
|-------|-------|-----------|
| `error: config file not found: /path/to/file.toml (check --config or OCX_CONFIG)` | [`--config`][arg-config] or [`OCX_CONFIG`][env-config] points to a non-existent file | Check the path; unlike the three discovery tiers, explicit paths must exist. To disable an ambient [`OCX_CONFIG`][env-config] without unsetting it, set it to the empty string. |
| `error: config file /path/to/file.toml exceeds maximum allowed size (<SIZE> bytes > 65536 bytes); OCX config files are typically under 1 KiB — did you point at the wrong file` | A config file is larger than the 64 KiB safety cap | The hint usually explains it — a `--config` flag or `OCX_CONFIG` env var pointed at a non-config file (e.g. an archive or binary). |
| `error: invalid TOML at /path/to/file.toml: ...` | TOML syntax error in the config file | Fix the TOML syntax error at the indicated location |
| `error: failed to read config file /path/to/file.toml: ...` | The file exists but cannot be read — permission denied, the path is a directory, or another I/O failure | Check file permissions; [`--config`][arg-config] and [`OCX_CONFIG`][env-config] must point to a regular, readable file. |

## Project Configuration — `ocx.toml` {#project-config}

The tiers above configure ocx itself. `ocx.toml` is a different file with a different lifecycle — see the [Project Toolchain guide][user-project] for discovery and locking. This section is the schema reference for the `ocx.toml` tables and keys that carry environment and resolve-time declarations: [`[group.<name>]`](#project-config-groups), [`[env]`](#project-config-env), [`[package."<id>"]`](#project-config-package), and the toolchain-level `lazy-mode` / `lazy-report` keys.

### `[group.<name>]` — `tools` and `env` {#project-config-groups}

Each named group is a table with exactly two optional sub-tables: `tools` (the same binding-name-to-identifier map the top-level `[tools]` table holds) and `env` (see the [value grammar](#project-config-env) below). A group with neither sub-table is a valid, empty group.

```toml
[tools]                       # default group's tools
foo = "ocx.sh/foo:1"

[env]                         # default group's env
CI = "1"

[group.ci.tools]              # named group's tools
bar = "ocx.sh/bar:1"

[group.ci.env]                # named group's env
SOURCE_DATE_EPOCH = "0"
```

A tool binding declared directly under `[group.<name>]` — not inside its `tools` sub-table — is a parse error naming the group and pointing at the fix, `ExitCode::ConfigError` (78):

```
error: group `ci` declares tool bindings directly
  --> ocx.toml
   |
   |  [group.ci]
   |  bar = "ocx.sh/bar:1"
   |
   = tool bindings belong under `[group.ci.tools]`
   = `[group.ci]` holds only the `tools` and `env` sub-tables
```

An unrecognized sub-table (a typo such as `[group.ci.tolos]`) is rejected the same way, naming the offending key. `[group.default]` and `[group.all]` remain reserved names and are rejected at parse regardless of their contents — see [`ocx run`][cmd-run] for the full group-keyword semantics.

A group also accepts an optional `lazy-mode` scalar, overriding the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder] for every tool declared under that group:

```toml
[group.ci]
lazy-mode = "always"

[group.ci.tools]
shellcheck = "ocx.sh/shellcheck:0.11"
```

There is no group-tier `lazy-report` — see [`[package."<id>"]`](#project-config-package) below for why.

This schema applies identically to the `--global` tier file at `$OCX_HOME/ocx.toml`.

### `[package."<id>"]` {#project-config-package}

Per-package resolve-time settings, keyed by the canonical `registry/repository[:tag]` string:

```toml
[package."ocx.sh/kitware/cmake:3.28"]
no-patches = true
lazy-mode  = "always"
lazy-report = "progress"
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `no-patches` | boolean | `false` | Decline the site-tier [patch][config-patches] companion overlay for this base — see [Per-package opt-out](#keys-patches-no-patches) above. |
| `lazy-mode` | `"never"` \| `"always"` | *(inherit)* | Package-tier override of the [`lazy-mode` resolution ladder][in-depth-lazy-loading-ladder] — the most specific config tier, only outranked by [`--lazy-mode`][arg-lazy-mode]. |
| `lazy-report` | `"silent"` \| `"progress"` | *(inherit)* | Package-tier override of the `lazy-report` ladder. |

The match for every field in this table is by canonical `registry/repository` — tag and digest are stripped, so a `[package."<id>"]` entry follows every tag of that package, not just the one written in the key.

`lazy-mode` and `lazy-report` are both **excluded from `declaration_hash`** — like `no-patches`, they change *when* or *how loudly* a tool materializes, never *which* digest resolves, so editing either does not invalidate `ocx.lock`.

`lazy-report` is settable here even though there is no `[group.<name>]` tier for it. `lazy-mode` is resolved while composing, when the selected group is known; `lazy-report` is resolved later, inside the separate `ocx launcher shim` process a generated shim execs into on first invocation — a process that receives only a pinned identifier and a basename, with no way to learn which group composed the tool. See [Deferred Tools][in-depth-lazy-loading] for the full ladder and lifecycle.

### Toolchain-level `lazy-mode` and `lazy-report` {#project-config-toolchain-lazy}

Two more bare scalar keys sit at the top level of `ocx.toml`, alongside `[tools]` — the least specific config tier of each ladder, only outranked by `[group.<name>]`, `[package."<id>"]`, and the CLI flag:

```toml
lazy-mode   = "always"
lazy-report = "silent"

[tools]
cmake = "ocx.sh/kitware/cmake:3.28"
```

Both accept the same value sets as their `[package."<id>"]` counterparts and are excluded from `declaration_hash` for the same reason. Below both of these, [`OCX_LAZY_MODE`][env-ocx-lazy-mode] and [`OCX_LAZY_REPORT`][env-ocx-lazy-report] are the last tier before each ladder's floor (`never` / `silent`). See [Deferred Tools][in-depth-lazy-loading] for the full five-tier `lazy-mode` ladder and the four-tier `lazy-report` ladder.

### `[env]` value grammar {#project-config-env}

Each entry in `[env]` or `[group.<name>.env]` is either a bare string — a **constant** that replaces any earlier value for the same key — or a table with an explicit `type`:

```toml
[env]
CI = "1"                                            # string → constant, same as below
JAVA_OPTS = { type = "constant", value = "-Xmx2g" }
PATH = { type = "path", value = "node_modules/.bin" }
GODEBUG = { type = "list", separator = ",", value = "gctrace=1" }
```

| `type` | Behavior |
|--------|----------|
| `constant` (implicit for the bare-string form) | Replaces any earlier value for the key. |
| `path` | Prepends to the key (typically `PATH`). A relative `value` resolves against the **project root** — the directory holding `ocx.toml` — never the process's current working directory; an absolute `value` passes through unchanged. |
| `list` | Appends to the key, joined by `separator`, removing any earlier occurrence of the same contribution first. |

`list` accepts one more field, valid only alongside it:

| Field | Required | Description |
|---|---|---|
| `separator` | No | The string this contribution joins to the key's existing value. Must be non-empty and must not contain `=`, a newline, or a carriage return when given — a footgun-guard error names the field, not a byte offset (exit 78). Omit it to inherit whatever separator another contributor to the same key already declared — a package's own `list` entry, another group's, or [`--env`][cmd-run] — falling back to a single space only when nothing established one. See [Env Composition][env-composition-list] for the full per-key agreement rule. A `separator` alongside `constant` or `path` is rejected (exit 78). |

There is no interpolation in v1 — every value is literal. The `path` type is what makes a project-local directory like `node_modules/.bin` expressible without one: no `${projectRoot}` token is needed, because relative resolution already targets the project root.

The [`--env`][cmd-run] flag takes the same three types, written `KEY[:TYPE[:SEP]]=VALUE`, with one deliberate difference: a relative `path` value there resolves against the **current directory**, not the project root. A checked-in file must mean the same thing from any subdirectory; a flag is composed by whatever script invokes `ocx`, and the current directory is the one base that script can compute.

Two key classes are rejected everywhere `[env]` can appear — the project table, every `[group.<name>.env]`, and the [`--env`][cmd-run] flag on `ocx run`:

- A key that is not a POSIX environment-variable name (`[A-Za-z_][A-Za-z0-9_]*`).
- A key starting `OCX_` or `__OCX_`. Without this rejection, a checked-in `ocx.toml` could set `OCX_DEFAULT_REGISTRY`, `OCX_INDEX`, `OCX_OFFLINE`, or any other resolution-affecting variable and reconfigure how `ocx` itself resolves for every contributor who clones the repository. Rejection happens at parse for `[env]` / `[group.<name>.env]` (`ExitCode::ConfigError`, 78) and at flag-parse for `--env` (`ExitCode::UsageError`, 64) — see [`--env`][cmd-run] and [`OCX_ENV`][env-ocx-env] for the flag form and the forwarded wire key.

`[env]` entries carry no visibility axis. Unlike a package's own declared env, a project is never a dependency of anything, so there is no interface/private surface to gate — which is also why the project-tier commands carry no `--self` flag at all. See [Project Environment][env-composition-project-env] in the Environment Composition reference for where these entries land in the full resolution order.

## JSON Schemas {#schemas}

OCX publishes JSON Schemas for every config, project, and patch file at stable URLs. IDEs and language servers ([taplo][taplo], [yaml-language-server][yaml-ls], VS Code, Zed) consume them for autocompletion, hover docs, and validation.

| File | Schema URL |
|------|------------|
| `config.toml` (any tier) | [`https://ocx.sh/schemas/config/v1.json`][schema-config] |
| `ocx.toml` (project) | [`https://ocx.sh/schemas/project/v1.json`][schema-project] |
| `ocx.lock` (project lock — machine-generated) | [`https://ocx.sh/schemas/project-lock/v3.json`][schema-project-lock] |
| `metadata.json` (package) | [`https://ocx.sh/schemas/metadata/v1.json`][schema-metadata] |
| Patch descriptor (`ocx patch publish --descriptor`) | [`https://ocx.sh/schemas/patch/v1.json`][schema-patch] |

`ocx init` writes a `#:schema https://ocx.sh/schemas/project/v1.json` directive on the first line of every generated `ocx.toml`, so [taplo][taplo]-aware editors pick the schema up automatically with no extra wiring. To opt other files in by hand, prepend the same directive at the top of the file. A patch descriptor is plain JSON, so add a `"$schema": "https://ocx.sh/schemas/patch/v1.json"` key to get the same autocompletion and validation while authoring it. The `project-lock` schema carries a top-level `$comment` flagging it as machine-generated — never hand-edit `ocx.lock`; rerun [`ocx lock`][cmd-lock] instead.

## Future Config Keys {#future}

::: details Not yet implemented in v1

These sections are documented here so the format design is stable before they land. They do not exist in the current release.

### Per-registry fields beyond `index` and `trusted_hosts` {#future-registries-fields}

The [`[registries.<name>]`](#keys-registries) table is live in v1 with [`index`](#keys-registries-index) and [`trusted_hosts`](#keys-registries-trusted-hosts). Future per-registry fields will slot in without breaking existing configs:

```toml
# Future shape (not in v1 — only index and trusted_hosts are implemented today):
[registries.private]
index = "https://index.company.example"
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
[sigstore]: https://www.sigstore.dev/
[cosign]: https://github.com/sigstore/cosign
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow

<!-- schemas -->
[schema-config]: https://ocx.sh/schemas/config/v1.json
[schema-project]: https://ocx.sh/schemas/project/v1.json
[schema-project-lock]: https://ocx.sh/schemas/project-lock/v3.json
[schema-metadata]: https://ocx.sh/schemas/metadata/v1.json
[schema-patch]: https://ocx.sh/schemas/patch/v1.json

<!-- in-depth -->
[config-indepth]: ../in-depth/configuration.md
[in-depth-indices-public]: ../in-depth/indices.md#public-index
[in-depth-indices-dispatch]: ../in-depth/indices.md#local-dispatch
[in-depth-indices-declared-names]: ../in-depth/indices.md#public-index-declared-names
[in-depth-indices-servable]: ../in-depth/indices.md#servable
[in-depth-lazy-loading]: ../in-depth/lazy-loading.md
[in-depth-lazy-loading-ladder]: ../in-depth/lazy-loading.md#deferred-tools-ladder

<!-- commands -->
[arg-config]: ./command-line.md#arg-config
[arg-offline]: ./command-line.md#arg-offline
[arg-lazy-mode]: ./command-line.md#arg-lazy-mode
[cmd-lock]: ./command-line.md#lock
[cmd-run]: ./command-line.md#run
[cmd-env-root]: ./command-line.md#env-root
[cmd-direnv-export]: ./command-line.md#direnv-export
[cmd-package-exec]: ./command-line.md#package-exec
[cmd-package-env]: ./command-line.md#package-env
[cmd-config-setup]: ./command-line.md#config-setup
[cmd-config-test]: ./command-line.md#config-test
[cmd-self-setup]: ./command-line.md#self-setup
[cmd-self-update]: ./command-line.md#self-update
[cmd-config-update]: ./command-line.md#config-update
[cmd-config-push]: ./command-line.md#config-push
[cmd-package-verify]: ./command-line.md#package-verify
[cmd-package-sign]: ./command-line.md#package-sign
[cmd-package-verify-attestations]: ./command-line.md#package-verify-attestations

<!-- environment -->
[env-ocx-home]: ./environment.md#ocx-home
[env-default-registry]: ./environment.md#ocx-default-registry
[env-config]: ./environment.md#ocx-config
[env-no-config]: ./environment.md#ocx-no-config
[env-offline]: ./environment.md#ocx-offline
[env-remote]: ./environment.md#ocx-remote
[env-insecure-registries]: ./environment.md#ocx-insecure-registries
[env-mirrors]: ./environment.md#ocx-mirrors
[env-log]: ./environment.md#ocx-log
[env-ocx-patches]: ./environment.md#ocx-patches
[env-ocx-managed-config]: ./environment.md#ocx-managed-config
[env-ocx-no-config-refresh]: ./environment.md#ocx-no-config-refresh
[env-ocx-env]: ./environment.md#ocx-env
[env-ocx-lazy-mode]: ./environment.md#ocx-lazy-mode
[env-ocx-lazy-report]: ./environment.md#ocx-lazy-report

<!-- user guide -->
[user-guide-managed-config]: ../user-guide.md#managed-config
[user-guide-managed-config-incompatible]: ../user-guide.md#managed-config-incompatible
[user-project]: ../user-guide.md#project

<!-- env composition -->
[env-composition-patch-opt-out]: ./env-composition.md#patch-opt-out-scope
[env-composition-project-env]: ./env-composition.md#project-env
[env-composition-list]: ./env-composition.md#composition-order-list

<!-- patches user guide -->
[patches-user-guide]: ../user-guide/patches.md
[env-no-update-check]: ./environment.md#ocx-no-update-check
[env-no-modify-path]: ./environment.md#ocx-no-modify-path
[env-ocx-binary-pin]: ./environment.md#ocx-binary-pin
[xdg-basedir]: ./environment.md#external-xdg-config-home
[env-sigstore-trusted-root]: ./environment.md#ocx-sigstore-trusted-root
[in-depth-self-hosted-sigstore]: ../in-depth/self-hosted-sigstore.md
[fulcio]: https://github.com/sigstore/fulcio
[rekor]: https://github.com/sigstore/rekor
