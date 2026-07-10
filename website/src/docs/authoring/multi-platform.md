---
outline: deep
---
# Multi-Platform Packages

Most binary tools ship per-platform builds — different bytes for Linux/amd64, Linux/arm64, Darwin/arm64, Windows/amd64. The naive distribution approach is one tag per platform: `mytool:1.0.0-linux-amd64`, `mytool:1.0.0-darwin-arm64`. That works, but it pushes the platform-resolution problem onto every consumer's install script. OCX uses [OCI Image Indexes][oci-image-index] instead — one tag, multiple manifests, OCX picks the right one at install time based on the consumer's platform.

This page covers the publisher view: how to assemble a multi-platform package, how `ocx package push` builds the index, and the digest-stability properties you can rely on.

## One Tag, Many Manifests {#concept}

An [OCI Image Index][oci-image-index] is a manifest of manifests — a single descriptor that points at one image manifest per platform. When a consumer runs `ocx package install mytool:1.0.0`, OCX fetches the index, finds the manifest matching the consumer's platform, and pulls only that manifest's layers. No conditional logic in install scripts, no platform-suffixed tags to keep in sync.

The publisher equivalent of "build for amd64, then arm64, then push the index" collapses to: push each platform separately under the same tag, and OCX assembles the index for you. When [`ocx package push`][cmd-package-push] sees a tag that already has a manifest, it merges the new platform into the existing index rather than replacing it. (`--new` on the first push tells the cascade resolver to skip that lookup; everywhere else it is a no-op.)

## The Per-Platform Push Pattern {#pattern}

The hand-publishing flow (the [`ocx_mirror`][mirror-pipeline] tool runs the same pattern from a YAML spec):

1. **Bundle each platform's content with [`ocx package create`][cmd-package-create].** Pass `-i mytool:1.0.0 -p <platform>` and let `-o .` infer the output name. Each create call drops a `<name>-<tag>-<os>-<arch>.tar.xz` archive (`<name>` is the OCI repository's last segment) plus a sibling `<…>-metadata.json` sidecar — the pair [`ocx package push`][cmd-package-push] picks up automatically. The full sidecar/inferred-name convention lives in [Bundle Anatomy → sidecars][authoring-bundle-sidecars].
2. **Push the first platform with `--new --cascade`.** `--cascade` keeps your rolling-tag aliases (`1.0`, `1`, `latest`) in sync; `--new` skips the existence lookup that cascade would otherwise issue against a tag that does not yet exist. Omit `-m`: push reads the sidecar next to the layer.
3. **Push subsequent platforms with `--cascade`** (no `--new`). OCX detects the existing manifest, merges the new platform into the image index, re-points the tag at the index digest, and updates each rolling alias.

```sh
ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
ocx package create build -i mytool:1.0.0 -p linux/arm64 -m metadata.json -o .

ocx package push -n -c -p linux/amd64 -i mytool:1.0.0 mytool-1.0.0-linux-amd64.tar.xz
ocx package push    -c -p linux/arm64 -i mytool:1.0.0 mytool-1.0.0-linux-arm64.tar.xz
```

After both pushes, `mytool:1.0.0` resolves to an image index with two platform descriptors, and any rolling aliases (`1.0`, `1`, `latest`) point at the same index digest. A consumer on Linux/arm64 fetches only the Linux/arm64 manifest's layers; an amd64 Linux runner fetches only the amd64 manifest's layers. Add `-p darwin/arm64`, `-p darwin/amd64`, or `-p windows/amd64` the same way for the rest of the matrix.

<Terminal src="/casts/authoring/package-multi-platform.cast" title="Publishing a multi-platform package" collapsed />

The recording also runs [`ocx index update`][cmd-index-update] and [`ocx package install`][cmd-install] after the second push so you can see the consumer side: a single tag, the right platform's layers fetched, the binary on the candidate symlink ready for `ocx package exec`.

## Use the Same Metadata Across Platforms {#metadata}

The default and recommended pattern: ship one `metadata.json` that covers every platform. Env entries, dependencies, entrypoints — all of them apply uniformly. The platform-specific bits live in the archive, not in the metadata.

When platforms genuinely diverge — say, Windows needs different env keys, or a platform has different entry-point names — the [`ocx_mirror`][in-tree-mirror-spec] spec accepts a per-platform metadata override. Hand-driven publishers can pass `--metadata <path>` per push and use a different file each time. In a spec, the `metadata` block names the default plus per-platform overrides:

