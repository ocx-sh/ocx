---
title: "ocx package: publishing"
---
<!-- doc_type: reference -->

## `ocx package` {#package}

Operations related to packages (e.g. bundling or deploying).

```text
ocx package <COMMAND>
```

## `ocx package announce` {#package-announce}

Observe an owner-curated set of registry tags and publish the rebuilt entry into the index.

Reads the committed index entry, re-observes the given tags, and writes the rebuilt entry to a local directory (`--output`). Otherwise it opens a pull or merge request against the index repository.

The request comes from a fork with `--fork`, else from a branch on the index repository itself, which needs push access there. A run that changes nothing reports as unchanged and commits nothing; it opens a request only to recover one an earlier run left unopened.

A request needs a forge credential: `OCX_ANNOUNCE_TOKEN`, or the job token under `--transport git` inside a GitLab job. Writing to `--output` works without one on a public index repository; a private one needs `OCX_ANNOUNCE_TOKEN` to read the base.

GitHub and GitLab are both supported, on public hosts and self-hosted instances; on GitLab the change arrives as a merge request. A self-hosted host must name its forge with `--forge` (exit 64 otherwise).

A hostname gives no reliable signal, and a wrong guess would send the credential to the wrong API.

GitLab groups nest, so `--index-repo gitlab.example.com/acme/platform/tooling/index` names project `index` in group path `acme/platform/tooling`, and `--fork` takes the same form; a nested namespace is rejected on GitHub. A dotted top-level group is ambiguous (`acme.team/platform/index` reads as host `acme.team`), so write the host out.

An instance mounted under a path prefix is not supported.

`announce` never commits to the index's default branch. Both forges use `OCX_ANNOUNCE_TOKEN`; under `--transport git` the push can be split onto `OCX_ANNOUNCE_GIT_TOKEN`, and inside a GitLab job with neither set the job's `CI_JOB_TOKEN` carries both.

Running `announce` again for a package already announced from the same branch updates the existing request in place. When the index's default branch has moved since it opened, the branch is rebuilt on the current base as a single commit.

That commit carries every tag already announced, so the request stays mergeable.

An unchanged run opens no request, with two exceptions. A branch still carrying commits the index lacks gets a request opened or reused.

An unchanged run whose open request can no longer merge fails with exit 65 naming the branch (close the request or delete the branch). `--output` writes the whole entry on every run, unchanged included, except for a `--tags-file` that lists no tag, which exits 0 before writing anything.

Every run also observes the description published by `ocx package description push`. When its artifact has moved, the entry's description block (title, summary, keywords, content-addressed README and logo) is rebuilt and `desc_status` reads `updated`.

A description the index records but the registry no longer serves stops the run.

A package with no committed entry exits 79: the first-time claim is `ocx package claim`.

The following are dropped from the set and reported in `reserved_tags_dropped`. They are a tag that is not a version, the OCX-internal `__ocx` namespace (which carries the keep tag), and the legacy `sha256.<hex>` keep tag.

`--tags-from-registry` filters them silently. A `--tags` set that resolves to nothing but reserved tags exits 64.

A `--tags-file` that lists no tag, or only reserved ones, exits 0 before any registry or forge work unless `--yank` or `--unyank` was given. A pipeline can therefore feed it the empty file `ocx package prune` writes.

Announce observes only the tags it is given and reads the canonical registry, never a `[mirrors]` entry or a cache. A committed row it is not given is carried through unchanged, in committed order.

A given tag the registry reports as `MANIFEST_UNKNOWN` is gone; a removed row's `o/` object is deleted with it unless another row still references it. `--tags` keeps replace semantics, so a committed tag it does not name is dropped.

Additions and removals travel in one announce, so a pipeline makes one index change.

```text
ocx package announce [OPTIONS] <PACKAGE>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--ephemeral` |  | Mark the tags this run adds as removable without review |
| `--forge` | `github\|gitlab` | Which forge hosts the index repository |
| `--fork` | `REPOSITORY` | Open (or update) the request from this fork, as `[HOST/]NAMESPACE/PROJECT` |
| `--index-repo` | `REPOSITORY` | Index repository the request targets, as `[HOST/]NAMESPACE/PROJECT` |
| `--output`, `-o` | `DIRECTORY` | Write the rendered index entry under this directory instead of opening a request. Works without a credential |
| `--refresh` |  | Re-observe every committed tag; remove ephemeral rows whose tag is gone, and report durable ones |
| `--tags` | `TAGS` | Replace the curated tag set with this comma-separated list. A currently-committed tag that is not named here is dropped. So is a reserved tag named here: an `__ocx` tag (the keep tag included) or a legacy `sha256.<hex>` one is not a version, so the run still succeeds and reports the drops |
| `--tags-file` | `PATH` | Add, update or remove the tags listed in this file: a listed tag the registry no longer has is removed from the index. Rows the file does not list are left as they are |
| `--tags-from-registry` |  | Add every tag the registry holds; remove ephemeral rows whose tag is gone, and report durable ones |
| `--transport` | `api\|git` | How the request is written |
| `--unyank` | `TAG` | Clear the yanked marker from a tag. Repeat for multiple tags |
| `--yank` | `TAG` | Mark a tag as yanked. Repeat for multiple tags. Requires `--yank-reason`; only applies to a tag already in the curated set |
| `--yank-reason` | `TEXT` | Reason recorded on every tag named by `--yank` in this run |

