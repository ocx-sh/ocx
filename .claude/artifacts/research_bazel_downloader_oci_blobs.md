# Research: Bazel downloader for OCI registry blobs

## Metadata

**Date:** 2026-11-08
**Domain:** devops
**Triggered by:** `/hex-architect` xhigh on `.agents/discussions/bazel-hermetic-package-bundles.md` — research axis "Bazel downloader specifics" (owner pick)
**Expires:** 2027-05-08
**Caveat:** items marked UNVERIFIED had no primary source read.

## Direct Answer

`repository_ctx.download_and_extract(url, sha256=<layer digest hex>, canonical_id=<digest>, type="tar.gz"|"tar.xz"|"tar.zst")` fits an OCI layer blob: the registry digest of the compressed blob is its sha256, so one checksum is both integrity and repository-cache key, and an extensionless blob URL extracts once `type` is set. The downloader does no bearer-token challenge — a repo rule must exchange the token (rules_oci pattern) or a credential helper must supply headers. Open risk: Bazel forwards `Authorization` across cross-host redirects ([bazelbuild/bazel#29660](https://github.com/bazelbuild/bazel/issues/29660), open). No known ruleset extracts OCI layers natively in a repo rule.

## Findings

### `download` / `download_and_extract`

Sources: [repository_ctx](https://bazel.build/rules/lib/builtins/repository_ctx), [StarlarkBaseExternalContext.java](https://raw.githubusercontent.com/bazelbuild/bazel/master/src/main/java/com/google/devtools/build/lib/bazel/repository/starlark/StarlarkBaseExternalContext.java)

- `sha256` and `integrity` (SRI) are mutually exclusive.
- The repository cache is keyed by checksum. `canonical_id` restricts hits to entries added with the same id, and the docs recommend it whenever a checksum is set.
- A checksum permits plain `http` URLs.
- `auth` is per URL and `headers` applies to all URLs.
- `block=False` runs downloads in parallel (Bazel 7.1+, used by rules_oci).
- `type` accepts zip, jar, war, aar, nupkg, whl, tar, tar.gz, tgz, gz, tar.xz, txz, xz, tar.zst, tzst, zst, tar.bz2, tbz, bz2, ar, deb, 7z, tar.br, br. It is inferred from the URL extension unless set; the downloaded file is renamed to carry the extension before extraction.
- `--experimental_repository_cache_hardlinks` (default false) hardlinks on a cache hit.
- `bazel vendor` stores extracted repo trees, not archives. Absolute symlinks out of the external dir are not rewritten, and Windows needs `--windows_enable_symlinks` ([vendor](https://bazel.build/external/vendor)).

### Auth and redirects

- Credential helper protocol ([design](https://raw.githubusercontent.com/bazelbuild/proposals/main/designs/2022-06-07-bazel-credential-helpers.md)):
  - Bazel runs `helper get` and writes `{"uri": …}` to its stdin.
  - The helper answers `{"headers": {…}}` on stdout.
  - `--credential_helper=[pattern=]path` takes an exact host or `*.domain`; the most specific match wins.
  - A non-zero exit or malformed output aborts.
- The downloader does no WWW-Authenticate challenge or token exchange.
- rules_oci exchanges the token in the repo rule ([authn.bzl](https://raw.githubusercontent.com/bazel-contrib/rules_oci/main/oci/private/authn.bzl)):
  - It reads docker config and runs credential helpers via `rctx.execute`.
  - It has a built-in challenge map (Docker Hub, GHCR, Quay, ECR Public, ACR) plus `www_authenticate_challenges`.
  - It downloads the realm token, anonymously when no credentials exist, and passes `auth={url: Bearer …}`.
- Alternative: Tweag's [credential-helper OCI provider](https://github.com/tweag/credential-helper/blob/main/docs/providers/oci.md). It defaults to ghcr.io, index.docker.io, quay.io and public.ecr.aws.
- Redirects are followed. [#29660](https://github.com/bazelbuild/bazel/issues/29660) (open, P3, 9.1.0) reports that `Authorization` is forwarded across cross-host and HTTPS→HTTP redirects. Fix status is unknown.
- Presigned storage URLs may reject an extra Authorization header, so the failure is a refusal rather than a leak.
- UNVERIFIED: whether the credential helper is re-invoked for the redirect host.

### `--downloader_config`

- Directives are `rewrite` (regex with back-references), `allow`, `block` and `all_blocked_message` ([Aspect](https://aspect.build/blog/configuring-bazels-downloader), [format](https://bazel.googlesource.com/bazel/+/c45bb5f61bd764d72d653bbda8bf179c1753d268/bazel_downloader.cfg)). It was formerly `--experimental_downloader_config`.
- No public example rewrites `/v2/<repo>/blobs/<digest>` to Artifactory, zot or Nexus. The checksum still guards content after a rewrite.

### Prior art

- rules_oci `oci_pull` ([pull.bzl](https://raw.githubusercontent.com/bazel-contrib/rules_oci/main/oci/private/pull.bzl)) calls `rctx.download` per blob (`sha256` only for `sha256:` digests). It keeps layers compressed in an OCI layout and never extracts them. A blob without a verified digest gets a warning and is not cached.
- rules_img ([repo](https://github.com/bazel-contrib/rules_img)) fetches only the manifest and config at repo time; layers are lazy, never extracted. Users include Spotify and OpenAI; its credential chain starts with the Bazel credential helper.
- DataDog [rules_oci_bootstrap](https://github.com/DataDog/rules_oci_bootstrap) — not read.
- No ruleset found unpacks multi-layer OCI content natively into a repo.
- Bazel's tar extractor has a history of link fixes:
  - [hardlinks #574](https://bazel.googlesource.com/bazel/+/e1cd9509862aef684b4dbbdfd15d0b877fb8fad3%5E%21/)
  - [symlinks before targets](https://bazel.googlesource.com/bazel/+/6ff38a644c0cf038a0ee994513fd3af115130b2c)
  - [self-referential hardlinks](https://bazel.googlesource.com/bazel/+/3ff33278b01b056a85a6540bffaa0a7b4022b672)
  - [`strip_components` with links, PR #30044](https://bazel.googlesource.com/bazel/+/6516fe1a79855fd2aa1e01ec8c99adf68fdd9164)
- UNVERIFIED: `.wh.` whiteouts, cross-layer hardlinks, and overwrite order when layers are extracted into one directory. To `download_and_extract`, whiteouts are plain files.

### Windows

- No documented tar symlink or junction behaviour in repo rules.
- Bazel recommends a short `--output_user_root`, 8.3 names, and `--windows_enable_symlinks` with Developer Mode or elevation ([windows.mdx](https://bazel.googlesource.com/bazel/+/master/docs/configure/windows.mdx)).
- Dangling symlinks work with that flag per [#10298](https://github.com/bazelbuild/bazel/issues/10298); this is one maintainer's test.

### Offline and resilience

Source: [flag reference](https://bazel.build/reference/command-line-reference)

- `--experimental_repository_downloader_retries` defaults to 5 and `--http_timeout_scaling` to 1.0.
- `--distdir` is searched before the network; it is deprecated.
- `--repository_cache` defaults to `{output_user_root}/cache/repos/v1`.
- The cache is checked first, so a sha256 hit succeeds with the URL unreachable, provided `canonical_id` matches.
- `--nofetch` semantics were not confirmed.

## Recommendation (researcher's opinion)

- Extract each layer into its own directory with `download_and_extract(..., sha256=, canonical_id=, type=<from media type>)`.
- Do the token exchange in the repo rule and pass `auth=`. Make a credential helper the opt-in alternative.
- Before committing, run two spikes:
  - Redirected blob GETs on ghcr.io and Docker Hub with `auth=` set.
  - An overlay fixture (whiteouts, hardlinks, symlinks) on Linux and Windows.
- Adoption: rules_oci is the community standard (per-blob `download`); rules_img and Tweag's credential-helper are rising.

## negative

- There is no prior art for native layer extraction in a repo rule.
- The downloader cannot answer a bearer challenge.
- #29660 is open: Authorization crosses origins on redirect.
- `--distdir` is deprecated; `--experimental_repository_cache_hardlinks` is still experimental.
- Windows tar-symlink behaviour is undocumented.
- No public blob-URL rewrite example exists.
- `bazel vendor` stores trees, not blobs.

## leads

- TarFunction / CompressedTarFunction source: cross-layer hardlinks and symlink ordering.
- DataDog rules_oci_bootstrap.
- #29660 fix release.
- Whether the credential helper is invoked for the redirect host.
- `--fetch=false` with a warm repository cache.
- ghcr.io 307s to signed URLs.
