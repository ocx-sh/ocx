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
      "required": true
    }
  ]
}
```

The `match` field is a flat glob. `*` matches any character including `/`, `:`, and `@`,
so `*` matches every package and `ocx.sh/java:*` matches any version of the JDK hosted at
`ocx.sh`.

Rules are evaluated in order and unioned: a Java install matched by both rules above gets
both companions composed in.

### One companion per runtime {#patches-how-per-runtime}

The descriptor above ships two companions for what is conceptually one CA bundle: a
generic `ca-bundle` companion matched against every package (`*`), and a separate
`jdk-truststore` companion matched only against `ocx.sh/java:*`. That split is the shape
every CA-bundle descriptor needs, because each language runtime discovers trusted CAs its
own way.

`SSL_CERT_FILE` is the closest thing to a universal override, but it is an
[OpenSSL][openssl] convention, not a language standard: [`curl`][curl], [`git`][git], and
Python's [`ssl`][python-ssl] module honor it because they link against [OpenSSL][openssl]
or defer to its lookup.
[Go][go]'s `crypto/x509` only honors `SSL_CERT_FILE`/`SSL_CERT_DIR` on Linux and the
BSDs — on Windows and macOS it calls the OS-native certificate store instead and ignores
both variables. [Java][java-tools] never reads `SSL_CERT_FILE`; it has no environment
variable for its default trust store at all.

| Runtime | CA override |
|---------|-------------|
| OpenSSL-linked tools ([`curl`][curl], [`git`][git], Python [`ssl`][python-ssl]) | `SSL_CERT_FILE` / `SSL_CERT_DIR` |
| [Go][go] `crypto/x509` | `SSL_CERT_FILE` / `SSL_CERT_DIR` on Linux/BSD only; ignored on Windows and macOS |
| [Java][java-tools] | No environment variable. Set the `javax.net.ssl.trustStore` system property via `JAVA_TOOL_OPTIONS`, or [`keytool`][keytool] `-importcert` the certificate into the JVM's `cacerts` keystore directly. |

A descriptor rule's `match` glob is how a patches tier expresses "this runtime needs its
own companion": one rule per runtime whose CA mechanism differs, each pointing at the
package that knows how to install it. There is no single companion that reaches every
runtime at once.

`JAVA_TOOL_OPTIONS` is itself a space-separated option list, not a single value — a `constant`
entry would erase whatever flags the base JDK package or another companion already put there,
and a [`path`][reference-env-path] entry would join with the platform path separator instead of
a space and prepend instead of append. The `jdk-truststore` companion above declares it as
[`list`][reference-env-list], so its trust-store flag appends after anything already
contributed:

```json
{
  "env": [
    {
      "key": "JAVA_TOOL_OPTIONS",
      "type": "list",
      "separator": " ",
      "value": "-Djavax.net.ssl.trustStore=${installPath}/cacerts",
      "visibility": "interface"
    }
  ]
}
```

`visibility` is `interface`: the companion contributes this flag to the base package's
consumers without needing it for any runtime of its own. See [Appending option
lists][authoring-env-surface-lists] for when to reach for `list` in your own packages.

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

Plain output adds a `Source` column naming the companion and the descriptor rule glob that
admitted it (e.g. `corp/jdk-trust:1.0 (rule: ocx.sh/java:*)`) for every companion-sourced
entry; JSON output carries the same provenance as `"source": { "kind": "patch", "rule": "...",
"companion": "..." }`.

To ask the same question about a base without reading through the full composed
environment, use [`ocx patch why`][cmd-patch-why]:

```sh
ocx patch why java:21
```

```
Variable     Rule          Companion
JAVA_TRUST   ocx.sh/java:* corp/jdk-trust:1.0
```

A base with no applicable patch prints "no patches apply" and exits `0` — not an error.
`patch why` is the narrower diagnostic: only the `Variable | Rule | Companion` provenance
table, without the rest of the composed environment `--show-patches` prints alongside it.

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

`ocx patch test` composes the descriptor onto a base package in a scratch environment,
without touching the live registry or the real `$OCX_HOME`, so you can verify it before
publishing:

<Terminal src="/casts/user-guide/patches-test.cast" title="Testing a patch descriptor locally" collapsed />

```sh
ocx patch test \
  --descriptor ./my-descriptor.json \
  java:21
```

Without a trailing command, `patch test` prints the composed environment so you can inspect
which entries the companion contributes; add `-- java -version` (or any command) to run it
in that environment instead:

```sh
ocx patch test \
  --descriptor ./my-descriptor.json \
  java:21 -- java -version
```

If the companion package is not yet published, supply a local archive instead of pulling it
from the registry:

```sh
ocx patch test \
  --descriptor ./my-descriptor.json \
  --companion-archive ./ca-bundle-1.0.tar.xz \
  java:21 -- java -version