## `ocx package attest` {#package-attest}

Attach an in-toto attestation to a published package manifest.

Signs a caller-supplied predicate document (an SBOM, provenance, VEX) with the same keyless Sigstore machinery `sign` uses. It publishes the result as an OCI referrer on the target manifest.

The shape depends on whether a signing identity is visible in the environment (the same override-token and ambient-CI check `sign` uses). With one present, the predicate is wrapped in a DSSE-enveloped in-toto Statement naming the target manifest digest as its `subject`.

The Statement is signed through the keyless pipeline `sign` uses. It is pushed as a referrer that `ocx package verify --attestation` and `ocx package sbom` find.

With none visible, `--type` must resolve to an SBOM media type (`cyclonedx`, `spdx`, `spdxjson`, or a full URI resolving to one). The predicate document is pushed verbatim as the referrer payload, typed by its own media type, with no envelope, certificate or Rekor entry.

Any other `--type` without a signing identity is refused before any network call (exit 64, `unsigned_type_unsupported`), since an unsigned provenance or vulnerability statement carries no attribution worth publishing.

If an identity is detected but acquiring a usable token then fails (Fulcio unreachable, an ambient CI token rejected), that is a hard error. The command never falls back to publishing unsigned.

`--offline` exits 81, checked before the predicate file is opened or any token is resolved.

`ocx package push --sbom` is sugar for `attest --type cyclonedx` against the digest a push just wrote. Use `attest` directly for a standalone predicate, an SPDX predicate, several predicate types on one manifest, or an attestation for something this invocation did not publish.

The `--tags` sweep behaves exactly as for `sign`, with one attestation per distinct index, and the token sources are identical to `sign`.

`--format json` reports `identifier`, `subject_digest`, `predicate_type` and `signed`, plus the `referrer_digest`, `bundle_digest` or `sidecar_digest` that was written.

```text
ocx package attest [OPTIONS] --predicate <PATH> --type <TYPE> <IDENTIFIER>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--fulcio-url` | `URL` | Fulcio CA endpoint (the keyless certificate issuer) |
| `--identity-token-file` | `PATH` | Read the OIDC identity token from this file (highest precedence) |
| `--identity-token-stdin` |  | Read the OIDC identity token from stdin (second precedence) |
| `--key` | `REF` | Sign or verify with a key pair instead of keyless Sigstore |
| `--no-cache` |  | Bypass the referrers-capability cache for this invocation |
| `--no-rekor-upload` |  | Skip the Rekor entry |
| `--no-tty` |  | Suppress the interactive browser OAuth fallback (CI / headless) |
| `--platform`, `-p` | `PLATFORM` | Narrow into one platform of an image index |
| `--predicate` | `PATH` | Predicate document to attach (JSON), at most 15 MiB; on Unix a symlink at PATH is refused |
| `--rekor-upload` |  | Record the signature in the Rekor transparency log |
| `--rekor-url` | `URL` | Rekor transparency-log endpoint |
| `--signature-format` | `bundle\|simplesigning\|both` | Signature wire format: bundle (default), simplesigning, or both. A sidecar appends a layer, never replacing one; `attest` with no signing identity refuses it (exit 64) |
| `--tags` | `TAG` | Tags to sweep. Repeatable, and accepts a comma-separated list |
| `--tags-file` | `PATH` | Read tags from a file, one per line or comma-separated |
| `--type` | `TYPE` | Predicate type: an alias (cyclonedx, spdx, spdxjson, slsaprovenance1, link, vuln, openvex, custom) or a full URI. `custom` wraps the predicate in cosign's Data/Timestamp envelope; `slsaprovenance` and `slsaprovenance02` exit 64, so pass `slsaprovenance1` |

## `ocx package cascade` {#package-cascade}

Audit and repair the rolling tags a published package cascades into.

Pushing `3.28.1` is supposed to leave `3.28`, `3` and `latest` pointing at it for every platform it ships. `check` reports where that is no longer true; `repair` re-points the tags using content the registry already holds.

```text
ocx package cascade <COMMAND>
```

## `ocx package cascade check` {#package-cascade-check}

Report where a package's rolling tags disagree with its versions.

Reads every version tag the registry holds for each package and folds them into the rolling-tag state the cascade rules imply. It reports each difference.

That covers a rolling tag missing a platform, one pointing at outdated content, one that was never created, and any entry nothing accounts for. For an `ocx.sh/...` package it also compares the live public index with the registry, so an entry that is stale there is reported too.

