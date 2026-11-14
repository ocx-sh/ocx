State: parked · Updated: 2026-11-13
Participants: owner + Claude; one session, 2026-11-13 (split out of `.agents/discussions/signing-dogfood.md`)

# Discussion: Signing dogfood, loop C — ocx-mirror SBOMs, signed registry mirroring and index signing

Parked on purpose, not abandoned. The owner cut this out of the signing-dogfood loop on 2026-11-13 after an evaluation put one loop over seven repositories at roughly 10 to 15 percent for the full target, and asked for committed files that restore the whole context later. Resume with `/hex-discuss signing-dogfood-mirror`; the interview is nearly complete, so a resume goes to the open questions and then the restate. Nothing here was ratified: no restate was answered with a yes for this part.

The three parts of the split:

- **Loop A**, `.agents/discussions/signing-dogfood.md`: `ocx-sh/ocx`, `ocx-sh/ocx-mirror` and `ocx-sh/e2e-package`. Dev pre-release channel, own dispatched release workflow with release, candidate and dev modes, per-platform SBOM and signing, backfill, `ocx package prune` on GHCR, client carry-overs. Runs first. The owner moved ocx-mirror's own release cycle into loop A on 2026-11-13, after this file was first committed, so loop C starts from a mirror that already releases that way.
- **Loop B**, `.agents/discussions/signing-dogfood-index-catalog.md`: index bot, index, catalog, theme, website.
- **Loop C**, this file.

B and C can run in parallel. Both start once loop A's dev channel exists, because both consume an unreleased ocx through it. Loop C also reuses loop A's release, SBOM and backfill workflow shape for ocx-mirror's own release.

## Intent

Owner, 2026-11-13, for the whole effort: "I want to target a loop, we need to dogfood the signing now, that is adopting the indexbot, catalog, ocx-mirror and ocx. This should be done in a none-breaking way."

The part of the owner's target image that is loop C:

- ocx-mirror releases publish an SBOM for each platform individually; each platform and its SBOM are signed. ocx-mirror has a manual workflow to backfill SBOMs and signatures.
- ocx-mirror supports the same release and backfill for what it mirrors, with dedicated new interfaces for SBOM retrieval, improving the mirror SDK as well. Where no SBOM can be provided, one is generated from the information at hand, especially for the PyPI use case; anchore/syft or other Anchore products are to be considered.
- ocx-mirror mirroring of registries supports, and is tested to support, mirroring signatures and SBOMs, including signatures of SBOMs. It also supports re-signing, end-to-end tested: of the index, of the package releases and of the SBOMs. Proven to work with subsets as well.
- Everything is end-to-end tested, including a new `e2e-mirror` repository.

Re-signing an index needs index signing to exist, which was designed on 2026-11-13 and never built; the owner put it in scope with `ocx.sh` rolled out to `warn`.

