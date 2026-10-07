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
every JDK install", and from then on every `ocx exec` or `ocx package exec` for any JDK
version automatically picks up the right CA bundle. No package forks, no per-version
maintenance.

:::info Analogy: patches and mirrors
The `[patches]` tier is the execution-environment twin of the `[mirrors]` tier.
`[mirrors]` adapts *where bytes come from*; `[patches]` adapts *what environment a binary
runs in*. Both are operator-controlled, opt-in, and configured in the same `config.toml`
config file.
:::

## How it works {#patches-how}

A patch descriptor is a small JSON document stored in your organization's OCI registry.
It declares rules: when an installed package's identifier matches a glob pattern, apply
these companion packages to its execution environment.

At `ocx exec` time, OCX fetches the descriptor, identifies the matching companions, and
composes their environment entries as part of the package they match: each entry lands on
the surface the package itself is read through, as if the package had declared it. The
base package is never modified. See [Companions Are Part of Their Target][patches-how-part-of-target].

A companion's `integrations` block, if it declares one, contributes the same way any
package's does — attributed to the companion's own identifier, never merged into anything
the base or another companion declared. See [Patch Companions Contribute
Too][env-composition-integrations-companions] in the environment composition reference
for the row shape and ordering rules.

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

The `match` field is a flat glob with three shapes:

- **`registry/repo`** (no tag, no digest) matches every tag and digest of that repository.
- **`registry/repo:<tag-glob>`** matches by tag. A reference with no tag and no digest counts
  as `latest`; a digest-only reference has no tag.
- **`registry/repo@<digest-glob>`** matches by digest, whatever advisory tag the reference
  carries. `registry/repo@*` matches any digest-pinned reference.

`*` matches any character including `/`, `:`, and `@`, so `*` alone matches every package.
`ocx.sh/java:*` matches any version of the JDK hosted at `ocx.sh`, including a reference
with no tag. A glob such as `ocx.sh/java*` also matches `ocx.sh/javafx`.

| Rule | `ocx.sh/java:21` | `ocx.sh/java` | `ocx.sh/java@sha256:H` | `ocx.sh/java:21@sha256:H` |
|---|---|---|---|---|
| `ocx.sh/java` | match | match | match | match |
| `ocx.sh/java:*` | match | match | no | match |
| `ocx.sh/java:latest` | no | match | no | no |
| `ocx.sh/java:21*` | match | no | no | match |
| `ocx.sh/java@*` | no | no | match | match |
| `ocx.sh/java@sha256:H` | no | no | match | match |

The columns are the forms a reference takes:

- `ocx.sh/java:21`: `ocx package install ocx.sh/java:21`.
- `ocx.sh/java`: `ocx package install ocx.sh/java`, with no tag.
- `ocx.sh/java@sha256:H`: a digest-only `ocx.toml` entry, with its resolved digest.
- `ocx.sh/java:21@sha256:H`: a tagged `ocx.toml` entry, with its resolved digest.

`H` is a concrete digest. A digest in a pattern matches the platform-specific manifest digest,
not the multi-platform index digest.

A bare `ocx.toml` entry such as `java = "ocx.sh/java"` counts as `:latest`, so it takes the
fourth form: `ocx.sh/java:latest@sha256:H`.

`ocx package install ocx.sh/java:21` discovers on the reference as you typed it: the first two
forms. `ocx package install ocx.sh/java@sha256:H` matches the third. The lock-driven commands
(`ocx lock`, `ocx add`, `ocx update`, `ocx pull`) and composing the environment at `exec` or `env`
match the pinned reference with its digest: the last two forms. That is why an `ocx.sh/java@*` rule
matches at exec but not when you install `ocx.sh/java:21` by tag.

A registry-wide glob such as `ocx.sh/*:*` also matches digest references, because the `:` in
`sha256:` satisfies it. `*:*` can match a registry port as well (`localhost:5000/java`), so scope
a rule to a registry host unless matching everything is the intent.

When one package is declared under two tags, the tag of the first binding in selection
order is the one matched. Selection order is the groups in the order you select them
(`all` puts `default` first), then bindings sorted by name. `ocx direnv export` and the
global toolchain ignore the selection order and take the first binding in lock order
(groups, then bindings, by name).

Tags are advisory here — a companion that must always apply should match on the bare
repository (or a digest) rather than a tag. When the lock is stale, `ocx direnv export` and the global
toolchain match patch rules without the declared tag until `ocx lock` runs.

Rules are evaluated in order and unioned: a Java install matched by both rules above gets
both companions composed in.

### Companions are part of their target {#patches-how-part-of-target}

A companion's env var carries the same [`visibility`][reference-visibility] any package's
var does, and it is read on the surfaces that visibility names for the package it patches.
A package has two [surfaces][env-composition-surfaces]: the interface surface its consumers
see, and the private surface its own generated launchers see (also
[`ocx package env --self`][cmd-package-env] and `ocx package exec --self`).

