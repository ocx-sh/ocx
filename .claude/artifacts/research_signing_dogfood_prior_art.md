# Research: Signing dogfood, prior art

## Metadata
- Date: 2026-11-13
- Lane: prior-art web scan (read-only; no recommendation, no ranking).
- Method: WebFetch of primary pages where they loaded; WebSearch summaries otherwise. Tags: **[F]** = page fetched and read; **[S]** = from a search-result summary only (page not read); **[M]** = recalled from memory, not verified this session; **[3P]** = third-party blog, not project documentation.
- Coverage: axes 1, 2, 5 reasonably covered; 3, 4, 6 partial. Cut or not reached: crates.io trusted-publishing page (fetch returned no content), Homebrew trusted-publisher/bottle signing rules, Kubernetes and distro-archive signing policy (memory only), GitHub Packages UI, Docker Hub "signed" badge, zot sync docs (redirect page only), crane and `cosign copy` docs (404), Syft SPDX/CycloneDX tool-marking fields (not fetched), syft binary-classifier coverage list.
- Fetch reliability note: WebFetch summarises through a small model; several answers said "page does not cover X". Those are reported as "not found on that page", not as "does not exist".

---

## Axis 1. SBOM for an artifact one did not build

### What can be derived, by input kind
**(a) Upstream release archive of native binaries**
- Syft scan types: directory scans use "installed packages + declared dependencies", image scans "installed packages only"; the catalogers guide does not describe archives as a scan type. [F] https://oss.anchore.com/docs/guides/sbom/catalogers/
- The `binary` tag covers `binary-classifier-cataloger`, `cargo-auditable-binary-cataloger`, `dotnet-portable-executable-cataloger`, `go-module-binary-cataloger`. [F] same page. The classifier works by pattern-matching known software (version strings in files of known projects). From memory, it covers a fixed list of well-known projects (e.g. busybox, python, node, openssl-style libs), not arbitrary binaries. [M] No fetched source states the list; `syft cataloger list` prints the live set. [S]
- A native binary with no embedded metadata (C/C++/Zig/stripped) yields at most a classifier hit or nothing. No fetched source quantifies accuracy.
- trivy fs: output is CycloneDX or SPDX 2.3; the page does not distinguish lockfile detection from binary detection and does not mention Go/Rust binaries or wheels. Filesystem mode does not look for pre-existing embedded SBOM files (`.spdx`, `.cdx.json`) per the support table. [F] https://trivy.dev/docs/latest/guide/supply-chain/sbom/
- cdxgen: Apache-2.0; CLI, library (ESM, Node 24+), server; input types "source, images, binaries, git URLs, purls"; CycloneDX 1.6/1.7/2.0 plus SPDX 3.0.1 JSON-LD export. Binary analysis not described in detail. [F] https://github.com/CycloneDX/cdxgen

**(b) Python wheels and a PEP 751 lock**
- PEP 770: Final, Standards Track, resolved 2025-04-11; reserves `.dist-info/sboms/`; install tools MUST copy files from it; content opaque, producers SHOULD use CycloneDX/SPDX and SHOULD include creation time and creating tool; indices may validate but must not reject unknown standards. Now a historical doc; canonical text is the PyPA specs page. [F] https://peps.python.org/pep-0770/
- Adoption per the PEP: reference implementation is an auditwheel fork emitting CycloneDX for bundled shared libs (worked with Syft and Grype); follow-up informational PEP on turning packaging metadata into SBOMs and a tool benchmark were planned; no mainstream generator listed. [F] same URL. Current adoption beyond that not verified.
- Syft Python: `python-installed-package-cataloger` (directory + image; reads site-packages/dist-info) and `python-package-cataloger` (directory only; reads requirements.txt, setup.py etc). [F] catalogers guide. No fetched source mentions a `pylock.toml` cataloger. [S] (search for it found nothing)
- cyclonedx-python: Apache-2.0 (OWASP); inputs are Python environments, Poetry, Pipenv, requirements.txt; uv/PDM only via their virtualenvs; no mention of PEP 751 / `pylock.toml`; CycloneDX only. [F] https://github.com/CycloneDX/cyclonedx-python
- cdxgen Python: requirements.txt, setup.py, pyproject.toml, poetry.lock trees; C/Python via bundled "atom". `pylock.toml` not mentioned. [F] cdxgen page.
- Consequence for a lock-file-derived SBOM: the lock names resolved distributions (and, per PEP 751, hashes); a wheel's own `.dist-info/sboms/` (if the upstream shipped one) is a separate, author-supplied document. Not verified: whether any tool reads `.dist-info/sboms/` when generating.

