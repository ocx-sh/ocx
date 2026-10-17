---
title: "ocx package: consuming"
---
<!-- doc_type: reference -->

## `ocx package deps` {#deps}

Show the dependency tree for one or more installed packages.

Default output is a tree of declared dependencies. Each line names a package with its tag, the visibility its parent declared for the edge (absent on a root), the digest, and `repeated` when the package already appeared earlier in the tree.

A repeated package is not expanded again.

`--flat` prints the resolved evaluation order instead, the exact order `ocx package exec` and `ocx package env` compose the environment in, one row of package, visibility and digest each.

`--why <DEP>` traces every path from the given packages to one dependency, each printed as a chain such as `myapp:1.0 -> ocx.sh/kitware/cmake:3.28 -> ocx.sh/gcc:13`. With several packages, the command builds their combined graph.

Works on locally present packages only (no auto-install) and never consults `ocx.toml`. It walks structural metadata and never resolves env values, so a package whose env references an undeclared dependency still appears.

Only two things drop a package from the output, each logged at warn level. One is a `metadata.json` that fails to parse or validate.

The other is an env-var modifier type this ocx does not recognize.

```text
ocx package deps [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--depth` | `N` | Limit tree depth (default: unlimited). Only applies to tree view |
| `--flat` |  | Show the flattened evaluation order instead of the tree |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--self` |  | Expose the package's full dep set, including private (self-only) edges. Affects `--flat`. See `ocx exec --help` for the full surface semantics |
| `--why` | `DEP` | Explain why a dependency is pulled in (matches by registry and repository; tag is ignored) |

## `ocx package deselect` {#package-deselect}

Remove the current-version symlink for one or more packages.

The package is deselected but not uninstalled: its candidate symlink and object-store content stay. Removing the `current` symlink also makes the package's entry-point launchers unreachable through `current/entrypoints/`.

Removing an already-absent link is not an error. Use `ocx package uninstall` to remove the installed files too.

```text
ocx package deselect <PACKAGE>...
```

## `ocx package env` {#package-env}

Print the resolved environment variables for one or more installed packages.

Plain output is an aligned `Key`, `Type`, `Value` table. When binaries, entry points or integration namespaces are admitted, it ends with one hint line counting each, for example `5 binaries available (...); 2 integration namespaces (...); use --format json for the full list`.

`--format json` emits `items` plus `binaries`, `entrypoints` and `integrations` arrays; `--shell[=NAME]` emits the only eval-safe form. A package's dependencies contribute their variables in topological order, dependencies before dependents.

Packages missing from the object store are installed, with their transitive dependencies, and a package a `[[trust.policy]]` covers is signature-verified before its environment is composed.

`--candidate`, `--current` and `--link` resolve through an install symlink instead and never auto-install. `--self` selects the publisher's own view (private and public entries) instead of the consumer view (public and interface); `integrations` is always empty under `--self`.

```text
ocx package env [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--candidate` |  | Resolve the content path via the installed candidate symlink (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`) |
| `--ci` | `github (github-actions)\|gitlab (gitlab-ci)` | Write the composed environment into a CI system's persistence channel. Bare `--ci` autodetects the provider and exits 64 when none is found |
| `--current` |  | Resolve the content path via the current-selected symlink (`~/.ocx/symlinks/<registry>/<repo>/current`) |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--export-file` | `PATH` | Write the GitLab export to this file instead of stdout |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--link` | `PATH` | Resolve the content path via a link written by `ocx package install --link <PATH>` |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--self` |  | Expose the package's full env, including private (self-only) entries. See `ocx exec --help` for full view semantics |
| `--shell` | `ash\|ksh\|dash (sh)\|bash\|elvish\|fish\|batch (cmd)\|powershell (pwsh)\|zsh\|nushell (nu)` | Target shell for eval-safe export lines. A bare `--shell` autodetects it from `$SHELL` or the parent process and exits 64 when undetectable |
| `--show-patches` |  | Annotate each entry with its origin package or companion identifier |

## `ocx package exec` {#package-exec}

Runs installed packages.

Identifiers are OCI references (for example `kitware/cmake:3.28`), resolved through the index and installed when missing. A package a `[[trust.policy]]` covers is signature-verified before it runs.

Dependencies' environments apply in topological order before the package's own, and entries layer in the order the identifiers appear. For a project driven by `ocx.toml`, use `ocx exec`.

Stdin is always inherited, so piped input reaches the command unchanged. On Unix ocx replaces itself with the command, so signals reach it directly and it keeps ocx's process id.

On Windows ocx spawns the command, waits, and forwards its exit code.

`--rm` is the exception on Unix: cleanup must run after the command, so ocx stays resident as its parent.

The command is looked up once, before it starts. Only the composed packages' own `PATH` contributions are searched, never the ambient `PATH` or the working directory.

A match that is itself an ocx launcher is refused. `--rm` runs the same lock-free store collection as `ocx package uninstall --purge` over the packages this run composed, so concurrent invocations sharing one `OCX_HOME` can race.

Use it for one-off runs, not in a loop.

- Alias: `x`

```text
ocx package exec [OPTIONS] <PACKAGES>... <COMMAND>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--candidate` |  | Resolve the content path via the installed candidate symlink (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`) |
| `--clean` |  | Start with a clean environment containing only the package variables, instead of inheriting the current shell environment |
| `--current` |  | Resolve the content path via the current-selected symlink (`~/.ocx/symlinks/<registry>/<repo>/current`) |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--link` | `PATH` | Resolve the content path via a link written by `ocx package install --link <PATH>` |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--records-dir` | `DIRECTORY` | Directory to write this invocation's execution record into |
| `--records-name` | `TEMPLATE` | Filename template for the record, e.g. `{time}-{host}-{pid}.json` |
| `--rm` |  | Remove the packages from the store once the command finishes |
| `--self` |  | Expose the package's full env, including its private (self-only) entries. Off by default: only public + interface entries are loaded (the consumer view). Generated launchers use `ocx launcher exec` which enables self-view internally |