```

`--companion-archive` reads the archive's metadata sidecar (`<archive-stem>-metadata.json`,
the same naming [`ocx package test`][cmd-package-test]'s `--metadata` flag defaults to) and
requires an `identifier` field inside it — there is no `-i` flag on `patch test` itself to
supply one instead. That identifier must match one of the descriptor's companion entries
exactly: registry, repository, and tag. Spell it out in full — a bare identifier with no
registry qualifies against your configured default registry, not the `[patches]` registry,
so `ca-bundle:1.0` can silently resolve somewhere the descriptor never pointed. A mismatch
fails loud (exit 64), naming the nearest descriptor entry it found instead of a generic
"not found".

The base package has no such affordance: it must always be published and pullable.
`--companion-archive` only lets you preview a companion before it exists on the registry,
not the package you are patching.

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
  --descriptor ./my-descriptor.json \
  --global

# Per-package descriptor (applies to java only):
ocx patch publish \
  --descriptor ./my-descriptor.json \
  java:21
```

:::tip Bootstrapping a new patch registry
You do not need a `[patches]` config block to publish the first descriptor. Pass
`--registry <HOST/PATH>` to target a patch registry ad-hoc — it overrides the configured
tier, or stands in when none is configured:

```sh
ocx patch publish \
  --descriptor ./my-descriptor.json \
  --registry registry.corp.example/ocx-patches \
  --global
```

`ocx patch test --registry …` composes against the same ad-hoc registry for a local preview.
Roll out the `[patches]` config to consumers once the registry is seeded.
:::

### 4. Freeze for reproducible builds {#patches-maintainer-freeze}

OCI tags are mutable. The same `ca-bundle:latest` tag may point to a different digest
tomorrow. For production builds that need byte-for-byte reproducibility, write a snapshot:

```sh
ocx patch freeze
```

This resolves every companion and descriptor currently in use and writes
`patches.snapshot.json` beside `ocx.lock` (or beside `$OCX_HOME/ocx.lock` when run with
[`--global`][cmd-global-flag]). Companions are pinned per `repository:tag`, so a
descriptor that names one repository at two tags freezes both versions independently.

The file is derived state, not something to hand-edit: it records a format version, and a
version this `ocx` does not read is refused (exit [`65`][exit-codes]) with the remedy to
re-run `ocx patch freeze`. Re-freezing is offline and takes no longer than the first run.

Point [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot] at the file to make all composition
prefer the pinned digests:

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
```

With the snapshot in place, `ocx run` uses the frozen companion digests and skips live tag
lookups even offline. To return to floating (live) tags, unset the variable.

Freezing the patch tier is a deliberate opt-in and is independent of
[`--frozen`][arg-frozen], which scopes to the package tier: patches float by design, so a
companion resolves live under that flag exactly as it does without it.

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

Without `--platform`, `patch sync` resolves companions for **every concrete ship platform**, not
just the platform running the sync — the same default [`ocx lock`][cmd-lock] uses. This is
`patch sync`'s one sanctioned multi-platform fan-out: an explicit enumeration over the concrete
matrix, not a selection among candidates. A synced descriptor/companion set is a shared artifact:
if it only covered the maintainer's own platform, a teammate on a different OS or architecture
would silently miss a required companion and hit a failed (or worse, unpatched) launch. Pass a
single `--platform` to narrow to just that platform's companions.

## Enforcement {#patches-enforcement}

The `required` field controls what happens when a matched companion is unavailable.

| required | Companion unavailable |
|----------|----------------------|
| `true` (default) | Execution aborts with an error. Use for CA bundles, proxy config — anything that makes running without the companion unsafe. |
| `false` | OCX logs a warning and continues. Use for convenience overlays (metrics endpoints, license server hints) where running without the companion is acceptable. |

The same posture governs patch **discovery** at install time. Installing a base package
triggers a lazy lookup of the patch descriptors on the registry. If that registry is empty
or unreachable, a non-required tier (`required = false`) logs a warning and installs the base
without companions; a required tier fails the install closed, because OCX cannot confirm that
no mandated companion applies. A registry that simply carries no descriptor yet is not an
error under either posture — discovery records "no patch" and moves on.

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
[package."ocx.sh/kitware/cmake:3.28"]
no-patches = true
```

A system-required tier always applies regardless of `no-patches` — enforcement wins over the
opt-out.

**Where this takes effect.** The opt-out is read from the project's `ocx.toml`, so it only
applies where that file is directly in scope: [`ocx run`][cmd-run], [`ocx env`][cmd-env-root],
and [`ocx direnv export`][cmd-direnv-export]. Each of these composes the environment itself
after reading the project config.

A tool that `ocx run` launches can still reach the opt-out one hop further: if that tool
re-enters ocx through its own generated launcher, `ocx run` forwards the opt-out to the child
process over [`OCX_PATCHES`][env-ocx-patches], so the launcher honors the same suppression
its parent did. A **direct** launcher invocation — one not spawned by an opt-out-forwarding
`ocx run`, including a package run through [`ocx package exec`][cmd-package-exec] — has no
opt-out to decode and composes the companion overlay as if `no-patches` were never set.