**(c) Rust or Go binaries**
- cargo-auditable embeds a zlib-compressed JSON dependency tree in section `.dep-v0` (<4 kB for 400+ deps; URLs/paths redacted; reproducible). Readers: Syft 1.15.0+ (older need `--catalogers all`), Trivy 0.31.0+, Grype 0.83.0+, osv-scanner, blint, `auditable2cdx`. Limits: only binaries built with it; statically linked C libs not tracked; does not defend against supply-chain attack. Adopted by Alpine, NixOS, openSUSE, Void, Chimera, Wolfi; Ubuntu 26.04 for select packages. [F] https://github.com/rust-secure-code/cargo-auditable
- A Rust binary from an upstream that did NOT use cargo-auditable has no such section; no fetched source describes recovery from such a binary (panic-path strings etc. are not claimed anywhere I read).
- Go: `go-module-binary-cataloger` reads module info embedded by the Go toolchain (`runtime/debug.BuildInfo`), applicable to directory and image scans. [F catalogers list; mechanism M]
- cargo-cyclonedx is a source/Cargo.lock-side generator (needs the source tree), so it applies to a build one controls, not to an unseen upstream binary. [M]. This repo's AGENTS.md already lists `cargo-cyclonedx` in `[group.docs.tools]` for its own SBOM page.

### Licences and use as CLI vs library
- Syft: Apache-2.0; "CLI tool and Go library"; formats CycloneDX, SPDX, Syft JSON, can convert. [F] https://github.com/anchore/syft (README-level only; no usage details, no release number shown).
- cdxgen, cyclonedx-python: Apache-2.0 [F]. Trivy licence not stated on the page read (Apache-2.0 from memory [M]).
- Other Anchore products: nothing fetched; Grype (scanner) is out of scope.

### How the document marks itself as tool-generated
- CycloneDX 1.6: `metadata.tools` lists tools used to create/enrich/validate the BOM; `metadata.lifecycles[].phase` in {design, pre-build, build, post-build, operations, discovery, decommission}; component `manufacturer` "common in components created through automated processes", `authors` for manual; `supplier` may be manufacturer, distributor or repackager. The page does not say which lifecycle a post-hoc scan should use (inference only: `post-build`/`discovery`). [F] https://cyclonedx.org/docs/1.6/json/ (first 100k of 221k chars read; `evidence` section not read)
- SPDX: `creationInfo.creators` with `Tool:` entries (from memory [M]); not fetched.
- PEP 770 says producers SHOULD record creation time and creating tool [F], which is the only wheel-level convention.
- CISA minimum elements: 2025 draft (comments to 2025-10-03) added tool name, component hash, licence and "generation context" (how/when/by whom); separated "SBOM Author" from "Software Producer"; a 2026 final replaced the 2021 NTIA set and reportedly renames supplier to "component producer". Secondary sources only; primary PDF not read. [S] https://www.cisa.gov/sites/default/files/2025-08/2025_CISA_SBOM_Minimum_Elements.pdf
- Upstream-vs-redistributor: the schema-level tools are `supplier`/`manufacturer` (CycloneDX) and the author/producer split (CISA). No fetched source describes a convention for "this SBOM was written by a redistributor, not the upstream". Trivy fs does not ingest an upstream SBOM found in a directory [F], so a scan-and-replace pipeline would discard an author-supplied one unless the redistributor handles it.

