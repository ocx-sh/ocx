---
title: "Platforms"
---
<!-- doc_type: reference -->

A platform is one string that names an OS/architecture pair, optionally refined by CPU variant and required OS features, or `any` for platform-agnostic content. The same string is the `--platform` value, a key in [`ocx.lock`][in-depth-project-lock-format]'s per-platform digest map and a field in the build receipt [`ocx package create`][cmd-package-create] writes beside a bundle.

## Grammar {#grammar}

A platform is the literal `any`, or an `os`/`arch` pair optionally refined by a CPU variant and a list of required OS features. `os` and `arch` are closed sets: adding a value to either requires an OCX release.

```text
platform      = "any" | specific ;
specific      = os "/" arch [ "/" variant ] [ "+" feature-list ] ;

os            = "linux" | "darwin" | "windows" | "wasip1" | "wasip2" ;
arch          = "amd64" | "arm64" | "wasm" ;

variant       = value ;                       (* e.g. "v7", "v8" *)
feature-list  = feature { "," feature } ;
feature       = value ;                       (* e.g. "libc.glibc" *)
```

- `/` introduces `variant` and a single `+` introduces the comma-separated feature list, so `linux/arm64/v8` (a variant) is never confused with a `+`-list.
- `any` carries no fields: `any+libc.glibc` and `any/amd64` are rejected as malformed.

| Example | Meaning |
|---|---|
| `linux/amd64` | Linux on x86-64, no refinements |
| `linux/arm/v7` | Linux on 32-bit ARM, [`variant`][oci-image-index] `v7` |
| `linux/amd64+libc.glibc` | Linux on x86-64, requires the `libc.glibc` [`os.features`][oci-image-index] tag |
| `linux/amd64+libc.glibc,libc.musl` | Linux on x86-64, requires both glibc and musl (a dual-libc override) |
| `linux/arm64/v8+libc.glibc` | Every field combined |
| `wasip1/wasm` | A WebAssembly module targeting the WASI 0.1 ABI |
| `any` | Platform-agnostic: no OS, architecture, or feature requirement |

## Supported pairs {#supported-pairs}

Only the pairs below parse, because a valid `os` and a valid `arch` do not always name an artifact (`linux/wasm` and `wasip1/amd64` describe nothing). A pairing outside the list is refused while parsing, before resolution is attempted, and the refusal lists these pairs.

| Platform | Tier |
|---|---|
| `linux/amd64` | Native |
| `linux/arm64` | Native |
| `darwin/amd64` | Native |
| `darwin/arm64` | Native |
| `windows/amd64` | Native |
| `windows/arm64` | Native |
| `wasip1/wasm` | Wasm |
| `wasip2/wasm` | Wasm |

- Native: pairs OCX itself runs on and CI tests against. Host detection can produce any of them, and an omitted `--platform` resolves to one.
- Wasm: distribution labels only. No host reports `wasip1` or `wasip2`, so they are reachable only through an explicit `--platform`. See [publishing for wasm][authoring-multi-platform-wasm].
- Surfaces: the one grammar for [`--platform`][cmd-platform-option], the `[tool.platforms]` keys in [`ocx.lock`][in-depth-project-lock-format] and the [`ocx package create`][cmd-package-create] build receipt.

### Escaping {#grammar-escaping}

`variant` and each feature value are registry-controlled strings, so the four structural characters are percent-escaped wherever they appear inside a value. Every other byte passes through verbatim, and ordinary values such as `v7` and `libc.glibc` never need escaping.

| Character | Role | Escape |
|---|---|---|
| `%` | escape introducer | `%25` |
| `/` | `os`/`arch`/`variant` separator | `%2F` |
| `+` | feature-list introducer | `%2B` |
| `,` | feature separator | `%2C` |

- A `%` not followed by one of the four codes is a parse error.
- The encoding round-trips: parsing a platform string and re-rendering it yields the identical string. It serves as both the CLI flag value and a `BTreeMap` key in `ocx.lock`.

