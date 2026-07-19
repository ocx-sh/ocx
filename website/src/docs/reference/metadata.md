---
layout: doc
outline: deep
---
# Metadata {#metadata}

Every OCX package includes a `metadata.json` file that declares how OCX should extract
and configure the package at install time. Publishers create this file alongside the
package archive; OCX stores it in the [object store][fs-objects] after installation and
reads it whenever the package environment is resolved.

A formal [JSON Schema][schema-url] is available for editor autocompletion and validation.
Add a `$schema` field to get instant feedback in VS Code, JetBrains, and other editors
that support [JSON Schema][json-schema]:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1
}
```

## Format {#format}

### Top-Level Structure {#format-top-level}

The metadata file is a JSON object with a `type` discriminator. Currently only the
`bundle` type is supported.

| Field | Type | Required | Description |
|---|---|---|---|
| `type` | string | Yes | Discriminator tag. Must be `"bundle"`. |
| `version` | integer | Yes | Format version. Currently `1`. |
| `strip_components` | integer | No | Leading path components to strip during extraction. |
| `env` | array | No | [Environment variable declarations](#env). |
| `dependencies` | array | No | [Package dependencies](#dependencies). |
| `entrypoints` | object | No | [Named entry points](#entry-points), keyed by command name. |
| `binaries` | array | No | [Declared executable names the package puts on `PATH`](#executables). |

::: details Why a type discriminator?
The `type` field allows future metadata formats (e.g. `"manifest"`, `"virtual"`) without
breaking existing packages. Parsers that encounter an unknown type can reject the file with
a clear error rather than silently misinterpreting fields.
:::

### Minimal Example {#format-minimal}

A package with no environment variables and no extraction options:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1
}
```

### Full Example {#format-full}