### Against / limits
- Scan-derived SBOMs describe what the scanner recognised, not what the author built; the cargo-auditable page itself warns SBOMs are not a reliable post-attack impact tool [F].
- Syft's own docs state image scans assume installation is complete and directory scans mix declared and installed [F]; an extracted release archive is a directory scan, so declared-dependency catalogers (requirements.txt, go.mod, Cargo.lock if shipped) may emit components that were never part of the shipped binary. Not demonstrated in a source; inference.

---

## Axis 2. Copying signatures, SBOMs and attestations between registries

| Tool | What the source says | Source |
|---|---|---|
| `oras cp -r` | Copies artifact plus referrers; docs show a signature attached to an SBOM (nested), visible in `oras discover -o tree`; option still labelled preview in current docs per 3P; `--to/--from-distribution-spec` selects referrers API vs tag fallback (`v1.1-referrers-tag`). Official page URL 404'd for me; claims come from search summary. | [S] https://oras.land/docs/commands/oras_copy ; [3P] https://oneuptime.com/blog/post/2026-08-11-copy-container-images-with-signatures-sboms-attestations/markdown |
| `regctl image copy` | `--referrers` includes referrers; `--referrers-src/-tgt` external locations; `--digest-tags` copies `sha256-<digest>.*` tags (legacy cosign `.sig/.att/.sbom`); `-p/--platform` restricts to one platform; `--force-recursive` repairs missing nested content. Page says nothing on referrers of referrers. | [F] https://regclient.org/cli/regctl/image/copy/ |
| `skopeo copy` | Man page: `--preserve-digests` (fails if digests cannot be preserved; use with `--all`), `--sign-by` (simple signing), `--sign-by-sigstore[-private-key]`, `--remove-signatures`, `--remove-list-signatures`, `--multi-arch system|all|index-only|list`; subset/`index-only` makes sparse lists that usually fail unless instances already exist; `--strip-removed-platforms` requires removing signatures. No mention of referrers. Issues: copying cosign artifacts (containers/image #1946) and subject-attached artifacts (#1848) are open feature requests; registry-to-registry copy does not carry a cosign signature (skopeo #2203). | [F] https://github.com/containers/skopeo/blob/main/docs/skopeo-copy.1.md ; [S] issue numbers via search summary |
| `crane copy` | [3P] post says no recursive referrer copy guarantee. Official docs not read. | [3P] oneuptime link above |
| `cosign copy` | Official doc URL 404'd. From memory: copies signatures/attestations/SBOM tags for an image, `--only`, `--platform`, `--force`; tag-based pre-v3 layout. Unverified. | [M] |
| Harbor replication | Cosign signatures are "accessories" replicated with the artifact under the artifact's rule; Harbor-to-Harbor keeps the link, to non-Harbor registries signatures appear as referrers; only manual/scheduled triggers apply (older 2.7 doc said event-based impossible); deleting artifact deletes signatures. | [S] https://goharbor.io/docs/edge/working-with-projects/working-with-images/sign-images/ |
| zot sync | Docs page fetched was a redirect stub; not read. | not covered |

- Referrers of referrers (signature on an SBOM): only `oras cp -r` is documented (via search summary) as handling the nested case; regctl docs are silent. Recursive copy follows only subject-based referrers; legacy `.sig` tags and artifacts in a separate `COSIGN_REPOSITORY` must be copied separately. [3P] oneuptime post (cosign v3 stores bundles as OCI 1.1 referrers by default, same source).
- Index digest on partial copy: skopeo's `--preserve-digests`/sparse-list text shows that copying a platform subset produces a different index unless a sparse index is pushed as-is [F skopeo page]. regctl `--platform` copies one platform [F]; whether it rewrites the index was not stated. A signature on the original index digest does not match a rewritten index (inference from digest semantics, no source states it).
- Registries without referrers API: tag fallback schema (`sha256-<digest>` tag holding an index of referrers) is in the OCI distribution spec (M, not fetched). Concerns: concurrent-writer races on the fallback tag, retention/GC (3P post). ECR: one 3P post reports 405 on pushing an OCI 1.1 referrer manifest via recursive copy as of 2026-03-06; an AWS containers-roadmap issue (#2783) describes a related SPDX-referrer failure. [S] https://github.com/aws/containers-roadmap/issues/2783
- Re-signing by the mirror operator: no primary source found that states a convention. Evidence adjacent: skopeo `--sign-by`/`--sign-by-sigstore` adds a signature for the destination name [F]; Homebrew's backfill (axis 4) is the one documented case of a different identity signing on behalf of upstream, with verifiers distinguishing by signer identity and date. Cosign verification distinguishes by certificate identity/issuer, not by "kind of signer" (M).

---

## Axis 3. Per-platform vs image-index signing; per-platform SBOMs

- cosign, from one practitioner's notes (not project docs): index-only signing verifies at the index but a platform manifest fails "no signatures found"; per-manifest signing verifies each but index unsigned; `cosign sign --recursive` signs index and every platform manifest; `cosign attest --recursive` attested only the index in the author's test; no `--recursive` on `cosign verify*`. [F] https://github.com/lcarva/notes/blob/main/sigstore-multi-arch.md (one author, version unspecified, behaviour may have changed; sign doc string "multi-arch image AND all referenced, discrete images" from search [S])
- docker buildx attestations: stored as manifests referenced by the image index, one attestation manifest per platform image manifest, each with one layer; with `oci-artifact=true` they carry a `subject` and `artifactType`. The page does not say how this affects the index digest or covers signing/referrers. [F] https://docs.docker.com/build/metadata/attestations/ . Consequence (inference): attestations live in the index, so index content depends on them.
- Chainguard: each platform image carries its own single-arch SBOM; the index SBOM represents variants as materials/relationships ("variants of"), recursively discovered; `mediaType` field non-standard. Signing mechanics not described on the page read. [F] https://www.chainguard.dev/unchained/sboms-in-a-multi-architecture-world
- GitHub `actions/attest-sbom`: binds a named artifact digest to an SPDX/CycloneDX JSON SBOM as in-toto attestation, Sigstore-signed, stored in the GitHub attestations API, verified via `gh attestation verify`; the action is deprecated, v4+ wraps `actions/attest`. Page does not address multi-platform or one-per-subject. [F] https://github.com/actions/attest-sbom ; storage and public/private Sigstore instance details [F] https://docs.github.com/en/actions/concepts/security/artifact-attestations
- Homebrew bottles: one attestation per bottle file (per OS/arch bottle) in the Homebrew/homebrew-core workflow [M, not fetched this axis]; no multi-platform index concept.
- Not covered: `attest-build-provenance` subject handling for several files.

---

## Axis 4. Retroactively signing already-published releases

- Homebrew: pre-existing bottles were backfilled with attestations signed by a different identity (`trailofbits/homebrew-brew-verify`, workflow `backfill_signatures.yml`). Verification order: (1) provenance from homebrew-core `publish-commit-bottles.yml`; (2) else a backfill attestation predating a cutoff date; (3) else hard fail. Reason: avoid downgrade by stripping metadata, and rebuilding old bottles was infeasible. Cutoff stops an attacker using a backfill attestation to inject a new bottle. The proposal describes backfilled signatures as notarisations, not build provenance. Date of the cutoff not in the article. [F] https://blog.trailofbits.com/2024/05/14/a-peek-into-build-provenance-for-homebrew/ ; proposal [S] https://repos.openssf.org/proposals/build-provenance-and-code-signing-for-homebrew ; `BACKFILL_CUTOFF` constant [S] https://rubydoc.brew.sh/Homebrew/Attestation.html
- PyPI: attestations attach at upload; Warehouse design issue deferred augmenting existing files as out of scope. Help Net Security (2026-11-09) reports a PyPI Security tab flagging files uploaded >14 days after initial release and a July 2026 refusal of late uploads; secondary source only. [S] https://redirect.github.com/pypi/warehouse/issues/15871 ; https://www.helpnetsecurity.com/?p=390724 . The Security tab text says releases with no attestations are "common for older releases", i.e. no backfill. [F] same article.
- npm: provenance generated at publish; nothing found about adding to existing versions (inference from design, not stated). [S]
- Kubernetes (release signing with cosign from v1.24 era) and distro archives (Debian/Fedora signing at archive level): not fetched; not claimed.
- Transparency-log meaning of a later-dated entry: no primary source found stating it; the Homebrew cutoff is the closest design (a log/attestation timestamp after the cutoff is rejected as backfill). The implication that a Rekor entry's integrated time proves signing time, not build time, is from memory [M].
- Objections: none documented in the Trail of Bits article; Trail of Bits own caveat is that provenance does not stop a malicious but legitimately built package [F].

---

## Axis 5. How package UIs show signature, provenance, SBOM state

- PyPI: per-file attestation info on the files page / file details (blog says UI page showing whether attestations exist) [S] https://blog.pypi.org/posts/2024-11-14-pypi-now-supports-digital-attestations/ ; Security tab (blog 2026-11-06 per press) states per release: full provenance (every file has >=1 attestation), none, partial/inconsistent (warning), changed provenance (different repo/workflow vs previous version, or attestations stopped; warning); each file shows attestation type (PyPI Publish vs SLSA Build Provenance) and a link to the transparency log. [F] https://www.helpnetsecurity.com/?p=390724 . Verification happens at upload (max two attestations, one per predicate; identities from GitHub Actions, GitLab CI/CD, Google Cloud) [F] https://docs.pypi.org/attestations/ ; where "verified" is computed for display not stated. Optional per file. This is the one found design that handles a stale/changed state as an explicit warning compared to the previous version.
- deps.dev: shows PyPI attestations panel per version (PyPI Publish attestation example) and npm SLSA provenance; the blog does not say deps.dev itself verifies or what states it shows. [F] https://blog.deps.dev/pypi-attestations/
- npm provenance badge: generated at publish, registry checks statement against name/version/digest [S] https://socket.dev/glossary/package-provenance ; UI detail not fetched [M].
- Artifact Hub: the docs I read cover "Verified publisher" (publisher puts `artifacthub-repo.yml` with repositoryID; flag set next time the tracker processes the repo; unchanged repos skipped) [F https://artifacthub.io/docs/topics/repositories/]. Security report: scanner runs twice an hour; latest version rescanned daily, older weekly; versions older than one year and with >15 images not scanned [F https://artifacthub.io/docs/topics/security_report/]. How the "signed" badge is detected was not found in either page. So: state computed by a crawler/tracker, per version, with periodic recompute.
- Docker Scout / Docker Hub: `docker scout attest list/get --verify`; SBOM and provenance attestations at build used instead of Scout's own SBOM; image details page shows package/vuln breakdown from Scout analysis. "Signed" badge and staleness not found. [S] https://docs.docker.com/guides/docker-scout/attestations.md
- GitHub: attestations stored in the attestations API (public: also Sigstore public-good log); consumer verification via `gh attestation verify`; UI display not described on the page read. [F] https://docs.github.com/en/actions/concepts/security/artifact-attestations
- Harbor: shows signed state as a green tick on artifacts with a cosign accessory [S] https://goharbor.io/docs/edge/working-with-projects/working-with-images/sign-images/ (presence, not a verification).
- crates.io: not covered.
- States observed across sources: PyPI has none / full / partial / changed; Harbor and Artifact Hub-style displays show presence. No fetched source shows a "failed verification" state in a UI.

---

## Axis 6. Rules for changing who may publish

- npm: up to 10 trusted-publisher connections per package (the 2026-09-03 changelog added multiple; older docs said one); existing connections cannot be edited, delete and recreate; allowed actions per connection (`npm stage publish` always, `npm publish`/`dist-tag` optional); new unvalidated configs expire after 2 days (docs) / 48 hours (GitHub 2026-10-02 changelog); identity change restarts the window; binding switches to immutable repository ID after first successful publish; `issue_comment` events rejected; checks at publish: exact case-sensitive workflow filename match, `id-token: write`, GitHub-hosted runners only; no validation at save time. Auth needed to add/remove not stated in the docs page; the `npm trust` man page says first request needs 2FA (search summary). Per-package enforcement: "Require two-factor authentication and disallow tokens" blocks tokens but trusted publishing keeps working; provenance can be disabled via `publishConfig.provenance:false`. Rotation: OIDC tokens are per-publish; no rotation story. [F] https://docs.npmjs.com/trusted-publishers ; [F] https://thehackernews.com/2026/10/github-tightens-npm-publishing-after.html ; [S] https://github.blog/changelog/2026-09-03-multiple-trusted-publishing-configurations-for-npm
- npm "SlugFest": trust keyed on owner/repo name until first publish; reclaiming a freed name could publish; >1,500 packages exposed, no confirmed exploitation; reported 2026-06-06; mitigations above. [F] https://startwithidentity.com/blog/2026-10-13-npm-trusted-publishing-slugfest-let-reclaimed-repo-names-publish-packages/ (the article says NuGet keys on numeric IDs, via search [S])
- PyPI: publishers belong to projects, not users; removing a maintainer does not remove their publishers; GitHub workflow rename indistinguishable except by repository-state claims; GitLab namespace delete/recreate can publish until first use; short-lived token (<=15 min); attestations presented as complement, optionality not stated on that page. Add/remove procedure and permissions not in the page read. [F] https://docs.pypi.org/trusted-publishers/security-model/
- RubyGems: only gem owners create trusted publishers on existing gems; "pending publishers" for new gems convert on first push and make the pusher owner; multiple publishers per gem; environment must match if configured; add/edit/remove not described; attestations not mentioned. [F] https://guides.rubygems.org/trusted-publishing/
- crates.io: search summary only: must publish once manually, fields owner/repo/workflow/optional environment; numeric-ID handling unknown. [S, one third-party guide]
- Homebrew: not covered.

---

## negative:
- Official `oras copy`, `cosign copy`, and `cosign sign` doc URLs 404 or lacked the relevant content; findings for those rest on search summaries and one third-party blog (OneUptime, 2026-08-11) that I could not corroborate with project docs.
- No source found that documents how a verifier tells "upstream signature" from "mirror operator re-signature" other than by signer identity/date (Homebrew cutoff).
- No fetched source documents referrers-of-referrers behaviour for regctl, skopeo, crane, zot sync, Harbor.
- skopeo: referrers/OCI artifact copy is an open feature request per issue titles (search summary); not re-checked against current main.
- Syft docs I read do not list archive scanning, a `pylock.toml` cataloger, or the classifier's package list.
- cyclonedx-python and cdxgen pages do not mention PEP 751 / `pylock.toml`.
- cosign `--recursive` for `attest` did not attest platform manifests in the single practitioner test; may be version-specific.
- No UI source shows a "failed verification" state.
- npm "can't backfill provenance" is an inference, not a stated policy. PyPI "no late attestations" comes from a news article, not PyPI docs.
- npm docs vs changelog conflict on one vs ten trusted-publisher configurations (docs fetched now say 10).

## leads:
- Read the OCI distribution spec referrers section and tag-fallback schema: needed to state exactly what breaks without the referrers API (axis 2).
- Read regclient source for `--referrers` recursion depth: answers signature-on-SBOM copy for regctl.
- Run `syft cataloger list` and a scan of a sample Go, Rust, and stripped C release archive: gives measured accuracy; docs do not.
- PyPA specs page for `.dist-info/sboms` and any generator that reads it (auditwheel fork, uv, build backends): current PEP 770 adoption.
- Warehouse source / docs for how the Security tab computes "changed provenance" (at upload vs at view): closest prior art for stale-state handling.
- Artifact Hub tracker source: how cosign signatures on OCI repos are detected and the "signed" badge computed.
- Sigstore cosign v3 docs on bundles and referrers (and `cosign verify` behaviour for index vs platform): replaces the single-author notes.
- crates.io trusted-publishing docs and RFC; Homebrew `brew verify` current status and `BACKFILL_CUTOFF` value.
- Kubernetes release-signing and Debian/Fedora archive-signing policy pages (axis 4 cut).
- SPDX 2.3/3.0 creationInfo and CycloneDX `evidence`/`lifecycles` guidance for post-hoc generated SBOMs (axis 1 marking).
