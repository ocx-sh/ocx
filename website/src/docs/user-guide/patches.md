---
outline: deep
---

# Patching packages for your infrastructure {#patches}

Your organization runs a JDK that works fine externally, but inside the corporate network
every TLS handshake fails because Java does not trust the internal CA bundle. The upstream
JDK package does not carry your CA bundle — and it should not: the bundle is your
organization's concern, not the upstream maintainer's.

The patches tier solves this without forking upstream packages. You publish a tiny companion
package that carries your CA bundle, write a descriptor that says "apply this companion to
every JDK install", and from then on every `ocx run` or `ocx package exec` for any JDK
version automatically picks up the right CA bundle. No package forks, no per-version
maintenance.

:::info Analogy: patches and mirrors
The `[patches]` tier is the execution-environment twin of the `[mirrors]` tier.
`[mirrors]` adapts *where bytes come from*; `[patches]` adapts *what environment a tool
runs in*. Both are operator-controlled, opt-in, and configured in the same `config.toml`
config file.
:::

## How it works {#patches-how}

A patch descriptor is a small JSON document stored in your organization's OCI registry.
It declares rules: when an installed package's identifier matches a glob pattern, apply
these companion packages to its execution environment.

At `ocx run` time, OCX fetches the descriptor, identifies the matching companions, and
composes their `interface` environment entries on top of the base package's entries. The
base package is never modified.

```json
{
  "version": 1,
  "rules": [
    {
      "match": "*",
      "packages": ["registry.corp.example/infra/ca-bundle:latest"]
    },
    {
      "match": "ocx.sh/java:*",
      "packages": ["registry.corp.example/infra/jdk-truststore:1.0"],
      "required": false
    }
  ]
}
```

The `match` field is a flat glob. `*` matches any character including `/`, `:`, and `@`,
so `*` matches every package and `ocx.sh/java:*` matches any version of the JDK hosted at
`ocx.sh`.

Rules are evaluated in order and unioned: a Java install matched by both rules above gets
both companions composed in.

## Consumer experience {#patches-consumer}

<Terminal src="/casts/user-guide/patches-consumer.cast" title="Running packages with patch overlays" collapsed />

Once a site administrator configures the `[patches]` tier, consumers need no special
commands. Install a base package and run as usual:

```sh
ocx package install java:21
ocx package exec java:21 -- java -version
```

The companion packages install automatically during `ocx patch sync`, or during the next
`ocx run` / `ocx package exec` for new packages. The composed environment is visible with:

```sh
ocx package env java:21 --show-patches
```

Plain output annotates companion-sourced entries with `(patch)` so you can see exactly
which variables came from companions.

## Maintainer workflow {#patches-maintainer}

<Terminal src="/casts/user-guide/patches-maintainer.cast" title="Publishing patch descriptors" collapsed />

The maintainer (the person who authors and publishes patch descriptors) follows a
four-step loop.

### 1. Write a descriptor {#patches-maintainer-descriptor}

Create a JSON file following the descriptor schema. The only required fields are `version`
(must be `1`) and `rules`. Add the optional `$schema` key to get autocompletion and
validation from any [taplo][taplo]- or VS-Code-style editor while you author the file — the
schema is published at [`https://ocx.sh/schemas/patch/v1.json`][schema-patch]:

```json
{
  "$schema": "https://ocx.sh/schemas/patch/v1.json",
  "version": 1,
  "rules": [
    {
      "match": "*",
      "packages": ["registry.corp.example/infra/ca-bundle:latest"],
      "required": true
    }
  ]
}
```

`required: true` means the companion must be available; if it is not, the exec fails
rather than running without the CA bundle. This is the default and the safe choice for
security-critical companions like CA bundles.

Use `required: false` for non-security companions (license servers, metrics endpoints)
where running without the companion is acceptable.

### 2. Test locally without publishing {#patches-maintainer-test}

`ocx patch test` composes the descriptor onto a base package in a scratch environment
without touching the live registry or the real `$OCX_HOME`. This lets you verify the
descriptor before publishing:

```sh
ocx patch test \
  --descriptor-file ./my-descriptor.json \
  java:21
```

Without a trailing command, `patch test` prints the composed environment so you can
inspect which entries the companion contributes.

To run a command in the composed environment:

```sh
ocx patch test \
  --descriptor-file ./my-descriptor.json \
  java:21 -- java -version
```

If the companion package is not yet published, supply a local archive:

```sh
ocx patch test \
  --descriptor-file ./my-descriptor.json \
  --companion-archive ./ca-bundle-1.0.tar.xz \
  java:21 -- java -version
```

### 3. Publish the companion, then the descriptor {#patches-maintainer-publish}

Publish the companion package first — the descriptor only references it by identifier:

```sh
ocx package push \
  --identifier registry.corp.example/infra/ca-bundle:1.0 \
  ca-bundle.tar.xz
```

Then publish the descriptor. Use `--global` for a descriptor that applies to all
packages, or pass a base identifier to create a per-package descriptor:

