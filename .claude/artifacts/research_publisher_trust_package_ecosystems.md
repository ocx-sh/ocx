# Research: Publisher signer trust in package ecosystems

## Metadata

- Date: 2026-10-25
- Domain: supply-chain security
- Triggered by: ADR publisher trust distribution (`.agents/discussions/release-signing-trust-default.md`)
- Expires: 2027-04-25
- Axis: product/competitive landscape. Question: how established ecosystems let a publisher declare which identity signs their package, and how every client then trusts that declaration, judged against requirements 1-5 together (self-service onboarding without a client release; change safety; offline; operator override; storage and mutation).
- Method: primary docs, specs and source. Read directly (raw text, not a summary tool): Go modules reference, Homebrew `attestation.rb`/`env_config.rb`, npm CLI `verify-signatures.js`, pacote `registry.js`, the live npm keys endpoint, the live Sigstore TUF repository, Arch wiki source and `archlinux-keyring` README, Fulcio OID doc, protobuf.dev news. Everything else passed through a page-summarising fetch tool; those claims quote the page but were not re-read in full. Claims resting on the repo-local lanes `.agents/research/research_signer_binding_ecosystems.md` and `.agents/research/research_trust_metadata_distribution.md` are tagged "lane" and are not re-verified here.
- Source age: entries marked [OLD] are older than 18 months (before 2025-04-25). Design documents that are old but still the live specification are kept and flagged.
- Scope note: the two lanes above cover signer binding and TUF mechanics. This document adds the cross-system comparison on requirements 1-5, the system families the lanes skipped (aqua/mise, Nix, APT, RPM, Arch, Alpine, Gradle, Scoop/winget/Chocolatey), and the pattern analysis.

## Direct answer

1. No surveyed ecosystem satisfies requirements 1-5 with one mechanism. Every ecosystem that gets requirement 1 (publisher self-service, no client release) does it by making a repository key the vouching party; the client never sees per-publisher declarations (npm, PyPI, crates.io, RubyGems, NuGet, apt, Alpine, Nix). The one exception that ships per-package verification declarations to clients as data is aqua-registry, and its registry is unauthenticated (S31).
2. The mechanism that handles change safety (requirement 2) in production is overlap plus time-anchored validity of the vouching party's keys: npm (new key valid from 2025-01-13, old key valid to 2025-01-29, signature accepted if publish time precedes `expires`), NuGet (append-only certificate list, RFC 3161 timestamps, re-sign on compromise), Debian, Alpine and Fedora (new key shipped before use). Systems that make clients pin the publisher's own key (Gradle, Docker Content Trust, Maven PGP, PEP 480) break on rotation until every consumer updates, or never reached adoption.
3. Publisher-held per-package keys certified by a root are the declining pattern: Docker Content Trust shuts down 2026-12-08 at under 0.05 percent of pulls (S34), PEP 480 is still Draft after 12 years (S12), conda-content-trust is End of Support from September 2026 (S29), Protobuf dropped C# NuGet author signing in June 2026 (S19).
4. Declaration data consumed only at ingest by the repository is the norm for trusted publishing (npm, PyPI, NuGet, RubyGems, crates.io; five independent adopters). A declaration shipped to clients is only as strong as the channel that authenticates it. aqua calls its own registry "low risk to be tampered" and verifies nothing about it beyond a ref pin and an optional checksum (S31); PEP 740 states it "does not increase (or decrease) trust in the index itself" (S10).
5. Trusted-publisher identities everywhere match the tuple (repository plus owner, workflow file path, optional environment), never the git ref, and the post-2026 consensus is to pin immutable numeric IDs after the first successful publish (npm, NuGet, PyPI owner ID, OpenSSF) (S1, S19, S32). OCX's index already stores numeric owner IDs and Fulcio already carries `Source Repository Identifier` and `Owner Identifier` extensions (S33).
6. Recommendation in one line: make the index a repository that vouches (signed snapshot, ingest-verified publisher declarations, one rotating anchor with overlap, per-scope operator override through the existing `[[trust.policy]]`), take onboarding semantics from PyPI/npm/NuGet (pending declaration, activation on first signed publish, ID pinning), and do not use per-publisher root-certified keys or per-tag TUF. Details and caveats under Recommendation.

## Trends

- Established: repository-signed metadata with the anchor shipped as a keyring package (APT, RPM, Alpine, Arch, Fedora) [S25-S28]; Nix and Go as single-key, single-log anchors [S23, S24]; registry-side trusted publishing since 2023 (npm, PyPI, RubyGems, crates.io since 2025-07, NuGet since 2025) [S1, S9, S15, S17, S19]; PGP on Maven Central, unbound to namespaces [S20].
- Trending: Sigstore bundles and attestations (npm, PyPI, RubyGems, Homebrew, GitHub, Maven Central portal validation since 2025-01, prefix.dev) [S4, S11, S20, S22, S29]; immutable-ID binding and short activation windows for trusted publishers (npm 48 hours, NuGet 7 days) after the SlugFest disclosure of 2026-10-13 [S1, S19, S36]; trusted-publishing-only publish modes (crates.io) [S15]; lock-recorded provenance (mise, PEP 751) [S13, S31].
- Emerging: TUF for low-churn trust metadata through tuf-on-ci (Sigstore, GitHub) and for namespace-scoped key delegation (the `registry.npmjs.org` delegation in the Sigstore repo, read live) [S8, lane]; Rust's 2026 goal to ship TUF verifiable mirroring for rustup first [S16]; install-time verification of attestations is announced but not shipped in pip or uv [S14].
- Declining: publisher-held per-package keys certified by a root (DCT, PEP 480, conda-content-trust, NuGet author signing) [S12, S19, S29, S34]; client-hard-coded signer identities that need a client release per change (Homebrew) are being kept only because the `gh` verifier exists [S22].

## Key findings

1. **Trusted publishing is registry-internal.** npm stores per-package connections (owner, repository, workflow file, optional environment; up to 10), PyPI per project, crates.io per crate after a manual first release, RubyGems per gem with pending publishers, NuGet per policy owner with a package-ID glob. None exposes the declaration to clients. [npm](https://docs.npmjs.com/trusted-publishers) [PyPI pending](https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/) [crates.io](https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07) [RubyGems](https://guides.rubygems.org/trusted-publishing/) [NuGet](https://learn.microsoft.com/en-us/nuget/nuget-org/trusted-publishing)
2. **Pending declaration plus activation on first signed publish is the standard answer to "declare before the package exists".** PyPI and RubyGems convert a pending publisher to a normal one on first use; a name taken first invalidates the PyPI pending publisher. npm expires an unvalidated configuration after 2 days and "binds it to the repository's immutable identity" on first publish. NuGet keeps a policy "temporarily active" for 7 days until GitHub's token supplies repository and owner IDs, which prevents resurrection. crates.io has no pending state. [PyPI](https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/) [npm](https://docs.npmjs.com/trusted-publishers) [NuGet](https://learn.microsoft.com/en-us/nuget/nuget-org/trusted-publishing)
3. **Name-bound declarations failed in production.** SlugFest (disclosed 2026-10-13): npm trusted publishing identified a repository by its owner/name slug until first publish, so a freed slug could mint publish tokens; over 1,500 packages exposed. Fix: 48-hour expiry of unvalidated configurations, binding to the immutable identity on first publish, `issue_comment` tokens rejected. [The Hacker News](https://thehackernews.com/2026/10/github-tightens-npm-publishing-after.html) (single secondary source; the fix itself is confirmed on [docs.npmjs.com](https://docs.npmjs.com/trusted-publishers)). The OpenSSF guidance recommends storing stable IDs and has a "Resurrection and Rename Attacks" section ([OpenSSF](https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html)).
4. **A valid signature from the right identity is not a trustworthy build.** May 2026: 84 malicious npm artifacts carried valid provenance naming the legitimate repository, workflow and ref; an expected-builder check would have passed ([SLSA blog](https://slsa.dev/blog/2026/05/mini-shai-hulud-what-slsa-can-and-cannot-do)). Identity pinning bounds credential theft and registry tampering, not workflow compromise.
5. **Clients mostly do not enforce publisher identity by default.** `npm audit signatures` is an explicit command that checks the registry signature and attestation validity, with no expected-publisher comparison ([npm CLI source](https://raw.githubusercontent.com/npm/cli/latest/lib/utils/verify-signatures.js)). pip and uv do not verify PEP 740 attestations at install; `pylock.toml` has an `attestation-identities` table that no locker fills and no installer reads ([pydevtools](https://pydevtools.com/handbook/explanation/why-pylock-toml-includes-digital-attestations.md), [PEP 751](https://peps.python.org/pep-0751/)). cargo and Maven verify nothing. Homebrew shells out to `gh attestation verify` with the signer repository hard-coded in its source ([attestation.rb](https://raw.githubusercontent.com/Homebrew/brew/main/Library/Homebrew/attestation.rb)).
6. **npm rotation is the cleanest observed model.** Live keys endpoint: two keys, one `expires` 2025-01-29, one `expires` null ([registry.npmjs.org/-/npm/v1/keys](https://registry.npmjs.org/-/npm/v1/keys)). The Sigstore TUF repository serves the same set as a delegated target with `validFor` windows (old key to 2025-01-29, new key from 2025-01-13, per usage `npm:signatures` and `npm:attestations`) ([tuf-repo-cdn.sigstore.dev](https://tuf-repo-cdn.sigstore.dev/), delegation `registry.npmjs.org`, threshold 1, expires 2027-03-14). The CLI fetches keys via TUF and falls back to the registry endpoint, and rejects a signature only if the package's publish time (or, for attestations, the Rekor integrated time) is at or after `expires` ([pacote source](https://raw.githubusercontent.com/npm/pacote/main/lib/registry.js)). Weak point: for registry signatures the time source is the registry's own packument `time` field.
7. **NuGet is the closest to all five requirements, as a composite.** Author certificates are registered per account by the publisher; nuget.org validates and countersigns every package; the repository's certificate list is published in the service index (`signingCertificates`, usually append-only); on compromise the source re-signs all affected packages and drops the certificate from the list; RFC 3161 timestamps keep signatures valid past certificate expiry; `trustedSigners` (author, repository, `owners`) and `signatureValidationMode=require` give the operator override. Costs: default mode is `accept`, so unsigned and untrusted-signed packages install; strict clients broke on the 2021 and 2024 repository certificate changes (NU3034) until they added the new certificate. [reference](https://learn.microsoft.com/en-us/nuget/reference/signed-packages-reference) [repository signatures API](https://learn.microsoft.com/en-us/nuget/api/repository-signatures-resource) [trust boundaries](https://learn.microsoft.com/en-us/nuget/consume-packages/installing-signed-packages) [2024 notice](https://devblogs.microsoft.com/nuget/the-nuget-org-repository-signing-certificate-will-be-updated-as-soon-as-april-8th-2024/) [register certificate](https://learn.microsoft.com/nuget/create-packages/sign-a-package) [technical details](https://github.com/NuGet/Home/wiki/Package-Signatures-Technical-Details)
8. **Author-signing (publisher keys) lost adopters even where it is self-service.** Protobuf is dropping C# NuGet author signing and keeps the repository signature ([protobuf.dev, 2026-06-16](https://protobuf.dev/news/2026-06-16/)); lane reports about 15 percent adoption among top groups in 2019 (lane).
9. **aqua-registry is the only per-package, client-consumed declaration, and it fails closed on drift.** The registry YAML carries `cosign` (including `certificate_identity`, `certificate_oidc_issuer`, `certificate_github_workflow_ref`), `slsa_provenance`, `minisign` and `github_artifact_attestations` (`signer_workflow` or looser `signer_repo`, `source_ref`, `deny_self_hosted_runners`), templated per version, replaced wholesale (not merged) in `version_overrides`. 2026-05-05: mise failed installs of uv 0.11.9 with "No GitHub artifact attestations found ... but they are expected per aqua registry configuration" until the registry entry was fixed; workaround was disabling the check. [cosign fields](https://aquaproj.github.io/docs/reference/registry-config/cosign) [attestation fields](https://aquaproj.github.io/docs/reference/registry-config/github-artifact-attestations) [incident thread](https://station.railway.com/questions/deployment-failing-with-error-no-git-hub-e603dc8a) [registry fix](https://newreleases.io/project/github/aquaproj/aqua-registry/release/v4.511.1)
10. **aqua/mise coupling of registry version to client.** aqua pins the registry by `ref` in the user's `aqua.yaml`; mise bundles an aqua-registry snapshot per release, with opt-in `registry_floating`; the lockfile is "a trust input", a locked install does not repeat the provenance check unless `locked_verify_provenance` is set, and "A locked install is not an offline install". [aqua registry](https://aquaproj.github.io/docs/reference/registry/) [mise aqua](https://mise.jdx.dev/dev-tools/backends/aqua.html) [mise lock](https://mise.jdx.dev/dev-tools/mise-lock.html)
11. **Operator override is mature and per-scope in the older systems.** apt `Signed-By` per source, `Trusted`, per-source `Check-Valid-Until`; NuGet per-source `trustedSigners`; Nix `trusted-public-keys`, `extra-*`, `trusted-substituters`, `require-sigs`; Go `GOSUMDB`/`GONOSUMDB`/`GOPRIVATE` and sumdb mirroring by a proxy; aqua policy file needing `aqua policy allow` and re-approval after any edit; pacman `SigLevel` per repository and local signing; aptly signs a mirror with its own key. [apt sources](https://manpages.debian.org/trixie/apt/sources.list.5.en.html) [Nix conf](https://nix.dev/manual/nix/stable/command-ref/conf-file) [Go](https://go.dev/ref/mod) [aqua policy](https://aquaproj.github.io/docs/reference/security/policy-as-code/git-policy) [Arch](https://wiki.archlinux.org/title/Pacman/Package_signing) [aptly](https://www.aptly.info/doc/aptly/publish/)
12. **Publisher-declared configuration shipped by a publisher and confirmed once by the user exists in Nix.** Flake `nixConfig` can set only a small allowed set of options without confirmation; the rest need a prompt unless `accept-flake-config` is on (default false, experimental). [Nix conf](https://nix.dev/manual/nix/stable/command-ref/conf-file)
13. **Anchors travel as packages with overlap in the distros.** Debian: new archive keys are announced "well in advance"; one ftp-master key per release; third-party repos ship their own keyring package ([ftp-master](https://ftp-master.debian.org/keys.html), [wiki](https://wiki.debian.org/DebianRepository/UseThirdParty)). Arch: master keys co-sign packager keys (at least three), delivered by `archlinux-keyring`, a dependency of `base`; stale keyrings need `pacman -Sy archlinux-keyring` first ([wiki](https://wiki.archlinux.org/title/Pacman/Package_signing), [README](https://gitlab.archlinux.org/archlinux/archlinux-keyring/-/raw/master/README.md)). Alpine [OLD]: new 4096-bit keys shipped in 3.15 and held back from edge until later ([post, 2022-07-14](https://alpinelinux.org/posts/Alpine-edge-signing-keys-rotated.html)). Fedora [OLD; Fedora docs and a 2013 list thread, found by search and not re-read]: next release key shipped inside the current release package. dnf imports `gpgkey=` keys on first use with a prompt ([dnf](https://dnf.readthedocs.io/en/latest/conf_ref.html)).
14. **A rotation that depends on every consumer editing a file breaks consumers.** Gradle: new signing key not in the keyring fails the build until explicitly trusted ([Gradle](https://docs.gradle.org/current/userguide/dependency_verification.html)). Datadog [OLD]: hosts without the new RPM key get `NOKEY` ([Datadog](https://docs.datadoghq.com/agent/faq/rpm-gpg-key-rotation-agent-6)). NuGet strict clients: NU3034.
15. **Backfilling old releases has a precedent with a cutoff.** Homebrew accepts backfill attestations from a fixed repository only if their verified timestamp is not after 2024-03-14, so a compromised backfill workflow cannot forge later ones ([attestation.rb](https://raw.githubusercontent.com/Homebrew/brew/main/Library/Homebrew/attestation.rb)).
16. **TUF is deployed where change is rare.** Sigstore (3-of-5 root, timestamp expiring about a week out, read live) and GitHub (3-of-7, lane) run it for trust roots. PyPI's PEP 458 (created 2013, Accepted, deferred 2019 for funding) has no confirmed production serving ([PEP 458](https://peps.python.org/pep-0458/), lane); Rust ruled out a direct TUF repository over crates.io at roughly 250,000 crates and 3-5 updates a minute and started with rustup (lane, [goals](https://goals.rust-lang.org/2026/mirroring.html)); conda-content-trust (TUF-based, root metadata shipped with conda) is End of Support from September 2026 ([Anaconda](https://www.anaconda.com/blog/conda-signature-verification), single source); Docker's Notary v1 shuts down 2026-12-08 ([Docker](https://www.docker.com/blog/docker-content-trust-retirement-and-migration-guidance/)).
17. **Offline.** Signature-in-artifact plus cached anchor works offline (apt, Nix, NuGet, pacman, Gradle with `key-servers enabled=false`). Verification that calls a service does not (Homebrew via `gh`). Freshness is the offline hazard: apt `Valid-Until` has a per-source switch used for archive suites; TUF timestamps expire in days; sigstore-python's `offline` mode returns cached trust files without verifying them (lane). Go keeps tiles and signed tree heads in the module cache.
18. **Unverified or thin:** Alpine key handling (only the 2022 post plus secondary hits); RPM rotation practice (Fedora SOP and Datadog only); Arch database signing status (wiki note from 2018 [OLD]); crates.io page itself (JS-rendered; blog used); Maven Central namespace-to-key binding (lane says no check; the Sonatype GPG page does not say either way); RubyGems and JSR client verification (not documented in the pages read); winget source-index signing (not read); Homebrew default state of attestation verification (not confirmed).

## Comparison matrix

Legend: Y = met by the mechanism as documented, P = partly, N = not, n/a = no client-side mechanism exists. IDs resolve in Sources. "Client" means the package manager binary.

### Requirements 1-5 per system

| System | 1 Self-service onboarding, no client release | 2 Change safety (add, rotate, revoke, old releases) | 3 Offline / pinned lock | 4 Operator override | 5 Storage, mutation, stale or rollback detection |
|---|---|---|---|---|---|
| aqua / mise-aqua | P: PR to registry YAML declares verification per package [S31]; client must have the new registry (aqua `ref` bump; mise bundles a snapshot per release, `registry_floating` opt-in) | P: per-version config via `version_overrides`, replace-not-merge; drift fails closed (uv 0.11.9, 2026-05-05) [S31] | P: pinned registry plus `aqua-checksums.json` with `AQUA_ENFORCE_CHECKSUM`; mise lock skips repeat provenance, "not an offline install" [S31] | Y: policy file, only standard registry by default, `aqua policy allow` re-approval; custom registries; per-check disables [S31] | P: git repo plus tag; registry checksum only when enforcement on; no signature, no freshness |
| npm | Y server-side: per-package trusted publisher in UI/CLI [S1]; new-package case not stated, config must be used within 2 days | Y for registry keys: overlap and `expires` vs publish/integrated time [S5, S7, S8]; publisher edits delete+recreate; ID-bound after first publish [S1] | P: keys via TUF with endpoint fallback; offline not documented [S6] | P: keys fetched from the registry host [S6]; no documented client policy for identities | P: declarations in registry DB, invisible; keys in TUF delegation (threshold 1) [S8]; no rollback detection for packuments |
| PyPI | Y server-side incl. pending publishers [S9] | P: attestation per file; provenance objects mutable [S10]; verifiers can see a changed publisher [S11] | N: pip, uv do not verify [S14]; `attestation-identities` unpopulated [S13] | n/a (policy deliberately undefined by PEP 740) [S10] | P: DB in Warehouse; index trusted [S10]; PEP 458 TUF not confirmed in production [S12] |
| crates.io | P: owners configure after a manual first publish; no pending state [S15] | P: owner edits; blocked triggers; trusted-publishing-only mode [S15] | n/a (cargo verifies nothing) | n/a | P: registry DB; Rust PKI plan (2023) [OLD] and 2026 TUF goal start with rustup [S16] |
| RubyGems | Y: per-gem plus pending for new gems; reusable-workflow fields [S17] | not stated [S17] | n/a (client verification not found) | n/a | registry DB |
| JSR | P: provenance only via native GitHub Actions publish [S18] | not stated | n/a (no client verification described) | n/a | registry; manifest signing "planned" [S18] |
| NuGet | Y: author cert registered by publisher, nuget.org validates and countersigns, trusted publishing policies [S19] | Y: append-only repository cert list, re-sign on compromise, timestamps; strict clients need `trusted-signers sync` (NU3034 in 2021/2024) [S19] | Y: signature in package, cert fingerprints local; revocation may fail open [S19] | Y: `trustedSigners` author/repository/`owners`, `signatureValidationMode` [S19] | P: certs served from service index; default mode `accept`; no freshness [S19] |
| Maven Central / Gradle | P: namespace proof plus PGP key on keyserver; Sigstore bundles optional, portal-validated since 2025-01 [S20] [OLD] | N: new key breaks Gradle consumers until trusted [S21] | Y: Gradle keyring file, key servers disabled [S21] | Y: per-group/artifact `trusted-key`, `ignored-key` [S21] | P: `verification-metadata.xml` in project git |
| Homebrew | P: third-party taps automatic (tap repo is signer); core signer in client; custom-remote taps unsupported [S22] | P: change = client release; backfill cutoff 2024-03-14 [S22] | N: shells out to `gh attestation verify` [S22] | P: `HOMEBREW_NO_VERIFY_ATTESTATIONS` only | n/a: identities in client source |
| Go | n/a: no publisher declaration; path = identity [S23] | n/a for signers; sumdb key change = new Go release (lane) | Y: `go.sum`, module cache, cached tiles and tree heads [S23] | Y: `GOSUMDB`, `GONOSUMDB`, `GOPRIVATE`, proxy-mirrored sumdb [S23] | Y: Merkle log, signed tree heads, inclusion and consistency proofs [S23] |
| Nix | n/a for publishers; cache operator's key added by consumers; flake `nixConfig` prompts [S24] | Y: several keys trusted at once, `host-N` naming by convention [S24] | Y: signature in narinfo, keys in config | Y: `substituters`, `trusted-public-keys`, `extra-*`, `require-sigs` [S24] | P: static `nix.conf`; no freshness |
| APT | Y for third-party repos: ship keyring package plus sources entry [S25] | Y: keyring package carries several keys; one archive key per release [S25] | Y: cached InRelease; per-source `Check-Valid-Until` off [S25] | Y: per-source `Signed-By`, `Trusted`; aptly re-sign [S25, S39] | P: signed hashes; `Date`, `Valid-Until`; archive key compromise not defended [S25] |
| RPM / dnf | Y: `gpgkey=` list, imported on first use with prompt [S26] | P: new key must be imported else `NOKEY` (Datadog) [S26] [OLD] | Y: keys in rpmdb / per-repo keyring | Y: per-repo `gpgcheck`, `repo_gpgcheck`, `gpgkey` [S26] | P: signed `repomd.xml` optional; `repo_gpgcheck` default False in dnf |
| Arch | P: new packager needs at least 3 master signatures; shipped in `archlinux-keyring`, no pacman release [S27] | P: revoke by keyring release; stale keyring breaks upgrades [S27] | Y: local keyring; clock must be right | Y: `SigLevel` per repo, `pacman-key --lsign-key` [S27] | P: keyring in git with signed tags [S27]; database signing "work in progress" (2018 note) [OLD] |
| Alpine | P: key in `alpine-keys` package [S28] [OLD] | P: staged: new keys shipped, used later [S28] [OLD] | Y: keys in `/etc/apk/keys` (secondary) | P: user adds key files (secondary) | thin evidence |
| conda | N: conda-content-trust End of Support from 2026-09 [S29]; prefix.dev Sigstore uses trusted publishing, client verification unspecified [S29] | n/a | n/a | n/a | n/a |
| Chocolatey, winget, Scoop | P: moderated PR/manifest; hash per installer [S30] | n/a | P: hashes in manifest | n/a | git repository / moderation; no signature checks documented [S30] |

### Anchor and threats per system

Threat columns: H = index/registry/mirror host compromise; P = publisher CI or key compromise; R = rollback/freeze.

| System | What the client embeds or consults as anchor | H | P | R |
|---|---|---|---|---|
| aqua / mise-aqua | user's `aqua.yaml` `ref` pin and policy file; cosign/slsa-verifier and Sigstore root come with the tools; mise: registry snapshot inside the release | N: registry unauthenticated | Y for release tampering when identity declared; N for compromised legitimate workflow | N |
| npm | Sigstore TUF root in `@sigstore/tuf`, registry keys as a delegated target, registry endpoint fallback over TLS | key compromise total; TUF delegation threshold 1 | N: identity not compared; valid provenance on malicious publish (2026-05) | N documented |
| PyPI | nothing for pip/uv (TLS); Sigstore root for attestations | N (PEP 740: index trusted) | detect publisher change only | N |
| crates.io, RubyGems, JSR | nothing (TLS) | N | N | N |
| NuGet | Windows root store, `trustedSigners` fingerprints, repository certs from service index (trust on first fetch, `sync`) | P: author signature survives repository compromise; repository key compromise handled by re-sign | P: registered cert limits who can publish; compromised signing infra passes | N |
| Maven / Gradle | none by default; Gradle: committed `verification-metadata.xml` and keyring | n/a | group-level key trust is a risk (Gradle docs) | N |
| Homebrew | repositories and cutoff date in `brew` source; GitHub trusted root via `gh` | tap repo compromise = signer compromise | repository identity only | N |
| Go | sumdb public key inside the `go` command | Y | N (first malicious version stays logged) | Y |
| Nix | `cache.nixos.org-1` key as built-in default | cache key compromise total | n/a (cache signs) | N |
| APT | keyring files (`debian-archive-keyring`) or per-source `Signed-By` | Y for mirrors; N for archive key compromise [S25] | n/a (distro builds) | P: `Date`, `Valid-Until` |
| RPM / dnf | `gpgkey=` imported by the user; distro key packages | P | n/a | N |
| Arch | master key fingerprints verified by the user at `pacman-key --populate` | master key compromise total | packager key revoked by keyring update | N documented |
| Alpine | `alpine-keys` package | P | n/a | N |
| conda | root metadata shipped with conda (End of Support) | P | P | Y by design (TUF) |

## Patterns

Pattern evidence uses Y/P/N against requirements 1-5; "n/a" where the pattern has no client-side step.

### P1. Registry-side trusted publishing (publish authorisation only; clients blind)

- Adopters: npm, PyPI (pending), RubyGems (pending), crates.io, NuGet, JSR via GitHub link, prefix.dev.
- Req: 1 Y, 2 P (config edits are safe because nothing downstream checks them), 3 n/a, 4 n/a, 5 P (registry database; owner-controlled; invisible to clients).
- Threats: defends stolen long-lived tokens; does not defend registry compromise or compromised workflows.
- Known failures: SlugFest slug binding (npm, 2026-10), `issue_comment` token abuse (fixed 2026-10), `pull_request_target` cache poisoning (2026-05) [S35, S36].

### P2. The repository vouches (signed index or countersignature; client trusts the repository key)

- Adopters: APT InRelease, RPM repomd, Alpine APKINDEX, Nix binary caches, NuGet repository signature, npm registry signatures, Arch databases (partly). Variant: transparency log vouching for content (Go sumdb).
- Req: 1 Y (publisher uploads; repository validates at ingest), 2 Y when keys overlap and validity is time-anchored (npm, NuGet, Debian), 3 Y, 4 Y (per-source key selection), 5 P (signed metadata with `Date`/`Valid-Until`; rollback defence client-defined; single-key compromise is total unless thresholded).
- Known failures: strict-policy clients break on repository key changes (NuGet NU3034 2021, 2024; Alpine edge; Arch stale keyring); archive suites with expired `Valid-Until` need per-source overrides; "trusting nuget.org as a signer trusts every package on it" (lane); the apt-secure man page lists archive-key compromise as out of scope [S25].

### P2a. Anchor distribution as a keyring package with staged overlap (technique within P2)

- Adopters: Debian (`debian-archive-keyring`), Arch (`archlinux-keyring`), Alpine (`alpine-keys`), Fedora (`fedora-repos`/release packages), Datadog (`gpgkey=` list).
- Req: 1 Y (no manager release; the anchor update rides the package channel under the previous trust), 2 Y (next key shipped before use), 3 Y, 4 Y, 5 P.
- Known failures: clients that skipped the keyring update fail (Arch `-Sy archlinux-keyring`; Datadog `NOKEY`); initial bootstrap still needs an install-medium anchor.

### P3. Per-publisher keys certified by a repository or root key

- Adopters: Arch (master keys sign packager keys), NuGet (registered author certificates plus countersignature), Docker Content Trust (TUF delegations), PEP 480 (Draft), conda-content-trust (key manager role), Gradle trusted keys (client-side variant).
- Req: 1 P (a human or ceremony step; NuGet self-service), 2 P (NuGet removal blocks only new submissions, existing packages unaffected; DCT loss of root key is unrecoverable), 3 Y, 4 Y, 5 P.
- Known failures: adoption collapse (DCT under 0.05 percent, shutdown 2026-12-08; PEP 480 Draft since 2014; conda-content-trust End of Support; Protobuf dropped NuGet author signing). Highest operations cost of all patterns.

### P4. Per-package verification config in a versioned, git-hosted registry

- Adopters: aqua-registry (consumed by aqua and mise); partial analogues: Homebrew taps (tap repo is the signer), winget-pkgs, Scoop buckets, Chocolatey (manifest plus hash, no signer).
- Req: 1 Y for the data, P for reaching clients (`ref` bump or client release), 2 P (per-version overrides; drift), 3 P, 4 Y (policy file), 5 P (git history; no signature on the registry).
- Known failures: attestation-expectation drift (uv 0.11.9, 2026-05-05); override replace footgun; registry compromise bypasses everything it declares.

### P5. Client-side policy file with pinned allow-list, often trust on first use

- Adopters: Gradle `verification-metadata.xml`, NuGet `trustedSigners`, Notation `trustpolicy.json`, apt `Signed-By`, Nix `trusted-public-keys` and flake prompt, pacman local signing, dnf `gpgkey`, `gh`/cosign verify flags, Sigstore policy-controller.
- Req: 1 N (each consumer or project must carry it), 2 P (breaks until updated), 3 Y, 4 Y (it is the override), 5 P (file in project git, reviewed).

### P6. Identity or key compiled into the client

- Adopters: Homebrew (signer repositories, backfill cutoff), Go (sumdb key), Nix (default cache key), Sigstore clients (TUF root).
- Req: 1 N unless the compiled item is a rotating root, 2 P, 3 Y, 4 P (environment override: `GOSUMDB`, cosign `--root`), 5 n/a. Change costs a client release.

### P7. TUF-delivered trust metadata with delegations

- Adopters: Sigstore root-signing (including the `registry.npmjs.org` delegation), GitHub (tuf-on-ci), conda-content-trust (End of Support), Docker Notary v1 (retired), PEP 458 (designed), Rust 2026 mirroring goal (rustup first).
- Req: 1 P, 2 Y (root chaining, delegation revocation), 3 P (expiry bounds offline; offline modes may skip verification), 4 Y (custom root and mirror in cosign, `gh`), 5 Y (versions, expiry, snapshot).
- Known failures: cost of ceremonies and re-signing; production adoption only for low-churn data; high-churn registries deferred or declined (PyPI, crates.io).

### P8. Lock-recorded verification evidence

- Adopters: mise.lock (provenance kind plus checksum; a "trust input"), PEP 751 `attestation-identities` (Final, unpopulated), Gradle metadata, `go.sum`, `aqua-checksums.json`.
- Req: 3 Y, others n/a. Known failure: nothing detects a provenance downgrade except `mise lock` refusing when provenance cannot be re-established for new bytes [S31]; the PEP 751 table is read by no installer [S14].

### Cross-cutting techniques (not patterns)

- T1 Immutable-ID binding plus activation window (npm, NuGet, PyPI owner ID, OpenSSF).
- T2 Overlap and staging: publish the next key before use (Debian, Alpine, Fedora, npm 16-day overlap).
- T3 Time-anchoring: validity checked against when the signature was made (npm publish and Rekor time, NuGet RFC 3161, Sigstore `validFor`, Homebrew backfill cutoff).
- T4 Ingest-time verification: the repository verifies the publisher's signature or token at submission (PyPI rejects non-trusted-publisher attestations at upload, NuGet validates at submission, npm exchange).
- T5 Fail-closed on missing expected evidence (aqua/mise) versus fail-open (NuGet `accept`, Homebrew gated): the first produced the uv incident, the second leaves unsigned installs working.

### Pattern x requirement summary

| Pattern | 1 | 2 | 3 | 4 | 5 | Defends H | Defends P | Defends R |
|---|---|---|---|---|---|---|---|---|
| P1 registry trusted publishing | Y | P | n/a | n/a | P | N | N | N |
| P2 repository vouches | Y | Y | Y | Y | P | Y if key separate from host | N | P |
| P2a keyring package anchor | Y | Y | Y | Y | P | n/a | n/a | N |
| P3 per-publisher certified keys | P | P | Y | Y | P | Y | Y (blast radius) | P |
| P4 versioned registry config | Y (data) | P | P | Y | P | N | P | N |
| P5 client allow-list | N | P | Y | Y | P | n/a | P | N |
| P6 compiled-in anchor | N | P | Y | P | n/a | n/a | n/a | N |
| P7 TUF | P | Y | P | Y | Y | Y | P | Y |
| P8 lock evidence | n/a | n/a | Y | n/a | n/a | n/a | n/a | P |

## Sources

Age flag: [OLD] = older than 18 months at 2026-10-25. "Read directly" = raw text read, not summarised.

| ID | Source | URL | Date / age |
|---|---|---|---|
| S1 | npm trusted publishing | https://docs.npmjs.com/trusted-publishers | edited 2026-10-06 |
| S2 | npm verifying registry signatures | https://docs.npmjs.com/verifying-registry-signatures | current |
| S3 | npm about registry signatures (key fields) | https://docs.npmjs.com/about-registry-signatures | current |
| S4 | npm provenance (automatic for public repos; stated on the trusted publishing page) | https://docs.npmjs.com/trusted-publishers | edited 2026-10-06 |
| S5 | npm registry keys endpoint, read directly | https://registry.npmjs.org/-/npm/v1/keys | fetched 2026-10-25 |
| S6 | npm CLI `verify-signatures.js`, read directly | https://raw.githubusercontent.com/npm/cli/latest/lib/utils/verify-signatures.js | fetched 2026-10-25 |
| S7 | pacote `registry.js`, read directly | https://raw.githubusercontent.com/npm/pacote/main/lib/registry.js | fetched 2026-10-25 |
| S8 | Sigstore TUF repository, read directly (targets v14, delegation `registry.npmjs.org` v9, root/timestamp) | https://tuf-repo-cdn.sigstore.dev/ | fetched 2026-10-25 |
| S9 | PyPI pending publishers | https://docs.pypi.org/trusted-publishers/creating-a-project-through-oidc/ | current |
| S10 | PEP 740 (historical; canonical spec on PyPA) | https://peps.python.org/pep-0740/ | Final 2024-07 [OLD] |
| S11 | PyPI attestations and security model | https://docs.pypi.org/attestations/ and https://docs.pypi.org/attestations/security-model/ | current |
| S12 | PEP 458 and PEP 480 | https://peps.python.org/pep-0458/ , https://peps.python.org/pep-0480/ | 2013-2019 [OLD], still the specs |
| S13 | PEP 751 (`attestation-identities`) | https://peps.python.org/pep-0751/ | Final 2025-03-31 [OLD, borderline] |
| S14 | Why pylock includes attestations (installer support) | https://pydevtools.com/handbook/explanation/why-pylock-toml-includes-digital-attestations.md | undated; mentions 2026-03 |
| S15 | crates.io trusted publishing | https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07 , https://blog.rust-lang.org/2026/01/21/crates-io-development-update | 2025-07, 2026-01 |
| S16 | Rust goals 2026 mirroring (via search and lane); Rust Foundation signing plan | https://goals.rust-lang.org/2026/mirroring.html , https://rustfoundation.org/media/improving-supply-chain-security-for-rust-through-artifact-signing | 2026; plan 2023-12 [OLD] |
| S17 | RubyGems trusted publishing | https://guides.rubygems.org/trusted-publishing/ | current |
| S18 | JSR trust and provenance | https://jsr.io/docs/trust | current |
| S19 | NuGet: signed packages, repository signatures API, trust boundaries, trusted publishing, signing, technical details, 2024 notice, protobuf | https://learn.microsoft.com/en-us/nuget/reference/signed-packages-reference [OLD 2018/2021]; https://learn.microsoft.com/en-us/nuget/api/repository-signatures-resource [OLD]; https://learn.microsoft.com/en-us/nuget/consume-packages/installing-signed-packages; https://learn.microsoft.com/en-us/nuget/nuget-org/trusted-publishing (updated 2026-09); https://learn.microsoft.com/nuget/create-packages/sign-a-package; https://github.com/NuGet/Home/wiki/Package-Signatures-Technical-Details [OLD]; https://devblogs.microsoft.com/nuget/the-nuget-org-repository-signing-certificate-will-be-updated-as-soon-as-april-8th-2024/ [OLD]; https://protobuf.dev/news/2026-06-16/ (read directly) | as marked |
| S20 | Maven Central: GPG requirements; Sigstore validation news | https://central.sonatype.org/publish/requirements/gpg/ ; https://central.sonatype.org/news/20250128_sigstore_signature_validation_via_portal/ | news 2025-01-28 [OLD] |
| S21 | Gradle dependency verification | https://docs.gradle.org/current/userguide/dependency_verification.html | current |
| S22 | Homebrew `attestation.rb`, `env_config.rb` (read directly) | https://raw.githubusercontent.com/Homebrew/brew/main/Library/Homebrew/attestation.rb ; https://raw.githubusercontent.com/Homebrew/brew/main/Library/Homebrew/env_config.rb | fetched 2026-10-25 |
| S23 | Go modules reference (read directly); Go blog | https://go.dev/ref/mod ; https://go.dev/blog/module-mirror-launch [OLD, 2019-08-29, still the design] | fetched 2026-10-25 |
| S24 | Nix `nix.conf` reference (incl. `accept-flake-config`, flake `nixConfig`) | https://nix.dev/manual/nix/stable/command-ref/conf-file | current |
| S25 | Debian: third-party repos, sources.list(5), apt-secure(8), ftp-master keys, repository format | https://wiki.debian.org/DebianRepository/UseThirdParty ; https://manpages.debian.org/trixie/apt/sources.list.5.en.html ; https://manpages.debian.org/trixie/apt/apt-secure.8.en.html ; https://ftp-master.debian.org/keys.html ; https://wiki.debian.org/DebianRepository/Format | current; wiki undated |
| S26 | dnf configuration; Datadog RPM rotation (Fedora key staging: Fedora docs and a 2013 list thread, found by search, not re-read) | https://dnf.readthedocs.io/en/latest/conf_ref.html ; https://docs.datadoghq.com/agent/faq/rpm-gpg-key-rotation-agent-6 [OLD, 2019] | as marked |
| S27 | Arch package signing wiki (raw); archlinux-keyring README (read directly) | https://wiki.archlinux.org/title/Pacman/Package_signing ; https://gitlab.archlinux.org/archlinux/archlinux-keyring/-/raw/master/README.md | fetched 2026-10-25; db-signing note 2018 [OLD] |
| S28 | Alpine edge key rotation post | https://alpinelinux.org/posts/Alpine-edge-signing-keys-rotated.html | 2022-07-14 [OLD] |
| S29 | Anaconda content trust blog (with 2026-09 End of Support notice); conda-content-trust README; prefix.dev Sigstore for conda | https://www.anaconda.com/blog/conda-signature-verification [OLD, 2021-05-05]; https://raw.githubusercontent.com/conda/conda-content-trust/main/README.md ; https://prefix.dev/blog/securing-the-conda-package-supply-chain-with-sigstore | 2025-08-28 for prefix.dev |
| S30 | winget repository submission; Chocolatey moderation; Scoop manifests | https://learn.microsoft.com/en-us/windows/package-manager/package/repository ; https://docs.chocolatey.org/en-us/community-repository/moderation/ ; https://github.com/ScoopInstaller/Scoop/wiki/App-Manifests | winget page 2024-11 [OLD] |
| S31 | aqua docs and registry schema; mise aqua backend and lock; incident evidence | https://aquaproj.github.io/docs/reference/registry-config/cosign ; https://aquaproj.github.io/docs/reference/registry-config/github-artifact-attestations ; https://aquaproj.github.io/docs/reference/security/checksum ; https://aquaproj.github.io/docs/reference/security/cosign-slsa ; https://aquaproj.github.io/docs/reference/security/policy-as-code/git-policy ; https://aquaproj.github.io/docs/reference/registry/ ; https://mise.jdx.dev/dev-tools/backends/aqua.html ; https://mise.jdx.dev/dev-tools/mise-lock.html ; https://station.railway.com/questions/deployment-failing-with-error-no-git-hub-e603dc8a (2026-05-05) ; https://newreleases.io/project/github/aquaproj/aqua-registry/release/v4.511.1 | current |
| S32 | OpenSSF Trusted Publishers for All Package Repositories | https://repos.openssf.org/trusted-publishers-for-all-package-repositories.html | updated 2025-12 |
| S33 | Fulcio OID info (`.1.15` Source Repository Identifier, `.1.17` Owner Identifier; read directly) | https://github.com/sigstore/fulcio/blob/main/docs/oid-info.md | fetched 2026-10-25 |
| S34 | Docker Content Trust retirement | https://www.docker.com/blog/docker-content-trust-retirement-and-migration-guidance/ | 2026-06-16 |
| S35 | SLSA blog, Mini Shai-Hulud | https://slsa.dev/blog/2026/05/mini-shai-hulud-what-slsa-can-and-cannot-do | 2026-05-15 |
| S36 | SlugFest disclosure | https://thehackernews.com/2026/10/github-tightens-npm-publishing-after.html | 2026-10-13 |
| S37 | FOSDEM 2026 abstract: attestations in npm, PyPI, RubyGems, Maven Central | https://archive.fosdem.org/2026/schedule/event/BCFZP7-current-state-programming-language-attestations/ | 2026 |
| S38 | Repo-local prior lanes | `.agents/research/research_signer_binding_ecosystems.md`, `.agents/research/research_trust_metadata_distribution.md`, `.agents/research/research_stable_signer_identity.md` | 2026-10-25 |
| S39 | aptly publish docs | https://www.aptly.info/doc/aptly/publish/ | undated |

## Recommendation

Opinionated, for the ADR. It is a pattern choice, not a wire design.

### Fit to OCX

OCX owns the index, the bot and the client; it does not own storage. That is the shape of a package repository, so the strongest prior art is P2 (the repository vouches), with onboarding semantics from P1 and override semantics from P5. P3 and per-tag P7 need infrastructure OCX does not have and have the worst adoption record.

### Recommended pattern set

1. **Spine: P2, the index vouches for ingest-verified content.** Publishers keep signing artifacts with Sigstore (phase 1). The index bot verifies at ingest (T4) that the digest in a tag refresh carries a signature from the identity the publisher declared, then the index publishes a signed snapshot, and the client's default trust decision rests on that snapshot. Why this fits: it is the only pattern family that meets requirements 1, 2, 3 and 4 together in production (apt, NuGet, npm, Nix; Findings 7, 11, 13, 17). A publisher signer rotation or revocation then never touches clients: the vouching key rotates on its own overlap schedule, and old entries were verified when ingested (NuGet: "existing packages aren't affected"). Offline works from cached snapshot plus digest-pinned `ocx.lock` (apt, Go, Nix model). Cost: index or anchor-key compromise is total (apt-secure scope statement) and a compromised publisher workflow still passes (Finding 4), so the anchor must be protected and a human gate kept on declaration edits, which the index already has for `repository` and `owners`.

2. **Onboarding: copy PyPI/npm/NuGet semantics for the declaration.** A package declares its signer before its first release (pending state), the declaration becomes active only on the first successful signed publish that supplies immutable IDs (Fulcio `Source Repository Identifier` and `Owner Identifier`, OIDs 1.3.6.1.4.1.57264.1.15 and .17), and it expires if unused (npm 2 days, NuGet 7 days). Match the tuple all five registries use (repository and owner ID, workflow file path, optional environment), never the git ref. This also answers the phase-1 question on a stable identity: pin the workflow path and IDs, not the per-release SAN. Prior art covers the declaration when it is consumed at ingest (five registries). Prior art for the same field being shipped to clients as the trust decision is aqua alone, and aqua's channel is unauthenticated, so treat it as an increment (point 5), not the base.

3. **Anchor and rotation: one embedded anchor, rotated with overlap, delivered like a keyring package (P2a).** Embed exactly one thing, the index-snapshot signer (or a root that certifies it), and ship rotations as data through the channel OCX already has for operator config (`ocx config push`, digest-pinned `[managed]` source), with the next key published before use (Debian, Alpine, Fedora, npm 16-day overlap) and time-anchoring (T3) so that a verifier asks "was this valid when it was signed" (npm, NuGet, Sigstore `validFor`). Choose between a single rotating key and TUF root chaining by the threat you must cover: TUF buys rollback/freeze defence and threshold protection of the anchor and is proven for small, rarely changing documents (Sigstore 3-of-5, GitHub 3-of-7 via tuf-on-ci), but PyPI never deployed it, Rust declined it for crate-scale data, and conda-content-trust ended. If the ADR adopts TUF, scope it to the trust-anchor document only, not to per-tag data. If it does not, the apt `Date`/`Valid-Until` pair is the minimum rollback and freeze defence; define the offline behaviour of that freshness check explicitly (apt's per-source switch is the precedent; sigstore-python's unverified `offline` is the anti-precedent).

4. **Operator override: per-scope replacement of the anchor, using the existing `[[trust.policy]]` tiers (P5).** Precedents: apt `Signed-By` per source, NuGet repository entries with `owners`, Go `GOSUMDB`/`GONOSUMDB`/`GOPRIVATE`, aqua policy with re-approval after edits, aptly mirrors re-signing with their own key. A private mirror that "accepts only its own signatures" is the aptly/NuGet-private-feed shape: the mirror re-vouches and the client trusts the mirror's anchor for that scope. Keep the existing rule that a locked tier cannot be displaced.

5. **Optional increment, only on top of 1-4: ship the verified declaration to clients (aqua-style, P4).** Publish the ingest-verified signer inside the signed snapshot so strict operators can verify publisher identity end to end (mirror operators, NuGet `owners` restriction analogue). Do not do this on an unsigned index (aqua's weakness) and do not make a missing signature fail closed for the whole fleet without an operator switch (the uv incident, 2026-05-05).

6. **Lockfile: no change needed for requirement 3.** Digest pins plus cached signed snapshot give offline and frozen installs (apt, Go, Nix). Prior art for recording evidence in the lock exists (mise.lock records provenance kind and treats it as a trust input; PEP 751 `attestation-identities`), but no installer enforces the PEP 751 field and mise warns that the lock then needs review. Given the owner's rejection, record it as available prior art, not as a requirement.

### Rejected or deferred, with reasons

- P3 per-publisher certified keys: lowest adoption, highest operations cost, DCT and conda-content-trust ended, PEP 480 Draft since 2014.
- Per-tag or per-package TUF delegation: the Rust project judged it prohibitive at crate scale; PyPI's PEP 458 has no confirmed production serving after 13 years. The only namespace delegation in production found is one key, threshold 1, in the Sigstore repository (`registry.npmjs.org`).
- P6 compiled-in signer identities per product family (Homebrew model): needs a client release per change, which requirement 1 forbids; Homebrew core can only pin the repository, not the workflow, because core uses several workflows.
- Validity windows on per-package signer entries: windows are proven for the vouching party's keys and CAs (npm `validFor`, Sigstore `trusted_root`, NuGet notBefore/notAfter plus timestamps), never for per-package publisher identities. With a vouching index the need disappears because old entries stay verified from ingest time.

### Residual risks the ADR must own

- Index or snapshot-signing compromise defeats the default path (apt-secure); mitigation options are a protected signing environment, a human gate on declaration edits, threshold or TUF for the anchor, and Rekor monitoring of the index identity (lane).
- Publisher workflow compromise passes every identity check (Finding 4); out of scope for any signer-binding scheme.
- Time source: npm registry-signature validity uses the registry's own `time` field; prefer a transparency-log integrated time (Rekor) when anchoring ingest records.
- Backfilling older releases ("also sign older releases") should follow Homebrew: a dedicated, narrowly named backfill identity and a hard cutoff date after which backfill signatures are not accepted (Finding 15).
- Unconfirmed in this research: crates.io and RubyGems client verification, Maven namespace-to-key binding, Alpine and RPM details beyond the sources listed, whether Homebrew attestation verification is now on by default.