A language runtime with multiple environment variables, archive stripping, a dependency, and named entry points:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "strip_components": 1,
  "env": [
    {
      "key": "PATH",
      "type": "path",
      "required": true,
      "value": "${installPath}/bin",
      "visibility": "public"
    },
    {
      "key": "JAVA_HOME",
      "type": "constant",
      "value": "${installPath}",
      "visibility": "public"
    },
    {
      "key": "LD_LIBRARY_PATH",
      "type": "path",
      "required": false,
      "value": "${installPath}/lib"
    }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/gcc:13@sha256:a1b2c3d4e5f6..."
    }
  ],
  "binaries": ["cmake", "cpack", "ctest"],
  "entrypoints": {
    "cmake": {},
    "fmt": { "command": "cmake-format" }
  }
}
```

Here `cmake` dispatches to a binary named `cmake` (empty object, the common case), while the `fmt` launcher dispatches to a differently-named binary `cmake-format`. `binaries` separately declares that `cmake`, `cpack`, and `ctest` exist as executables on the interface `PATH` — see [Executables](#executables).

## Environment Variables {#env}

The `env` array declares environment variables that OCX exposes when running
commands with the package (via [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env]).

Each entry is an object with a `key`, a `type` (`path` or `constant`), and a `value`
template. Two placeholders are available in `value`:

- **`${installPath}`** — replaced with the absolute path to this package's content directory.
- **`${deps.NAME.installPath}`** — replaced with the absolute path to a declared dependency's content directory, where `NAME` is the dependency's repository basename (or its explicit `name` field if one is declared). Useful for pointing consumers at a dependency's installation directory.

`${installPath}` and `${deps.NAME.installPath}` may appear multiple times and can be combined in the same value (e.g. `${installPath}/bin:${deps.cmake.installPath}/bin`). OCX validates at publish time that every `${deps.*}` reference names a declared dependency.

### Path Variables {#env-path}

Path variables are **prepended** to any existing value of the environment variable,
separated by the platform path delimiter.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"path"`. |
| `required` | boolean | No | If `true`, the resolved path must exist on disk. Defaults to `false`. |
| `value` | string | Yes | Value template. Supports `${installPath}` and `${deps.NAME.installPath}`. |
| `visibility` | string | No | Entry visibility. See [Entry Visibility](#env-entry-visibility). Default: `"private"`. |

```json
{
  "key": "PATH",
  "type": "path",
  "required": true,
  "value": "${installPath}/bin"
}
```

When `required` is `true` and the resolved path does not exist, the operation fails
with an error. Set `required` to `false` for optional paths like `lib/` directories
that may not be present on all platforms.

### Constant Variables {#env-constant}

Constant variables **replace** any existing value of the environment variable.

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | Yes | Environment variable name. |
| `type` | string | Yes | Must be `"constant"`. |
| `value` | string | Yes | Value template. Supports `${installPath}` and `${deps.NAME.installPath}`. |
| `visibility` | string | No | Entry visibility. See [Entry Visibility](#env-entry-visibility). Default: `"private"`. |

```json
{
  "key": "JAVA_HOME",
  "type": "constant",
  "value": "${installPath}"
}
```

Constants are useful for home directory variables (`JAVA_HOME`, `CARGO_HOME`) and
fixed values that do not depend on the install path (e.g. a version string).

### Entry Visibility {#env-entry-visibility}

Each `env` entry carries a `visibility` field that controls which surface the entry
contributes to when composing the runtime environment. The field is distinct from the
[dependency-edge `visibility`](#dependencies-visibility), which controls how a
dependency's env propagates to its dependents.

| Value | Interface surface (`--self` off) | Private surface (`--self` on) | Use case |
|---|---|---|---|
| `private` (default) | No | Yes | Internal paths the package's own launchers need; not part of the public contract. |
| `public` | Yes | Yes | Variables consumers should see — `PATH`, `JAVA_HOME`, tool-specific prefix paths. Both surfaces. |
| `interface` | Yes | No | Values forwarded to consumers but not used by the package's own runtime — `PKG_CONFIG_PATH`, library include hints. |

`"sealed"` is rejected at parse time on `env` entries — a declared entry that is invisible
on both surfaces is dead configuration.

See [Env Composition][env-composition] for how these entry values interact with
dependency-edge visibility during the full composition walk.

::: info CMake vocabulary — a memory aid, not a contract
OCX entry visibility shares vocabulary with [CMake's `target_compile_definitions`][cmake-compile-defs] (`PRIVATE`, `PUBLIC`, `INTERFACE`), but the two govern different axes. CMake's keyword on a declaration controls what that target *publishes to its build consumers* at compile time. OCX's `visibility` field on an `env` entry controls which of the **package's own two runtime surfaces** that entry contributes to: `private` = self-only (the package's internal runtime), `public` = both surfaces, `interface` = consumer-only.

Use the CMake vocabulary as a memory aid — the terms carry the same directional intuition — but do not rely on behavioral parity. In OCX, entry visibility partitions a publisher's own declared env entries across two runtime surfaces. Dep reachability is a separate concern governed by [dependency-edge visibility](#dependencies-visibility).

See the [Authoring Guide migration section][authoring-migration] if you are updating packages that predate the entry visibility field.
:::

## Dependencies {#dependencies}

The `dependencies` array declares packages that must be present for this package to function.
Every dependency identifier is pinned by <Tooltip term="OCI digest">A SHA-256 fingerprint identifying a specific
build. The digest — not the tag — is the authoritative identifier. The tag is advisory, included for
readability and future update tooling.</Tooltip>, ensuring the same dependency graph is reproduced
on every machine regardless of the current registry state.

The `metadata.json` you author and the `metadata.json` OCX publishes are not always the same
bytes. See [Authoring vs Published](#dependencies-authoring-vs-published) below for the two shapes,
[Manifest Pins, Never Index Pins](#dependencies-manifest-pins) for what the digest is allowed to
point at, and [Per-Platform Pins](#dependencies-per-platform-pins) for the sidecar-only fields that
[`ocx package create`][cmd-package-create] writes for dependencies that ship platform-specific builds.

### Authoring vs Published {#dependencies-authoring-vs-published}

The sidecar you hand to [`ocx package create`][cmd-package-create] is a superset of the
`metadata.json` OCX publishes. In the authoring sidecar, a dependency identifier needs only an
explicit registry — the digest is optional:

```json
{ "identifier": "ocx.sh/java:21" }
```

An identifier with no digest tells `ocx package create --platform <PLATFORM>` to resolve it against
the selected index and rewrite the sidecar in place: a concrete `--platform` pins the resolved
manifest digest directly on the identifier, while `--platform any` records it in a per-dependency
[pin map](#dependencies-per-platform-pins) instead (an `any`-targeted package can only depend on
dependencies that themselves offer an `any` manifest). The rewritten sidecar — not the one you
hand-wrote — is what you commit alongside the archive and hand to
[`ocx package push`][cmd-package-push]. `push` reads that file, verifies every dependency is
pinned, and refuses to publish (exit 65) anything still tag-only.

The **published** form is what the registry stores and what [`ocx package install`][cmd-package-install]
reads: every dependency identifier carries a manifest digest, and the sidecar-only
per-dependency `platforms` pin map is gone.

```json
{ "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...", "visibility": "public" }
```

::: tip A published `metadata.json` is a valid sidecar
Because the published form is a strict subset of the authoring form, an already-published
`metadata.json` — fully pinned, no sidecar fields — parses as authoring metadata unchanged.
`ocx package create` and `ocx package push` both accept it as-is; there is nothing to migrate.
:::

### Manifest Pins, Never Index Pins {#dependencies-manifest-pins}

The digest in a published dependency identifier must reference a platform **manifest**, never an
[OCI Image Index][oci-image-index]. Pinning a dependency's index digest looks attractive at first —
a single identifier could then resolve to whichever platform an installing host needs, the same way
an ordinary package reference does at install time — but it does not survive the dependency
publisher's next release.

An index digest identifies one *version* of a tag's index. When the dependency publisher pushes a
new platform, or re-pushes an existing one, [`ocx package push`][cmd-package-push] rewrites the tag's
index to include the new platform descriptor — the old index digest is no longer referenced by any
tag and becomes eligible for the registry's garbage collector on its next sweep. A dependency pinned
to that now-untagged index digest starts 404ing the moment GC runs. The child platform *manifests*
have no such problem: every successor index still references them, so they survive indefinitely.

[`ocx package push`][cmd-package-push] enforces this at publish time: it resolves each dependency's
pin against its registry and rejects the push (exit 65) if the resolved digest is an image index
rather than a manifest, naming the offending dependency.

::: info The same rule governs the project lock
[`ocx.lock`][in-depth-project-lock] pins each tool to a **per-platform leaf manifest digest**, never
the index digest — see [Lock format][in-depth-project-lock-format]. Package dependencies follow the
same rule, for the same reason: the index digest is a moving target across a publisher's release
history; the leaf manifest digest is not.
:::

### Dependency Entry {#dependencies-entry}

| Field | Type | Required | Description |
|---|---|---|---|
| `identifier` | string | Yes | OCX identifier with an explicit registry. In the authoring sidecar the digest is optional — a tag-only identifier tells [`ocx package create`][cmd-package-create] to resolve it. In the published form the digest is mandatory and must reference a platform manifest, never an OCI Image Index (see [above](#dependencies-manifest-pins)). The tag is always advisory. e.g. `ocx.sh/java:21@sha256:a1b2c3d4e5f6...`, `ghcr.io/myorg/tool@sha256:...`. |
| `platforms` | object | No | Authoring-sidecar-only. Per-dependency manifest pin map — see [Per-Platform Pins](#dependencies-per-platform-pins). Written by `ocx package create --platform any` as a single `"any"`-keyed entry; stripped at publish. |
| `name` | string | No | Short name used to reference this dependency in `${deps.NAME.installPath}` templates. When set, this name is used instead of the repository basename. Must match `^[a-z0-9][a-z0-9_-]*$` and be at most 64 characters. Useful when two dependencies share the same basename (e.g. `myorg/cmake` and `upstream/cmake`) or when the basename is long. |
| `visibility` | string | No | Controls how the dependency's environment variables propagate. Default: `sealed`. See [Visibility](#dependencies-visibility). |

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "required": true, "value": "${installPath}/bin", "visibility": "public" },
    { "key": "JDK_HOME", "type": "constant", "value": "${deps.java.installPath}", "visibility": "public" }
  ],
  "dependencies": [
    {
      "identifier": "ocx.sh/java:21@sha256:a1b2c3d4e5f6...",
      "visibility": "private"
    },
    {
      "identifier": "ocx.sh/cmake:3.28@sha256:f6e5d4c3b2a1...",
      "name": "cmake"
    }
  ]
}
```

### Per-Platform Pins {#dependencies-per-platform-pins}

A package built with [`ocx package create --platform any`][cmd-package-create] (platform-agnostic
content, such as a script) targets no specific platform of its own, so it performs no
platform-specific resolution. That means it can only depend on dependencies that themselves offer
an `any` manifest — [`ocx package create`][cmd-package-create] records that resolved pin in a
per-dependency `platforms` map instead of a plain digest, because a leaf manifest carries no
platform descriptor of its own and a bare `@digest` pin could never be verified as genuinely
`any`-offered:

```json
{
  "identifier": "ocx.sh/mytool:1.0",
  "platforms": { "any": "sha256:aaaa..." }
}
```

The map key uses the same [canonical platform grammar][reference-platforms] as every other
platform string in OCX — `--platform`, `ocx.lock`'s per-platform table, all of it. A hand-authored
or pre-existing sidecar may carry other keys (a concrete platform, or a feature-tagged one like
`linux/amd64+libc.glibc`); at publish time each key is projected onto the platform being published
through the same [compatibility relation and scoring][reference-platforms-compatibility]
`Index::select` uses at install time — so a plain `linux/amd64` key covers a feature-tagged
package platform like `linux/amd64+libc.glibc`, but a feature-tagged key does not cover the plain
platform (fail-closed: the offer's declared features must be a subset of what the target
platform's requirement offers). A platform the map does not cover at all makes the projection fail
(exit 65), naming the uncovered platform.

If a dependency offers no `any` manifest at all, `ocx package create --platform any` fails (exit
65) naming the dependency — there is no partial coverage to fall back to. The same rule extends to
already-pinned dependencies: `create` and [`ocx package push`][cmd-package-push] both reject a
direct digest pin anywhere in an `any`-targeted bundle's dependency list (exit 65), because that
pin's `any`-ness cannot be verified either. See
[Dependencies with Platform-Specific Builds][authoring-dependency-platforms] for the full
publisher workflow.

A pin map entry is a snapshot of the dependency's platform coverage at `create` time, not a live
query. If the dependency later adds an `any` manifest where none existed before, the consuming
package does not automatically pick it up — re-run `ocx package create --platform any` against a
refreshed index, then `ocx package push`, to re-resolve.

### Visibility {#dependencies-visibility}

Each dependency's `visibility` field controls how its environment variables propagate through the
dependency chain. The model is inspired by [CMake's `target_link_libraries`][cmake-tll] visibility
(PUBLIC/PRIVATE/INTERFACE).

The struct has two boolean axes — `private` (self-axis: visible to the package's own runtime)
and `interface` (consumer-axis: propagated to consumers). The four named constants map to the
four `(private, interface)` combinations.

| Value | Private surface (`--self`) | Interface surface (default) | Use case |
|---|---|---|---|
| `sealed` (default) | No | No | Structural dependency — content accessed by path, not env. Most deps. |
| `private` | Yes | No | Package's own shims need the dep's env; consumers don't. |
| `public` | Yes | Yes | Both the package and consumers need the dep's env. |
| `interface` | No | Yes | Meta-packages that forward env to consumers without using it. |

Surface gating: `has_private()` returns `true` for `private` and `public`; `has_interface()`
returns `true` for `public` and `interface`. The [composer][env-composition-edge-filter] uses
these accessors to gate TC entry emission per surface at exec time.

::: details Transitive Propagation via `through_edge` {#dependencies-through-edge}

When dependencies form a chain (Root → Dep → Transitive), visibility propagates using
`Visibility::through_edge(child_eff)`: if the child's effective visibility does not export
to consumers (`child_eff.has_interface() == false`), the result is `sealed`; otherwise the
edge passes through unchanged.

| Edge | Child effective | Result (from root) |
|---|---|---|
| `public` | `public` / `interface` | `public` |
| `public` | `private` / `sealed` | `sealed` |
| `private` | `public` / `interface` | `private` |
| `private` | `private` / `sealed` | `sealed` |
| `interface` | `public` / `interface` | `interface` |
| `interface` | `private` / `sealed` | `sealed` |
| `sealed` | any | `sealed` |

<span id="dependencies-merge"></span>When two paths reach the same dependency (diamond), the most open visibility wins — each axis
is OR-merged independently via `Visibility::merge`. This is computed at install time and stored
in `resolve.json`. See [Env Composition — Edge Filter][env-composition-edge-filter] for how the
pre-computed effective visibilities are used at exec time.
:::

::: info Compare with Nix and Guix

Functional package managers describe the same idea as **propagated dependencies**. In [Nix][nix],
[`propagatedBuildInputs`][nix-propagated] is the propagated counterpart to `buildInputs` —
dependencies of a package whose own dependencies cascade to indirect dependents without each
consumer having to relist them. In [Guix][guix], [`propagated-inputs`][guix-propagated] are
"similar to `inputs`, but the specified packages will be automatically installed to profiles
alongside the package they belong to."

OCX's `public` and `interface` visibilities are the same shape: they mark a dependency as
contributing its environment to consumers transitively. `private` is the OCX equivalent of plain
`buildInputs` / `inputs` — the package itself sees the env, consumers do not. `sealed` deliberately
contributes nothing to either side.
:::

### Ordering {#dependencies-ordering}

Array position defines the canonical order for environment composition. Dependencies are processed
in array order — the first entry's environment is applied first. This ordering is preserved
through transitive resolution: the full dependency graph is <Tooltip term="topologically sorted">
Kahn's algorithm with a lexicographic tiebreaker on identifier. Dependencies appear before their
dependents in the final list.</Tooltip>, deduplicated, and applied in that deterministic sequence.

### Registry Requirement {#dependencies-registry}

Every dependency identifier must include an explicit registry (`ocx.sh/java:21@sha256:a1b2c3d4e5f6...`, not just `java:21@sha256:a1b2c3d4e5f6...`).
Default registry resolution is not applied because the consumer may have a different default registry
than the publisher. Identifiers without an explicit registry are rejected at deserialization.

### No Version Ranges {#dependencies-no-version-ranges}

In the published form, the digest is the complete truth — there is nothing to resolve. The tag
portion of the identifier is purely informational: it records what the publisher pinned against and
enables future update tooling, but is never used for resolution. Writing the digest by hand is never
required — the authoring sidecar accepts a tag-only identifier and [`ocx package create`](#dependencies-authoring-vs-published)
computes the pin for you.

See [Dependencies][ug-dependencies] in the user guide for how dependencies affect
installation, environment composition, and garbage collection from a user's perspective.

## Entry Points {#entry-points}

The `entrypoints` object declares named launchers that `ocx install` generates at install time.
Each launcher is a small `.sh` shell script on Unix, or a native `<name>.exe` shim plus a
one-line `<name>.shim` sidecar on Windows, placed in an `entrypoints/` directory
inside the package directory. When the package is selected with `--select`, the per-repo `current`
symlink is flipped to the package root and consumers traverse `current/entrypoints` from the same
anchor to add the launchers to `PATH`.

Each launcher re-enters via [`ocx launcher exec`][cmd-launcher-exec] with the package root baked at
install time, preserving clean-environment execution semantics on every invocation. The launcher
resolves a *dispatch command* against the composed `PATH` from the package's [`env`](#env) block.
By default the dispatch command is the entry point's own name — the publisher declares the binary's
location once via `env` and the launcher exec resolver picks it up from there. A package that needs
the invocable name to differ from the binary it runs sets the optional `command` field (see below).

### Wire Shape {#entry-points-wire-shape}

`entrypoints` is a JSON object keyed by the invocable name. The map shape mirrors the Cargo
`[dependencies.X]`, Compose `services:`, and GitHub Actions `jobs:` idioms — uniqueness within a
package follows from JSON object key semantics, and per-entry fields land inside each value object.

| Position | Type | Required | Description |
|---|---|---|---|
| Key | string | Yes | The invocable name. Must match `^[a-z0-9][a-z0-9_-]*$` and be at most 64 bytes. Used as the launcher script filename and the command users invoke. |
| Value | object | Yes | Per-entry fields. `{}` is the common case: the invocable name *is* the dispatched command. |
| `command` | string | No | Dispatch target resolved on the composed `PATH` when it differs from the invocable name. Same `^[a-z0-9][a-z0-9_-]*$` / 64-byte rule as the key. Omit it (the common case) and the invocable name is dispatched directly. Example: expose `hello` while running a binary named `hello-bin`. Not interpolated — must be a plain slug, not a path. |
| `args` | array of strings | No | Fixed leading arguments prepended before user-supplied arguments when the launcher dispatches. Each element is one argv token (no shell word-splitting). `${installPath}` is interpolated in each element; `${deps.*}` tokens are rejected at publish time. Omit or supply an empty array — both are wire-identical; the field is absent in the serialized form when empty. See [Baked Arguments](#entry-points-args). |

### Baked Arguments {#entry-points-args}

The `args` field embeds fixed leading arguments into a generated launcher. On every invocation
the launcher prepends these arguments before the user's arguments, then passes the full list to
the dispatched command.

```json
{ "command": "python", "args": ["${installPath}/app/main.py"] }
```

Invoking `mytool a b` with this entrypoint runs `python <content>/app/main.py a b` — baked args
first, user args appended in left-to-right array order. Each element is one argv token; there is
no shell word-splitting, so paths with spaces work without escaping.

**`${installPath}` interpolation.** Each element of `args` supports the `${installPath}` token,
which resolves to the package's content directory — the same path that [`env`](#env) values
reference with the same token. The token may appear more than once within a single element.

**Token restrictions.** `${deps.*}` tokens are rejected at publish time with a dedicated error.
Dependency paths belong in the [`env`](#env) block where the visibility contract applies;
consumers read them at runtime from the composed environment. `command` is a plain slug resolved
on the composed `PATH`; it accepts no interpolation and cannot be a filesystem path. To reach a
dependency's binary in `command`, expose it through the dependency's `interface` or `public` env
entries.

### Disk Layout {#entry-points-disk-layout}

Generated launchers land in `entrypoints/` inside the package directory (a sibling of `content/`).
When the package is selected with `ocx install --select` or `ocx select`, the per-repo `current`
symlink is flipped to that package root, and consumers reach the launchers via
`{registry}/{repo}/current/entrypoints`. Packages with no entrypoints produce no `entrypoints/`
directory, so the same `current/entrypoints` path simply does not exist for them.

### Uniqueness {#entry-points-uniqueness}

The map shape gives intra-package uniqueness via JSON object key semantics. Duplicate keys in the
on-wire JSON are rejected at deserialization with a descriptive error rather than silently
last-wins (the `serde_json` default). Name collisions across different currently-selected
packages are detected at select time.

### Example {#entry-points-example}

```json
{
  "entrypoints": {
    "cmake": {},
    "ctest": {},
    "hello": { "command": "hello-bin" },
    "mytool": { "command": "python", "args": ["${installPath}/app/main.py"] }
  }
}
```

`cmake` and `ctest` dispatch the binaries of the same name. `hello` dispatches `hello-bin`
resolved on the composed `PATH`. `mytool` uses `args` to bake the script path: invoking
`mytool x y` runs `python <content>/app/main.py x y`.

## Executables {#executables}

CI matrices, [Bazel][bazel] toolchain rules, and devcontainer features often need one narrow
answer before they trust a package: does `cmake` actually resolve on `PATH` once this package
is installed? The composed environment ([`env`](#env)) tells a caller how `PATH` gets built, not
what resolves on it — answering that today means installing the package and probing `PATH`
directly.

The `binaries` array closes that gap. It is an optional, publisher-declared array of bare
executable names the package exposes on its interface `PATH` surface — a name a consumer can
look up without a round trip through installation.

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json",
  "type": "bundle",
  "version": 1,
  "env": [
    { "key": "PATH", "type": "path", "value": "${installPath}/bin", "visibility": "public" }
  ],
  "binaries": ["cmake", "cpack", "ctest"]
}
```

### A Claim, Not a Guarantee {#executables-claim}

`binaries` is unverified by default. [`ocx package push`][cmd-package-push] streams archives
straight to the registry — it never decompresses a layer — and a package can compose several
layers, including ones [reused by digest][cmd-package-push-layout] from an earlier push. No
point in the pipeline ([`create`][cmd-package-create], [`push`][cmd-package-push], or
[`install`][cmd-package-install]) ever has every layer's content tree materialized at once, so
full validation of the claim against disk is structurally impossible without breaking the
streaming-push, lazy-pull model the rest of OCX relies on.

:::info Same trust model as `package.json`
[npm's `bin` field][npm-bin], [Cargo's `[[bin]]` target name][cargo-bin], and [nixpkgs'
`meta.mainProgram`][nixpkgs-main-program] all make the identical trade: the field documents what
a consumer will find, and none of them are verified against the built artifact at publish time.
`binaries` follows the same convention on purpose — every packaging ecosystem surveyed converges
on a bare, unverified, documentation-grade name claim.
:::

On Windows, the claim reflects the **default** executable-resolution set (`.exe`, `.com`,
`.bat`, `.cmd`) — the fixed allowlist the create-time scan uses. A hardened child environment
with a customized `PATHEXT` may resolve fewer of these, so a claimed name backed only by a
`.bat`/`.cmd`/`.com` file is not guaranteed to resolve in every composed environment.

[`ocx package create`][cmd-package-create]'s `--bin-scan`/`--no-bin-scan` flags can fill or
verify the claim against the local content tree at publish time, closing the gap for content
`create` can actually see. A foreign or reused layer added later at `push` time was never part
of that scan — declaring its binaries is the publisher's job, by hand. See
[`package create`][cmd-package-create] for the scan modes.

### Interface Surface, Own Package Only {#executables-scope}

Only names reachable through a `${installPath}`-rooted [`path`](#env-path) variable with
[interface visibility](#env-entry-visibility) are eligible. Private, `libexec`-style
directories are never candidates — a name only a package's own launchers can see was never part
of the public contract `binaries` describes.

The claim is **own-package only, never transitive**: a package never lists a dependency's
executables. What is reachable *through* a dependency chain is a composition question — answered
by [`ocx env`][cmd-env-root] / [`ocx package env`][cmd-package-env]'s `binaries` array, attributed
per admitted package, not by a per-package metadata fact.

### `None` vs an Empty Array {#executables-none-vs-empty}

The two wire states carry different meaning, kept deliberately distinct:

```json
// undeclared — omitted entirely; predates this field, or the publisher never set it
{ "type": "bundle", "version": 1 }

// declared: the publisher asserts zero executables on PATH (e.g. an env-only package)
{ "type": "bundle", "version": 1, "binaries": [] }
```

A consumer reading `metadata.json` directly — an SBOM scanner, say — can tell "nobody has
declared this yet" apart from "the publisher looked, and there genuinely are none."

### Executables vs Entry Points {#executables-vs-entry-points}

`binaries` and [`entrypoints`](#entry-points) describe two different things and are never
confused with each other. `binaries` names files the publisher shipped in the content tree;
`entrypoints` names launchers OCX *generates* at install time. **A generated launcher name never
appears inside `binaries`** — it is not a file the publisher's archive contains, it is something
`ocx install` writes.

The same name can legitimately appear in both, without contradiction: `binaries: ["cmake"]`
declares that the underlying `cmake` binary exists, and `entrypoints: {"cmake": {}}` declares
that a generated launcher wraps it. At runtime the generated `entrypoints/` directory is
prepended ahead of `bin/` on the composed `PATH` (see [Disk Layout](#entry-points-disk-layout)),
so the launcher — not the raw binary — is what actually resolves; `binaries` still documents that
the wrapped binary is there.

### Writing the Field {#executables-writing}

`binaries` is always written as a plain array of bare strings, sorted and de-duplicated:

```json
{ "binaries": ["cmake", "cpack", "ctest"] }
```

There is no per-entry object form in the published schema — a `binaries` entry is a name,
nothing else. [`ocx package create --bin-scan`][cmd-package-create] can generate this array for
you from the content tree; a hand-authored `metadata.json` writes it exactly the same way.

Each name is validated against a grammar looser than an [entry point key](#entry-points-wire-shape)
— it admits names like `python3.13`, `c++`, and `MSBuild` that the entry-point slug pattern would
reject:

- ASCII printable characters only, no whitespace anywhere.
- Forbidden characters: `/ \ < > : " | ? *` — the Windows-reserved filename characters, since OCX
  materializes binary names as real files on Windows.
- No leading `-` (shell flag-lookalike hazard).
- No leading or trailing `.` (Unix hidden-file ambiguity; a trailing dot that Windows silently
  strips would otherwise collide on disk).
- Non-empty, at most 64 bytes.
- Reserved Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9`) are
  rejected case-insensitively, checked against the basename before the first `.` — so a suffixed
  alias like `CON.txt` is rejected too, matching how Windows reserves the device name regardless
  of extension.
- Two names in the same `binaries` array that differ only by case (`Cmake` and `cmake`) are
  rejected — a case-insensitive target filesystem would collide on them even though the two
  strings are distinct.
- Bare names only — never `.exe` or any other extension. Extension handling belongs to the
  platform-specific resolve step (see [A Claim, Not a Guarantee](#executables-claim)), never to
  the metadata.

## Extraction {#extraction}

### `strip_components` {#extraction-strip-components}

Many upstream archives wrap their content in a single top-level directory
(e.g. `cmake-3.28/bin/cmake`). Rather than repackaging, set `strip_components` to
remove leading path components when the package is assembled — analogous to `tar --strip-components`.

| Value | Effect |
|---|---|
| omitted / `0` | Extract as-is. |
| `1` | Remove one leading directory: `cmake-3.28/bin` → `bin`. |
| `2` | Remove two: `a/b/bin` → `bin`. |

`strip_components` is the package-wide default, applied to any layer that carries no
layout of its own. A multi-layer package can override it per layer with a `strip`/`prefix`
pair carried in the manifest layer descriptor's annotations, set via the `<ref>:strip=N,prefix=P`
syntax on [`ocx package push`][cmd-package-push] — see [layer layout][cmd-package-push-layout]
for the grammar and the fallback chain. There is no separate `layers` field in this schema;
per-layer layout lives entirely in the manifest, not in `metadata.json`.

## JSON Schema {#json-schema}

The schema is generated from the Rust source types and published at:

**[`https://ocx.sh/schemas/metadata/v1.json`][schema-url]**

### Editor Integration {#json-schema-editors}

Add `$schema` to the top of your `metadata.json` for instant validation and
autocompletion:

```json
{
  "$schema": "https://ocx.sh/schemas/metadata/v1.json"
}
```

### Generating Locally {#json-schema-local}

The schema is a build artifact generated from the OCX source. To regenerate:

```sh
task schema:generate
```

This writes the schema to `website/src/public/schemas/metadata/v1.json`.

### Validation {#json-schema-validation}

Validate a metadata file against the schema using [check-jsonschema][check-jsonschema]:

```sh
uvx check-jsonschema --schemafile https://ocx.sh/schemas/metadata/v1.json metadata.json
```

## Schema {#schema}

The metadata format carries an integer `version` field reserved for future schema evolution. Currently the only valid value is `1`. Top-level fields:

- `type` — discriminator (`"bundle"` only currently).
- `version` — integer schema version. Currently `1`.
- `strip_components` — optional leading path components to strip during extraction.
- `env` — optional declarations of environment variables.
- `dependencies` — optional package dependencies, digest-pinned in the published form. Each entry carries an `identifier`, optional `name` override (used as `NAME` in `${deps.NAME.installPath}` tokens), optional `visibility` controlling env propagation through the chain, and — authoring sidecar only — an optional `platforms` pin map. See [Dependencies](#dependencies).
- `entrypoints` — optional object keyed by the invocable name. Each value object carries two optional fields. `command`: the binary the generated launcher dispatches to when it differs from the invocable name (e.g. expose `fmt` while running `cargo-fmt`); follows the same slug constraint as the key (`[a-z0-9][a-z0-9_-]*`, at most 64 bytes); not interpolated; omitted means the invocable name is the dispatch target. `args`: array of fixed leading arguments prepended before user-supplied arguments at dispatch time; each element is one argv token; `${installPath}` is interpolated per element; `${deps.*}` tokens are rejected at publish time; omitted or empty are wire-identical — the field is absent in the serialized form when the array is empty.
- `binaries` — optional array of bare executable-name strings, sorted and unique: a publisher-declared, unverified claim of executables the package puts on `PATH`. Absent means undeclared; `[]` means the publisher asserts zero. Never lists a dependency's executables, and never lists an `entrypoints` launcher name. See [Executables](#executables).

Visibility model:

- `Dependency.visibility` — two-axis struct (`private` + `interface` booleans) with four named constants: `sealed` (default; neither axis set; content accessible by path only via `${deps.NAME.installPath}`), `private` (self-axis only; package's own runtime sees dep's env, consumers don't), `public` (both axes; package and consumers see dep's env), `interface` (consumer-axis only; dep's env forwarded to consumers without being used by the package itself). Algebra: `merge` for diamond dedup (OR per axis), `through_edge` for inductive TC composition. Accessors `has_interface()` / `has_private()` gate surface emission. See [Dependency Visibility](#dependencies-visibility) and [Env Composition][env-composition].
- `Var.visibility` — three-value entry-axis marker: `private` (default; private surface only), `public` (both surfaces), `interface` (interface surface only). `"sealed"` is rejected at parse — a declared entry visible on neither surface is dead configuration. See [Entry Visibility](#env-entry-visibility).

## OCI Platform Fields {#oci-platform}

`metadata.json` describes runtime configuration for a single package build. The OCI platform descriptor — the `platform` object in an [OCI Image Index][oci-image-index] entry — is separate and not part of `metadata.json`. It is declared by the publisher at push time (or generated by the mirror tool from the asset spec) and consumed by OCX at index resolution time. This section covers the JSON wire object; see [Platforms][reference-platforms] for the human-readable string form (`--platform`, lock keys, pin-map keys) and the compatibility relation OCX evaluates it against.

### `os.features` and libc tagging {#oci-platform-os-features}

The [OCI Image Index specification][oci-image-index] defines `os.features` as an optional array of strings encoding mandatory OS features the image requires. For non-Windows operating systems the specification leaves values implementation-defined.

OCX uses this field to encode the libc family requirement of a Linux binary. Two values are defined:

| Value | Meaning |
|---|---|
| `libc.glibc` | Binary links [GNU libc (glibc)][glibc]. Requires glibc on the installing host. |
| `libc.musl` | Binary links [musl libc][musl]. Requires musl on the installing host. |

A static binary (no runtime libc dependency) carries no `os.features` declaration — the absent or empty set matches every Linux host.

These values appear in the published OCI image index JSON:

```json
{
  "platform": {
    "architecture": "amd64",
    "os": "linux",
    "os.features": ["libc.glibc"]
  }
}
```

**Normalization:** OCX sorts and deduplicates `os_features` before serialization. The order in the YAML spec or the push command does not affect the wire format. Duplicate values collapse to one.

**RESERVED `features` field:** The OCI v1.1.1 specification marks the top-level `platform.features` field (not `os.features`) as RESERVED. OCX never serializes it and drops any value found in a foreign manifest with a warning.

See [libc Differentiation][authoring-libc] in the multi-platform authoring guide for the publisher workflow and YAML examples.

## Schema Changelog {#schema-changelog}

The integer `version` field is reserved for future schema evolution. Behavioral changes that do not require a version bump (because they are backwards-compatible additions or clarifications within `version: 1`) are recorded here.

### Version 1 — Current {#schema-changelog-v1}

All packages must declare `"version": 1`. This is the only valid value; other values are rejected at parse time.

Behavioral changes made within `version: 1` since the initial release:

| Change | Description |
|---|---|
| **Visibility default flip** | `Var.visibility` now defaults to `"private"` instead of `"public"`. Packages that relied on the old default emit no interface-surface env entries for un-tagged vars. Publishers must explicitly set `"visibility": "public"` to restore prior behavior for consumer-visible vars. |
| **Entry-axis addition** | `Var.visibility` gained the `"interface"` value: env entries visible on the interface surface but not the private surface. Previously only `"private"` and `"public"` were recognized; `"interface"` entries in older parsers will be rejected at deserialization. |
| **Baked entry-point arguments** | Entry-point values now accept an optional `args` array of fixed leading arguments prepended before user-supplied arguments at dispatch time. `${installPath}` is interpolated per element; `${deps.*}` tokens are rejected at publish time. An absent or empty `args` array is wire-identical to prior behavior — packages without `args` are unaffected. |
| **Dependency manifest pinning** | The authoring sidecar accepts a digest-optional dependency identifier (`ocx package create --platform` resolves it) and a per-dependency `platforms` pin map — see [Per-Platform Pins](#dependencies-per-platform-pins). The sidecar-only field is stripped at publish; the published wire format is unchanged. The published digest must reference a platform manifest, never an OCI Image Index — see [Manifest Pins, Never Index Pins](#dependencies-manifest-pins). `ocx package push` rejects an index-pinned or unpinned dependency (exit 65). |
| **Declared executables** | New optional `binaries` field: a sorted, unique array of bare executable-name strings, publisher-declared and unverified. Additive — absent is wire-identical to every package published before this field existed. `None` and `[]` are deliberately distinct wire states (unlike `entrypoints`, which collapses absent and empty). See [Executables](#executables). |

::: warning These changes affect existing packages
If you published packages before the visibility-default flip, their untagged env entries will no longer appear on the consumer surface. Add `"visibility": "public"` explicitly to vars that consumers should see.
:::

<!-- external -->
[json-schema]: https://json-schema.org/
[check-jsonschema]: https://github.com/python-jsonschema/check-jsonschema
[cmake-tll]: https://cmake.org/cmake/help/latest/command/target_link_libraries.html
[cmake-compile-defs]: https://cmake.org/cmake/help/latest/command/target_compile_definitions.html
[nix]: https://nixos.org/
[nix-propagated]: https://ryantm.github.io/nixpkgs/stdenv/stdenv/
[guix]: https://guix.gnu.org/
[guix-propagated]: https://guix.gnu.org/manual/en/html_node/package-Reference.html
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[glibc]: https://www.gnu.org/software/libc/
[musl]: https://musl.libc.org/
[bazel]: https://bazel.build/
[npm-bin]: https://docs.npmjs.com/cli/v11/configuring-npm/package-json#bin
[cargo-bin]: https://doc.rust-lang.org/cargo/reference/cargo-targets.html#binaries
[nixpkgs-main-program]: https://nixos.org/manual/nixpkgs/stable/#var-meta-mainProgram

<!-- schema -->
[schema-url]: /schemas/metadata/v1.json

<!-- in-depth -->
[exec-modes]: ../in-depth/environments.md#visibility-views
[env-composition]: ../in-depth/environments.md
[env-composition-edge-filter]: ../in-depth/environments.md#edge-filter
[in-depth-project-lock]: ../in-depth/project.md#lock
[in-depth-project-lock-format]: ../in-depth/project.md#lock-format

<!-- commands -->
[cmd-exec]: ./command-line.md#exec
[cmd-launcher-exec]: ./command-line.md#launcher-exec
[cmd-env]: ./command-line.md#env
[cmd-env-root]: ./command-line.md#env-root
[cmd-package-env]: ./command-line.md#package-env
[cmd-package-create]: ./command-line.md#package-create
[cmd-package-push]: ./command-line.md#package-push
[cmd-package-push-layout]: ./command-line.md#package-push-layout
[cmd-package-install]: ./command-line.md#package-install

<!-- reference -->
[reference-platforms]: ./platforms.md
[reference-platforms-compatibility]: ./platforms.md#compatibility

<!-- guide -->
[authoring-migration]: ../authoring/env-surface.md#migrating
[authoring-libc]: ../authoring/multi-platform.md#libc
[authoring-dependency-platforms]: ../authoring/multi-platform.md#dependency-platforms

<!-- internal -->
[fs-objects]: ../user-guide.md#file-structure-packages
[ug-dependencies]: ../user-guide.md#dependencies