Read-only. It authenticates for pull only and never writes anything.

Give a tag (`cmake:3.28`) to narrow the audit to that part of the graph.

Exits 0 when everything agrees, 65 when anything does not, and 64 when a package names a digest or a tag that is not a version.

Nothing re-checks the cascade graph if a cascade push is interrupted partway through. `check` finds that drift afterwards without republishing anything, and never writes to the registry or any local index.

Identifiers may be logical or physical.

A logical one (a namespace with an index configured, such as `ocx.sh/kitware/cmake:3.28`) resolves to its registry location as `ocx package install` does. It also fetches the namespace's live index root.

That adds a third finding layer, index staleness, which compares each observed alias digest with the digest committed in the index. A physical one (`ghcr.io/ocx-contrib/cmake:3.28`) skips that layer.

The tag on an identifier selects the scope. No tag audits every alias tag in every variant track.

`:latest` audits only the default variant's track.

A rolling tag such as `:3.28` or `:debug-3` audits that tag's subtree plus the path up to its root. A fully build-tagged leaf audits only the path up to the root.

The leaf is the source of truth and never a write target.

A bare variant name (`:debug`) and a digest reference are usage errors (exit 64). Identifiers naming the same package union their scopes into one report.

```text
ocx package cascade check <PACKAGES>...
```

## `ocx package cascade repair` {#package-cascade-repair}

Re-point a package's rolling tags at the content its versions imply.

Rebuilds the whole index for every rolling tag `cascade check` reports as wrong and pushes it, using only content the registry already serves. A repair publishes nothing new and only re-points.

The full plan is computed first, and every manifest it references is checked to still exist. A run that cannot be completed therefore writes nothing at all.

Repairing the registry does not update the public index. Pass `--tags-file PATH` to append the tags this run moved, one per line, then hand that file to `ocx package announce --tags-file PATH`.

Exits 0 when every attempted write succeeded. Exits 65 when a finding remains, including a tag that cannot be fixed without publishing new content.

Exits 64 when a package names a digest or a tag that is not a version, or when `--tags-file` is given more than one package.

`repair` writes by default, since `check` is already the read-only preview; `--dry-run` computes and reports the same plan. Identifier forms and scope are those of `check`.

For each broken alias it rebuilds the whole platform index entry from the same fold `check` diffs against. It preserves every observed entry the fold does not supersede.

That includes annotations already on the index, non-platform entries such as attestations, and orphaned alias tags whose child manifests still exist. A missing referenced manifest is dropped only if every entry naming that digest is an orphan slot.

Otherwise the whole alias is refused and reported while other aliases still write.

Writes are batched after the whole plan is built and run concurrently per tag.

After each write `repair` re-reads the tag and warns when the digest disagrees, which is evidence of a concurrent publisher. An outcome is `raced` only when nothing was written because the tag moved between read and write.

There is no conditional-request guard, so avoid repairing a repository with a publish in flight. An index-staleness finding is never something `repair` writes.

Reaching the public index with the fix is a second hop through `ocx package announce`. Refreshing one machine's local copy is `ocx index update`'s job.

`--tags-file` records exactly the tags whose write landed (not `raced`, `refused` or `failed` ones), unioned with every tag an index-staleness finding names, so one file covers both hops. It is written on every run, empty included, so a workflow can feed it to `announce` unconditionally.

A preview contributes no landed tag, and its plan is in the JSON report as each entry's `planned`.

A follow-up `announce` updates or creates the listed aliases; a repair never produces a removal.

```text
ocx package cascade repair [OPTIONS] <PACKAGES>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--dry-run` |  | Compute and print the same plan without writing anything |
| `--tags-file` | `PATH` | Append the rolling tags this run moved or created to this file, one per line, for `ocx package announce --tags-file`. Creates the file if absent and keeps the tags already in it. Takes one package per run, since the file names no package and `announce` publishes it against one. Written on a dry run too - a preview moves nothing, so it adds only the tags an index finding already names |

## `ocx package claim` {#package-claim}

Claim a package in the index so its tags can be announced.

Renders the package's index entry and opens a pull or merge request against the index repository, or writes the entry to a local directory with `--output`. Claiming an already-claimed package adds your owners to the entry and leaves everything else as committed.

Run it again with nothing new to say and it reports `unchanged` and opens no request.

The entry carries the package description the registry serves at `__ocx.desc`, refreshed on every claim. Publish one with `ocx package description push`.

Owners default to the CI environment's user variables, else to the identity behind the credential. Name them explicitly with `--owner`, which replaces the detected list rather than adding to it.

Opening a request needs a forge credential: `OCX_ANNOUNCE_TOKEN`, or the job token under `--transport git` inside a GitLab job. Writing to `--output` works without one on a public index repository; a private one needs `OCX_ANNOUNCE_TOKEN` to read the base.

