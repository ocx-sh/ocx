// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Subcommand;

/// OCI-tier package commands.
///
/// These commands operate on OCI identifiers directly and never consult
/// `ocx.toml`.  They own the `candidate`/`current` floating symlinks. The
/// toolchain-tier counterparts (`ocx env`, `ocx exec`) stay at root.
#[derive(Subcommand)]
pub enum Package {
    // No doc: a variant doc replaces the struct's help and would drop the credential guidance, which
    // `package_announce::tests::both_write_commands_render_the_credential_guidance` asserts.
    Announce(super::package_announce::PackageAnnounce),
    /// Attach an in-toto attestation to a published package manifest.
    #[command(long_about = "\
        Attach an in-toto attestation to a published package manifest.\n\n\
        Signs a caller-supplied predicate document (an SBOM, provenance, VEX) with the same \
        keyless Sigstore machinery `sign` uses. It publishes the result as an OCI referrer on the target \
        manifest.\n\n\
        The shape depends on whether a signing identity is visible in the environment (the same \
        override-token and ambient-CI check `sign` uses). With one present, the predicate is \
        wrapped in a DSSE-enveloped in-toto Statement naming the target manifest digest as its \
        `subject`. The Statement is signed through the keyless pipeline `sign` uses. It is pushed as a referrer that \
        `ocx package verify --attestation` and `ocx package sbom` find.\n\n\
        With none visible, `--type` must resolve to an SBOM media type (`cyclonedx`, `spdx`, \
        `spdxjson`, or a full URI resolving to one). The predicate document is pushed verbatim \
        as the referrer payload, typed by its own media type, with no envelope, certificate or \
        Rekor entry. Any other `--type` without a signing identity is refused before any network \
        call (exit 64, `unsigned_type_unsupported`), since an unsigned provenance or \
        vulnerability statement carries no attribution worth publishing.\n\n\
        If an identity is detected but acquiring a usable token then fails (Fulcio unreachable, \
        an ambient CI token rejected), that is a hard error. The command never falls back to \
        publishing unsigned. `--offline` exits 81, checked before the predicate file is opened or \
        any token is resolved.\n\n\
        `ocx package push --sbom` is sugar for `attest --type cyclonedx` against the digest a \
        push just wrote. Use `attest` directly for a standalone predicate, an SPDX \
        predicate, several predicate types on one manifest, or an attestation for something this \
        invocation did not publish. The `--tags` sweep behaves exactly as for `sign`, with \
        one attestation per distinct index, and the token sources are identical to `sign`.\n\n\
        `--format json` reports `identifier`, `subject_digest`, `predicate_type` and `signed`, \
        plus the `referrer_digest`, `bundle_digest` or `sidecar_digest` that was written.")]
    Attest(super::package_attest::PackageAttest),
    /// Audit and repair the rolling tags a published package cascades into.
    ///
    /// Pushing `3.28.1` is supposed to leave `3.28`, `3` and `latest` pointing
    /// at it for every platform it ships. `check` reports where that is no
    /// longer true; `repair` re-points the tags using content the registry
    /// already holds.
    #[command(subcommand)]
    Cascade(super::package_cascade::CascadeGroup),
    // No doc comment: see `Announce` above.
    Claim(super::package_claim::PackageClaim),
    /// Promote an already-published package to another registry or repository.
    #[command(long_about = "\
        Promote an already-published package to another registry or repository.\n\n\
        Copies the platform manifests, their blobs and their referrers verbatim, so the digest \
        never moves and every signature and lock pin naming it still resolves. The target's index \
        is merged one platform at a time, never overwritten.\n\n\
        The source is `registry/repository:tag`, or `registry/repository@sha256:<hex>` for one \
        platform manifest, in which case `--platform` is required because a platform manifest \
        carries no platform of its own. A source in a namespace a configured index serves, such \
        as `ocx.sh`, is resolved by that index. Its tag names the platforms the index recorded, not what the registry's tag holds now. A tag the index does not hold or has \
        yanked is refused. Its platform manifests are read from the registry the index points \
        to.\n\n\
        The target is written exactly as given.\n\n\
        Three kinds of object follow three rules, because only one is content. A platform \
        manifest and its blobs are copied byte for byte, so the digest never changes.\n\n\
        The tag's image index is merged one platform at a time, so copying `linux/amd64` never \
        removes a `darwin/arm64` the target already offers. Rolling tags (`1.4`, `1`, `latest`) \
        are recomputed against the target's tag list under `--cascade`, never carried over from \
        the source.\n\n\
        The report has one row per platform the target offers afterwards, labelled `added`, \
        `unchanged`, `replaced` or `kept (not in source)`. That tells a filtered promotion that \
        left a mixed index apart from a complete one.\n\n\
        A second identical copy changes nothing but is not free. Every platform re-fetches and \
        re-PUTs its leaf manifest and, with `--referrers`, its referrer set. Each index is \
        re-PUT. Only blob bodies are skipped, via a HEAD against the target.\n\n\
        A copy is not a re-sign. The signature travels with the manifest and still names the \
        identity that signed it in the source environment. Sign again at the target when \
        policy requires a production-specific attestation. `--offline` exits 81, since a copy \
        needs network access to both registries.")]
    Copy(super::package_copy::PackageCopy),
    /// Creates an archive from a local package directory.
    #[command(long_about = "\
        Creates an archive from a local package directory.\n\n\
        Bundles a directory into a compressed package archive ready for publishing. When the \
        package metadata declares dependencies, the dependency graph is validated for cycles \
        here, before the package reaches a registry.\n\n\
        With `--metadata`, `create` also compiles dependency pins. It validates the sidecar and \
        always rewrites it in canonical form next to the bundle, never as a byte copy of the \
        input file. `--platform` is then required.\n\n\
        It states what the artifact demands, not what the build host supplies. A static musl \
        binary cross-built on a glibc host demands neither the host's libc nor its architecture. Every dependency is pinned for that platform, and the platform is the label the package \
        is published under. A dependency with no compatible candidate, or with more than one, \
        fails with exit 65.\n\n\
        For `--platform any` a dependency must offer an `any` build, and a direct digest pin \
        anywhere in the dependency list is rejected (exit 65). Resolution honors `--remote`, \
        `--offline` and `--frozen`: with the last two a tag that is not cached exits 81, and a \
        tag absent from the index exits 79.\n\n\
        `create` records the platform and identifier in a build receipt, `<stem>-receipt.json`, \
        beside the bundle. The receipt has no schema and is never pushed; read it with `ocx \
        package receipt`.\n\n\
        `ocx package push` and `ocx package test` fall back to it for a `--platform` they are not \
        given. `push --identifier repo` without a tag takes the recorded version when the \
        receipt names the same repository. It is a fallback, never an authority.\n\n\
        With neither `--platform` nor `--identifier` no receipt is written.\n\n\
        `create` also compiles the `binaries` claim. With neither scan flag it scans the content \
        tree and fills an absent `binaries` into the resolved sidecar, and passes a declared list \
        through unverified.\n\n\
        `--bin-scan` also verifies a declared list: an executable missing from it, or a declared \
        name that exists but is not executable, fails with exit 65. `--no-bin-scan` never scans.\n\n\
        The scan only fills the sidecar written next to the output, never the authored `-m` file. It considers only `${installPath}`-rooted `path` variables with interface visibility and \
        no render modifier. Anything else needs a hand-authored `binaries`.\n\n\
        A scan needs a host \
        that can evaluate the target's executable convention. A Windows host targeting \
        anything but Windows (including `any`) therefore fails with exit 65 in every mode that would scan. \
        The exception is metadata with no `${installPath}`-rooted `PATH` variable of interface visibility, \
        which leaves nothing to scan. Hand-author `binaries` or pass `--no-bin-scan`.\n\n\
        An unreadable content directory exits 74.\n\n\
        With `--metadata` and a Linux or `any` target, `create` also checks the libc the binaries \
        need against the `os.features` the platform declares. It reads the ELF interpreter \
        header of every file placed on an interface `PATH` directory. The check runs before the \
        archive is written, so a refusal leaves no bundle. It reads only the dynamic loader. Other shared libraries and glibc versions are not checked, and non-Linux targets are not \
        checked at all.\n\n\
        `--no-libc-lint` skips it with a warning.")]
    Create(super::package_create::PackageCreate),
    /// Publish or fetch a package repository's catalog description.
    ///
    /// `description push` writes the description and `description pull` shows it.
    #[command(subcommand)]
    Description(super::package_description::DescriptionGroup),
    /// Deprecated spelling of `ocx package description push`; removed in 0.7.
    #[command(name = "describe", hide = true)]
    DeprecatedDescribe(super::package_description_push::PackageDescriptionPush),
    /// Deprecated spelling of `ocx package description pull`; removed in 0.7.
    #[command(name = "info", hide = true)]
    DeprecatedInfo(super::package_description_pull::PackageDescriptionPull),
    /// Show the dependency tree for one or more installed packages.
    #[command(long_about = "\
        Show the dependency tree for one or more installed packages.\n\n\
        Default output is a tree of declared dependencies. Each line names a package with its \
        tag, the visibility its parent declared for the edge (absent on a root), the digest, and \
        `repeated` when the package already appeared earlier in the tree. A repeated package is not \
        expanded again.\n\n\
        `--flat` prints the resolved evaluation order instead, the exact order `ocx package exec` \
        and `ocx package env` compose the environment in, one row of package, visibility and digest \
        each.\n\n\
        `--why <DEP>` traces every path from the given packages to one dependency, each printed as \
        a chain such as `myapp:1.0 -> ocx.sh/kitware/cmake:3.28 -> ocx.sh/gcc:13`. With several \
        packages, the command builds their combined graph.\n\n\
        Works on locally present packages only (no auto-install) and never consults `ocx.toml`. \
        It walks structural metadata and never resolves env values, so a package whose env \
        references an undeclared dependency still appears.\n\n\
        Only two things drop a package from the output, each logged at warn level. One is a \
        `metadata.json` that fails to parse or validate. The other is an env-var modifier type this ocx \
        does not recognize.")]
    Deps(super::deps::Deps),
    /// Print the resolved environment variables for one or more installed packages.
    #[command(long_about = "\
        Print the resolved environment variables for one or more installed packages.\n\n\
        Plain output is an aligned `Key`, `Type`, `Value` table. When binaries, entry points or \
        integration namespaces are admitted, it ends with one hint line counting each, for example \
        `5 binaries available (...); 2 integration namespaces (...); use --format json for the full \
        list`. `--format json` emits `items` plus `binaries`, `entrypoints` and `integrations` \
        arrays; `--shell[=NAME]` emits the only eval-safe form. A package's dependencies contribute their variables in topological order, \
        dependencies before dependents.\n\n\
        Packages missing from the object store are installed, with their transitive dependencies, \
        and a package a `[[trust.policy]]` covers is signature-verified before its environment is \
        composed.\n\n\
        `--candidate`, `--current` and `--link` resolve through an install symlink instead and \
        never auto-install. `--self` selects the publisher's own view (private and public \
        entries) instead of the consumer view (public and interface); `integrations` is always \
        empty under `--self`.")]
    Env(super::env::Env),
    /// Inspect one or more package references (candidates, metadata, or resolution chain).
    #[command(long_about = "\
        Inspect one or more package references (candidates, metadata, or resolution chain).\n\n\
        Read-only: nothing is installed and no symlinks are created. Default mode resolves \
        a tag through the index, so a cache miss may populate the local index and blob cache. \
        Unlike `ocx package test`, the identifier accepts an explicit `@digest`.\n\n\
        The output adapts to the reference. An image-index reference lists its platform \
        candidates (platform, child digest, media type, size) with no metadata loaded and no \
        platform selected.\n\n\
        A single-manifest reference, a flat tag or an `@digest` at an image manifest, emits the \
        declared metadata and the manifest's layers. `--resolve` platform-selects through the \
        index and adds the resolution chain (index, manifest, config).\n\n\
        `--closure` walks the declared dependencies and adds the metadata-only dependency closure \
        plus the `interface` and `private` surface projections. On an image index it \
        platform-selects first, as `--resolve` does, so it never returns the candidates listing.\n\n\
        `--closure` fails closed. If any dependency's manifest or metadata cannot be loaded, the \
        whole request fails rather than rendering a partial closure. A conflict exits 65 with \
        the report still printed in full. JSON is one envelope with `platform` (only with \
        `--resolve` or `--closure`), `packages` (one entry per requested identifier, in request \
        order) and `env` (the per-invocation `--env` overrides in flag order).\n\n\
        A script testing whether `--closure` ran checks the `closure` key, not `resolution`, \
        which follows the reference's shape.")]
    Inspect(super::package_inspect::PackageInspect),
    /// Install packages from a local or remote index (no `ocx.toml` touched).
    #[command(long_about = "\
        Install packages from a local or remote index (no `ocx.toml` touched).\n\n\
        Downloads the packages and their transitive dependencies into the object store and \
        creates a candidate symlink for each requested package; dependencies get no install \
        symlinks. This is the OCI-tier install and never touches `ocx.toml`; use `ocx add` for a \
        project.\n\n\
        A package that a `[[trust.policy]]` covers is verified before any layer downloads (see \
        `--verify`).\n\n\
        Installing a foreign platform fills the object store but leaves the host's candidate and \
        `current` symlinks alone. A platformless lookup therefore never resolves to a package the host \
        cannot run. On Windows a warning is printed when `PATHEXT` lacks `.CMD`, which the \
        generated entry-point launchers need.")]
    Install(super::install::Install),
    // No doc comment: see `Announce` above; `PackagePrune`'s `long_about` is the help.
    Prune(super::package_prune::PackagePrune),
    /// Downloads packages into the local object store without creating install symlinks.
    #[command(long_about = "\
        Downloads packages into the local object store without creating install symlinks.\n\n\
        Populates only the content-addressed object store; no candidate or current symlinks are \
        created. Transitive dependencies are pulled as well.\n\n\
        This is the primitive for CI, where reproducibility matters and symlink management is \
        unnecessary. For a project driven by `ocx.lock`, use `ocx pull` instead.\n\n\
        With a `[patches]` tier configured, the package's patch descriptors are revalidated and \
        the matching companions installed, as `package install` does; a `required` companion that \
        cannot be found exits 79. A policy-covered package is signature-verified before it \
        downloads, as `package install` does.\n\n\
        Reports the package root of each package, the same directory `package which` and `package \
        exec` resolve to. Concurrent pulls of one digest are safe.")]
    Pull(super::package_pull::PackagePull),
    /// Publish a package's layers and metadata to a registry.
    #[command(long_about = "\
        Publish a package's layers and metadata to a registry.\n\n\
        Uploads zero or more layers as OCI blobs, in the order given. It records them in one \
        image manifest for the single platform this invocation publishes. A zero-layer push \
        produces a config-only OCI artifact (a referrer-only or description-only manifest) and \
        requires `--metadata`.\n\n\
        To publish several platforms, run `push` once per platform under the same tag: each push \
        is merged into the existing image index, never replacing it.\n\n\
        `push` makes no dependency-resolution decisions; it is a gate. Every dependency in the \
        metadata sidecar must already carry a manifest digest pin for the published platform \
        (`ocx package create` resolves them). Otherwise `push` fails before uploading anything.\n\n\
        Exit 64 covers a missing platform or identifier with no build receipt to fall back on. It also covers a \
        bare `--ci-annotations` followed by a provider-shaped layer. Exit 65 covers a dependency that \
        is not digest-pinned or pins an image index. It also covers an `any` push that pins a digest its own \
        index does not advertise as `any`. Exit 79 covers a pinned manifest absent from its registry, and exit 80 covers failed authentication to a dependency's registry.\n\n\
        `--annotation` and `--ci-annotations` write annotations onto the image index of every tag \
        the push writes, cascade tags included. Omitting both leaves existing annotations \
        untouched, so an earlier annotation survives a later plain push. Title, description and \
        keywords for catalog display come from `ocx package description push`, not from \
        annotations.\n\n\
        `--format json` reports `platform_digests`, the platform manifest digest of each pushed \
        platform keyed by platform, never the index digest. It also reports `annotations_written`, \
        the annotations that landed on the index. Both keys are omitted when empty.\n\n\
        `signatures` has one row per platform `--sign` signed, and `attestation` records the \
        `--sbom` outcome (`status`, `predicate_type` and `signed`, or `kind` and `message` on \
        failure). `aliases_written` lists the tags `--default` aliased onto the bare track.")]
    Push(super::package_push::PackagePush),
    /// Print the build receipt `ocx package create` wrote beside a bundle
    #[command(long_about = "\
        Print the build receipt `ocx package create` wrote beside a bundle\n\n\
        The receipt records the `--platform` and `--identifier` a build was given; `ocx package \
        push` and `ocx package test` fall back to it when the flag is omitted. JSON output is \
        `{\"platform\": ..., \"identifier\": ...}`, each key present only when the build recorded \
        it.\n\n\
        The receipt is resolved beside the bundle by the stem rule `push` uses \
        (`hello-1.0.0.tar.xz` -> `hello-1.0.0-receipt.json`). The file itself is a build \
        artifact; this report is the contract an SDK reads instead.\n\n\
        Exits 79 when no receipt sits beside the bundle and 65 when the file exists but is not a \
        receipt this ocx can read.")]
    Receipt(super::package_receipt::PackageReceiptCommand),
    /// List or extract the SBOM attestations a published package carries.
    #[command(long_about = "\
        List or extract the SBOM attestations a published package carries.\n\n\
        The read-side counterpart to `ocx package attest`. A manifest can carry a signed \
        attestation (a DSSE bundle with a Fulcio certificate and a Rekor entry behind it). It can also carry an \
        unsigned attach with no signature over it. Either is found as a referrer (what `oras attach` \
        writes) or as the cosign `sha256-<hex>.sbom` sidecar tag. No referrer listing \
        reaches that tag, so it is read by name.\n\n\
        The payload layer's own media type is the only claim about the document, so it is both \
        the gate and the label. A layer typed outside the SBOM set is refused (exit 65, \
        `sbom_media_type_unsupported`).\n\n\
        Each invocation runs in one of two modes. `--verify` lists only documents whose signature \
        passed every check `verify --attestation` runs; an unsigned attach is refused rather than \
        listed (exit 77 when it is all the package carries).\n\n\
        It is the default whenever `--certificate-identity` or `--certificate-oidc-issuer` is \
        given or a `[[trust.policy]]` covers the package. `--no-verify` runs no cryptography at \
        all and reports every document as `verified: false` with no signer identity. It is the \
        default when no identity source resolves, and it is what lets `sbom` work with no Sigstore \
        setup.\n\n\
        Naming both flags is not an error (the later wins), but `--no-verify` with an identity \
        exits 64, and so does `--verify` with no identity source at all. An unverified entry \
        carries `verified: false` in plain and JSON output, and `summary.verification` names the \
        mode.\n\n\
        `--output` and `--summary` are mutually exclusive with each other and with the default \
        listing; `--summary` works in both modes.\n\n\
        With `--platform`, the command reads the platform manifest and the index behind it. A \
        platform SBOM supersedes an index SBOM only within the same `predicate_type`.\n\n\
        A superseded document stays in JSON with `shadowed: true`, while the plain table shows the \
        winner. Two documents of one type on the same subject shadow neither.\n\n\
        Candidates the scan rejected appear in `refused`, never silently. `summary.status` is \
        `partial_failure` whenever `refused` is non-empty, else `success`.\n\n\
        Each refused entry carries a `reason` and a `reason_kind` slug. Scripts branch on \
        `reason_kind`, never on `reason`. A `--summary` document that cannot be read gets \
        `sbom_summary_failed`.\n\n\
        Plain listings truncate `refused` to the first 20. JSON never truncates it.")]
    Sbom(super::package_sbom::PackageSbom),
    /// Set the current version of one or more packages.
    ///
    /// Resolves each package, checks that its content is already in the local object store, and
    /// points the package's `current` symlink at it. Nothing is downloaded.
    ///
    /// Entry-point name collisions surface when the packages are consumed (`ocx package env`, `ocx
    /// package exec`), not at select.
    Select(super::select::Select),
    /// Remove the current-version symlink for one or more packages.
    ///
    /// The package is deselected but not uninstalled: its candidate symlink and object-store
    /// content stay. Removing the `current` symlink also makes the package's entry-point launchers
    /// unreachable through `current/entrypoints/`.
    ///
    /// Removing an already-absent link is not an error. Use `ocx package uninstall` to remove the
    /// installed files too.
    Deselect(super::deselect::Deselect),
    /// Sign a published package's manifest (keyless Sigstore, via OCI Referrers).
    #[command(long_about = "\
        Sign a published package's manifest (keyless Sigstore, via OCI Referrers).\n\n\
        Keyless by default, or under `--key`. Keyless, it generates an ephemeral ECDSA P-256 \
        keypair and has Fulcio issue a short-lived certificate binding the key to your OIDC \
        identity. It then signs the manifest digest and logs a Rekor v1 `hashedrekord` entry (Rekor v2 \
        is not supported yet).\n\n\
        The resulting Sigstore bundle v0.3 is pushed as a referrer of the target manifest, where \
        `ocx package verify` and `cosign verify` find it. The push uses the Referrers API where the \
        registry has one and the fallback index where it does not. `--signature-format \
        simplesigning` (or `both`) also writes the cosign `sha256-<hex>.sig` sidecar tag for \
        consumers that read only that shape.\n\n\
        Signing needs network access: `--offline` exits 81.\n\n\
        `--tags` and `--tags-file` sweep the indices a publish left behind. `push` signs each \
        platform manifest inline, but an image index digest is final only after the last platform \
        merges. So `ocx package sign --tags-file tags.txt <identifier>` signs the index each \
        recorded tag now resolves to. `--platform` is refused alongside either flag.\n\n\
        A tag that resolves to a single manifest is skipped with a warning. The sweep continues \
        past a per-tag failure and exits non-zero at the end with every failure listed. When every \
        failure shares one exit code, the sweep returns that code; a mix returns 1.\n\n\
        A signature is a referrer of a digest, not a tag, so the cascade aliases of one release \
        share one signature. Tags after the first to name a given index are reported `covered`. \
        `sign` appends rather than replaces, so re-running a sweep adds one referrer per distinct \
        index, and `verify` reads at most eight signature candidates.\n\n\
        The OIDC identity token comes from the first available of `--identity-token-file`, \
        `--identity-token-stdin`, `OCX_IDENTITY_TOKEN`, ambient CI detection (GitHub Actions, \
        GitLab CI, CircleCI) and interactive browser OAuth, which `--no-tty` suppresses. There is \
        deliberately no flag taking the token itself: it would land in shell history and process \
        listings.\n\n\
        `--format json` reports `identifier`, `subject_digest`, `signer` and \
        `transparency_log_index`, with one `legs` row per signature format written.")]
    Sign(super::package_sign::PackageSign),
    /// Materialize a package locally (no registry round-trip) and run a command in its env.
    #[command(long_about = "\
        Materialize a package locally (no registry round-trip) and run a command in its env.\n\n\
        Mirrors the argument shape of `ocx package push`: the identifier as `-i/--identifier`, \
        then layers, then `--platform` and `--metadata`. The package is built into a temp \
        directory, dependencies are installed, and the environment is composed as for `ocx exec`. Either a trailing `-- CMD [ARGS...]` or `--script PATH` runs in it, and exactly one of the \
        two is required.\n\n\
        The package itself needs no registry round-trip. With a `[patches]` tier configured, though, \
        the command contacts the patch registry and composes the companions matching the \
        identifier, as an install would. The temp directory is deleted on any exit unless \
        `--keep` or `--output` is given.\n\n\
        A bare command name is looked up in the package's own directories before the host `PATH`, \
        and the package's copy of a name is never skipped. If the package ships an executable of \
        that name, it runs.\n\n\
        If it ships the name but the file is not executable, the command fails with exit 65 \
        naming the path and its permission bits. It does not fall back to a same-named host \
        binary and report a pass for something the package does not contain. If it does not \
        ship the name at all, the name resolves on the host `PATH` with a warning naming the \
        directories searched.\n\n\
        A name containing a path separator addresses a file directly. Catch a missing executable \
        bit at publish time with `ocx package create --bin-scan`.\n\n\
        Layers use the same syntax as `package push`, including the `:strip=N,prefix=P,from=REPO` \
        suffix (`from` is ignored here, since nothing is pushed). A digest layer is fetched on \
        demand; a missing digest blob under `--offline` or `--frozen` exits 81.")]
    Test(super::package_test::PackageTest),
    /// Verify a published package's Sigstore signature (keyless, via OCI Referrers).
    #[command(long_about = "\
        Verify a published package's Sigstore signature (keyless, via OCI Referrers).\n\n\
        Fetches the Sigstore bundle v0.3 referrer for the target. Then it checks the Fulcio \
        certificate chain against a trust root, the Rekor Signed Entry Timestamp, and the signature \
        over the subject manifest digest. It also checks the certificate identity and OIDC issuer against \
        the identity you supply as flags or have pinned in a `[[trust.policy]]` entry. All checks \
        must pass for exit 0.\n\n\
        `--certificate-identity` and `--certificate-oidc-issuer` are optional only when a \
        `[[trust.policy]]` scope covers the target; keyless verification needs an identity from \
        one source or the other.\n\n\
        `--offline` (or `OCX_OFFLINE`) scopes to the Sigstore trust services, the Rekor-key fetch \
        and TUF, not the registry. Verify still fetches the target and its referrer from the \
        registry in every mode. Offline verification needs a pinned Rekor key from \
        `--sigstore-trusted-root` (or another trust-root rung) or a fresh trust-root cache entry.\n\n\
        With no trust root from any rung and no cache entry, verify fetches the public-good root \
        over TUF. `--sigstore-trusted-root` pins a self-hosted deployment's own.\n\n\
        A keyless cosign sidecar (`sha256-<hex>.sig`, or `.att` under `--attestation`) needs \
        transparency-log evidence of its own. Its layer must carry a `dev.sigstore.cosign/bundle` \
        annotation. The annotation's Signed Entry Timestamp must verify against the log key, and its logged \
        body must bind to that signature or envelope. A Fulcio certificate lives about ten minutes, \
        so without an entry nothing shows the signature was made while it was live. The \
        sidecar is therefore refused (exit 65, `signature_invalid`).\n\n\
        `cosign attach signature` writes no such annotation and needs `--insecure-ignore-tlog` \
        here; `ocx package sign --signature-format simplesigning` does write one. \
        `--allow-unlogged-signature` is the opt-out for air-gapped CI.\n\n\
        `--attestation` swaps the referrer type verify looks for. Instead of a Sigstore-bundle \
        signature it fetches a DSSE-enveloped in-toto Statement and runs the same pipeline against \
        it. It also checks that the Statement's `subject` names the target digest with a \
        strong algorithm. `--type` narrows which `predicateType` matches; omit it to accept any.\n\n\
        The report has the same fields in either mode. Use `ocx package sbom` when the predicate \
        content is what you need back.\n\n\
        Name the signer in one of two ways. Pass both `--certificate-identity` and \
        `--certificate-oidc-issuer`, an exact-match pair that overrides any configured policy. Or \
        pass neither and let `[[trust.policy]]` decide. Verify then checks the operator-tier \
        (`config.toml`) policies against the target's canonical `registry/repository`. It falls \
        back to the project `ocx.toml` policies only when none match.\n\n\
        Reading `[[trust.policy]]` from `ocx.toml` is the one exception to OCI-tier commands \
        never consulting it, because trust policy is a security posture rather than toolchain \
        binding. Exactly one of the two flags is a usage error rejected at parse time (exit 64), \
        and so is neither flag with no covering policy (`no_identity_provided`).\n\n\
        Trust material carrying only CA anchors cannot verify anything, since the Fulcio \
        certificate's SCT is checked against the CT log key. Verify refuses it with exit 78 \
        (`trust root carries no CT log key`). `--sigstore-trusted-root` accepts only a Sigstore \
        trusted-root JSON carrying the anchors, CT log keys and Rekor key together.\n\n\
        A trusted-root file over 1 MiB, or present but unreadable at \
        `$OCX_HOME/sigstore/trusted-root.json`, exits 74 (`trust_root_unreadable`). An absent \
        `$OCX_HOME/sigstore/trusted-root.json` is not an error: resolution continues down the \
        ladder.\n\n\
        `--format json` reports `subject_digest`, the signer identity and `signatures`, one row \
        per signature found with its `discovery_method`, `signed_at` and `rekor_log_index`.")]
    Verify(super::package_verify::PackageVerify),
    /// Runs installed packages.
    #[command(long_about = "\
        Runs installed packages.\n\n\
        Identifiers are OCI references (for example `kitware/cmake:3.28`), resolved through the \
        index and installed when missing. A package a `[[trust.policy]]` covers is \
        signature-verified before it runs.\n\n\
        Dependencies' environments apply in topological order before the package's own, and \
        entries layer in the order the identifiers appear. For a project driven by `ocx.toml`, \
        use `ocx exec`.\n\n\
        Stdin is always inherited, so piped input reaches the command unchanged. On Unix ocx \
        replaces itself with the command, so signals reach it directly and it keeps ocx's process \
        id. On Windows ocx spawns the command, waits, and forwards its exit code.\n\n\
        `--rm` is the exception on Unix: cleanup must run after the command, so ocx stays \
        resident as its parent.\n\n\
        The command is looked up once, before it starts. Only the composed packages' own \
        `PATH` contributions are searched, never the ambient `PATH` or the working directory. A match \
        that is itself an ocx launcher is refused. `--rm` runs the same lock-free store \
        collection as `ocx package uninstall --purge` over the packages this run composed, so \
        concurrent invocations sharing one `OCX_HOME` can race. Use it for one-off runs, not in a \
        loop.")]
    #[command(visible_alias = "x")]
    Exec(super::exec::Exec),
    /// Remove an installed candidate for one or more packages.
    ///
    /// Removes the candidate symlink and its back-reference. Only the reference goes; `ocx clean`,
    /// or `--purge` right away, deletes a package nothing else references, a lock pin included.
    ///
    /// The `current` symlink stays unless `--deselect` is given.
    Uninstall(super::uninstall::Uninstall),
    /// Resolve installed packages and print their package-root (or, with
    /// `--candidate`/`--current`, install-symlink) paths.
    #[command(long_about = "\
        Resolve installed packages and print their package-root (or, with \
        `--candidate`/`--current`, install-symlink) paths.\n\n\
        Operates on OCI identifiers and never consults `ocx.toml`. The package root holds \
        `content/` (installed files), `entrypoints/` (generated launchers) and per-package files \
        such as `metadata.json`.\n\n\
        By default the content-addressed package root is printed; `--candidate`, `--current` and \
        `--link` print the stable install symlink instead, which targets the same root.\n\n\
        Nothing is downloaded, with or without `--lazy-mode`. Each entry names its kind: \
        `package` for a materialized root, or `shim` for a package composed with `--lazy-mode \
        always` whose content has not downloaded yet.\n\n\
        Install symlinks only ever resolve to `package`.")]
    Which(super::which::Which),
}

