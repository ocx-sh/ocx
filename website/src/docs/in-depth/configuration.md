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

## The Managed-Configuration Tier {#managed}

The three discovery tiers above are files a human, or a provisioning script, writes once when a machine is set up. That works until a setting needs to change *after* the fleet is already provisioned — rotating the corporate mirror, redirecting the patch registry to a new host, retiring an old default registry. Re-running a Dockerfile or a config-management playbook against every already-running CI runner and laptop is exactly the fleet-wide chore OCX is trying to spare you from doing by hand.

The `[managed]` tier solves this by turning the corporate config itself into an ordinary OCX package (its content is one `config.toml`) — the same distribution channel [`[mirrors]`][config-mirrors] and [`[patches]`][config-patches] already use, published with [`ocx config push`][cmd-config-push]. A small pointer (`source`, `required`, `refresh`, `interval`) lives in `$OCX_HOME/config.toml`; it resolves to that payload in the operator's registry, synced into local state, and merged above the user config on every invocation. Push a new version once, and every host that already adopted the pointer converges to it on its own schedule — no re-provisioning; versioned tags, cascades, and rollbacks come along for free because the config travels the package machinery.

:::info Pull-based convergence
This is a small-scale version of the pull-based configuration-management model [Puppet][puppet] and [Chef][chef] popularized for server fleets: an agent periodically checks a central manifest and converges local state to it, instead of an operator pushing changes to every host by hand. OCX's version is intentionally much smaller — one OCI package, one config file, no daemon, no console — but the shape (pull, converge, never block ordinary use) is the same idea. Fleet visibility is the pull-based complement: `ocx config update --check --format json` on any host reports source, digest, tracked tag, drift, and pause state for your existing inventory tooling.
:::

### Where it sits in the chain {#managed-position}