The claimed unit is the package, not the namespace prefix: the entry is `p/<namespace>/<package>.json`, so a second package under a claimed namespace needs its own `claim`. `claim` writes `owners`, `repository`, `name`, `upstream` and `desc`; `announce` publishes `tags` into an entry that already exists, and neither writes the other's fields.

A re-claim adds your resolved owners to the committed list (a union, committed owners first). It carries every other committed field through: `status`, `deprecated_message`, `created` and `tags` survive verbatim, and `desc` is re-observed.

`--upstream-org` replaces the committed `upstream` when given and carries it when omitted. A committed `name` that disagrees with the identifier you typed, or a committed `repository` other than `--repository`, is refused rather than silently repointed.

Nothing new to add reports `unchanged` and opens no request in every mode, `--output` included, because the comparison reads the index's base branch. Every claim reads the committed root, so a curated host's `trusted_hosts` refusal applies as under `announce`.

A re-claim that moves the description drops the README and logo blobs the previous entry named. A package with no description claims without a `desc` field, and `--output` writes the description's blobs beside the entry.

`--index-repo`, `--forge`, `--fork` and `--output` are one grammar shared with `announce`, including forge selection, nested GitLab groups and self-hosted instances.

An owner is a `login:id` pair, and the numeric id is what matters, since logins get renamed. Without `--owner`, ocx takes the CI user variables (`GITLAB_USER_LOGIN` with `GITLAB_USER_ID`, or `GITHUB_ACTOR` with `GITHUB_ACTOR_ID`, both halves required), else the identity behind the credential.

A bot account is refused at exit 64. The report's `owner_identity_source` says which rule produced the list.

`resolved` means the forge's users API answered. `ci_environment` means CI variables named it and the API was unreachable.

`asserted` means a `LOGIN:ID` was taken on your word, which is reachable on GitLab only under a job token, whose users API is closed.

A bare `LOGIN` with an unreachable users API is a usage error naming the `LOGIN:ID` form. A guessed id would write the wrong account into a governance field.

`--transport git` clones the index repository into a temporary directory, builds the commit there and creates the request from a single authenticated push. That is the only way a GitLab job token can open a merge request.

The credential can push and read the API, but not open one through it. It is GitLab-only and refused (exit 64) against GitHub, `--fork` and `--output`.

A split credential pair checks that the index project allows job-token pushes and that an allowlist admits the publisher, before anything is written. It exits 82 when job-token pushes are disabled and 77 when the allowlist does not admit the publisher.

A bare job token cannot read either setting, so it pushes and lets GitLab's own rejection decide, with exit 82. Only an administrator of the index project can grant access.

```text
ocx package claim [OPTIONS] --repository <REPOSITORY> <PACKAGE>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--forge` | `github\|gitlab` | Which forge hosts the index repository |
| `--fork` | `REPOSITORY` | Open (or update) the request from this fork, as `[HOST/]NAMESPACE/PROJECT` |
| `--index-repo` | `REPOSITORY` | Index repository the request targets, as `[HOST/]NAMESPACE/PROJECT` |
| `--output`, `-o` | `DIRECTORY` | Write the rendered index entry under this directory instead of opening a request. Works without a credential |
| `--owner` | `OWNER` | An owner of the package, as `LOGIN` or `LOGIN:ID`. Repeat for several, in the order they should be recorded |
| `--repository` | `REPOSITORY` | The physical OCI repository the package's bytes live in, as `oci://HOST/PATH` |
| `--transport` | `api\|git` | How the request is written |
| `--upstream-disclaimer` | `TEXT` | A disclaimer recorded on the index entry, for a package that is not operated by the upstream project |
| `--upstream-org` | `ORGANIZATION` | The upstream organization this package mirrors or repackages |
| `--upstream-repository-url` | `URL` | The upstream project's repository URL, as an absolute `http` or `https` URL carrying no embedded credentials |

## `ocx package copy` {#package-copy}

Promote an already-published package to another registry or repository.

Copies the platform manifests, their blobs and their referrers verbatim, so the digest never moves and every signature and lock pin naming it still resolves. The target's index is merged one platform at a time, never overwritten.

The source is `registry/repository:tag`, or `registry/repository@sha256:<hex>` for one platform manifest, in which case `--platform` is required because a platform manifest carries no platform of its own. A source in a namespace a configured index serves, such as `ocx.sh`, is resolved by that index.

Its tag names the platforms the index recorded, not what the registry's tag holds now. A tag the index does not hold or has yanked is refused.

Its platform manifests are read from the registry the index points to.

The target is written exactly as given.

Three kinds of object follow three rules, because only one is content. A platform manifest and its blobs are copied byte for byte, so the digest never changes.

The tag's image index is merged one platform at a time, so copying `linux/amd64` never removes a `darwin/arm64` the target already offers. Rolling tags (`1.4`, `1`, `latest`) are recomputed against the target's tag list under `--cascade`, never carried over from the source.