```yaml
metadata:
  default: metadata.json
  platforms:
    darwin/amd64: metadata-darwin.json
    darwin/arm64: metadata-darwin.json
    windows/amd64: metadata-windows.json
    windows/arm64: metadata-windows.json
```

## The Image Index Is Stable {#stability}

Each per-platform manifest's digest depends only on its own bytes. Push the *same archive bytes* twice and the manifest digest is identical — no platform-side rebuild churn. (Re-running `ocx package create` produces a different archive on each invocation; see [Bundle anatomy → stable archives][authoring-bundle-anatomy-stable] for why.) The index manifest's digest is a function of its descriptors, so it changes when (and only when) you add a platform or push a new build for an existing platform. That stability is what lets [`--cascade`][authoring-building-pushing-cascade] work cleanly across multi-platform releases — the rolling tag points at the index, the index points at per-platform manifests, every layer caches independently.

## libc Differentiation {#libc}

Linux ships two major libc implementations: [glibc][glibc] (the GNU C Library, used by Ubuntu, Debian, Fedora, and most distributions) and [musl][musl] (used by Alpine Linux and similar minimal environments). A binary compiled against glibc requires glibc at runtime; the wrong libc produces a clean failure at loader time (`version 'GLIBC_X.Y' not found`) before any code runs.

If your tool ships both a glibc build and a musl build, you can publish both under the same tag and let OCX pick the right one automatically. The mechanism is the [OCI `os.features` field][oci-image-index]: mark each manifest entry with `libc.glibc` or `libc.musl` and OCX's index resolution selects the one that matches the installing host.

### Auto-resolution {#libc-auto-resolution}

When a package index carries both a `libc.glibc` entry and a `libc.musl` entry under the same tag, consumers need no special flags. `ocx package install mytool:1.0.0` detects the host's libc family at startup, then applies subset matching: the entry whose `os.features` are a subset of the host's detected features wins. A pure glibc host (Ubuntu, Fedora, Debian) picks the glibc build; a pure musl host ([Alpine Linux][alpine]) picks the musl build.

A host that provides both loaders — for example, Ubuntu with [`musl-tools`][musl-tools] installed, or a multi-target CI runner — advertises `["libc.glibc", "libc.musl"]` as its feature set. Both the `libc.glibc` entry and the `libc.musl` entry match the host's features, and each declares exactly one `os_features` value, so their specificity is equal. Equal-specificity matches produce `SelectResult::Ambiguous`, so install fails (exit [`65`][exit-codes]) with an ambiguous-selection error that lists the conflicting variants. Disambiguate by passing `--platform linux/amd64+libc.glibc` or `--platform linux/amd64+libc.musl`.

:::info Comparison: how other tools handle this
[cargo-binstall][cargo-binstall] and [uv][uv] detect the host libc family at install time, but encode it in download URLs or filenames — the publisher must maintain separate assets and the resolution is a name-lookup, not a metadata match. [uv][uv] shipped a `UV_LIBC` manual override in July 2025 specifically because single-pick detection fails on dual-libc hosts. [Alpine Linux][alpine], [Wolfi][wolfi], and [Chainguard][chainguard] use separate repositories or tag suffixes (`-musl`, `-static`) to distinguish variants. Container tooling ([ORAS][oras], `docker pull`) does not use `os.features` for libc differentiation. OCX models the host as a set of libc families and performs set-subset matching inside one OCI index — no separate repos, no tag suffixes, no manual overrides needed.
:::

To force a specific variant — for example to install the musl build on a glibc host for testing — pass `--platform linux/amd64+libc.musl`. The `+libc.musl` suffix is appended to the standard `os/arch` value and overrides the host's detected feature set for that invocation only.

### Publishing glibc and musl variants {#libc-publishing}

The mirror tool's YAML config supports an object form for asset entries that adds `os_features` and an optional `platform` override. Two entries can share the same `(os, arch)` pair when their `os_features` differ.

