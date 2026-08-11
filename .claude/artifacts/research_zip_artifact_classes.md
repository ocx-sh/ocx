# Research: Which artifact classes actually benefit from a zip layer type

**Axis 3/3** for ocx-sh/ocx#183. Produced 2026-08-09 during `/hex-plan`
Discover+Research. Release-asset formats change — re-verify before citing in an
ADR older than ~6 months.

## Direct answer

The set of upstreams that (a) ship zip **and** (b) need zero install-time
relocation is small, and does **not** include the artifact class #183 leans on
(Python wheels — already excluded by the issue's own caveat). Two families
qualify:

1. **HashiCorp tools** — Terraform, and by the same tooling Vault, Consul, Nomad,
   Packer. Flat binary in a zip, on **all platforms including Linux/macOS**, not
   just Windows.
2. **protoc / protobuf** — `bin/<tool>` + `include/*.proto` flat in a zip, on
   **all platforms including Linux/macOS**.

Everything else that ships zip (CMake, Node.js, Go, ripgrep, Eclipse
Temurin/JDK) follows the classic split: `tar.gz` for Unix, zip for Windows only.
On Windows the exec-bit and symlink footguns below do not apply — but ocx already
covers those same tools footgun-free via `tar.gz` on Linux/macOS. Bazel and
kubectl ship raw binaries with no archive at all.

## 1. Asset-format survey

[CMake](https://cmake.org/download/) tar.gz Unix / zip Win ·
[Terraform](https://releases.hashicorp.com/terraform/1.13.0/) zip **all**
platforms, flat binary ·
[protoc](https://github.com/protocolbuffers/protobuf/releases) zip **all**
platforms, flat `bin/`+`include/` ·
[Gradle](https://services.gradle.org/distributions/) zip all platforms, single
leading dir · [Node](https://nodejs.org/download/release/v13.14.0/win-x64/), Go,
[ripgrep](https://github.com/BurntSushi/ripgrep/releases),
[Temurin](https://github.com/adoptium/temurin25-binaries/releases): tar.gz Unix /
zip Win · [Bazel](https://releases.bazel.build/6.5.0/release/index.html),
[kubectl](https://kubernetes.io/releases/download/): raw binary, no archive ·
[uv/ruff](https://github.com/astral-sh/uv/releases) (cargo-dist convention):
tar.gz Unix / zip Win.

## 2. "Zero relocation" ≠ "zero touch" — the exec bit is lost

Both qualifying families need no file moves — ocx's existing `strip`/`prefix`
layer layout already covers their shape. But both are confirmed to **lose the Unix
executable bit** in their own published zips, so ocx must still `chmod +x`
post-extract:

- [protocolbuffers/protobuf#10301](https://github.com/protocolbuffers/protobuf/issues/10301)
  — Google's own protoc zips ship `bin/protoc` without `+x`. Still open.
- [actions/toolkit#1722](https://github.com/actions/toolkit/issues/1722) —
  general "zip doesn't store Unix permissions".

Root cause is structural, not a one-off bug: Unix mode lives in the upper 16 bits
of the zip external-attributes field and is populated **only if the producer is
Unix-aware** — zero when built on a Windows CI runner
([dotnet/runtime#1548](https://github.com/dotnet/runtime/issues/1548),
[MS Learn zip/tar best practices](https://learn.microsoft.com/en-us/dotnet/standard/io/zip-tar-best-practices)).
Go's `archive/zip` behaves the same — Unix mode is opt-in via
`FileHeader.SetMode()` ([golang/go#36301](https://github.com/golang/go/issues/36301)).

**Planning consequence:** a zip layer cannot be extracted verbatim into a usable
tool tree. Some post-extract mode policy is mandatory, and it is a design decision
(which files get `+x`, and on whose say-so), not an implementation detail.

## 3. Symlinks are worse — an unofficial extension

Unix symlink-in-zip is an Info-ZIP-only convention (`S_IFLNK` in external attrs,
target path stored as file content):
[discuss.python.org](https://discuss.python.org/t/how-info-zip-represents-symlinks/4104).
OpenJDK's zipfs has no symlink support and unpacks them as regular files
([JDK-8268856](https://bugs.openjdk.org/browse/JDK-8268856)). Any
non-Info-ZIP-aware producer/consumer pair silently corrupts symlinks.

## 4. Supply-chain identity is not verified by digest equality

- **Homebrew** pins an explicit sha256 **in metadata**, checked independently of
  archive format ([Checksum Requirements](https://docs.brew.sh/Checksum-Requirements),
  [Security & Supply Chain](https://docs.brew.sh/Homebrew-Security-and-Supply-Chain)).
- **SLSA / Sigstore** verify `subject.digest` in a **recorded attestation** against
  the artifact ([cosign](https://blog.sigstore.dev/cosign-verify-bundles/)) — the
  same metadata-carried pattern.

Nobody relies on two independently-computed storage-layer digests (upstream CDN
vs OCI blob store) happening to match. That coincidence only holds if ocx stores
the layer **uncompressed** anyway, since gzip-wrapping — the normal OCI layer
convention — already breaks byte equality independent of the inner format.

## Recommendation

**Do not justify a new wire contract on the supply-chain-identity argument.** That
goal is better served by a vendor-checksum metadata field: format-agnostic, no new
contract.

The real win is ordinary and smaller: two vendor families ocx does not currently
mirror (HashiCorp, protoc), plus Windows-slice coverage of tools ocx already
mirrors via tar.gz for Unix. Treat #183 as ordinary **"add zip as a supported
archive format"** work — the same justification tar.gz support has — not as a
security or provenance feature.

Note that even then, "zero processing" is unachievable: ocx must fix up the exec
bit post-extract for both cross-platform-zip families.

## Sources

[Terraform releases](https://releases.hashicorp.com/terraform/1.13.0/) ·
[protobuf releases](https://github.com/protocolbuffers/protobuf/releases) ·
[protobuf#10301](https://github.com/protocolbuffers/protobuf/issues/10301) ·
[actions/toolkit#1722](https://github.com/actions/toolkit/issues/1722) ·
[dotnet/runtime#1548](https://github.com/dotnet/runtime/issues/1548) ·
[MS Learn zip/tar practices](https://learn.microsoft.com/en-us/dotnet/standard/io/zip-tar-best-practices) ·
[golang/go#36301](https://github.com/golang/go/issues/36301) ·
[Info-ZIP symlink thread](https://discuss.python.org/t/how-info-zip-represents-symlinks/4104) ·
[JDK-8268856](https://bugs.openjdk.org/browse/JDK-8268856) ·
[Homebrew Checksum Requirements](https://docs.brew.sh/Checksum-Requirements) ·
[Sigstore cosign verify](https://blog.sigstore.dev/cosign-verify-bundles/) ·
[CMake](https://cmake.org/download/) · [Gradle](https://services.gradle.org/distributions/) ·
[Temurin](https://github.com/adoptium/temurin25-binaries/releases) ·
[Bazel](https://releases.bazel.build/6.5.0/release/index.html) ·
[kubectl](https://kubernetes.io/releases/download/) ·
[uv releases](https://github.com/astral-sh/uv/releases)