The report has one row per platform the target offers afterwards, labelled `added`, `unchanged`, `replaced` or `kept (not in source)`. That tells a filtered promotion that left a mixed index apart from a complete one.

A second identical copy changes nothing but is not free. Every platform re-fetches and re-PUTs its leaf manifest and, with `--referrers`, its referrer set.

Each index is re-PUT. Only blob bodies are skipped, via a HEAD against the target.

A copy is not a re-sign. The signature travels with the manifest and still names the identity that signed it in the source environment.

Sign again at the target when policy requires a production-specific attestation. `--offline` exits 81, since a copy needs network access to both registries.

```text
ocx package copy [OPTIONS] <SOURCE>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--annotation` | `KEY=VALUE` | Record an OCI annotation on the target's image index. Repeatable |
| `--cascade` |  | Recompute the rolling tags (`1.4`, `1`, `latest`) at the target |
| `--dry-run` |  | Report what would be copied and write nothing. The cascade and keep-tag moves are not computed, so those report fields stay empty |
| `--identifier`, `-i` | `IDENTIFIER` | Full target reference, when the repository path or tag changes too; required when SOURCE names a digest. Conflicts with `--to` |
| `--keep-tag` |  | Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest published (default) |
| `--no-keep-tag` |  | Skip the keep tag |
| `--no-referrers` |  | Leave the signatures, SBOMs and attestations behind |
| `--platform`, `-p` | `PLATFORM` | Platform to copy. Repeatable |
| `--referrers` |  | Carry the signatures, SBOMs and attestations anchored to each manifest, plus the cosign `.sig`, `.att` and `.sbom` sidecar tags (default). A sidecar tag the target holds under a different manifest is reported, never overwritten, and the exit is 65. A target without the OCI 1.1 Referrers API exits 82 |
| `--to` | `REGISTRY` | Rewrite only the registry host, keeping the repository path and tag. Conflicts with `--identifier` |
| `--with-description` |  | Also copy the repository description (`__ocx.desc`): README and logo |

## `ocx package create` {#package-create}

Creates an archive from a local package directory.

Bundles a directory into a compressed package archive ready for publishing. When the package metadata declares dependencies, the dependency graph is validated for cycles here, before the package reaches a registry.

With `--metadata`, `create` also compiles dependency pins. It validates the sidecar and always rewrites it in canonical form next to the bundle, never as a byte copy of the input file.

`--platform` is then required.

It states what the artifact demands, not what the build host supplies. A static musl binary cross-built on a glibc host demands neither the host's libc nor its architecture.

Every dependency is pinned for that platform, and the platform is the label the package is published under. A dependency with no compatible candidate, or with more than one, fails with exit 65.

For `--platform any` a dependency must offer an `any` build, and a direct digest pin anywhere in the dependency list is rejected (exit 65). Resolution honors `--remote`, `--offline` and `--frozen`: with the last two a tag that is not cached exits 81, and a tag absent from the index exits 79.

`create` records the platform and identifier in a build receipt, `<stem>-receipt.json`, beside the bundle. The receipt has no schema and is never pushed; read it with `ocx package receipt`.

`ocx package push` and `ocx package test` fall back to it for a `--platform` they are not given. `push --identifier repo` without a tag takes the recorded version when the receipt names the same repository.

It is a fallback, never an authority.

With neither `--platform` nor `--identifier` no receipt is written.

`create` also compiles the `binaries` claim. With neither scan flag it scans the content tree and fills an absent `binaries` into the resolved sidecar, and passes a declared list through unverified.

`--bin-scan` also verifies a declared list: an executable missing from it, or a declared name that exists but is not executable, fails with exit 65. `--no-bin-scan` never scans.

The scan only fills the sidecar written next to the output, never the authored `-m` file. It considers only `${installPath}`-rooted `path` variables with interface visibility and no render modifier.

Anything else needs a hand-authored `binaries`.

A scan needs a host that can evaluate the target's executable convention. A Windows host targeting anything but Windows (including `any`) therefore fails with exit 65 in every mode that would scan.

The exception is metadata with no `${installPath}`-rooted `PATH` variable of interface visibility, which leaves nothing to scan. Hand-author `binaries` or pass `--no-bin-scan`.

An unreadable content directory exits 74.

With `--metadata` and a Linux or `any` target, `create` also checks the libc the binaries need against the `os.features` the platform declares. It reads the ELF interpreter header of every file placed on an interface `PATH` directory.

The check runs before the archive is written, so a refusal leaves no bundle. It reads only the dynamic loader.

Other shared libraries and glibc versions are not checked, and non-Linux targets are not checked at all.

`--no-libc-lint` skips it with a warning.