| Companion var | Target's consumers, `ocx package env`, shells, dependents | Target's own launchers, `--self` |
|---|---|---|
| `private` | no | yes, when the target is a root of the composition |
| `interface` | yes | no |
| `public` | yes | yes, when the target is a root of the composition |

A var that declares no `visibility` is `private`.

The private column applies only when the target is a root of the composition. A target that is
a dependency of the package being composed is admitted through its interface alone. Its
companion then contributes its `interface` and `public` vars wherever that dependency's
interface surface reaches, and its `private` vars never load. A private var therefore cannot
leak into the consumers of a package that depends on the target.

Three more rules follow from "as if the target had declared it":

- A companion's own dependencies are admitted the same way. Its private dependencies load
  only where its private side loads.
- A catch-all rule that matches a root and its dependencies composes the union of the two
  sides under `--self` and in launchers, with each entry applied once. The consumer view
  takes the interface side only.
- `${installPath}`, `${deps.*}` and `${self.env.*}` inside a companion resolve against the
  companion, not the target.

For example, `plantuml` has a private dependency on a JRE, and its entrypoint runs
`java -jar ${installPath}/plantuml.jar`. A companion matched to plantuml declares
`JDK_JAVA_OPTIONS` as `private`:

```json
{
  "env": [
    {
      "key": "JDK_JAVA_OPTIONS",
      "type": "list",
      "separator": " ",
      "value": "-Dhttps.proxyHost=proxy.corp.example",
      "visibility": "private"
    }
  ]
}
```

Only plantuml's launcher sees the flag. The JRE is shared by other Java packages, and
neither it nor plantuml's consumers receive it.

Patch entries apply after all package env, so site policy wins over what a package
declares. Project `[env]` and `--env` apply after them.

### Conflicts and launchers {#patches-how-conflicts}

A companion brings its own dependencies into the environment, so it can name a repository the
environment already carries. Suppose its closure reaches that repository at a different digest
than the base, or than an earlier companion. The later companion is refused.

A `required` companion fails the command with exit 65. An optional one is skipped with a
warning.

Two companion roots of the same repository at different tags are not a conflict. Both apply,
each as its own companion. A dependency the base or an earlier companion already composed is
emitted once.

A companion's own entrypoint launchers are never put on `PATH`, since they run the companion's
environment rather than its target's. Its own `binaries` claim is admitted, and so are the
claims and launchers of its dependencies, as for any dependency.

A generated launcher finds its package by directory, and one directory serves every name that
resolves to its digest. The command that composed the environment passes the names it used in
[`OCX_LAUNCH_IDENTITIES`][env-ocx-launch-identities], and the launcher matches rules against
each of them. So `ocx.sh/java:*` reaches the JDK's launchers when the JDK runs through
`ocx exec`, `ocx package exec`, `ocx package test` or a lazy tool's shim. It also reaches them from a `PATH` that
`ocx env --shell`, `ocx direnv export` or the shell hook composed, since those exports carry the
names too. A launcher run by its absolute path, or from a `PATH` no ocx command composed, matches
only `*` rules. A digest-only composition passes a name without a tag, which tag-anchored rules
skip as they do everywhere else.

### Execution time only {#patches-how-execution-time}