## `ocx package inspect` {#package-inspect}

Inspect one or more package references (candidates, metadata, or resolution chain).

Read-only: nothing is installed and no symlinks are created. Default mode resolves a tag through the index, so a cache miss may populate the local index and blob cache.

Unlike `ocx package test`, the identifier accepts an explicit `@digest`.

The output adapts to the reference. An image-index reference lists its platform candidates (platform, child digest, media type, size) with no metadata loaded and no platform selected.

A single-manifest reference, a flat tag or an `@digest` at an image manifest, emits the declared metadata and the manifest's layers. `--resolve` platform-selects through the index and adds the resolution chain (index, manifest, config).

`--closure` walks the declared dependencies and adds the metadata-only dependency closure plus the `interface` and `private` surface projections. On an image index it platform-selects first, as `--resolve` does, so it never returns the candidates listing.

`--closure` fails closed. If any dependency's manifest or metadata cannot be loaded, the whole request fails rather than rendering a partial closure.

A conflict exits 65 with the report still printed in full. JSON is one envelope with `platform` (only with `--resolve` or `--closure`), `packages` (one entry per requested identifier, in request order) and `env` (the per-invocation `--env` overrides in flag order).

A script testing whether `--closure` ran checks the `closure` key, not `resolution`, which follows the reference's shape.