```text
ocx package create [OPTIONS] <PATH>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--bin-scan` |  | Scan the content tree for executables the package puts on `PATH`, verifying a declared `binaries` claim or filling an absent one |
| `--compression-level` | `fast\|best\|default` | Compression level to use for the package bundle |
| `--extract` |  | Treat PATH as an archive (tar, gzip, xz, zstd, bzip2 or zip, read from the file name) and bundle what it extracts to |
| `--force`, `-f` |  | Force overwrite of output file if it already exists |
| `--identifier`, `-i` | `IDENTIFIER` | Identifier the bundle will be published under (e.g. `repo:2.0.0`) |
| `--metadata`, `-m` | `METADATA` | Path to a `metadata.json` file to validate, resolve, and write alongside the output bundle |
| `--no-bin-scan` |  | Skip the executable scan; the `binaries` field passes through unchanged, whether declared or absent |
| `--no-libc-lint` |  | Skip the libc check on the packaged binaries |
| `--output`, `-o` | `OUTPUT` | Output file or directory, if a directory is provided the filename will be inferred |
| `--platform`, `-p` | `PLATFORM` | Platform of the package content (e.g. `linux/amd64`, or `any` for platform-agnostic content) |
| `--strip-components` | `N` | Drop the leading N path components of every extracted entry |
| `--threads`, `-j` | `THREADS` | Number of compression threads (0 = auto-detect, 1 = single-threaded) |

## `ocx package description` {#package-description}

Publish or fetch a package repository's catalog description.

`description push` writes the description and `description pull` shows it.

```text
ocx package description <COMMAND>
```

## `ocx package description pull` {#package-description-pull}

Show description metadata (title, description, keywords) for one or more packages.

Reads each description from the `__ocx.desc` tag in the registry. JSON output carries a `descriptions` object keyed by the requested identifier, even for a single package.

Each value has `published` and, when it is true, the `title`, `description` and `keywords` the repository publishes.

Plain output prints a `== <id> ==` header line per package, even for one, followed by its description fields. A package in a namespace a configured index serves is read from the registry that index points it to.

The output stays keyed by the identifier as given.

`--save-readme` / `--save-logo` write a fixed filename and so require exactly one package.

```text
ocx package description pull [OPTIONS] <PACKAGE>...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--save-logo` | `SAVE_LOGO` | Save the logo to this file or directory (single package only) |
| `--save-readme` | `SAVE_README` | Save the README to this file or directory (single package only) |

## `ocx package description push` {#package-description-push}

Push or update description metadata for a package repository.

Pushes a README, optional logo, and catalog annotations to the __ocx.desc tag. When updating an existing description, only the provided fields are changed.

Omitted fields are preserved from the current description, so a `--title`-only push does not blank the README.

At least one metadata option, or `--from`, is required. `--from` cannot be combined with the field options.

The identifier names the repository; a tag on it is ignored.

```text
ocx package description push [OPTIONS] <IDENTIFIER>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--description` | `DESCRIPTION` | One-line summary for catalog display (sets org.opencontainers.image.description) |
| `--from` | `SOURCE` | Copy the whole description from another package repository; a source with no description or no repository exits 79 and leaves the target untouched |
| `--keywords` | `KEYWORDS` | Comma-separated search keywords (sets sh.ocx.keywords) |
| `--logo` | `LOGO` | Path to an optional logo image (PNG or SVG); bytes that do not match the extension exit 65 and leave the published description untouched |
| `--readme` | `README` | Path to the README markdown file; required on the first push to a repository with no description |
| `--title` | `TITLE` | Short title for catalog display (sets org.opencontainers.image.title) |

## `ocx package push` {#package-push}

Publish a package's layers and metadata to a registry.

Uploads zero or more layers as OCI blobs, in the order given. It records them in one image manifest for the single platform this invocation publishes.

A zero-layer push produces a config-only OCI artifact (a referrer-only or description-only manifest) and requires `--metadata`.

To publish several platforms, run `push` once per platform under the same tag: each push is merged into the existing image index, never replacing it.

`push` makes no dependency-resolution decisions; it is a gate. Every dependency in the metadata sidecar must already carry a manifest digest pin for the published platform (`ocx package create` resolves them).

Otherwise `push` fails before uploading anything.

Exit 64 covers a missing platform or identifier with no build receipt to fall back on. It also covers a bare `--ci-annotations` followed by a provider-shaped layer.

Exit 65 covers a dependency that is not digest-pinned or pins an image index. It also covers an `any` push that pins a digest its own index does not advertise as `any`.

Exit 79 covers a pinned manifest absent from its registry, and exit 80 covers failed authentication to a dependency's registry.

`--annotation` and `--ci-annotations` write annotations onto the image index of every tag the push writes, cascade tags included. Omitting both leaves existing annotations untouched, so an earlier annotation survives a later plain push.

Title, description and keywords for catalog display come from `ocx package description push`, not from annotations.

`--format json` reports `platform_digests`, the platform manifest digest of each pushed platform keyed by platform, never the index digest. It also reports `annotations_written`, the annotations that landed on the index.

Both keys are omitted when empty.