Patches are composed when OCX builds an environment to run something: `ocx exec`,
`ocx package exec`, `ocx package test`, `ocx env`, `ocx package env` and generated launchers.
Metadata-only views do not model them. `ocx package inspect --closure`, `ocx inspect --closure`
and the name sets a toolchain render derives for `PATH` describe the closure the packages
declare, without companions.

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
      "visibility": "public"
    }
  ]
}
```

`visibility` is `public`: the flag reaches the JDK's consumers, and the JDK's own launchers
when the JDK is the package being run. A JDK pulled in as a dependency receives the flag
through its interface alone. See [Appending option lists][authoring-env-surface-lists] for when
to reach for `list` in your own packages.

A catch-all rule such as `"match": "*"` matches every root and every dependency. Under
`--self` and in launchers it composes the union of the private and interface sides. The
consumer view takes the interface side only.

A value meant for everyone, like a CA bundle path or a truststore flag, belongs on a `public`
var. A `private` var misses consumers, and an `interface` var reaches a launcher only through
a dependency the rule matches.

## Consumer experience {#patches-consumer}

<Terminal src="/casts/user-guide/patches-consumer.cast" title="Running packages with patch overlays" collapsed />

Once a site administrator configures the `[patches]` tier, consumers need no special
commands. Install a base package and run as usual:

```sh
ocx package install java:21
ocx package exec java:21 -- java -version
```

The companion packages install automatically during `ocx patch sync`, or during the next
`ocx exec` / `ocx package exec` for new packages. Project commands discover too: `ocx lock`,
`ocx pull` and `ocx exec` install the companions of the tools in `ocx.toml`, with no
`ocx package install` first. `ocx lock --no-pull` skips discovery, `ocx pull` discovers only the
tools it pulls eagerly, and `ocx exec` resolves required companions only
(see [Working offline][patches-offline]). The composed environment is visible with:

```sh
ocx package env java:21 --show-patches
```

Plain output adds a `Source` column naming the companion and the descriptor rule glob that
admitted it (e.g. `corp/jdk-trust:1.0 (rule: ocx.sh/java:*)`) for every companion-sourced
entry; JSON output carries the same provenance as `"source": { "type": "patch", "rule": "...",
"companion": "..." }`.

A companion's `integrations` row needs none of that provenance machinery. It appears in
plain `ocx env` / `ocx package env` JSON output **unconditionally** — no `--show-patches`
flag, no `source` object. Attribution is simply the row's own `package` field, the same
field every package's integrations row carries whether or not a patch tier is involved.
See [Patch Companions Contribute Too][env-composition-integrations-companions] in the
environment composition reference.

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

`patch why` traces the surface the base's consumers see. Add `--self` to trace the private
surface instead, the one the base's own launchers see:

```sh
ocx patch why java:21 --self
```

A companion's `private` vars appear in that table and not in the consumer one.

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

`patch test` composes the surface the base's consumers see. Add `--self` to preview the
private surface, the one the base's own launchers see, which is where a companion's `private`
vars land:

```sh
ocx patch test \
  --descriptor ./my-descriptor.json \
  --self \
  java:21
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
`ocx patch freeze` reads what this machine has recorded, never an active snapshot, and it
fails with exit [`78`][exit-codes] when a project's `ocx.toml` exists but cannot be read,
rather than writing a snapshot without that project's companions.