```text
ocx package inspect [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--closure` |  | Compute the metadata-only dependency closure without installing |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--resolve` |  | Platform-select through the index and emit the OCI resolution chain (pinned identifier and walk-order chain digests: index, manifest, config) alongside the metadata and layers. Without `--resolve` or `--closure`, an image-index reference lists its platform candidates instead |

## `ocx package install` {#package-install}

Install packages from a local or remote index (no `ocx.toml` touched).

Downloads the packages and their transitive dependencies into the object store and creates a candidate symlink for each requested package; dependencies get no install symlinks. This is the OCI-tier install and never touches `ocx.toml`; use `ocx add` for a project.

A package that a `[[trust.policy]]` covers is verified before any layer downloads (see `--verify`).

Installing a foreign platform fills the object store but leaves the host's candidate and `current` symlinks alone. A platformless lookup therefore never resolves to a package the host cannot run.

On Windows a warning is printed when `PATHEXT` lacks `.CMD`, which the generated entry-point launchers need.

```text
ocx package install [OPTIONS] <PACKAGES>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--link` | `PATH` | Also link the installed package at PATH, which keeps it from `ocx clean`. PATH must be absent or an earlier `--link`, else exit 65; takes one package |
| `--no-verify` |  | Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY` |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--select`, `-s` |  | Also set the installed version as current (creates the current symlink) |
| `--verify` |  | Verify the package's Sigstore signature before installing (default) |

## `ocx package prune` {#package-prune}

Delete tags from a package's registry repository, guarded by the index.

Name the tags to delete, or pass `--prerelease` to select every build of one pre-release (`0.5.0-canary_<build>`) plus its rolling tag (`0.5.0-canary`). A pre-release build cascades only into its own pre-release, so a family never touches `x.y`, `x` or `latest`.

`--prerelease` selects from the registry's tag listing, never from the index.

`--keep-builds N` keeps the newest N builds and the rolling tag. Newest is version order, which compares build ids as text, so build ids must be fixed-width: push with `--build-timestamp=datetime`.

`=date` is not enough, since two builds on one day share a tag.

For a namespace with an index, prune reads the package's root from the configured index URL, never a mirror, to find the registry repository. Before any delete it refuses the whole run in two cases.

One is a selected tag listed without the ephemeral marker (exit 81). The other is a selected tag in the registry but not yet in the index (exit 75).

`--force` deletes such tags anyway and is required for a namespace with no index. A tag already gone from the registry is reported as absent.

Prune deletes registry tags only: never a digest, a platform manifest, a keep tag or a referrer, and it never writes the index. A keep tag pins the manifest past the tag's deletion: push ephemeral builds with `--no-keep-tag`, or prune frees no storage.

Pass `--tags-file PATH` to append each gone tag the index lists, then publish the removal with `ocx package announce --tags-file PATH`.

Builds are deleted oldest first and the rolling tag last. After each delete prune confirms the tag is gone with a manifest read (three tries, 250 ms apart).

A tag still present exits 75. Deleting a repository's last tag may remove the repository on some registries, which the confirmation accepts.

A delete is not conditional. A push that cascades into the rolling tag at the same moment can lose to it, so serialize the jobs of one track.

Tag deletion is optional in the OCI distribution spec. A registry that refuses it fails the first delete with exit 82 before anything is deleted, and 82 is never worth retrying.

The read of the root commits nothing to the local index.

Under `--format json` each tag row carries an `action`: `deleted`, `absent`, `would_delete`, `kept`, `refused` or `not_attempted`. A kept row's `reason` is `newest` or `rolling`.

A refused row's `reason` is `durable` or `not_in_index`.

Rows are in processing order, builds oldest first and the rolling tag last. A run that failed after entering the safeguard still prints this document alone on stdout, and the exit code is the status.

A routing failure prints the envelope alone, since the repository is not yet known.

Exits 0 when every selected tag is gone or kept, and 64 on a usage error. Exits 69 when the index or registry fails in a way a rerun will not change.

Exits 74 when the tags file cannot be written. Exits 75 when either is briefly unreachable, or a tag is not in the index yet or is still present after its delete.

Exits 78 when the index points at a forbidden host, and 79 when the index has no such package. Exits 80 when the credential cannot delete.

Exits 81 for a durable tag, a namespace with no index, or `--offline`. Exits 82 when the registry does not delete tags.

A dry run exits 81 or 75 exactly as the real run would, while 80 and 82 surface only on a real delete.

Details: <https://ocx.sh/docs/reference/command-line/package-consuming/#package-prune>

```text
ocx package prune [OPTIONS] <PACKAGE> [TAG]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--dry-run` |  | Run the safeguard and report; delete nothing, write nothing |
| `--force` |  | Delete tags the index does not mark ephemeral, or with no index to ask. A forced tag's index row stays until a reviewed announce removes it |
| `--keep-builds` | `N` | With `--prerelease`: keep the newest N builds and the rolling tag (N >= 1) |
| `--prerelease` | `VERSION` | Select every build of this pre-release and its rolling tag, e.g. `0.5.0-canary` |
| `--tags-file` | `PATH` | Append each gone tag the index lists, one per line, for `ocx package announce --tags-file`. Creates the file if absent and keeps the tags already in it. Written on every run that gets past selection, empty included and even when the run then fails partway, never on a dry run. A forced delete of a tag the index does not list writes nothing |

## `ocx package pull` {#package-pull}

Downloads packages into the local object store without creating install symlinks.

Populates only the content-addressed object store; no candidate or current symlinks are created. Transitive dependencies are pulled as well.

This is the primitive for CI, where reproducibility matters and symlink management is unnecessary. For a project driven by `ocx.lock`, use `ocx pull` instead.

With a `[patches]` tier configured, the package's patch descriptors are revalidated and the matching companions installed, as `package install` does; a `required` companion that cannot be found exits 79. A policy-covered package is signature-verified before it downloads, as `package install` does.

Reports the package root of each package, the same directory `package which` and `package exec` resolve to. Concurrent pulls of one digest are safe.

```text
ocx package pull [OPTIONS] <PACKAGES>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--no-verify` |  | Skip Sigstore signature verification. Equivalent env var: `OCX_NO_VERIFY` |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
| `--verify` |  | Verify the package's Sigstore signature before installing (default) |

## `ocx package receipt` {#package-receipt}

Print the build receipt `ocx package create` wrote beside a bundle.

The receipt records the `--platform` and `--identifier` a build was given; `ocx package push` and `ocx package test` fall back to it when the flag is omitted. JSON output is `{"platform": ..., "identifier": ...}`, each key present only when the build recorded it.

The receipt is resolved beside the bundle by the stem rule `push` uses (`hello-1.0.0.tar.xz` -> `hello-1.0.0-receipt.json`). The file itself is a build artifact; this report is the contract an SDK reads instead.

Exits 79 when no receipt sits beside the bundle and 65 when the file exists but is not a receipt this ocx can read.

```text
ocx package receipt <BUNDLE>
```

## `ocx package sbom` {#package-sbom}

List or extract the SBOM attestations a published package carries.

The read-side counterpart to `ocx package attest`. A manifest can carry a signed attestation (a DSSE bundle with a Fulcio certificate and a Rekor entry behind it).

It can also carry an unsigned attach with no signature over it. Either is found as a referrer (what `oras attach` writes) or as the cosign `sha256-<hex>.sbom` sidecar tag.

No referrer listing reaches that tag, so it is read by name.

The payload layer's own media type is the only claim about the document, so it is both the gate and the label. A layer typed outside the SBOM set is refused (exit 65, `sbom_media_type_unsupported`).

Each invocation runs in one of two modes. `--verify` lists only documents whose signature passed every check `verify --attestation` runs; an unsigned attach is refused rather than listed (exit 77 when it is all the package carries).

It is the default whenever `--certificate-identity` or `--certificate-oidc-issuer` is given or a `[[trust.policy]]` covers the package. `--no-verify` runs no cryptography at all and reports every document as `verified: false` with no signer identity.

It is the default when no identity source resolves, and it is what lets `sbom` work with no Sigstore setup.

Naming both flags is not an error (the later wins), but `--no-verify` with an identity exits 64, and so does `--verify` with no identity source at all. An unverified entry carries `verified: false` in plain and JSON output, and `summary.verification` names the mode.

`--output` and `--summary` are mutually exclusive with each other and with the default listing; `--summary` works in both modes.

With `--platform`, the command reads the platform manifest and the index behind it. A platform SBOM supersedes an index SBOM only within the same `predicate_type`.

A superseded document stays in JSON with `shadowed: true`, while the plain table shows the winner. Two documents of one type on the same subject shadow neither.

Candidates the scan rejected appear in `refused`, never silently. `summary.status` is `partial_failure` whenever `refused` is non-empty, else `success`.

Each refused entry carries a `reason` and a `reason_kind` slug. Scripts branch on `reason_kind`, never on `reason`.

A `--summary` document that cannot be read gets `sbom_summary_failed`.

Plain listings truncate `refused` to the first 20. JSON never truncates it.

```text
ocx package sbom [OPTIONS] <IDENTIFIER>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--certificate-identity` | `IDENTITY` | Expected certificate SAN (exact match) |
| `--certificate-oidc-issuer` | `URL` | Expected certificate OIDC issuer (exact match) |
| `--key` | `REF` | Sign or verify with a key pair instead of keyless Sigstore |
| `--no-cache` |  | Bypass the referrers-capability cache for this invocation |
| `--no-verify` |  | List documents without verifying anything; `--output` still writes the document, with one stderr warning naming the referrer digest |
| `--output`, `-o` | `PATH` | Write the SBOM document to PATH ("-" for stdout). Several candidates exit 65 (`multiple_attestations`), a terminal exits 64; under `--no-verify` a stderr warning names the referrer digest |
| `--platform`, `-p` | `PLATFORM` | Narrow into one platform of an image index |
| `--rekor-url` | `URL` | Rekor transparency-log endpoint |
| `--signature-format` | `FORMAT` | Signature wire format to accept: bundle or simplesigning |
| `--sigstore-trusted-root` | `PATH` | Trust-root override: a Sigstore trusted-root JSON (or a directory holding trusted_root.json), named by a bare path or a file:// one. A file over 1 MiB or unreadable exits 74 |
| `--summary` |  | Parse each SBOM and report component counts (CycloneDX 1.5-1.7 only) |
| `--type` | `TYPE` | Restrict to one predicate type (for example cyclonedx or spdx) |
| `--verify` |  | Require a verified signature; refuse unsigned attachments |

## `ocx package select` {#package-select}

Set the current version of one or more packages.

Resolves each package, checks that its content is already in the local object store, and points the package's `current` symlink at it. Nothing is downloaded.

Entry-point name collisions surface when the packages are consumed (`ocx package env`, `ocx package exec`), not at select.

```text
ocx package select [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |

## `ocx package uninstall` {#package-uninstall}

Remove an installed candidate for one or more packages.

Removes the candidate symlink and its back-reference. Only the reference goes; `ocx clean`, or `--purge` right away, deletes a package nothing else references, a lock pin included.

The `current` symlink stays unless `--deselect` is given.

```text
ocx package uninstall [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--deselect`, `-d` |  | Also remove the current symlink (equivalent to also running `ocx package deselect`) |
| `--no-purge` |  | Leave unlinked packages on disk for `ocx clean`. The default |
| `--purge` |  | Also delete each package this command unlinks, when nothing else references it |

## `ocx package verify` {#package-verify}

Verify a published package's Sigstore signature (keyless, via OCI Referrers).

Fetches the Sigstore bundle v0.3 referrer for the target. Then it checks the Fulcio certificate chain against a trust root, the Rekor Signed Entry Timestamp, and the signature over the subject manifest digest.

It also checks the certificate identity and OIDC issuer against the identity you supply as flags or have pinned in a `[[trust.policy]]` entry. All checks must pass for exit 0.

`--certificate-identity` and `--certificate-oidc-issuer` are optional only when a `[[trust.policy]]` scope covers the target; keyless verification needs an identity from one source or the other.

`--offline` (or `OCX_OFFLINE`) scopes to the Sigstore trust services, the Rekor-key fetch and TUF, not the registry. Verify still fetches the target and its referrer from the registry in every mode.

Offline verification needs a pinned Rekor key from `--sigstore-trusted-root` (or another trust-root rung) or a fresh trust-root cache entry.

With no trust root from any rung and no cache entry, verify fetches the public-good root over TUF. `--sigstore-trusted-root` pins a self-hosted deployment's own.

A keyless cosign sidecar (`sha256-<hex>.sig`, or `.att` under `--attestation`) needs transparency-log evidence of its own. Its layer must carry a `dev.sigstore.cosign/bundle` annotation.

The annotation's Signed Entry Timestamp must verify against the log key, and its logged body must bind to that signature or envelope. A Fulcio certificate lives about ten minutes, so without an entry nothing shows the signature was made while it was live.

The sidecar is therefore refused (exit 65, `signature_invalid`).

`cosign attach signature` writes no such annotation and needs `--insecure-ignore-tlog` here; `ocx package sign --signature-format simplesigning` does write one. `--allow-unlogged-signature` is the opt-out for air-gapped CI.

`--attestation` swaps the referrer type verify looks for. Instead of a Sigstore-bundle signature it fetches a DSSE-enveloped in-toto Statement and runs the same pipeline against it.

It also checks that the Statement's `subject` names the target digest with a strong algorithm. `--type` narrows which `predicateType` matches; omit it to accept any.

The report has the same fields in either mode. Use `ocx package sbom` when the predicate content is what you need back.

Name the signer in one of two ways. Pass both `--certificate-identity` and `--certificate-oidc-issuer`, an exact-match pair that overrides any configured policy.

Or pass neither and let `[[trust.policy]]` decide. Verify then checks the operator-tier (`config.toml`) policies against the target's canonical `registry/repository`.

It falls back to the project `ocx.toml` policies only when none match.

Reading `[[trust.policy]]` from `ocx.toml` is the one exception to OCI-tier commands never consulting it, because trust policy is a security posture rather than toolchain binding. Exactly one of the two flags is a usage error rejected at parse time (exit 64), and so is neither flag with no covering policy (`no_identity_provided`).

Trust material carrying only CA anchors cannot verify anything, since the Fulcio certificate's SCT is checked against the CT log key. Verify refuses it with exit 78 (`trust root carries no CT log key`).

`--sigstore-trusted-root` accepts only a Sigstore trusted-root JSON carrying the anchors, CT log keys and Rekor key together.

A trusted-root file over 1 MiB, or present but unreadable at `$OCX_HOME/sigstore/trusted-root.json`, exits 74 (`trust_root_unreadable`). An absent `$OCX_HOME/sigstore/trusted-root.json` is not an error: resolution continues down the ladder.

`--format json` reports `subject_digest`, the signer identity and `signatures`, one row per signature found with its `discovery_method`, `signed_at` and `rekor_log_index`.

```text
ocx package verify [OPTIONS] <IDENTIFIER>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--allow-unlogged-signature` |  | Accept a keyless cosign sidecar that carries no transparency-log entry |
| `--attestation` |  | Verify a signed in-toto attestation instead of an artifact signature |
| `--certificate-identity` | `IDENTITY` | Expected certificate SAN (exact match) |
| `--certificate-oidc-issuer` | `URL` | Expected certificate OIDC issuer (exact match) |
| `--key` | `REF` | Sign or verify with a key pair instead of keyless Sigstore |
| `--no-cache` |  | Bypass the referrers-capability cache for this invocation |
| `--platform`, `-p` | `PLATFORM` | Narrow into one platform of an image index |
| `--rekor-url` | `URL` | Rekor transparency-log endpoint |
| `--signature-format` | `FORMAT` | Signature wire format to accept: bundle or simplesigning |
| `--sigstore-trusted-root` | `PATH` | Trust-root override: a Sigstore trusted-root JSON (or a directory holding trusted_root.json), named by a bare path or a file:// one. A file over 1 MiB or unreadable exits 74 |
| `--type` | `TYPE` | Restrict to one predicate type (for example cyclonedx or spdx); requires `--attestation` |

## `ocx package which` {#which}

Resolve installed packages and print their package-root (or, with `--candidate`/`--current`, install-symlink) paths.

Operates on OCI identifiers and never consults `ocx.toml`. The package root holds `content/` (installed files), `entrypoints/` (generated launchers) and per-package files such as `metadata.json`.

By default the content-addressed package root is printed; `--candidate`, `--current` and `--link` print the stable install symlink instead, which targets the same root.

Nothing is downloaded, with or without `--lazy-mode`. Each entry names its kind: `package` for a materialized root, or `shim` for a package composed with `--lazy-mode always` whose content has not downloaded yet.

Install symlinks only ever resolve to `package`.

```text
ocx package which [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--candidate` |  | Resolve the content path via the installed candidate symlink (`~/.ocx/symlinks/<registry>/<repo>/candidates/<tag>`) |
| `--current` |  | Resolve the content path via the current-selected symlink (`~/.ocx/symlinks/<registry>/<repo>/current`) |
| `--lazy-mode` | `never\|always` | Control when a package's content downloads: now, or on first use |
| `--link` | `PATH` | Resolve the content path via a link written by `ocx package install --link <PATH>` |
| `--platform`, `-p` | `PLATFORM` | Target platform to resolve packages against |
