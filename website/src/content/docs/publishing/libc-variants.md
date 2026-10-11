---
title: "Publish glibc and musl variants"
description: "Publish one package whose Linux builds differ by C library, and let OCX pick the right build on each host."
---
<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

By the end of this page, one tag such as `acme/mytool:1.0.0` carries a glibc build and a musl build of your Linux binary. OCX installs the one that fits each host. You need a package you can already [push per platform][multi-platform-pattern] and a binary built once against each libc.

## Declare the libc {#libc}

Linux ships two major libc implementations. The GNU C Library ([glibc][glibc]) serves Ubuntu, Debian, Fedora and most distributions. Alpine Linux and similar minimal environments use [musl][musl].

A binary compiled against glibc needs glibc at runtime, and the failure when it is absent is not helpful. A host with too old a glibc reports `version 'GLIBC_X.Y' not found`. A host with no glibc at all, such as Alpine, reports `No such file or directory` for a file that is plainly there. The kernel cannot find the ELF interpreter the binary names.

Declaring the libc avoids this, and OCX does not let you skip it. An undeclared libc claims the build runs everywhere. So `ocx package create` reads the packaged binaries and refuses a Linux platform whose `os.features` do not cover what they need. [Checking the declared libc][cmd-package-create-libc-check] covers that check and its `--no-libc-lint` escape hatch.

The mechanism is the [OCI `os.features` field][oci-image-index]. Mark each manifest entry `libc.glibc` or `libc.musl` and OCX's index resolution selects the entry that matches the installing host.

### Publish both variants {#libc-publishing}

The mirror tool's YAML config supports an object form for asset entries. It adds `os_features` and an optional `platform` override, and two entries can share the same `(os, arch)` pair when their `os_features` differ.

```yaml
assets:
  # string form, no libc tag: matches every Linux host (static build)
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

The YAML key (`linux/amd64-glibc`) is a free-form identifier. When it differs from the real platform, the `platform` field gives the canonical OCI platform string. Two entries with the same `platform` but different `os_features` produce two separate image index entries under one tag.

The published image index then carries the `os.features` array in each platform descriptor:

```json
{
  "platform": {
    "architecture": "amd64",
    "os": "linux",
    "os.features": ["libc.glibc"]
  }
}
```

Without the mirror tool, declare the same `os_features` on the `--platform` value of [`ocx package push`][cmd-package-push]. Append `+libc.glibc` or `+libc.musl` to the `os/arch` value on both `create` and `push`, following the [per-platform push pattern][multi-platform-pattern]. `create` checks the declared libc against the binaries in each directory.

```sh
ocx package create build-glibc -m metadata.json -p linux/amd64+libc.glibc -o mytool-1.0.0-linux-amd64-glibc.tar.xz
ocx package create build-musl -m metadata.json -p linux/amd64+libc.musl -o mytool-1.0.0-linux-amd64-musl.tar.xz
ocx package push --cascade -p linux/amd64+libc.glibc -i acme/mytool:1.0.0 mytool-1.0.0-linux-amd64-glibc.tar.xz
ocx package push --cascade -p linux/amd64+libc.musl -i acme/mytool:1.0.0 mytool-1.0.0-linux-amd64-musl.tar.xz
```

Both pushes share the tag and the `(os, arch)` pair. They merge into one image index as two entries that differ only in `os_features`, the same result the mirror YAML produces.

:::caution[Generic OCI clients ignore `os.features`]
Generic OCI clients such as [`docker`][docker], [`podman`][podman], [ORAS][oras] and [`crane`][crane] resolve a multi-platform index through [containerd-style platform matchers][containerd-platforms]. Those key on `os`, `architecture` and `variant`, not on `os.features`, so a dual-libc index is ambiguous to them. Which entry they take is implementation-defined, typically the first match in index order, and it may be the wrong libc. When third-party tooling pulls these tags directly, pin the exact per-platform manifest by digest. Or route the pull through ocx so the `os.features` subset match applies.
:::

### Static binaries as the universal fallback {#libc-static}

Publish a statically linked binary with **no** `os_features` declaration. A musl-static build with no runtime libc dependency is the usual case.

```yaml
"linux/amd64":
  pattern: mytool-{version}-x86_64-unknown-linux-musl-static.tar.zst
  # no os_features: matches every Linux host regardless of detected libc