Point [`OCX_PATCH_SNAPSHOT`][env-ocx-patch-snapshot] at the file to make commands use only
the pinned digests:

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
```

With the snapshot in place, every command except `ocx patch sync` and `ocx patch freeze`
uses only the snapshot's descriptors and pinned digests and never looks up a live tag.
`ocx package install`, `ocx package pull`, `ocx pull` and `ocx lock` write no companion pin
and no descriptor state. A companion the snapshot omits stays out even when this machine
has a pin for it; a required one fails with exit [`79`][exit-codes]. `ocx patch sync` is
refused (exit `78`) because it advances pins. To return to floating (live) tags, unset the
variable.

On a fresh machine that has only the snapshot, a pinned descriptor or companion missing
from the local store is fetched by its frozen digest, never by tag, and nothing is
recorded. Under [`--offline`][arg-offline] nothing is fetched: a missing descriptor or
companion that is required fails with exit `79`.

`ocx clean` roots the companions a snapshot pins only while `OCX_PATCH_SNAPSHOT` is set.
Without it, clean keeps only recorded pins, so a companion that only the snapshot pins is
collected, and the next run fetches it again by digest (or fails offline). Keep the
variable exported when you run `ocx clean`. Running `ocx patch sync` first, with the
variable unset, protects those companions only while the live tags still name the frozen
digests.

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

`patch sync` re-fetches every descriptor for all installed packages, the tools locked in every
known project's `ocx.lock`, and the global descriptor, installs any newly-referenced companion
packages, and re-checks packages installed before the `[patches]` tier was added. It is safe to run frequently; it piggybacks on the same
index-update mechanism as `ocx index update`.

Other commands contact the patch registry less often. `ocx package install`, `ocx package pull`,
`ocx pull`, `ocx lock`, `ocx add`, `ocx update` and `ocx package test` re-check each descriptor
they have seen before, whether it was found or absent, and fetch it again when it changed.
`ocx exec` and `ocx env` use the cached state and fetch only a descriptor they have never seen.
A failed re-check falls back to the cache, unless the tier is `required`, where it fails the
command. Under a `required` tier, a descriptor that was present and is now gone is an error
(exit 79) until `ocx patch sync` records it as absent.

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
applies where that file is directly in scope: [`ocx exec`][cmd-run], [`ocx env`][cmd-env-root],
and [`ocx direnv export`][cmd-direnv-export]. Each of these composes the environment itself
after reading the project config.

A binary that `ocx exec` launches can still reach the opt-out one hop further: if that binary
re-enters ocx through its own generated launcher, `ocx exec` forwards the opt-out to the child
process over [`OCX_PATCHES`][env-ocx-patches], so the launcher honors the same suppression
its parent did. `ocx env`, `ocx direnv export` and the shell hook mark the opted-out package in
[`OCX_LAUNCH_IDENTITIES`][env-ocx-launch-identities], so a launcher run from the `PATH` they
export honors it too. Any other launcher invocation — by absolute path, or through
[`ocx package exec`][cmd-package-exec] — has no opt-out to decode and composes the companion
overlay as if `no-patches` were never set.

:::info Why not everywhere?
The opt-out lives in a project's `ocx.toml`. OCI-tier commands (`ocx package install`,
`ocx package env`, `ocx package exec`) never read `ocx.toml` — that is the whole point of the
tier split described in the [command reference][cmd-patch]. There is no project in scope for
them to opt anything out of.
:::

See [Patch Opt-Out Scope][env-composition-patch-opt-out] in the environment composition
reference for the full forwarding mechanics.

## Working offline {#patches-offline}

Composing the environment resolves companions from whatever is already installed locally. The one
exception is a **required** companion that has no recorded pin yet, for example a base installed
before its rule existed: `ocx exec`, `ocx package exec` and `ocx env` resolve it live and record the
pin. An unpinned optional companion is skipped with no network call; `ocx package install`,
`ocx package pull`, `ocx pull`, `ocx lock`, `ocx add`, `ocx update` and `ocx patch sync` pick it up.

That live resolve never happens under `--offline` or with a patch snapshot (`OCX_PATCH_SNAPSHOT`).
There, an unpinned required companion fails the command with exit 79 instead. The enforcement rule
above applies the same whether or not `--offline` is set.

```sh
export OCX_PATCH_SNAPSHOT="/workspace/patches.snapshot.json"
ocx --offline exec -- cmake --version
```

With a snapshot in place, the pinned digests are resolved from the local object store and no
network access is needed. The command above works for `required` companions only when each
pinned descriptor and companion is already in the local store; one that is missing fails
with exit 79 instead of being fetched.

`ocx env --no-pull` never reaches a registry either, for patch companions included: it
composes from what is installed, and a required companion that is not fails with exit 79.

Without a snapshot, `ocx --offline exec` still applies whatever companions are already
installed locally: an optional (`required = false`) companion that is not yet installed is
skipped silently (a debug log line), but a **required** companion that is not yet installed fails
closed and aborts the run — the same posture as running online. `--offline` never turns a required
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
a rolling tag the descriptor keeps naming. Composition reads the pin. It records one only when a
required companion has none, and never advances an existing pin. Under a patch snapshot no
command records a pin, including install, pull and lock. When a descriptor changes or a new
companion should be picked up, sync while online, then run `ocx patch freeze` again to capture the
result for the next reproducible build.

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
- [Environment composition][env-composition] — how companion entries compose onto the
  base package's interface and private surfaces.
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

<!-- internal -->
[patches-offline]: #patches-offline

<!-- reference -->
[reference-env-path]: ../reference/metadata.md#env-path
[reference-env-list]: ../reference/metadata.md#env-list
[reference-visibility]: ../reference/metadata.md#env-entry-visibility

<!-- authoring -->
[authoring-env-surface-lists]: ../authoring/env-surface.md#lists

<!-- configuration -->
[config-patches]: ../reference/configuration.md#keys-patches
[config-mirrors]: ../reference/configuration.md#keys-mirrors

<!-- environment -->
[env-ocx-patches]: ../reference/environment.md#ocx-patches
[env-ocx-launch-identities]: ../reference/environment.md#ocx-launch-identities
[env-ocx-patch-snapshot]: ../reference/environment.md#ocx-patch-snapshot

<!-- env composition -->
[env-composition]: ../reference/env-composition.md
[env-composition-surfaces]: ../reference/env-composition.md#visibility-surfaces
[env-composition-patch-opt-out]: ../reference/env-composition.md#patch-opt-out-scope
[env-composition-integrations-companions]: ../reference/env-composition.md#integrations-companions

<!-- commands -->
[cmd-patch]: ../reference/command-line.md#patch
[cmd-package-env]: ../reference/command-line.md#package-env
[cmd-patch-why]: ../reference/command-line.md#patch-why
[cmd-patch-sync]: ../reference/command-line.md#patch-sync
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-run]: ../reference/command-line.md#exec
[cmd-env-root]: ../reference/command-line.md#env-root
[cmd-direnv-export]: ../reference/command-line.md#direnv-export
[cmd-package-exec]: ../reference/command-line.md#package-exec
[cmd-package-test]: ../reference/command-line.md#package-test
[cmd-lock]: ../reference/command-line.md#lock
[cmd-global-flag]: ../reference/command-line.md#global-flag
[arg-offline]: ../reference/command-line.md#arg-offline
[arg-frozen]: ../reference/command-line.md#arg-frozen
[exit-codes]: ../reference/command-line.md#exit-codes

<!-- in-depth -->
[fs-index]: ../in-depth/indices.md#local

<!-- internal -->
[patches-how-part-of-target]: #patches-how-part-of-target