`signatures` has one row per platform `--sign` signed, and `attestation` records the `--sbom` outcome (`status`, `predicate_type` and `signed`, or `kind` and `message` on failure). `aliases_written` lists the tags `--default` aliased onto the bare track.

```text
ocx package push [OPTIONS] [LAYERS]...
```

| Flag | Value | Description |
| --- | --- | --- |
| `--annotation` | `KEY=VALUE` | Record an OCI annotation on the published image index. Repeatable |
| `--build-timestamp` | `datetime\|date\|none` | Append a UTC build-metadata segment to the published tag, which must be `X.Y.Z` (variant or pre-release allowed) with no build metadata yet |
| `--cascade` |  | Will cascade rolling releases, ie. pushing 1.2.3 will also update 1.2, 1, etc |
| `--ci-annotations` | `github (github-actions)\|gitlab (gitlab-ci)` | Stamp OCI annotations from the CI environment |
| `--default` |  | Let the pushed tag's variant also own the un-prefixed version track |
| `--fulcio-url` | `URL` | Fulcio CA endpoint (the keyless certificate issuer) |
| `--identifier`, `-i` | `IDENTIFIER` | Identifier under which the package is published (e.g. `repo:2.0.0`) |
| `--keep-tag` |  | Write a `__ocx.keep.sha256-<hex>` tag for each platform manifest published (default) |
| `--key` | `REF` | Sign or verify with a key pair instead of keyless Sigstore |
| `--metadata`, `-m` | `METADATA` | Path to the package metadata JSON file. Defaults to a sibling of the first file layer (e.g. `pkg.tar.gz` -> `pkg-metadata.json`). Required when no file layers are provided |
| `--no-keep-tag` |  | Skip the keep tag |
| `--no-rekor-upload` |  | Skip the Rekor entry |
| `--platform`, `-p` | `PLATFORM` | Target platform (e.g. `linux/amd64`, or `any` for platform-agnostic content) |
| `--rekor-upload` |  | Record the signature in the Rekor transparency log |
| `--rekor-url` | `URL` | Rekor transparency-log endpoint |
| `--sbom` | `PATH` | After the push, attest this CycloneDX SBOM against the pushed manifest. A failure does not undo the push, is recorded in the report and sets the exit code |
| `--sign` |  | Sign each platform manifest this push writes, inline. A failure does not undo the push, is recorded in the report and sets the exit code |
| `--signature-format` | `bundle\|simplesigning\|both` | Signature wire format: bundle (default), simplesigning, or both. A sidecar appends a layer, never replacing one; `attest` with no signing identity refuses it (exit 64) |
| `--tags-file` | `PATH` | After a successful push, append the pushed tag and any cascade tags to this file, one per line (creating it if absent), so `ocx package announce --tags-file` can pick them up |

## `ocx package sign` {#package-sign}

Sign a published package's manifest (keyless Sigstore, via OCI Referrers).

Keyless by default, or under `--key`. Keyless, it generates an ephemeral ECDSA P-256 keypair and has Fulcio issue a short-lived certificate binding the key to your OIDC identity.

It then signs the manifest digest and logs a Rekor v1 `hashedrekord` entry (Rekor v2 is not supported yet).

The resulting Sigstore bundle v0.3 is pushed as a referrer of the target manifest, where `ocx package verify` and `cosign verify` find it. The push uses the Referrers API where the registry has one and the fallback index where it does not.

`--signature-format simplesigning` (or `both`) also writes the cosign `sha256-<hex>.sig` sidecar tag for consumers that read only that shape.

Signing needs network access: `--offline` exits 81.

`--tags` and `--tags-file` sweep the indices a publish left behind. `push` signs each platform manifest inline, but an image index digest is final only after the last platform merges.

So `ocx package sign --tags-file tags.txt <identifier>` signs the index each recorded tag now resolves to. `--platform` is refused alongside either flag.

A tag that resolves to a single manifest is skipped with a warning. The sweep continues past a per-tag failure and exits non-zero at the end with every failure listed.

When every failure shares one exit code, the sweep returns that code; a mix returns 1.

A signature is a referrer of a digest, not a tag, so the cascade aliases of one release share one signature. Tags after the first to name a given index are reported `covered`.

`sign` appends rather than replaces, so re-running a sweep adds one referrer per distinct index, and `verify` reads at most eight signature candidates.

The OIDC identity token comes from the first available of `--identity-token-file`, `--identity-token-stdin`, `OCX_IDENTITY_TOKEN`, ambient CI detection (GitHub Actions, GitLab CI, CircleCI) and interactive browser OAuth, which `--no-tty` suppresses. There is deliberately no flag taking the token itself: it would land in shell history and process listings.

`--format json` reports `identifier`, `subject_digest`, `signer` and `transparency_log_index`, with one `legs` row per signature format written.

```text
ocx package sign [OPTIONS] <IDENTIFIER>
```

