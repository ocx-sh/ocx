---
outline: deep
---
# Migration Patterns

Most tools you will publish to OCX already ship binaries somewhere — a [Homebrew formula][homebrew], a [GitHub Release][gh-releases] zip, an [APT repository][apt-repos], or a vendor's signed installer. The publisher work is reformatting those artefacts into [OCI image manifests][oci-image-spec] without re-bundling content. This page covers the recurring migration patterns and points at the [`ocx_mirror`][in-tree-mirror-spec] tool that automates them.

## The `ocx_mirror` Pipeline {#mirror}

[`ocx_mirror`][in-tree-mirror-spec] is the automation backbone every in-tree mirror uses to keep public OCX packages in sync with their upstreams. It reads a YAML spec describing where to find upstream releases, which assets to grab per platform, and how to map them onto OCX metadata, then drives a download → bundle → push pipeline through the same `ocx_lib` publisher API the `ocx package` commands use.

The advantage over running the commands manually: the pipeline batches `--cascade` resolution across platforms in one publisher invocation, runs the upstream → archive transformation deterministically, and exposes a `versions.new_per_run` knob (`Option<usize>`, unlimited when omitted) so a single CI run doesn't spam the registry with a backlog of upstream releases. The cross-release [layer-reuse][authoring-layer-reuse] pattern is a hand-publisher technique — `ocx_mirror` always uploads each platform's archive afresh.

A minimal spec that wraps a single upstream tool:

```yaml
name: mytool
target:
  registry: ocx.sh
  repository: mytool

source:
  type: github_release
  owner: example-org
  repo: mytool
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"

assets:
  linux/amd64:
    - "mytool-.*-linux-x86_64\\.tar\\.gz"
  linux/arm64:
    - "mytool-.*-linux-aarch64\\.tar\\.gz"

asset_type:
  type: archive
  strip_components: 1

metadata:
  default: metadata.json
```

`cascade` defaults to `true`; set `cascade: false` only for repositories that should not advance rolling tags. A non-trivial worked example covers the multi-platform asset matrix, version-range filtering, and per-platform metadata overrides for a single upstream:

```yaml
name: cmake
target:
  registry: ocx.sh
  repository: cmake

source:
  type: github_release
  owner: Kitware
  repo: CMake
  tag_pattern: "^v(?P<version>\\d+\\.\\d+\\.\\d+)$"

# Multi-platform asset matrix: each platform lists one or more regexes;
# the union must resolve to exactly one asset filename per release.
assets:
  linux/amd64:
    - "cmake-.*-linux-x86_64\\.tar\\.gz"      # >= 3.20.0
    - "cmake-.*-Linux-x86_64\\.tar\\.gz"      # < 3.20.0
  darwin/arm64:
    - "cmake-.*-macos-universal\\.tar\\.gz"   # universal fat binary
  windows/amd64:
    - "cmake-.*-windows-x86_64\\.zip"         # >= 3.20.0
    - "cmake-.*-win64-x64\\.zip"              # < 3.20.0

asset_type:
  type: archive
  strip_components: 1

# Per-platform metadata overrides for a non-trivial upstream.
metadata:
  default: metadata.json
  platforms:
    darwin/arm64: metadata-darwin.json
    windows/amd64: metadata-windows.json

# Version-range filtering: only mirror releases >= 3.31.0, max 10 per run.
versions:
  min: "3.31.0"
  new_per_run: 10

skip_prereleases: true
cascade: true
```

## Repackaging GitHub Releases {#github-releases}

Most modern open-source binary tools ship via [GitHub Releases][gh-releases]. The mirror pipeline's `github_release` source type reads the release feed, downloads the assets matching the platform regex, and pushes them through the same `ocx_lib` publisher API used by `ocx package create` / `push`. Hand-driven publishers can mimic the same flow with a few [`curl`][curl] calls and a script — the wins from going through `ocx_mirror` are deterministic ordering across runs and the dedicated digest-verification slot (`verify.github_asset_digest`).

The platform regex matrix is the part publishers usually re-derive every time. Two [CMake][cmake]-style worked examples covering the asset-naming-changed-mid-release case:

```yaml
linux/amd64:
  - "cmake-.*-linux-x86_64\\.tar\\.gz"      # >= 3.20.0
  - "cmake-.*-Linux-x86_64\\.tar\\.gz"      # < 3.20.0
```

The pipeline applies every regex and requires the union of matches to resolve to exactly one asset filename per release; ambiguous matches abort with `Ambiguous`. Two regexes that match the same upstream filename are fine; two regexes that match different filenames are an error. See the CMake worked example [above](#mirror) for the full matrix.

## Repackaging Homebrew Formulae {#homebrew}

[Homebrew][homebrew] formulae are [Ruby][ruby] DSL programs, not declarative manifests — every formula encodes its own download URL, build steps, and install hook. There is no general-purpose "import a Homebrew formula" path. The migration pattern is to read the formula's `bottle do … root_url … sha256 …` block (for binary bottles) or the formula's `url` + `sha256` (for source releases), mirror those bytes into a `github_release`-shaped pipeline (or fetch directly), and write OCX `metadata.json` covering the env entries Homebrew would have set in its post-install script.

Binary Homebrew bottles unpack as `<formula>/<version>/bin/...`, so `strip_components: 2` matches the typical layout. Pair that with a `PATH` env entry marked `"visibility": "public"` (the default is `private`) so consumers actually see the binaries on PATH. Source-built Homebrew formulae do not migrate cleanly without rebuilding, since OCX is a binary package manager.

## Attaching Description Metadata {#describe}

Once the package is in the registry, publishers attach a README, logo, title, search keywords, and short description via [`ocx package describe`][cmd-package-describe]. The fields land as an OCI image manifest pushed under the dedicated internal tag `__ocx.desc`, so the description travels with every registry export and powers the [package catalog][catalog]. The round-trip is cheap to verify: [`ocx package info`][cmd-package-info] reads the same fields back, with `--save-readme` writing the published markdown to disk for diffing.

```sh
ocx package describe \
  --readme README.md \
  --logo logo.svg \
  --title "mytool" \
  --description "A small example tool" \
  --keywords cli,linting \
  mytool
ocx package info mytool
```

<Terminal src="/casts/authoring/package-describe.cast" title="Attaching package descriptions and reading them back" collapsed />

::: tip Re-publish runs do not auto-refresh descriptions
The `ocx_mirror` pipeline pushes packages, not descriptions — `ocx package describe` is a separate hand-driven step. Re-run it whenever your README or logo changes; otherwise the registry keeps the previously published `__ocx.desc` manifest.
:::

## See Also {#see-also}

- [`ocx package describe` reference][cmd-package-describe]
- [`ocx package info` reference][cmd-package-info]
- [Building & pushing][authoring-building-pushing] — when running mirror commands by hand

<!-- external -->
[gh-releases]: https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
[homebrew]: https://brew.sh/
[ruby]: https://www.ruby-lang.org/
[curl]: https://curl.se/
[cmake]: https://cmake.org/
[apt-repos]: https://wiki.debian.org/DebianRepository/Format
[oci-image-spec]: https://github.com/opencontainers/image-spec
[in-tree-mirror-spec]: https://github.com/ocx-sh/ocx/tree/main/crates/ocx_mirror

<!-- commands -->
[cmd-package-describe]: ../reference/command-line.md#package-describe
[cmd-package-info]: ../reference/command-line.md#package-info

<!-- internal -->
[catalog]: ../catalog.md

<!-- authoring -->
[authoring-building-pushing]: ./building-pushing.md
[authoring-layer-reuse]: ./building-pushing.md#layer-reuse