## Compatibility {#compatibility}

OCX evaluates the directed relation `is_compatible(required, offered)` wherever a platform decision is made. Those places are resolving an install against an [OCI Image Index][oci-image-index], reading a locked digest out of `ocx.lock` and pinning a dependency during [`ocx package create`][cmd-package-create].

### The relation {#compatibility-relation}

`is_compatible(required, offered)` reads "does `offered` satisfy `required`?", where `required` is the detected host, an explicit `--platform` value or a lock lookup key and `offered` is an image-index child platform. The relation is not symmetric.

- An `any` offer satisfies every requirement.
- An `any` requirement is satisfied only by an `any` offer.
- `variant` is strict equality, gated on the offer declaring a value. An offer that leaves `variant` unset imposes no constraint.
- `os.features` inverts: every feature the offer declares must be present in the requirement's host capabilities (`offered.os_features ⊆ required.os_features`), so an empty feature set on the offer matches every host.

| Required | Offered | Compatible? | Why |
|---|---|---|---|
| `linux/amd64` | `linux/amd64` | Yes | Exact match, no refinements |
| `linux/amd64` | `any` | Yes | An `any` offer satisfies every requirement |
| `any` | `linux/amd64` | No | An `any` requirement is satisfied only by an `any` offer |
| `linux/arm64/v8` | `linux/arm64` | Yes | Offer leaves `variant` unset, so it is unconstrained |
| `linux/arm64` | `linux/arm64/v8` | No | Offer declares `variant`, required has none |
| `linux/amd64+libc.glibc` | `linux/amd64` | Yes | `{} ⊆ {glibc}`: a bare offer runs on a glibc host |
| `linux/amd64+libc.glibc` | `linux/amd64+libc.musl` | No | `{musl} ⊄ {glibc}` |
| `linux/amd64+libc.glibc,libc.musl` | `linux/amd64+libc.glibc` | Yes | `{glibc} ⊆ {glibc,musl}`: a dual-libc host runs a single-libc offer |

An empty `os.features` is a positive claim: the artifact demands nothing of the host, so every host matches. Omitting a feature is never neutral.

- [`ocx package create`][cmd-package-create-libc-check] enforces this for the libc namespace on Linux targets.
- It reads the packaged binaries' dynamic loader and refuses a platform whose `os.features` do not cover what they need.

See [libc selection][authoring-libc-selection].

### Scoring {#compatibility-scoring}

When an image index offers several compatible manifests for one requirement, OCX picks the most specific, ranked lexicographically by these rules.

1. `Specific` outranks `any`: a `linux/amd64` offer beats a co-present `any` offer for a `linux/amd64` requirement.
2. Among `Specific` offers, more matched features win: `linux/amd64+libc.glibc` beats a bare `linux/amd64` for a glibc host.
3. A tie is ambiguous and never guessed: OCX reports it with exit [`65`][cmd-exit-codes], and the caller disambiguates with an explicit `--platform`.

## See Also {#see-also}

- [Multi-Platform Packages][authoring-multi-platform]
- [`--platform` flag reference][cmd-platform-option]
- [Dependencies][reference-dependencies]
- [Lock format][in-depth-project-lock-format]

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md

<!-- commands -->
[cmd-platform-option]: ./command-line/package-consuming.md#package-install
[cmd-package-create]: ./command-line/package-publishing.md#package-create
[cmd-exit-codes]: ./exit-codes.md

<!-- reference -->
[reference-dependencies]: ./metadata/dependencies.md#dependencies

<!-- in-depth -->
[in-depth-project-lock-format]: ../explanation/project-toolchain.md#lock-format

<!-- authoring -->
[authoring-multi-platform]: ../publishing/multi-platform.mdx
[authoring-libc-selection]: ../publishing/libc-variants.md#libc-selection
[authoring-multi-platform-wasm]: ../publishing/multi-platform.mdx#wasm
[cmd-package-create-libc-check]: ../publishing/build-and-push.mdx#create-libc-check