Non-breaking, as this discussion uses it *(Claude's reading)*: no existing install, config file, published package, index entry or mirror spec stops working. New checks ship opt-in or as a warning. Flipping a default to refuse is a later owner step.

Out of scope: vulnerability scanning; cutting a real ocx-mirror release or dispatching a backfill on production tags (the owner's acts); the `ocx.sh` index signature at `enforce`; rolling SBOMs and signing out to the mirror fleet repositories (`ocx-contrib/mirror-*`); a mirror mode that replaces upstream `publishers`; a generic SBOM command hook in ocx-mirror; the address-keyed `[sigstore."<host>"]` config tables; merging.

## Requirements

Provisional prose. Repositories: `ocx-sh/ocx-mirror` (local `/home/mherwig/dev/ocx-mirror`), `ocx-sh/ocx-mirror-sdk` (`/home/mherwig/dev/ocx-mirror-sdk`), `ocx-sh/ocx` for the client side of index signing, `ocx-sh/index` and `ocx-sh/indexbot` for the signed catalog, and a new fixture `ocx-sh/e2e-mirror` (it does not exist; `ocx-contrib/e2e-mirror` is a different, existing canary and stays as it is).

**ocx-mirror's own release**

- Moved to loop A (owner, 2026-11-13): the dispatched release workflow with release, candidate and dev modes, per-platform SBOM, signing, backfill and dev builds to `ghcr.io/ocx-sh/dev/mirror`. Loop C builds none of it; a resume checks that loop A delivered it.

**SBOMs for mirrored packages**

- A mirrored package gets an SBOM from the first source that yields one: the upstream's (located through a new SDK interface and a `url_index` field), syft (first-class, opt-in), the built-in minimal generator. For PyPI the minimal generator lists every wheel of the resolved lock with its hashes and licence. A generated SBOM says it was produced by the mirror, not by the upstream. The SBOM is attested with the signer the `sign:` block names.
- A package's SBOM names each OCX dependency as a component by digest (a PyPI package depends on its pinned interpreter package) and does not embed the dependency's SBOM.
- The existing backfill command (`ocx-mirror package pipeline sign`) also backfills SBOMs for packages already mirrored.

**Registry mirroring**

- `registry sync` carries signatures and SBOM attestations, tested. It gains subsets (packages, tags, platforms). With re-signing on, it adds the mirror's signature to each package and SBOM, appends the mirror's signer to `publishers` in the mirror's index, and signs that index; the signer is a key file or keyless on a configurable Sigstore instance. It verifies the upstream index signature while syncing unless told not to.

**Index signing (client, index, bot)**

- As designed in `.agents/discussions/release-signing-trust-default-2026-11-12.md` › Decisions › Index: `ocx index sign` (key or keyless), one signature over the exact bytes of `c/index.json` served beside it, a per-registry index signer list in the `publishers` vocabulary, an `expires` date, a client check on the remote-to-local write path with the modes off, warn and enforce, and a built-in signer for `ocx.sh` whose default mode is warn.
- The catalog schema in `ocx-sh/index` admits the signature fields (`schema/c-index.schema.json` sets `additionalProperties: false` today).
- The `ocx.sh` signing job (re-sign weekly and on every change, 30 days validity) and an alert that runs outside the index repository and fires when `expires` is less than 21 days away.
- `ocx package sbom` takes `--dependencies` / `--no-dependencies` (client work in `ocx-sh/ocx`; placed here by Claude, movable to loop A).

## Decisions

Working positions taken with the owner on 2026-11-13.

- **Index signing is in scope and `ocx.sh` rolls out to `warn`**, not `enforce`. Ordering constraint: the `ocx.sh` signing job must be deployed before a client with the `warn` default is released, otherwise every fresh fetch warns about an unsigned index.
- **Mirror SBOM source order, with syft as a first-class variant in the mirror spec**: upstream SBOM (located by the generator script through a new SDK interface and `url_index` field, or a release asset) → `syft` (opt-in variant; the generated workflows provision a pinned syft as an ocx package) → built-in minimal generator. No generic command hook. Reason: the SDK runs before any download and cannot scan content, and a free-form command cannot tell the generated workflows which tool to install or pin. Owner's first wording: "there should be an sdk hook, if not present a minimal generator is used", with syft supported and configurable.
- **Registry sync with re-signing appends the mirror's signer** (owner: "reasonable first usage"): upstream signatures travel verbatim and the upstream `publishers` entries stay; the mirror adds its own signature and its own entry in the mirror's index. The mirror's signer is configurable as a key file or as keyless on a Sigstore instance (public or an operator's own Fulcio and TUF root), as ocx-mirror's existing `sign:` block already distinguishes. The spec shape must leave room for a later option that replaces the upstream entries; that option is not built now.
- **The end-to-end mirror repository is a new `ocx-sh/e2e-mirror`**, beside `ocx-sh/e2e-package` and `ocx-sh/e2e-index`.
- **`ocx package sbom` and dependencies** (owner's direction, not final): a package's SBOM does not silently absorb its dependencies' SBOMs; a flag pair in the flattened clap style, `--dependencies` / `--no-dependencies`, selects whether the SBOMs of the dependency closure are returned too.
- **Production release acts stay with the owner**: the loop lands workflows by pull request and proves them in `ocx-sh/e2e-mirror`. It cuts no release, dispatches no backfill on production tags and writes nothing to `ocx.sh` or to the production package `ghcr.io/ocx-sh/ocx/mirror`.
- **Pre-releases live in a GHCR dev namespace** (`ghcr.io/ocx-sh/dev/cli`, `ghcr.io/ocx-sh/dev/mirror`, working names), chosen by the owner after `dev.ocx.sh` was found to serve no registry any more (checked 2026-11-13: `https://dev.ocx.sh/v2/` answers 404 with the website's page). Built by loop A for both ocx and ocx-mirror.
- **Loop shape** (owner, for every loop of this effort): build and integrate until the end-to-end cases work together, and fixing what integration turns up is not a review round and is not capped; then three to five Sonnet subagents act as end users in isolated environments and try to break things, each bug getting a regression test and a fix; only then the review rounds, at very coarse granularity, at most 4, with a pasted hard cap of 12. A blocker found in review is fixed even at the limit. Against drift: a round that finds only smaller or unrelated issues ends the reviewing; those are fixed and no further review starts.
- **Grants the owner stated** (they bind a run only when the owner's `/hex-loop` invocation repeats them): for a new `e2e-mirror` and the other e2e repositories, "you are explicilty allowed to create, force push to, make tags, releases or change settings including gh secrets, same holds for other e2e repos"; dispatching signed dev-channel runs of the release workflow in `ocx-sh/ocx-mirror`, publishing to the dev location only; pushing locally built pre-releases to that location. Not granted: merging, a production release, tag or backfill dispatch, any write to `ocx.sh`.

## Threads

- **Two ratified ocx-mirror decisions are reversed by the target image** (`ocx-mirror` repository, `.agents/discussions/mirror-signing.md`: "SBOM: no, only the signatures"; registry-to-registry copies carry upstream signatures verbatim and add none; "a mirror is a transport and filters nothing"). The target adds SBOM generation, re-signing on a registry mirror, and subsets. The owner asked for all three in the target image but did not confirm the reversal in those words; a resume confirms it first.
- **Already broken outside this work**: `ocx-contrib/e2e-mirror` pins `cli` and `mirror` on `dev.ocx.sh`, and `ocx-contrib/mirror-pypi` pushes there. Both fail until re-pointed.

## Research

Committed copies under `.claude/artifacts/`, all dated 2026-11-13.

- `research_signing_dogfood_codebase.md` (sections 1.3, 1.4, 2, 3). ocx-mirror already has a `sign:` spec block (`crates/ocx_mirror_spec/src/sign_config.rs`), a backfill command and a `registry sync` that carries referrers (depth 2, at most 64) and cosign sidecar tags verbatim, with no platform or tag filter (`registry_copy.rs`). It has no SBOM code. It publishes in-process for GitHub and URL sources and through an `ocx` subprocess for PyPI. The SDK is a Python library for generator scripts that emit `url_index` JSON and run before any download; its schema lacks the `{url, sha256}` asset form ocx-mirror accepts. The PyPI flow knows each wheel's name, version, sha256, `RECORD` paths, summary, keywords and licence; `Requires-Dist` is not read. A signed SBOM is one Sigstore-bundle referrer whose DSSE envelope is the signature; there is no separate signature-of-SBOM referrer. `ocx package copy` carries referrers per platform manifest; the image index is re-merged, not byte-identical. Index signing is absent in the client; a fetched root's hash is compared with `c/index.json`, but a mismatch is recovered from, not refused.
- `research_signing_dogfood_dev_channel.md`. ocx-mirror links ten ocx crates through its `external/ocx` submodule (v0.6.6) and shells out to a pinned released ocx (0.6.3); its release does not use cargo-dist. `deploy-dev.yml` in ocx-mirror pushed to `dev.ocx.sh`, which is gone.
- `research_signing_dogfood_prior_art.md` (axes 1 to 4). syft finds Rust dependencies only when the upstream used `cargo-auditable` and little in opaque native binaries; wheel metadata is read well. PEP 770 is final with thin adoption. Only `oras cp -r` is documented as copying a signature on an SBOM; a platform subset changes the index digest. Homebrew is the one documented signature backfill.
- `research_signing_dogfood_competitive.md` (question 4). No vendor mirror documents re-signing or verifying upstream signatures at sync.
- `research_signing_dogfood_archaeology.md`. Mirror signing landed 2026-09-03. Constraints met again: ghcr.io has no Referrers API (fallback tag); signing an image index on ghcr.io fails ([ocx-sh/ocx#694](https://github.com/ocx-sh/ocx/issues/694)); verify caps signature candidates at 8 silently ([ocx-sh/ocx#403](https://github.com/ocx-sh/ocx/issues/403)); `package copy` of a signed package to a registry without referrers never exits 0 ([ocx-sh/ocx#392](https://github.com/ocx-sh/ocx/issues/392)). Also [ocx-sh/ocx-mirror#65](https://github.com/ocx-sh/ocx-mirror/issues/65), [ocx-sh/ocx#695](https://github.com/ocx-sh/ocx/issues/695) (the unpushed ocx-mirror lockstep patch for the publisher check).

## Related

- `.agents/discussions/release-signing-trust-default-2026-11-12.md`: the index-signature design (signer list, `expires`, `index_publishers`, `index_signature`, `index_expiry`, the alert) and the mirror consequences ("a mirror that rewrites the catalog re-signs with its own key; the client still verifies every package against `publishers`").
- `.claude/artifacts/adr_publisher_trust_distribution.md`, `.claude/artifacts/system_design_publisher_trust.md` (index signing is its "slice 4").
- In the `ocx-mirror` repository: `.agents/discussions/mirror-signing.md`, `adr_mirror_signing.md`, `adr_registry_mirror_sync.md`.
- Worktree `.agents/worktrees/sat-ocx-mirror` (branch `hex/publisher-trust-lockstep`, unpushed).

## Open questions

- [NEEDS CLARIFICATION: What is the default of `ocx package sbom --dependencies` and what does it return?]
  Recommended: default `--no-dependencies`; with the flag, one document per package of the closure, each verified against its own `publishers`, never a merged document.
- [NEEDS CLARIFICATION: Are the address-keyed `[sigstore."<host>"]` tables of the 2026-11-12 design part of this loop?]
  Recommended: no — replacing `[trust.sigstore]` is a config break and this effort is non-breaking. A mirror signer is a key file, or keyless on the instance `[trust.sigstore]` already names.
- [NEEDS CLARIFICATION: Is signing the image index on ghcr.io fixed ([ocx-sh/ocx#694](https://github.com/ocx-sh/ocx/issues/694)) here, if loop A left it?]
  Recommended: fix it — ocx-mirror signs the index above the platform manifests; the per-platform signatures remain what install verifies.
- [NEEDS CLARIFICATION: In which order do the pull requests land?]
  Recommended: ocx → ocx-mirror → SDK → index; ocx-mirror's submodule pin moves to the released ocx by a follow-up the owner triggers after releasing.
- [NEEDS CLARIFICATION: How does a subset keep upstream signatures valid when the image index is rewritten?]
  Recommended: tag and package subsets copy indexes byte for byte; a platform subset writes a new index, which carries no upstream index signature, and relies on the per-platform signatures that install verifies.
- Check on resume: what loop A built (workflow names, SBOM generator, dev-channel task) is reused, not rebuilt.

## Verification

Each check is shown red on the failing case and green on the passing one. Everything that needs a real certificate, a real registry or a served index runs in `ocx-sh/e2e-mirror`, dispatched, never per pull request.

- **Mirror SBOMs**: a mirrored package carries an SBOM from each of the three sources in turn, attested by the mirror's signer; an already mirrored package gains one through backfill.
- **Registry sync**: a sync of a subset (some packages, some tags, some platforms) keeps every upstream signature and SBOM attestation verifiable against the upstream `publishers`; with re-signing, a client configured only with the mirror's key verifies packages, SBOMs and the index with no Sigstore network access.
- **Index signing**: an index signed with a key and one signed keylessly verify; an expired or tampered one warns under warn and refuses under enforce; an unchanged client talking to an unsigned index behaves as before except for the `ocx.sh` warning.
- **Non-breaking**: an existing mirror spec without the new blocks renders the same workflows as before; the previous ocx release still installs from an index whose catalog is signed.
- **Loop phases**: as under Decisions › Loop shape.