:::info Why not everywhere?
The opt-out lives in a project's `ocx.toml`. OCI-tier commands (`ocx package install`,
`ocx package env`, `ocx package exec`) never read `ocx.toml` — that is the whole point of the
tier split described in the [command reference][cmd-patch]. There is no project in scope for
them to opt anything out of.
:::

See [Patch Opt-Out Scope][env-composition-patch-opt-out] in the environment composition
reference for the full forwarding mechanics.

## Working offline {#patches-offline}

Composing the environment never touches the network: `ocx run`, `ocx package exec`, and `ocx env`
always resolve companions from whatever is already installed locally, snapshot or not.
`--offline` only affects whether OCX can *discover and install* companions in the first
place, at `ocx package install` or `ocx patch sync` time — it changes nothing about how an
already-resolved toolchain composes its environment. That means the enforcement rule above
applies exactly the same whether or not `--offline` is set.

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
ocx --offline run -- cmake --version
```

With a snapshot in place, the pinned digests are resolved from the local object store and no
network access is needed — the command above works even for `required` companions.

Without a snapshot, `ocx --offline run` still applies whatever companions are already
installed locally: an optional (`required = false`) companion that is not yet installed is
skipped with a warning, but a **required** companion that is not yet installed fails closed
and aborts the run — the same posture as running online. `--offline` never turns a required
companion into an optional one. Run `ocx patch sync` while you still have network access (or
let the lazy install-time hook do it during `ocx package install`) so every required
companion is already in the local store before you disconnect.

## Where companion pins live {#patches-pins}

A companion's tag→digest binding is written to `$OCX_HOME/state/patch-companions/`, one
JSON file per patch-registry repository — never into the [local index][fs-index]
(`$OCX_HOME/index/`). A companion repository owns **zero bytes** there: no root document,
and no dispatch object either, so committing an index tree to git never picks one up.
A companion is a package the descriptor named on the operator's
behalf, not one you asked for by name, so its binding belongs to the patch tier's own
state rather than the package-tier pin `ocx index update` maintains. One consequence
follows directly: [`ocx index update`][cmd-index-update] can never make a companion
resolvable, and [`--frozen`][arg-frozen] — which freezes that index — does not reach a
companion at all. Patches float by design.

A companion's pin only ever advances on [`ocx patch sync`][cmd-patch-sync], including for
a rolling tag the descriptor keeps naming. Composition reads the pin; it never writes one
back. When a descriptor changes or a new companion should be picked up, sync while online,
then run `ocx patch freeze` again to capture the result for the next reproducible build.

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
- [Command reference: `patch`][cmd-patch] — `publish`, `sync`, `freeze`, `test`, `why`.
- [Command reference: `--frozen`][arg-frozen] — the package tier's freeze, and why it
  leaves the patch tier floating.
- [Indices: Local Index][fs-index] — where the package-tier pin `ocx index update`
  maintains lives, and how it differs from a companion's pin.

<!-- external -->
[asciinema]: https://asciinema.org/
[taplo]: https://taplo.tamasfe.dev/
[openssl]: https://www.openssl.org/
[go]: https://pkg.go.dev/crypto/x509
[java-tools]: https://docs.oracle.com/en/java/javase/21/docs/specs/man/java.html
[curl]: https://curl.se/
[git]: https://git-scm.com/
[python-ssl]: https://docs.python.org/3/library/ssl.html
[keytool]: https://docs.oracle.com/en/java/javase/21/docs/specs/man/keytool.html

<!-- schemas -->
[schema-patch]: https://ocx.sh/schemas/patch/v1.json

<!-- reference -->
[reference-env-path]: ../reference/metadata.md#env-path
[reference-env-list]: ../reference/metadata.md#env-list

<!-- authoring -->
[authoring-env-surface-lists]: ../authoring/env-surface.md#lists

<!-- configuration -->
[config-patches]: ../reference/configuration.md#keys-patches
[config-mirrors]: ../reference/configuration.md#keys-mirrors

<!-- environment -->
[env-ocx-patches]: ../reference/environment.md#ocx-patches
[env-ocx-patch-snapshot]: ../reference/environment.md#ocx-patch-snapshot

<!-- env composition -->
[env-composition]: ../reference/env-composition.md
[env-composition-patch-opt-out]: ../reference/env-composition.md#patch-opt-out-scope

<!-- commands -->
[cmd-patch]: ../reference/command-line.md#patch
[cmd-patch-why]: ../reference/command-line.md#patch-why
[cmd-patch-sync]: ../reference/command-line.md#patch-sync
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-run]: ../reference/command-line.md#run
[cmd-env-root]: ../reference/command-line.md#env-root
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[cmd-package-exec]: ../reference/command-line.md#package-exec
[cmd-package-test]: ../reference/command-line.md#package-test
[cmd-lock]: ../reference/command-line.md#lock
[cmd-global-flag]: ../reference/command-line.md#global-flag
[arg-frozen]: ../reference/command-line.md#arg-frozen
[exit-codes]: ../reference/command-line.md#exit-codes

<!-- in-depth -->
[fs-index]: ../in-depth/indices.md#local