```sh
# Global descriptor (applies to all packages):
ocx patch publish \
  --descriptor-file ./my-descriptor.json \
  --global

# Per-package descriptor (applies to java only):
ocx patch publish \
  --descriptor-file ./my-descriptor.json \
  java:21
```

### 4. Freeze for reproducible builds {#patches-maintainer-freeze}

OCI tags are mutable. The same `ca-bundle:latest` tag may point to a different digest
tomorrow. For production builds that need byte-for-byte reproducibility, write a snapshot:

```sh
ocx patch freeze
```

This resolves every companion and descriptor currently in use and writes
`patches.snapshot.json` beside `ocx.lock`. Point `OCX_PATCH_SNAPSHOT` at this file to
make all composition prefer the pinned digests:

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
```

With the snapshot in place, `ocx run` uses the frozen companion digests and skips live
tag lookups even offline.

To return to floating (live) tags, unset `OCX_PATCH_SNAPSHOT`.

:::tip Float vs freeze
Leave `OCX_PATCH_SNAPSHOT` unset during development so you always pull the latest
companion. Set it in CI or before a release to lock the companion digests alongside your
project's `ocx.lock`.
:::

## Refreshing descriptors {#patches-sync}

After the `[patches]` tier is configured, keep descriptors and companions current with:

```sh
ocx patch sync
```

`patch sync` re-fetches every descriptor for all installed packages and the global descriptor,
installs any newly-referenced companion packages, and re-checks packages installed before
the `[patches]` tier was added. This is the only command that contacts the patch registry.
It is safe to run frequently; it piggybacks on the same index-update mechanism as
`ocx index update`.

## Enforcement {#patches-enforcement}

The `required` field controls what happens when a matched companion is unavailable.

| required | Companion unavailable |
|----------|----------------------|
| `true` (default) | Execution aborts with an error. Use for CA bundles, proxy config — anything that makes running without the companion unsafe. |
| `false` | OCX logs a warning and continues. Use for convenience overlays (metrics endpoints, license server hints) where running without the companion is acceptable. |

System administrators can set `required = true` in `/etc/ocx/config.toml` to make the
entire patch tier non-overridable. A system-level required tier cannot be redirected or
suppressed by a user-level config file.

:::warning System-required patches
When a `[patches]` tier is declared in the system config (`/etc/ocx/config.toml`) with
`required = true` (or no `required` line, which defaults to `true`), the tier is locked.
User-level config files, `OCX_PATCHES`, and per-package `no-patches` opt-outs cannot
override a system-required tier. This is the fail-closed security posture for corporate
CA distribution.
:::

## Per-package opt-out {#patches-no-patches}

A project can opt a specific base package out of the patch tier by adding `no-patches =
true` to the project's `ocx.toml`:

```toml
[package."ocx.sh/cmake:3.28"]
no-patches = true
```

This is honored for user-scope and project-scope patch tiers. A system-required tier
always applies regardless of `no-patches`.

## Working offline {#patches-offline}

Once companions are installed and a snapshot is written, patches work fully offline.

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
ocx --offline run -- cmake --version
```

The snapshot pinned digests are resolved from the local object store. No network access
is needed.

Without a snapshot, `ocx --offline run` applies whatever companions are already installed
locally and skips companions not yet cached, logging a warning for each.

## Configuration {#patches-config}

Site administrators configure the patch tier in `config.toml`:

```toml
[patches]
registry = "registry.corp.example/ocx-patches"
path = "{registry}/{repository}"
required = true
```

`registry` points to the OCI registry that hosts patch descriptors. `path` is a template
that determines the per-package sub-path; `{registry}` expands to the slugified registry
host of the base package, `{repository}` to its repository path. The default template
`{registry}/{repository}` is suitable for most setups.

For the full field reference, see the [`[patches]` configuration section][config-patches].

## In depth {#patches-in-depth}

- [Configuration reference: `[patches]`][config-patches] — all fields, scopes, defaults.
- [Environment reference: `OCX_PATCHES`][env-ocx-patches] — how the resolved tier is
  forwarded to subprocesses.
- [Environment reference: `OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot] — the snapshot
  path variable.
- [Environment composition][env-composition] — how companion `interface` entries compose
  onto the base package's execution environment.
- [`[mirrors]` reference][config-mirrors] — the transport-level sibling to the patch tier.
- [Command reference: `patch`][cmd-patch] — `publish`, `sync`, `freeze`, `test`.

<!-- external -->
[asciinema]: https://asciinema.org/
[taplo]: https://taplo.tamasfe.dev/

<!-- schemas -->
[schema-patch]: https://ocx.sh/schemas/patch/v1.json

<!-- configuration -->
[config-patches]: ../reference/configuration.md#keys-patches
[config-mirrors]: ../reference/configuration.md#keys-mirrors

<!-- environment -->
[env-ocx-patches]: ../reference/environment.md#ocx-patches
[env-ocx-patch-snapshot]: ../reference/environment.md#ocx-patch-snapshot

<!-- env composition -->
[env-composition]: ../reference/env-composition.md

<!-- commands -->
[cmd-patch]: ../reference/command-line.md#patch
