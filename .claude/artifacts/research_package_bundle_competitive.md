# Research: How comparable tools export and relocate prebuilt packages

## Metadata

**Date:** 2026-11-08
**Domain:** packaging
**Triggered by:** `.agents/discussions/bazel-hermetic-package-bundles.md` (competitive/vendor lane)
**Expires:** 2027-05-08
**Gap:** Guix pack manual returned 403 — unverified.

## Direct Answer

Two-form designs exist (pixi-pack: local-channel transport tar + `pixi-unpack` writing `activate.sh`/`.bat`; Nix: binary cache vs experimental `nix bundle`). Re-import splits between a dedicated command (Nix `--import`, fails on absent references) and pointing the tool at a cache (Spack `mirror add`, Nix substituter). Relocation by prefix rewriting fails on longer target prefixes (Spack, Homebrew, mise).

## Nix

- Unit: `nix-store --export` = named objects without closure (feed `--query --requisites`); `nix copy` = closure by default. ([export](https://nix.dev/manual/nix/stable/command-ref/nix-store/export.html), [nix copy](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-copy.html))
- Forms: export stream; `nix copy --to file://…` signed, compressed binary cache used as substituter ([store type](https://nix.dev/manual/nix/stable/store/types/local-binary-cache-store.html)); runnable `nix bundle`, experimental, Linux-only ([bundle](https://nix.dev/manual/nix/stable/command-ref/new-cli/nix3-bundle.html)).
- Re-import: `nix-store --import`; fails when references are absent.
- Relocation: none — absolute hash-addressed store paths; third-party [relocatable.nix](https://github.com/Ninlives/relocatable.nix) rewrites at extraction.
- Integrity: `--no-check-sigs` skips trusted-key signature requirement.

## Spack ([binary caches](https://spack.readthedocs.io/en/latest/binary_caches.html), [mirrors](https://spack.readthedocs.io/en/latest/mirrors.html))

- Unit: `spack buildcache push` = spec + deps; `spack mirror create -a` = an environment's needs.
- Forms: cache in a directory, URL, or `oci://` registry; URL caches = JSON manifest per spec + `blobs/sha256/<2>/`; OCI caches distroless unless `--base-image`.
- Re-import: no import command — `mirror add`, `update-index`, install; `--use-buildcache only`.
- Relocation: path rewriting; fails if the new prefix is longer → source build; mitigation `padded_length`.
- Integrity: GPG on by default; index manifests unsigned; no Sigstore.

## Homebrew ([Bottles](https://docs.brew.sh/Bottles), [Reproducible Builds](https://docs.brew.sh/Reproducible-Builds))

- Unit: one formula's bottle, per OS/arch sha256. One form only.
- Relocation: `@@HOMEBREW_PREFIX@@` / `@@HOMEBREW_CELLAR@@` placeholders; `cellar :any` / `:any_skip_relocation`; otherwise prefix must match. Failure: overlong prefix broke a zip-payload executable in mise ([mise#11632](https://github.com/jdx/mise/pull/11632)).
- Integrity: sha256 mandatory; no attestations documented. GHCR OCI structure undocumented.

## conda / conda-pack / pixi-pack

- conda-pack: whole env tarball; `conda-unpack` fixes prefixes (mostly shebangs, [#336](https://github.com/conda/conda-pack/issues/336)); one-way, target OS must match. ([conda-pack](https://conda.github.io/conda-pack/))
- pixi-pack: transport `environment.tar` = local channel + `repodata.json`, from `pixi.lock`, `--platform` selectable, unpacks only on its platform; `pixi-unpack` → `./env` + `activate.sh`/`activate.bat`; `--create-executable` self-extracting; conda/micromamba can re-install from the bundled channel. No integrity story. ([pixi-pack](https://pixi.prefix.dev/latest/deployment/pixi_pack/))

## Docker save/load

- `docker save` = all parent layers and tags; all platform variants by default; `--platform` limits to one; missing platform → "no suitable export target found". ([docker save](https://docs.docker.com/reference/cli/docker/image/save/))

## negative

- `nix bundle` states no relocation limits; 2026 post argues Nix still needs relocatable binaries ([fzakaria](https://fzakaria.com/2026/06/21/nix-needs-relocatable-binaries)); fixed-length path padding called hacky ([Discourse](https://discourse.nixos.org/t/idea-for-very-minimally-changing-nix-cache-nixos-org-to-allow-custom-store-paths-without-rebuilding-the-world/8153)).
- conda-pack unpack is one-way; needs every package in conda's cache.
- `nix copy --substitute-on-destination` SSH-only — no fallback for missing paths in file:// caches.
- pixi-pack: no integrity; sdists unsupported.

## leads

- Guix pack `--relocatable` (retry fetch).
- Spack OCI manifest/layer layout from source — closest OCI-as-store analogue.
- Homebrew GHCR bottle manifest and attestations from `brew` source.
- Sigstore/cosign bundles as a portable signature carrier — none of the surveyed tools documents one.
- mise / aqua offline modes.