```yaml
assets:
  # legacy string form — no libc tag; matches every Linux host (static build)
  "darwin/arm64": mytool-.*-aarch64-apple-darwin\.tar\.zst

  # object form: declare libc.glibc on the glibc-linked build
  "linux/amd64-glibc":
    platform: linux/amd64
    pattern: mytool-.*-x86_64-unknown-linux-gnu\.tar\.zst
    os_features: [libc.glibc]

  # object form: declare libc.musl on the musl-linked build
  "linux/amd64-musl":
    platform: linux/amd64
    pattern: mytool-.*-x86_64-unknown-linux-musl\.tar\.zst
    os_features: [libc.musl]
```

The YAML key (`linux/amd64-glibc`) is a free-form identifier — when it differs from the actual platform, the `platform` field provides the canonical OCI platform string. Two entries with the same `platform` but different `os_features` produce two separate image index entries under the same tag.

After mirroring, the published image index carries the `os.features` array in each platform descriptor:

```json
{
  "platform": {
    "architecture": "amd64",
    "os": "linux",
    "os.features": ["libc.glibc"]
  }
}
```

Hand-driven publishers (not using the mirror tool) declare the same `os_features` directly on [`ocx package push`][cmd-package-push]'s `--platform` value — append `+libc.glibc` or `+libc.musl` to the standard `os/arch` value, following [the Per-Platform Push Pattern](#pattern):

```sh
ocx package push -n -c -p linux/amd64+libc.glibc -i mytool:1.0.0 mytool-1.0.0-linux-amd64-glibc.tar.xz
ocx package push    -c -p linux/amd64+libc.musl  -i mytool:1.0.0 mytool-1.0.0-linux-amd64-musl.tar.xz
```

Both pushes share the same tag and `(os, arch)`, so they merge into the same image index as two entries differing only in `os_features` — the same result the mirror YAML's object form produces.

:::warning Generic OCI clients ignore `os.features`
Generic OCI clients — [`docker`][docker], [`podman`][podman], [ORAS][oras], and [`crane`][crane] — resolve a multi-platform index through [containerd-style platform matchers][containerd-platforms] that key on `os`, `architecture`, and `variant`. They do not select on `os.features`, so a dual-libc index is ambiguous to them: which of the two entries they take is implementation-defined (typically the first match in index order), and it may be the wrong libc. When third-party tooling pulls these tags directly, pin the exact per-platform manifest by digest, or route the pull through ocx so the `os.features` subset match applies.
:::

### Static binaries as the universal fallback {#libc-static}

A statically linked binary (e.g. a musl-static build that has no runtime libc dependency) should be published with **no** `os_features` declaration:

```yaml
"linux/amd64":
  pattern: mytool-{version}-x86_64-unknown-linux-musl-static.tar.zst
  # no os_features — matches every Linux host regardless of detected libc
```

An empty `os_features` set is a subset of every host's features, so the entry matches a glibc host, a musl host, and a host where libc could not be detected. This is the "runs on every Linux host" idiom — prefer it when you control the build and can produce a fully static binary.

### How OCX selects at install time {#libc-selection}

When a consumer runs `ocx package install mytool:1.0.0` on a Linux host, OCX detects the libc family in two stages: *discover, then identify*. It first **discovers** where the host's dynamic loaders live — it reads the [`PT_INTERP`][pt-interp] (the embedded interpreter path) from an ordered allowlist of system binaries (`/usr/bin/env`, `/bin/sh`, `/bin/ls`), probing each in turn and stopping at the first that yields an interpreter path; that path names the host's exact native loader wherever it sits. It then scans the canonical loader directories for any additional loaders. It then **identifies** each discovered loader by its `--version` banner. It enumerates **all** libc families the host provides — a host may advertise more than one (e.g. glibc plus musl on a machine that has `musl-tools` installed). Detection is a one-time probe cached for the process lifetime.

Reading the loader path off a present binary rather than guessing fixed paths is what lets detection work on hosts that put the loader somewhere non-standard. [Gentoo Prefix][gentoo-prefix], Homebrew-on-Linux, and custom sysroots all resolve libc-aware. [NixOS][nixos] resolves libc-aware when [nix-ld][nix-ld] is active (nix-ld installs an FHS shim at the canonical loader path, which the probe picks up normally); without nix-ld, the probe binaries on NixOS are statically linked and carry no `PT_INTERP`, so detection falls back to an empty set — install then matches only entries with no `os_features`, and you can override with `--platform`.