impl Package {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        match self {
            Package::Announce(announce) => announce.execute(context).await,
            Package::Attest(attest) => attest.execute(context).await,
            Package::Cascade(cascade) => cascade.execute(context).await,
            Package::Claim(claim) => claim.execute(context).await,
            Package::Copy(copy) => copy.execute(context).await,
            Package::Create(create) => create.execute(context).await,
            Package::Description(description) => description.execute(context).await,
            Package::DeprecatedDescribe(push) => {
                super::deprecated::warn_renamed(&context, &super::deprecated::PACKAGE_DESCRIBE);
                push.execute(context).await
            }
            Package::DeprecatedInfo(pull) => {
                super::deprecated::warn_renamed(&context, &super::deprecated::PACKAGE_INFO);
                pull.execute(context).await
            }
            Package::Deps(deps) => deps.execute(context).await,
            Package::Env(env) => env.execute(context).await,
            Package::Inspect(inspect) => inspect.execute(context).await,
            Package::Install(install) => install.execute(context).await,
            Package::Prune(prune) => prune.execute(context).await,
            Package::Pull(pull) => pull.execute(context).await,
            Package::Push(deploy) => deploy.execute(context).await,
            Package::Receipt(receipt) => receipt.execute(context).await,
            Package::Sbom(sbom) => sbom.execute(context).await,
            Package::Select(select) => select.execute(context).await,
            Package::Deselect(deselect) => deselect.execute(context).await,
            Package::Sign(sign) => sign.execute(context).await,
            Package::Test(test) => test.execute(context).await,
            Package::Verify(verify) => verify.execute(context).await,
            Package::Exec(exec) => exec.execute(context).await,
            Package::Uninstall(uninstall) => uninstall.execute(context).await,
            Package::Which(which) => which.execute(context).await,
        }
    }
}