The seed pointer is itself part of the ordinary `$OCX_HOME/config.toml` file — the [OCX home tier](#tier-ocx-home) above. But the package it resolves to is a **synthetic fifth tier**: a locally cached snapshot, folded in after the three static-file tiers and below [`OCX_CONFIG`][env-config]/[`--config`][arg-config]. See the [full precedence table][config-precedence] for exactly where it sits among every other source.

Before that snapshot is merged, OCX checks that its embedded provenance — which `source` it was actually fetched from — matches the currently effective `source` (an [`OCX_MANAGED_CONFIG`][env-ocx-managed-config] override, or else the seed): same registry and repository, and — when the seed pins a digest — exactly that digest. Tags float within the repository, so a host pinned to an older version with `ocx config update <version>` still passes. A cross-repository mismatch is treated as if no snapshot exists at all, even when `required = false`. This closes a real failure mode: a CI job that reuses a cached `$OCX_HOME` from a *different* pipeline (pointed at a different managed-config source) must never silently inherit that other pipeline's mirrors, patch registry, or default registry.

The client that fetches the package is itself built only from **local-only** config tiers — the payload's own `[mirrors]` never contributes to the route used to fetch it. This is deliberate defense-in-depth alongside the [one-hop `[managed]` strip][config-managed-one-hop] that already keeps a fetched payload from redirecting the tier that fetched it: a compromised or misconfigured payload cannot also redirect the *transport* used to fetch its own next refresh.

::: warning Trust boundary
The identity gate above defends against the wrong content being merged — a cross-repository or pin-violating snapshot, most often a stale `$OCX_HOME` reused across CI pipelines. It does not defend against an attacker who already has write access to `$OCX_HOME`. `config.toml`, the snapshot (`state/managed-config/snapshot.json`), and the pause file are ordinary local state, at the same trust level as any other file such an attacker could edit directly. The tier's digest pins bind what was fetched from the registry at sync time; they are not a tamper-evidence check against what is sitting on disk afterward.
:::

### Why refresh never blocks a command {#managed-refresh}

Some corporate policy-sync tools contact a central server on every invocation — reasonable for an always-connected device, disastrous for a build tool that needs to run identically on a laptop on a plane and a CI runner behind a firewall. Config resolution in OCX never does that: every ordinary command reads whatever snapshot is already on disk, with zero network access.

The only things that ever touch the network are an explicit [`ocx config update`][cmd-config-update] and an optional background tick. The tick itself never blocks the command it rides along with — it is a throttled, best-effort probe (governed by [`interval`][config-managed-refresh]) that either applies a new snapshot silently (`refresh = "apply"`), prints a one-line stderr advisory (`refresh = "notify"`, the default), or does nothing (`refresh = "manual"`). A registry that is down, or a network that is unreachable, degrades the tick to a no-op — it never turns into a command failure.

The tick also skips itself before ever contacting the registry in the same three situations OCX's own [update check][env-ocx-no-update-check] silences itself: inside CI (`CI` set), under [`--offline`][arg-offline] (or [`OCX_OFFLINE`][env-offline]), and whenever stderr is not a terminal. Two more conditions gate it further: an active [pause][cmd-config-update] and the [`interval`][config-managed-interval] throttle window not yet having elapsed. [`OCX_NO_CONFIG_REFRESH`][env-ocx-no-config-refresh] is the explicit kill switch layered on top of all five. The practical consequence is worth calling out: `refresh = "apply"` never auto-converges a CI runner or another headless host, no matter how the tier is configured — those hosts converge only through an explicit [`ocx config update`][cmd-config-update].

### Offline and `required` {#managed-offline}

[`required`][config-managed-required] decides whether an absent-or-mismatched snapshot is fatal, and that decision is made against **local disk state**, not network reachability. `required = true` (the default) fails closed identically online and offline — `SnapshotRequired`, exit 78, until [`ocx config update`][cmd-config-update] syncs one. `required = false` lets the command proceed with the tier contributing nothing, whether or not the registry happens to be reachable at that moment.

This means a machine that adopted the tier once, then goes fully offline — a laptop on a plane, an air-gapped runner reusing a warm `$OCX_HOME` — resolves configuration identically to when it was last online. The snapshot is the tier's entire runtime state.

::: tip Learn more
[Managed-configuration walkthrough][user-guide-managed-config] — corporate onboarding, CI recipe, publisher recipe, staged rollout.
[`[managed]` reference][config-managed] — every field, type, default, error condition.
[`ocx config update` reference][cmd-config-update] — exit codes, JSON shape, `--check`.
:::

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

`OCX_NO_CONFIG=1` skips the **discovered chain** — the system, user, and `$OCX_HOME` tiers — and the [managed-configuration snapshot](#managed) candidate, including the `OCX_MANAGED_CONFIG` env-override read: hermetic means hermetic. Explicit paths (`--config` and `OCX_CONFIG`) still load, because they represent deliberate intent rather than ambient environment.

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
- [Managed-configuration walkthrough][user-guide-managed-config] — corporate onboarding, CI recipe, publisher recipe
- [`ocx config update` reference][cmd-config-update] — exit codes, JSON shape, `--check`

<!-- external -->
[xdg-basedir]: https://specifications.freedesktop.org/basedir-spec/latest/
[apple-dirs]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html
[cargo-config]: https://doc.rust-lang.org/cargo/reference/config.html
[uv-config]: https://docs.astral.sh/uv/configuration/files/
[puppet]: https://www.puppet.com/
[chef]: https://www.chef.io/

<!-- reference -->
[config-ref]: ../reference/configuration.md
[config-precedence]: ../reference/configuration.md#precedence
[config-mirrors]: ../reference/configuration.md#keys-mirrors
[config-patches]: ../reference/configuration.md#keys-patches
[config-managed]: ../reference/configuration.md#keys-managed
[config-managed-required]: ../reference/configuration.md#keys-managed-required
[config-managed-refresh]: ../reference/configuration.md#keys-managed-refresh
[config-managed-interval]: ../reference/configuration.md#keys-managed-interval
[config-managed-one-hop]: ../reference/configuration.md#keys-managed-one-hop
[env-ref]: ../reference/environment.md
[env-config]: ../reference/environment.md#ocx-config
[env-offline]: ../reference/environment.md#ocx-offline
[env-ocx-managed-config]: ../reference/environment.md#ocx-managed-config
[env-ocx-no-config-refresh]: ../reference/environment.md#ocx-no-config-refresh
[env-ocx-no-update-check]: ../reference/environment.md#ocx-no-update-check
[arg-config]: ../reference/command-line.md#arg-config
[arg-offline]: ../reference/command-line.md#arg-offline
[cmd-config-update]: ../reference/command-line.md#config-update
[cmd-config-push]: ../reference/command-line.md#config-push
[user-guide-managed-config]: ../user-guide.md#managed-config