The selection rule is subset semantics: an index entry matches when every `os_features` value the candidate declares is present in the host's detected features. A glibc host therefore picks the `libc.glibc` entry; a musl host picks `libc.musl`; a dual-libc host (detected features `["libc.glibc", "libc.musl"]`) matches both the `libc.glibc` and the `libc.musl` entry; a host where detection failed (a truly minimal container with no readable dynamic loader) matches only entries with no `os_features`.

:::info Alpine Linux with gcompat
[gcompat][gcompat] is a glibc compatibility layer for Alpine. When installed, it lets glibc-linked binaries run on a musl host. OCX does **not** treat a gcompat host as a glibc host — the probe reads what the dynamic linker identifies as, and on Alpine that is always the musl loader. A gcompat host installs the `libc.musl` variant, not `libc.glibc`. This is predictable and falsifiable: the loader tells the truth about what it is, not what it can emulate.
:::

### Diagnosing selection {#libc-debug}

[`ocx about`][cmd-about] reports the detected libc family and the supported-platform set that `Index::select` uses for resolution. Run it to confirm what OCX sees before publishing or troubleshooting a failed install.

```
...
Platform   linux/amd64, any
Libc       libc.glibc
...
```

When the host provides more than one libc family, the `Libc` row lists each one (e.g. `Libc       libc.glibc, libc.musl`). On a host where libc detection failed, the `Libc` row is absent and the supported set contains only the bare `os/arch` entries — install then falls back to entries with empty `os_features`. For machine-readable output use `ocx --format json about`, which includes a `libc` array (empty when undetected).

## Dependencies with Platform-Specific Builds {#dependency-platforms}

A package built once with `--platform any` — a shell script, a Python entry point, anything without native code of its own — can still depend on a package that ships different manifests per platform, such as the native binary the script wraps. A single dependency identifier can only carry one digest, so which platform's build does that digest name?

[`ocx package create --platform any`][cmd-package-create] answers this by resolving each unpinned dependency's advertised platforms into a per-dependency pin map instead of a single digest, and deriving your own package's target-platform set as the intersection of what every dependency actually covers — a package cannot claim to run on a platform one of its dependencies doesn't ship.

### Pin Maps and the Target-Platform Set {#dependency-platforms-target-set}

Write the dependency tag-only, same as any other:

```json
{ "dependencies": [{ "identifier": "ocx.sh/cmake:3.28" }] }
```

If `ocx.sh/cmake:3.28` ships `linux/amd64` and `darwin/arm64` manifests, `ocx package create -p any` rewrites the sidecar with a pin map and a bundle-level target set:

```json
{
  "dependencies": [
    {
      "identifier": "ocx.sh/cmake:3.28",
      "platforms": { "linux/amd64": "sha256:aaaa...", "darwin/arm64": "sha256:bbbb..." }
    }
  ],
  "platforms": ["linux/amd64", "darwin/arm64"]
}
```

A dependency that ships only a platform-agnostic build (its own tag resolves to a single `any` manifest) collapses to a plain digest pin instead — no map noise for the common case. When more than one dependency is platform-specific, the target set is the intersection across all of them: a dependency available only on `linux/amd64` limits the whole package to `linux/amd64`, no matter how broadly the others reach. An intersection with no member at all fails `create` (exit 65) rather than publish a package nobody can install.

[`ocx package push`][cmd-package-push] reads the embedded target set and decides the fan-out for you:

```sh
# Publishes one manifest per platform in the embedded set (linux/amd64 and darwin/arm64).
ocx package push -i mytool:1.0.0 mytool.tar.xz

# Narrows the push to the linux/amd64 member only.
ocx package push -p linux/amd64 -i mytool:1.0.0 mytool.tar.xz

# Fails (exit 64): windows/amd64 is not a member of the embedded set.
ocx package push -p windows/amd64 -i mytool:1.0.0 mytool.tar.xz
```

A sidecar with no embedded target set — one built without `--platform`, or hand-authored with every dependency already pinned — keeps requiring `--platform` on push, exactly as [described earlier][authoring-building-pushing-dependency-pins].

### Snapshot Semantics {#dependency-platforms-snapshot}

A pin map is a snapshot of the dependency's platform coverage at `create` time, not a live query. If `ocx.sh/cmake:3.28`'s publisher later adds a `windows/amd64` manifest, your already-published package does not retroactively gain `windows/amd64` support — the pin map still lists only the platforms that existed when you ran `create`. Re-run `ocx package create --platform any` against a refreshed index, then `ocx package push`, to widen your package's own target set to match.