```

An empty `os_features` set is a subset of every host's features. The entry therefore matches a glibc host, a musl host, and a host where libc could not be detected. Prefer this idiom when you control the build and can produce a fully static binary.

### Auto-resolution {#libc-auto-resolution}

When an index carries both a `libc.glibc` entry and a `libc.musl` entry under one tag, consumers need no flags. `ocx package install acme/mytool:1.0.0` detects the host's libc family at startup, then applies subset matching. The entry whose `os.features` are a subset of the host's detected features wins.

A pure glibc host (Ubuntu, Fedora, Debian) picks the glibc build. A pure musl host ([Alpine Linux][alpine]) picks the musl build.

A host that provides both loaders advertises `["libc.glibc", "libc.musl"]`. Ubuntu with [`musl-tools`][musl-tools] installed and a multi-target CI runner are examples. Both entries match, and each declares exactly one `os_features` value, so their specificity is equal. Install fails with exit [`65`][exit-codes] and an ambiguous-selection error that lists the conflicting variants.

Disambiguate with `--platform linux/amd64+libc.glibc` or `--platform linux/amd64+libc.musl`. The suffix is appended to the `os/arch` value and overrides the host's detected feature set for that invocation only. Use it to install the musl build on a glibc host for testing.

### How OCX selects at install time {#libc-selection}

On a Linux host, OCX reads the loader path from a present system binary and identifies each loader by its `--version` banner. It enumerates every libc family the host provides, so a host may advertise more than one. The probe runs once per process.

An index entry matches when every `os_features` value it declares is present in the host's detected features. A host where detection failed matches only entries with no `os_features`. When several entries match, OCX picks the most specific, and a tie is ambiguous and never guessed. The [platforms reference][reference-platforms-compatibility] carries the full relation and ranking.

Detection on non-FHS hosts, on NixOS without nix-ld and on Alpine with gcompat has its own symptoms and fixes in [Platform troubleshooting][troubleshooting-platforms]. A gcompat host is a musl host, so it installs the `libc.musl` variant.

### Diagnose selection {#libc-debug}

[`ocx about`][cmd-about] reports the detected libc family and the host platform that index selection resolves against. Run it to confirm what OCX sees before you publish or troubleshoot a failed install.

```text
...
Platform   linux/amd64
Libc       libc.glibc
...
```

When the host provides more than one libc family, the `Libc` row lists each one, for example `Libc       libc.glibc, libc.musl`. When libc detection failed, the `Libc` row is absent. The host still resolves as the bare `os/arch` platform with no declared features, so install matches only manifests declaring no `os_features`.

For machine-readable output use `ocx --format json about`. It includes a `libc` array, empty when undetected, and a `features` array of every host `os.features` tag.

## See also {#see-also}

- [Publish for several platforms][multi-platform]: the per-platform push pattern this page builds on
- [Platforms reference][reference-platforms]: the grammar and the compatibility relation

[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[glibc]: https://www.gnu.org/software/libc/
[musl]: https://musl.libc.org/
[alpine]: https://www.alpinelinux.org/
[musl-tools]: https://packages.ubuntu.com/search?keywords=musl-tools
[oras]: https://oras.land/
[docker]: https://docs.docker.com/reference/cli/docker/manifest/
[podman]: https://podman.io/
[crane]: https://github.com/google/go-containerregistry/blob/main/cmd/crane/README.md
[containerd-platforms]: https://github.com/containerd/platforms

[exit-codes]: ../reference/exit-codes.md
[troubleshooting-platforms]: ../troubleshooting/platforms.mdx
[cmd-package-create-libc-check]: ./build-and-push.mdx#create-libc-check
[cmd-package-push]: ../reference/command-line/package-publishing.md#package-push
[cmd-about]: ../reference/command-line/commands.md#about
[reference-platforms]: ../reference/platforms.md
[reference-platforms-compatibility]: ../reference/platforms.md#compatibility
[multi-platform]: ./multi-platform.mdx
[multi-platform-pattern]: ./multi-platform.mdx#pattern
