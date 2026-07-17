---
layout: doc
outline: deep
---
# Platforms {#platforms}

Every OCX package targets a platform — a specific OS/architecture pair, an OS/architecture pair
refined by CPU variant, a set of required OS capabilities, or `any` for platform-agnostic content.
OCX needs one string form for that value that works everywhere: the
`--platform` flag, [`ocx.lock`][in-depth-project-lock-format]'s per-platform digest map, and
dependency pin maps in [`metadata.json`][reference-metadata]. This page is the canonical
reference for that string and for the compatibility relation OCX evaluates it against.

## Grammar {#grammar}

A platform is either the literal `any`, or an `os`/`arch` pair optionally refined by a CPU
variant and a list of required OS features:

```
os/arch[/variant][+feature[,feature...]]      |      any
```

```text
platform      = "any" | specific ;
specific      = os "/" arch [ "/" variant ] [ "+" feature-list ] ;

os            = "linux" | "darwin" | "windows" ;
arch          = "amd64" | "arm64" ;

variant       = value ;                       (* e.g. "v7", "v8" *)
feature-list  = feature { "," feature } ;
feature       = value ;                       (* e.g. "libc.glibc" *)
```

| Example | Meaning |
|---|---|
| `linux/amd64` | Linux on x86-64, no refinements |
| `linux/arm/v7` | Linux on 32-bit ARM, [`variant`][oci-image-index] `v7` |
| `linux/amd64+libc.glibc` | Linux on x86-64, requires the `libc.glibc` [`os.features`][oci-image-index] tag |
| `linux/amd64+libc.glibc,libc.musl` | Linux on x86-64, requires both glibc and musl (a dual-libc override) |
| `linux/arm64/v8+libc.glibc` | Every field combined |
| `any` | Platform-agnostic — no OS, architecture, or feature requirement |

`/` introduces `variant`, and a single `+` introduces the comma-separated feature list — each
occupies a structurally distinct slot, so `linux/arm64/v8` (a variant) can never be confused with
a `+`-list. `os` and `arch` are closed sets (see the table above); adding a value to either
requires an OCX release. `any` carries no fields — `any+libc.glibc` and `any/amd64` are both
rejected as malformed.

This is the single grammar for every platform string surface in OCX: [`--platform`][cmd-platform-option],
[`ocx.lock`][in-depth-project-lock-format]'s `[tool.platforms]` keys, and the per-dependency
`platforms` pin map in the [authoring metadata sidecar][reference-per-platform-pins]. One publisher
libc tag needs one encoding everywhere it appears, not a different spelling per file.

### Escaping {#grammar-escaping}

`variant` and each feature value are registry-controlled strings — a publisher could in principle
choose a value containing `/`, `+`, or `,`. To keep the grammar unambiguous, OCX percent-escapes
the four structural characters wherever they appear inside a value:

| Character | Role | Escape |
|---|---|---|
| `%` | escape introducer | `%25` |
| `/` | `os`/`arch`/`variant` separator | `%2F` |
| `+` | feature-list introducer | `%2B` |
| `,` | feature separator | `%2C` |

Every other byte passes through verbatim, so ordinary values — `v7`, `v8`, `libc.glibc` — are
byte-identical to their raw form and never need escaping in practice. A `%` not followed by one
of the four two-character codes above is a parse error.

This makes the platform string an injective, round-tripping encoding of the underlying value:
parsing a platform string and re-rendering it always produces the identical string back. That
property is what lets the same string double as both the CLI flag value and a `BTreeMap` key in
`ocx.lock` and pin maps — no separate key-encoding format is needed.

## Compatibility {#compatibility}

Selecting the right manifest for a host is not a string comparison. A host that advertises
`linux/amd64+libc.glibc` should install a manifest published bare as `linux/amd64` (no libc
requirement declared), but a manifest published `linux/amd64+libc.musl` should not match that same
host at all — the strings differ, and the *relationship* between them determines the outcome, not
equality. OCX evaluates a directed relation, `is_compatible(required, offered)`, everywhere a
platform decision is made: resolving a fresh install against an [OCI Image Index][oci-image-index],
reading a locked digest out of `ocx.lock`, and pinning a dependency during
[`ocx package create`][cmd-package-create].

### The relation {#compatibility-relation}

`is_compatible(required, offered)` reads "does `offered` satisfy the requirement `required`?".
`required` is what the caller needs satisfied — the detected host, an explicit `--platform`
value, or a lock lookup key. `offered` is a candidate — an image-index child platform, or a
pin-map key.

The relation is not symmetric, and the asymmetry is deliberate:

- **An `any` offer satisfies every requirement.** A platform-agnostic artifact (a shell script, a
  JAR) runs anywhere, so it matches any host.
- **An `any` requirement is satisfied only by an `any` offer.** A host OCX could not detect (an
  unsupported OS, a failed probe) can run platform-agnostic content, never a `Specific` binary —
  there is nothing to check compatibility against.