### libc-Tagged Dependencies {#dependency-platforms-libc}

The three-tier lookup that projects a pin map onto a specific platform — exact match, then the same platform with `os.features` stripped, then `any` — treats a plain `linux/amd64` dependency pin as covering a libc-tagged package platform like `linux/amd64+libc.glibc`, but not the reverse: a dependency pinned only at a `libc.glibc`-tagged key does not cover the plain `linux/amd64` platform. This mirrors the fail-closed `can_run` subset rule [described above][libc-selection] — a consumer that only advertises the bare platform should not silently receive a build resolved against a libc it never declared.

::: info Two encodings, one platform
The pin map's keys use the same key format as [`ocx.lock`'s per-platform table][in-depth-project-lock-format] — `;osf=libc.glibc` — while the bundle-level `platforms` target set (and the `--platform` flag itself) use the `+libc.glibc` form seen throughout this page. Both name the same platform; see the [Per-Platform Pins reference][reference-per-platform-pins] for the full encoding rule.
:::

## See Also {#see-also}

- [`ocx about` reference][cmd-about]
- [`ocx package push` reference][cmd-package-push]
- [Storage in depth — multi-layer packages][in-depth-storage-multi-layer]
- [Versioning in depth — platforms][in-depth-versioning-platforms]
- [Building & pushing][authoring-building-pushing] — cascade, layer reuse, BYO archives, dependency pin resolution
- [Declaring dependencies][authoring-dependencies] — when to depend, visibility, `name` overrides
- [Per-Platform Pins reference][reference-per-platform-pins] — pin map shape, lookup rule, target-set derivation
- [Migration patterns][authoring-migration] — `ocx_mirror` per-platform spec

<!-- external -->
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[in-tree-mirror-spec]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror
[glibc]: https://www.gnu.org/software/libc/
[musl]: https://musl.libc.org/
[gcompat]: https://gitlab.alpinelinux.org/alpine/gcompat
[alpine]: https://www.alpinelinux.org/
[pt-interp]: https://refspecs.linuxfoundation.org/elf/elf.pdf
[nixos]: https://nixos.org/
[nix-ld]: https://github.com/nix-community/nix-ld
[gentoo-prefix]: https://wiki.gentoo.org/wiki/Project:Prefix
[musl-tools]: https://packages.ubuntu.com/search?keywords=musl-tools
[cargo-binstall]: https://github.com/cargo-bins/cargo-binstall
[uv]: https://github.com/astral-sh/uv
[wolfi]: https://wolfi.dev/
[chainguard]: https://www.chainguard.dev/
[oras]: https://oras.land/
[docker]: https://docs.docker.com/reference/cli/docker/manifest/
[podman]: https://podman.io/
[crane]: https://github.com/google/go-containerregistry/blob/main/cmd/crane/README.md
[containerd-platforms]: https://github.com/containerd/platforms

<!-- commands -->
[exit-codes]: ../reference/command-line.md#exit-codes
[cmd-package-create]: ../reference/command-line.md#package-create
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-index-update]: ../reference/command-line.md#index-update
[cmd-install]: ../reference/command-line.md#package-install
[cmd-about]: ../reference/command-line.md#about

<!-- reference -->
[reference-per-platform-pins]: ../reference/metadata.md#dependencies-per-platform-pins

<!-- in-page -->
[libc-selection]: #libc-selection

<!-- in-depth -->
[in-depth-storage-multi-layer]: ../in-depth/storage.md#multi-layer
[in-depth-versioning-platforms]: ../in-depth/versioning.md#platforms
[in-depth-project-lock-format]: ../in-depth/project.md#lock-format

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-building-pushing-cascade]: ./building-pushing.md#cascade
[authoring-building-pushing-dependency-pins]: ./building-pushing.md#dependency-pins
[authoring-bundle-anatomy-stable]: ./bundle-anatomy.md#stable
[authoring-dependencies]: ./dependencies.md
[authoring-migration]: ./migration.md
[authoring-bundle-sidecars]: ./bundle-anatomy.md#sidecars

<!-- mirror pipeline -->
[mirror-pipeline]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror
