# Research: Signing dogfood, competitive and vendor scan

## Metadata
- Date: 2026-11-13
- Lane: competitive/vendor web scan. Read-only. No recommendation, no ranking. Extends `research_signing_dogfood_prior_art.md` (axis 5); does not repeat PyPI Security tab or deps.dev.
- Tags: **[F]** page fetched (WebFetch summarises via a small model, so "page does not say X" means "not found on that page"); **[S]** search-result summary only; **[M]** recalled, not verified this session; **[3P]** third-party blog or issue tracker, not vendor documentation.
- Not reached: aqua `slsa_provenance`/`minisign` pages and aqua policy page (404), Homebrew `HOMEBREW_VERIFY_ATTESTATIONS` default in current brew (man page truncated), zot sync page beyond the one article, JFrog remote-repo docs (redirect, not followed), Quay Jira ticket bodies (redirect), Sonatype UI, Artifact Hub source code, winget/Scoop official pages, Chainguard apk verification page.

---

## 1. Tool managers and their registries

| Tool | Where expected signer is declared | Verification default | What registry site/CLI shows | Deliberately not done (as documented) |
|---|---|---|---|---|
| aqua | Registry entry, per package: `cosign` (signature/key/cert/bundle asset locations, `certificate_identity*`, `certificate_oidc_issuer`, `certificate_github_workflow_*`), `slsa_provenance`, `minisign`, `github_artifact_attestations` (`signer_workflow` or `signer_repo`, `predicate_type`, `deny_self_hosted_runners`, `source_ref`, `source_digest`) | On when the entry declares it; disable via `-disable-cosign`/`-disable-slsa` or `AQUA_DISABLE_COSIGN`/`AQUA_DISABLE_SLSA`; failure stops the install | Not found in fetched pages (no registry-site UI described) | Entry without a block = no verification for that package. `signer_workflow` and `signer_repo` are mutually exclusive. `version_overrides` replaces the whole `github_artifact_attestations` block rather than merging |
| mise (aqua backend and others) | Reads aqua registry entries (bundled snapshot, `registry_floating` opt-in, custom `aqua.registries`); user config declares nothing about signers | `aqua.cosign`, `aqua.minisign`, `aqua.github_attestations`, `aqua.slsa` all default `true`; each runs only if the recipe declares it; mise does cosign/minisign itself, no binaries needed | `mise lock` records provenance (SLSA, Cosign, Minisign, GH attestation) next to the checksum in `mise.lock` | A lockfile `provenance` field "doesn't prove that mise verified the bytes"; `mise install` may skip repeating provenance when checksum+provenance are recorded unless `locked_verify_provenance` is set; cannot add provenance to releases that never had it |
| Homebrew | Hard-coded in brew: signer identity is homebrew-core's `publish-commit-bottles.yml` workflow, with a backfill identity and cutoff (prior-art axis 4) | Opt-in: `HOMEBREW_VERIFY_ATTESTATIONS`; docs say "We hope to make attestation verification the default in future" | `brew verify` (dev command) prints "<bottle> has a valid attestation"; no UI described for formulae.brew.sh (page only describes it as a JSON API from GitHub Pages) | Not default on as of the docs page fetched; current default in latest brew not confirmed |
| winget | Nowhere for a signer; manifest carries `InstallerSha256` (and `SignatureSha256` for MSIX) | Hash checked at install | Not found | Signature requirement claims conflict between sources; treated as unverified |
| Scoop | Nowhere; manifest `hash` field | Hash checked | Not found | No publisher identity check, hash only |
| Nix | User/system config: `trusted-public-keys` plus `require-sigs` (default on); cache key shipped with NixOS for cache.nixos.org | On: only signed substitutes from trusted keys are used | Warning "not signed by any of the keys in trusted-public-keys"; `nix path-info --sigs` | Signature is by cache key, not by the package author |
| asdf | Nowhere; plugin = a git repo URL or short name in the plugins index | No verification described | n/a | Docs state no trust model; trust rests on the repo you add; `plugin update` takes latest default-branch commit |
| ubi | Nowhere | Page says nothing about signatures or checksums | n/a | Offers `--min-age-days` and `--tag` as supply-chain mitigations instead |
| eget | Nowhere | SHA-256 verified only if a `<asset>.sha256`/`.sha256sum` sibling asset exists; abort on mismatch | `--sha256` prints hash | Signatures not mentioned on the page |
| Chainguard apk | Public key published beside the repo (`packages.wolfi.dev/os/wolfi-signing.rsa.pub`); client trusts keys in `/etc/apk/keys` | apk verifies against trusted keys | Not found | A 2026 advisory (apko before 1.2.7 did not check downloads against the signed APKINDEX checksum) shows index-vs-package binding was a bug class |

Sources:
- aqua `cosign` fields and identity flags [F] https://aquaproj.github.io/docs/reference/registry-config/cosign
- aqua attestations fields [F] https://aquaproj.github.io/docs/reference/registry-config/github-artifact-attestations
- aqua default on, disable switches, failure behaviour [F] https://aquaproj.github.io/docs/reference/security/cosign-slsa/
- aqua registry-config index lists four attributes [F] https://aquaproj.github.io/docs/reference/registry-config/
- mise aqua defaults [F] https://mise.jdx.dev/dev-tools/backends/aqua.html and https://mise.jdx.dev/configuration/settings.html
- mise lock provenance [F] https://mise.jdx.dev/dev-tools/mise-lock.html
- Homebrew opt-in [F] https://docs.brew.sh/Homebrew-Security-and-Supply-Chain ; `brew verify` source text [S] https://docs.brew.sh/rubydoc/Homebrew/DevCmd/Verify.html ; 2024 opt-out variable `HOMEBREW_NO_VERIFY_ATTESTATIONS` [S, 3P] https://til.simonwillison.net/homebrew/no-verify-attestations
- winget/Scoop hashes [S] https://learn.microsoft.com/cs-cz/windows/package-manager/winget/hash ; https://github.com/lukesampson/scoop/wiki/App-Manifest-Autoupdate
- Nix `require-sigs` default [S] https://mynixos.com/nixpkgs/option/nix.settings.require-sigs
- asdf plugin page [F] https://github.com/asdf-vm/asdf/blob/master/docs/manage/plugins.md
- ubi [F] https://github.com/houseabsolute/ubi ; eget [F] https://github.com/zyedidia/eget
- Chainguard apk key and apko advisory [S] https://www.wiz.io/vulnerability-database/cve/cve-2026-42575 ; https://man.archlinux.org/man/extra/apk-tools/apk-keys.5.en

Observed signer-declaration locations: in the registry entry (aqua, mise via aqua); in code (Homebrew); in user config (Nix); nowhere (asdf, ubi, eget, Scoop, winget). The aqua design is the only found one that carries a structured, per-package identity pin in a shared registry and lets the user turn verification off per tool-wide switch.

Against / caveats: mise states that a recorded `provenance` field is not proof of verification [F]. aqua's own page does not say what happens when the block is absent [F]. Homebrew default unconfirmed for current brew.

---

## 2. Signed-state UI

| Product | States shown | present vs verified | failed/mismatch state | Per-version | Where computed |
|---|---|---|---|---|---|
| Docker Hub | Badges: Docker Official Image, Docker Hardened Image/Chart, Docker-Verified Publisher, Docker-Sponsored Open Source. For DHI, an Attestations section lists each attestation with type (SBOM, VEX), predicate type URI and a digest reference | The Attestations section is a listing; verification is CLI (`docker scout attest get --verify`) | CLI only: "no matching signatures: signature not found in transparency log" | Attestations listed when exploring a variant; no tag-list indicator found | Badges: program membership (not computed from signatures). Verify: client CLI at run time |
| Docker Scout | CLI `attest list/get --verify`; SBOM/provenance build attestations preferred over Scout's own | `--verify` separate from `list` | CLI error line above; `--skip-tlog` validates against Docker's key without log check | per image tag/digest | CLI, at run time |
| GitHub Packages / artifact attestations | Attestations shown in repo Actions tab; org Packages view with linked artifacts, storage and deployment records; ghcr package page lists attestation manifests as an "unknown/unknown" platform (community bug report) | Docs describe on-demand `gh attestation verify`; no UI verified state found | none found | not described | Attest action at build; verification at consumer |
| Artifact Hub | "Signed" label; "Verified publisher" (ownership via `artifacthub-repo.yml`, not a signature) | Detection of presence per the evidence below; whether it cryptographically verifies was not found | none found | Label is per package version (digest field per version); security report rescan latest daily, older weekly | Tracker/crawler, periodic |
| npm | "Built and signed on GitHub Actions" style badge near version, with a Provenance section linking commit, workflow, run | Registry checks statement at publish; consumer-side `npm audit signatures` prints counts of verified signatures and attestations | none found | Only versions published with provenance show it; older versions lack it | Publish time (registry), consumer verify at CLI |
| Quay | Old UI showed a shield icon on cosign-signed tags; new UI lacked it (ticket targets a fix in 3.16) | Presence | none found | Per tag | Computed by Quay from signature tags/referrers |
| Harbor | Notation: "signed status" green tick. Cosign: shown only as an expandable Accessories table (name, type, size, created) | Harbor "doesn't say" it verifies; verification is client-side `notation verify`; optional project content-trust enforces signing before pull | none | Per artifact | At push/accessory link |
| JFrog | "Evidence" (signed attestation) attached to package/version via CLI; DSSE verification at creation only if a public key alias is given, and server-side at deployment only if the key exists in Artifactory | Verify is conditional on key presence | not found in UI | Evidence attaches to package name+version+repo | At creation/deployment |
| Sonatype Nexus | Stores and serves cosign signatures as referrers; no UI state found | Not enforced server-side | none | n/a | Client |

Evidence for Artifact Hub detection (all [3P], from PRs of projects that publish to it): HTTP repos are probed for `<chart>.tgz.prov`; OCI repos for the provenance layer; the listing registered from an HTTP repo ignored a cosign signature on the OCI copy; key declared by `artifacthub.io/signKey` annotation. The changelog (via search summary) lists "Signed packages label (Helm provenance file)" and "Check cosign sigstore signature in Helm OCI repos" but I could not read the changelog page to date either.

Sources:
- Docker badges [F] https://docs.docker.com/docker-hub/image-library/trusted-content/ ; DHI Attestations section and verify output [F] https://docs.docker.com/dhi/how-to/verify/
- GitHub attestation docs [F] https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations ; unknown/unknown [S, 3P] https://github.com/orgs/community/discussions/45969 ; linked artifacts [S] https://github.blog/changelog/2026-01-20-strengthen-your-supply-chain-with-code-to-cloud-traceability-and-slsa-build-level-3-security/
- Artifact Hub Verified publisher [F] https://artifacthub.io/docs/topics/repositories/ ; security report timing [F] https://artifacthub.io/docs/topics/security_report/ ; detection evidence [S, 3P] https://github.com/dmazhukov/cronguard/pull/121 (fetched, quote above) and https://github.com/openartifact/helm-charts/pull/3
- npm [F] https://docs.npmjs.com/generating-provenance-statements ; badge text [S] https://github.blog/2023-04-19-introducing-npm-package-provenance/ ; badge placement [3P] https://dev.to/tsweb/publishing-npm-packages-with-provenance-55mh
- Quay icon and toggles [S, tracker] https://issues.redhat.com/browse/PROJQUAY-9579 (not opened; status unconfirmed); https://issues.redhat.com/browse/PROJQUAY-7019
- Harbor [F] https://goharbor.io/docs/edge/working-with-projects/working-with-images/sign-images/
- JFrog evidence [S] https://docs.jfrog.com/governance/docs/evidence-management
- Nexus [F] https://help.sonatype.com/en/oci-cli-usage.html

Correction to the earlier scan: its axis 5 says Harbor shows a green tick on artifacts with a cosign accessory. The fetched Harbor page ties the green tick to Notation and shows cosign only as an Accessories table [F].

Across all rows, no UI was found with a computed "verified" state next to a "failed" state; the only failure strings found are CLI output (Docker Scout, npm audit counts). Badge-type states in Docker Hub are programme membership, not signature results.

---

## 3. Per-version matrices

- Chainguard Images Directory: a Versions tab with a table whose columns are Version (tag), Pull URL, compressed size, last changed; separate SBOM and Provenance tabs that default to the latest tag. Docs say versions released before 2023-11-15 have no SBOM data. So security metadata is per-image-tab with a tag selector, not a version-by-security matrix. [S] https://edu.chainguard.dev/chainguard/chainguard-images/how-to-use/images-directory/ , https://images.chainguard.dev/directory/image/tekton-chains/versions
- PyPI Security tab (already in prior scan): per-release states across versions including "changed provenance" relative to the previous release [F via news] https://www.helpnetsecurity.com/?p=390724
- Harbor, Quay: per-artifact/per-tag lists with signature indicator or accessory expansion (section 2); Quay's old UI had a "Show/Hidden Signatures" toggle motivated by repos with many tags carrying signatures and SBOMs [S] PROJQUAY-7019.
- GitHub ghcr: one package version list; attestation manifests show as an unknown/unknown platform row [3P].
- npm versions list and Docker Hub tag list: no found source states a signature or provenance column. [M] recalled that npm's versions tab does not carry one; unverified.

No registry UI with a version-by-column matrix of signed / SBOM / provenance states was found.

---

## 4. Mirrors and proxies in vendor products

| Product | Signatures / referrers / SBOMs | Re-sign | Upstream verification on sync |
|---|---|---|---|
| ECR pull-through cache | Since 2026-04-17, referrers API requests reach upstream and cache signatures, SBOMs, attestations into the private repo; before, listing returned nothing from upstream [F] https://aws.amazon.com/about-aws/whats-new/2026/04/amazon-ecr-pull-through-cache-referrers/ | Page makes no statement | Page says it enables "end-to-end image signature verification" workflows but not how ECR verifies |
| Harbor replication | Cosign signatures replicate with the artifact under the artifact's rule (event-based excluded: "cannot replicate signature without image") [F-via-search] https://goharbor.io/docs/2.9.0/working-with-projects/working-with-images/sign-images/ . Open issue, replication from Artifactory/Docker Registry endpoints copies legacy `.sig` but skips OCI 1.1 referrers (Cosign v3 signatures, attestations, SBOMs); triaged bug, no fix as of 2026-10 [F] https://github.com/goharbor/harbor/issues/23210 | none stated | none stated |
| Harbor proxy cache | Docs fetched do not mention signatures, accessories, referrers or SBOMs; advice is only to keep endpoints in your trust boundary [F] https://goharbor.io/docs/edge/administration/configure-proxy-cache/ | none stated | none stated |
| zot sync | `syncLegacyCosignTags` default `true` (copies `sha256-<digest>.sig` style tags and `.sbom`); `onlySigned: true` limits sync to signed images ("notary or cosign") but is presence-only per the page; digests preserved; streaming on-demand sync lets bytes reach clients before digest verification finishes; docker-schema-2 upstreams need `http.compat: ["docker2s2"]` to keep signatures/referrers. Referrers named only in the timeout description [F] https://zotregistry.dev/v2.1.23/articles/mirroring/ | none stated | `onlySigned` is a filter, not verification (page does not say it verifies) |
| Quay mirroring | A tracker bug says Quay mirror hardcodes skopeo `--remove-signatures`, stripping cosign and others; unresolved in the snippet [S, tracker] https://issues.redhat.com/browse/PROJQUAY-10861 | n/a | n/a |
| Nexus proxy | Nexus stores and serves cosign signatures as referrers; docs do not say whether proxy repositories pull referrers from upstream; no server-side enforcement [F] https://help.sonatype.com/en/oci-cli-usage.html | none | none (client-side `cosign verify` only) |
| Artifactory remote | Referrers API since 7.90.1 [S] https://jfrog.com/help/r/jfrog-artifactory-documentation/use-referrers-rest-api-to-discover-oci-references ; remote-repo handling of referrers/`.sig` not found; JFrog remote docs not read (redirect) | none | none found |
| Google Artifact Registry remote | Remote-repo docs fetched say nothing on signatures, attachments or referrers; a Google page lists attached artifacts via OCI 1.1 referrers for Docker repos (does not say if remote repos included) [F] https://docs.cloud.google.com/artifact-registry/docs/repositories/remote-repo ; [S] https://docs.cloud.google.com/artifact-registry/docs/manage-metadata-with-attachments | none | none |

General causes named for "signature disappears after mirroring" [3P]: referrers not copied, subject digest changed (platform subset, media-type conversion), signature on a child manifest rather than the index, separate `COSIGN_REPOSITORY`, legacy `.sig` vs referrers format, registry without referrers API or fallback, proxy cache fetching lazily without referrers, retention policies treating untagged referrers as garbage. https://oneuptime.com/blog/post/2026-08-11-cosign-signature-disappears-after-image-mirroring/markdown

Across all vendors found: no product documents re-signing during mirror/proxy, and none documents verifying the upstream signature at sync time. ECR's wording is the only one that points that way and states no mechanism.

---

## negative:
- aqua policy page and the `slsa_provenance`/`minisign` registry pages: 404 or not fetched; policy model not covered. The aqua `cosign` page does not state whether verification runs with no block.
- Homebrew: docs fetched say opt-in; whether newer brew flipped the default was not confirmed. The Bottles page has no attestation content.
- Artifact Hub: no official page found describing "Signed" detection or whether cosign verification is cryptographic; all detection evidence is 3P PRs, and one reports the cosign check does not apply to HTTP-registered repos.
- Docker Hub: no tag-list signed/attestation indicator found; Scout UI states not found.
- GitHub: no official description of a verified/failed state in the UI.
- npm package-page details: docs page does not describe the page; badge details are 3P.
- Quay ticket outcomes and Harbor 2.x referrer replication: unconfirmed.
- Artifactory remote, GAR remote, Harbor proxy cache, Nexus proxy: docs fetched are silent on referrers, so "silent" is not "unsupported".
- Search-summary claims (tagged [S]) were not corroborated by opening the page.

## leads:
- Read Artifact Hub tracker source (`cmd/tracker`, Helm and OCI handlers) for exactly what "signed" checks and whether cosign is verified or only detected.
- Open aqua `slsa_provenance`/`minisign` pages and the policy page (`aqua-policy.yaml`) once the right URLs are found.
- Check current `brew` source for the `HOMEBREW_VERIFY_ATTESTATIONS` default and what `brew info` prints.
- Read the Docker Hub "Explore attestations on Docker Hub" DHI page and screenshot the tag page for any signed indicator.
- Test an Artifactory remote repo and a GAR remote repo against a cosign v3-signed image with `oras discover`: docs are silent.
- Read zot's sync source/config for referrers sync and `onlySigned` check depth.
- Open Quay PROJQUAY-9579/10861 on redhat.atlassian.net for status and fix versions.
- Read the Chainguard directory image page structure (Versions/SBOM/Provenance tabs) as the closest per-version display found.