- **`os.features` inverts.** A feature value on the *offer* names a capability the binary
  *demands of the host*, so the direction flips relative to the required/offered naming: every
  feature the offer declares must be present in what the requirement offers as host capabilities
  (`offered.os_features ⊆ required.os_features`). `variant` does not invert — it is strict
  equality, gated on the offer declaring a value at all. An offer that leaves `variant` unset
  imposes no constraint.

| Required | Offered | Compatible? | Why |
|---|---|---|---|
| `linux/amd64` | `linux/amd64` | Yes | Exact match, no refinements |
| `linux/amd64` | `any` | Yes | An `any` offer satisfies every requirement |
| `any` | `linux/amd64` | No | An `any` requirement is satisfied only by an `any` offer |
| `linux/arm64/v8` | `linux/arm64` | Yes | Offer leaves `variant` unset — unconstrained |
| `linux/arm64` | `linux/arm64/v8` | No | Offer declares `variant`, required has none |
| `linux/amd64+libc.glibc` | `linux/amd64` | Yes | `{} ⊆ {glibc}` — a bare offer runs on a glibc host |
| `linux/amd64+libc.glibc` | `linux/amd64+libc.musl` | No | `{musl} ⊄ {glibc}` |
| `linux/amd64+libc.glibc,libc.musl` | `linux/amd64+libc.glibc` | Yes | `{glibc} ⊆ {glibc,musl}` — a dual-libc host runs a single-libc offer |

The [libc differentiation][authoring-libc] page walks through the dual-libc case end to end,
including how OCX detects which libc families a host provides.

### Scoring {#compatibility-scoring}

An image index can offer more than one compatible manifest for the same requirement — a bare
`linux/amd64` entry alongside a `linux/amd64+libc.glibc` entry, for instance. OCX picks the
**most specific** compatible offer, ranked lexicographically:

1. **`Specific` outranks `any`.** A `linux/amd64` offer beats a co-present `any` offer for a
   `linux/amd64` requirement — a targeted binary is always preferred over a platform-agnostic
   fallback when both are available.
2. **Among `Specific` offers, more matched features win.** A `linux/amd64+libc.glibc` offer beats
   a bare `linux/amd64` offer for a glibc host — the more specific declaration wins.
3. **A tie is ambiguous, never guessed.** Two offers scoring equally — for example a dual-libc
   host against separate `libc.glibc` and `libc.musl` entries, each declaring exactly one feature
   — cannot be ranked against each other. OCX reports the ambiguity (exit [`65`][cmd-exit-codes])
   instead of picking one; the caller disambiguates with an explicit `--platform`.

This single relation and scoring rule is what makes fresh installs, `ocx.lock` reads, and
dependency pinning agree: the same host or requirement always resolves to the same answer
regardless of which of the three call sites is asking.

## Shared Digests {#shared-digests}

Two distinct platform keys pointing at the same manifest digest is legitimate and expected — OCX
never rejects it. The canonical case is [Rosetta 2][rosetta-2]: a publisher who ships only a
`darwin/amd64` build can also list that identical manifest under `darwin/arm64`, so Apple
Silicon hosts install and run it through Rosetta's x86-64 translation instead of failing to find
an arm64 build at all. The two platform keys are genuinely different — one names the binary's
native architecture, the other names a host that can run it via translation — but both may point
at the same bytes.

::: info Why this isn't a bug
It is tempting to think two keys with the same digest indicates a publishing mistake — surely a
platform-specific build should have a unique digest? [OCI Image Index][oci-image-index]
entries are content-addressed by design: the digest identifies the *bytes*, and nothing in the
spec (or in OCX) requires a one-to-one mapping between platform descriptors and content. A
generic OCI client that deduplicates identical layers across platform entries relies on exactly
this property.
:::

## See Also {#see-also}

- [Multi-Platform Packages][authoring-multi-platform] — the publisher workflow: building,
  pushing, and libc differentiation
- [`--platform` flag reference][cmd-platform-option] — the CLI surface for every command that
  resolves against a platform
- [Per-Platform Pins][reference-per-platform-pins] — the dependency pin map that uses this same
  grammar
- [Lock format][in-depth-project-lock-format] — the `ocx.lock` `[tool.platforms]` table

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[rosetta-2]: https://support.apple.com/en-us/HT211861

<!-- commands -->
[cmd-platform-option]: ./command-line.md#package-install
[cmd-package-create]: ./command-line.md#package-create
[cmd-exit-codes]: ./command-line.md#exit-codes

<!-- reference -->
[reference-metadata]: ./metadata.md
[reference-per-platform-pins]: ./metadata.md#dependencies-per-platform-pins

<!-- in-depth -->
[in-depth-project-lock-format]: ../in-depth/project.md#lock-format

<!-- authoring -->
[authoring-multi-platform]: ../authoring/multi-platform.md
[authoring-libc]: ../authoring/multi-platform.md#libc
