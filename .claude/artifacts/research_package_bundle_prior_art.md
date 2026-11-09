# Research: Hermetic prebuilt toolchains in Bazel and OCI-layout bundles — prior art

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (hex-discuss entry wave, prior-art lane)
**Expires:** 2027-05-08
**Coverage gaps:** oras cp docs, regctl docs, toolchains_llvm / hermetic_cc_toolchain READMEs, rules_python, rules_go, bazel-lib, Helm/Flux/Homebrew not fetched — search snippets or unresearched.

## Direct Answer

Rulesets that keep tools in an external store outside Bazel (rules_nixpkgs) are RBE-hostile unless the store is shared with executors. Rulesets that fetch into the output base by digest (rules_oci) inherit Bazel's offline tooling. OCI image layout has no standard home for referrers, so signatures/SBOMs survive only by tool convention.

## Bazel offline mechanisms (apply to every ruleset)

- `bazel vendor --vendor_dir` — committable external repos, no network or repo cache needed. Excludes `local`/`configure` repo rules; absolute symlinks out of the external dir are not rewritten; goes stale on target/config/Bazel change; lockfile-only extensions need `--lockfile_deps`. https://bazel.build/external/vendor
- `--repository_cache` preferred (fill via `bazel fetch`, build `--nofetch`); `--distdir` deprecated but searched first. https://bazel.virtuslab.com/book/3~1~9/ , https://opensecura.googlesource.com/3p/lowrisc/opentitan/+/refs/heads/master/util/prep-bazel-airgapped-build.sh
- `--downloader_config` rewrites/allows/blocks URLs — a mirror mechanism, carries no content. https://blog.aspect.build/configuring-bazels-downloader
- Repository cache is keyed by sha256 of the downloaded file — fits OCI digests only when the digest is the sha256 of the fetched blob.

## Rulesets

- **rules_oci `oci_pull`** — repo rule, Bazel downloader, output base; digest-pinned (tag → "not reproducible" warning unless `reproducible = False`); one repo per platform + CPU-selecting alias, variants explicit; repo rules cannot read response headers → known-registry map / `www_authenticate_challenges`; auth from Docker config or `--credential_helper`. https://github.com/bazel-contrib/rules_oci/blob/main/docs/pull.md
- **rules_multitool** — JSON lockfile (`url`, `sha256`, `os`, `cpu`, `kind` file/archive/pkg) read by a module extension; `@multitool//tools/<name>` resolves per os/cpu; toolchains registered from hubs; lockfiles merge, last write wins. Closest analogue to a digest lockfile feeding repo rules. https://github.com/theoremlp/rules_multitool
- **toolchains_llvm** — sysroot as host path (RBE-hostile) or filegroup label (RBE-capable). `toolchains_llvm_bootstrapped` claims RBE out of the box with no host sysroot. https://registry-preview.bazel.build/modules/llvm/0.8.7 , https://github.com/cerisier/toolchains_llvm_bootstrapped , https://fosdem.org/2026/schedule/event/zero-sysroot_hermetic_llvm_cross-compilation_using_bazel
- **hermetic_cc_toolchain (zig)** — built on `zig cc`; offline/absolute-path behaviour not found.
- **rules_nixpkgs** — repo rules run `nix-build`, symlink `/nix/store` paths into the exec root, outside Bazel's control; RBE only with a store shared to executors (Tweag: read-only NFS); compilers worst case. https://github.com/tweag/rules_nixpkgs/issues/180 , https://www.tweag.io/blog/2024-02-29-remote-execution-rules-nixpkgs/
- **rules_python, rules_go, bazel-lib** — unresearched; OpenTitan pre-fetches `@go_sdk//...` into a repository cache for air-gap.

## OCI layout as offline transport

- **oras** — `oras cp --to-oci-layout`; referrers copied only with `-r`; copying an index copies all child manifests. Layout spec has no referrers response; oras stores referrers as plain manifests in `index.json`; subject-annotation proposal open, unmerged. Fixes in changelog for `cp -r` when an index child is a subject. https://oras.land/docs/commands/oras_cp , https://oras.land/docs/how_to_guides/distributing_oci_layouts , https://github.com/opencontainers/image-spec/pull/1171 , https://src.opensuse.org/pool/oras/src/branch/factory/oras.changes
- **crane** — `crane pull --format oci` writes a layout; `--annotate-ref` keeps the source ref (oci format only); default format tarball; referrers not mentioned. https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane_pull.md
- **skopeo** — signatures copied by default, `--remove-signatures` strips; `oci:` / `oci-archive:` both directions; cosign tag-signatures not referrer-copied (open ask). https://man.archlinux.org/man/skopeo-copy.1.en.txt , https://github.com/containers/skopeo/issues/1673
- **regclient** — docs moved (https://regclient.org/usage/regctl/), not read.
- **Helm, Flux, Homebrew-on-GHCR** — not researched; https://attested-delivery.github.io/docs/concepts/03-attestations-that-survive-promotion/ surfaced, unread.

## negative

- Host-absolute sysroot/store paths block RBE and relocation (rules_nixpkgs #180, toolchains_llvm).
- `bazel vendor` skips `local`/`configure` rules and leaves absolute symlinks — an external-store-with-symlinks rule hits both.
- Tag-pinned `oci_pull` is flagged non-reproducible.
- Repo rules cannot read response headers → registry auth workarounds.
- OCI layout has no standard referrer place — linkage lost or tool-specific.
- Cosign tag-stored signatures escape referrer-aware copy.
- `--distdir` deprecated in favour of the repository cache.

## leads

- `repository_ctx.download` / `module_ctx.download` with `integrity` + `canonical_id` — whether a repo rule can fetch straight from an exported bundle via the repository cache.
- rules_python / rules_go source — platform lists, lockfile hashes, `--repository_cache` honouring.
- image-spec #1171 and regclient `ocidir` referrer handling — portable signatures/SBOMs in a bundle.
- `containers/image` `dir:` / `oci:` transports — annotation and signature preservation.
- Helm / Flux / Homebrew bottles — non-container OCI artifact export prior art.
- `toolchains_llvm_bootstrapped` — current direction for RBE-safe prebuilt toolchains.