| Flag | Value | Description |
| --- | --- | --- |
| `--fulcio-url` | `URL` | Fulcio CA endpoint (the keyless certificate issuer) |
| `--identity-token-file` | `PATH` | Read the OIDC identity token from this file (highest precedence) |
| `--identity-token-stdin` |  | Read the OIDC identity token from stdin (second precedence) |
| `--key` | `REF` | Sign or verify with a key pair instead of keyless Sigstore |
| `--no-cache` |  | Bypass the referrers-capability cache for this invocation |
| `--no-rekor-upload` |  | Skip the Rekor entry |
| `--no-tty` |  | Suppress the interactive browser OAuth fallback (CI / headless) |
| `--platform`, `-p` | `PLATFORM` | Narrow into one platform of an image index |
| `--rekor-upload` |  | Record the signature in the Rekor transparency log |
| `--rekor-url` | `URL` | Rekor transparency-log endpoint |
| `--signature-format` | `bundle\|simplesigning\|both` | Signature wire format: bundle (default), simplesigning, or both. A sidecar appends a layer, never replacing one; `attest` with no signing identity refuses it (exit 64) |
| `--tags` | `TAG` | Tags to sweep. Repeatable, and accepts a comma-separated list |
| `--tags-file` | `PATH` | Read tags from a file, one per line or comma-separated |

## `ocx package test` {#package-test}

Materialize a package locally (no registry round-trip) and run a command in its env.

Mirrors the argument shape of `ocx package push`: the identifier as `-i/--identifier`, then layers, then `--platform` and `--metadata`. The package is built into a temp directory, dependencies are installed, and the environment is composed as for `ocx exec`.

Either a trailing `-- CMD [ARGS...]` or `--script PATH` runs in it, and exactly one of the two is required.

The package itself needs no registry round-trip. With a `[patches]` tier configured, though, the command contacts the patch registry and composes the companions matching the identifier, as an install would.

The temp directory is deleted on any exit unless `--keep` or `--output` is given.

A bare command name is looked up in the package's own directories before the host `PATH`, and the package's copy of a name is never skipped. If the package ships an executable of that name, it runs.

If it ships the name but the file is not executable, the command fails with exit 65 naming the path and its permission bits. It does not fall back to a same-named host binary and report a pass for something the package does not contain.

If it does not ship the name at all, the name resolves on the host `PATH` with a warning naming the directories searched.

A name containing a path separator addresses a file directly. Catch a missing executable bit at publish time with `ocx package create --bin-scan`.

Layers use the same syntax as `package push`, including the `:strip=N,prefix=P,from=REPO` suffix (`from` is ignored here, since nothing is pushed). A digest layer is fetched on demand; a missing digest blob under `--offline` or `--frozen` exits 81.

```text
ocx package test [OPTIONS] --identifier <IDENTIFIER> [LAYERS]... [-- <COMMAND>...]
```

| Flag | Value | Description |
| --- | --- | --- |
| `--clean` |  | Strip ambient parent env before composing - only `OCX_*` config and composed package vars reach the child. Mirrors `ocx exec --clean` |
| `--env` | `NAME\|KEY[:TYPE[:SEP]]=VALUE` | Set an environment variable, or pass one by name. An unset NAME adds nothing; a TYPE without a value exits 64 |
| `--identifier`, `-i` | `IDENTIFIER` | Identifier under which the package is materialized. Tag form (`repo:tag`) only; an explicit `@digest` is rejected (the digest is computed locally during this command and supplying one would conflict) |
| `--junit` | `PATH` | Write a JUnit XML report for the scripted run to PATH, truncating an existing file, even on a red run; needs `--script`, and an unwritable path exits 74 |
| `--keep` |  | Preserve the temp build directory after the command exits. Path is printed to stderr. Default temp root is `$OCX_HOME/temp/test/` |
| `--metadata`, `-m` | `METADATA` | Path to the package metadata JSON file. Defaults to a sibling of the first file layer (e.g. `pkg.tar.gz` -> `pkg-metadata.json`). Required when no file layers are provided |
| `--output`, `-o` | `OUTPUT` | Materialize into DIR instead of an auto-managed temp dir. DIR must not exist (created by ocx) or be empty. Implies keep - the dir is never deleted by ocx. Must reside on the same filesystem as `$OCX_HOME/layers/` - hardlink assembly does not fall back to copy |
| `--platform`, `-p` | `PLATFORM` | Target platform (e.g. `linux/amd64`). An explicit value is used as given. Omit it to take the platform the build receipt beside the bundle recorded; a usage error (exit 64) when neither names one. Parity with `package push` |
| `--script` | `PATH\|-` | Path to a Starlark test script. Mutually exclusive with the trailing command. When given, the materialized package env is interpreted by the embedded engine instead of exec'ing a command |
| `--self` |  | Compose the package's private env surface (default: interface surface). Same semantics as `ocx exec --self` / `ocx env --self` |
