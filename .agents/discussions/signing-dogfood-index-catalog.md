State: parked · Updated: 2026-11-13
Participants: owner + Claude; one session, 2026-11-13 (split out of `.agents/discussions/signing-dogfood.md`)

# Discussion: Signing dogfood, loop B — index bot, index, catalog, theme and website

Parked on purpose, not abandoned. The owner cut this out of the signing-dogfood loop on 2026-11-13 after an evaluation put one loop over seven repositories at roughly 10 to 15 percent for the full target, and asked for committed files that restore the whole context later. Resume with `/hex-discuss signing-dogfood-index-catalog`; the interview is nearly complete, so a resume goes to the open questions and then the restate. Nothing here was ratified: no restate was answered with a yes for this part.

The three parts of the split:

- **Loop A**, `.agents/discussions/signing-dogfood.md`: `ocx-sh/ocx` and `ocx-sh/e2e-package`. Dev pre-release channel, own release workflow replacing cargo-dist, per-platform SBOM and signing, backfill, client carry-overs. Runs first.
- **Loop B**, this file.
- **Loop C**, `.agents/discussions/signing-dogfood-mirror.md`: `ocx-sh/ocx-mirror`, its SDK, `ocx-sh/e2e-mirror`, and index signing.

B and C can run in parallel. Both start once loop A's dev channel exists, because both consume an unreleased ocx through it.

## Intent

Owner, 2026-11-13, for the whole effort: "I want to target a loop, we need to dogfood the signing now, that is adopting the indexbot, catalog, ocx-mirror and ocx. This should be done in a none-breaking way."

The part of the owner's target image that is loop B:

- The index bot implements the new rules for `publishers` changes and optional enforcement of signatures, tested in `e2e-index`.
- The catalog gets a signed badge and an SBOM badge (two, or one merged) showing whether a package has a valid, broken or no signature and whether it has an SBOM. The details page shows this for multiple tags, similar to the compatibility matrix; possibly a new box with the publishers and the signature details, or a security matrix.
- Added by the owner during the discussion: the OCX website and the shared website component library. The components that visualise SBOMs and dependencies live once, in the shared library, at high quality; the catalog reuses them and the OCX website adopts them too, so the presentation is homogeneous across the repositories.

Non-breaking, as this discussion uses it *(Claude's reading)*: no existing install, config file, published package or index entry stops working. New checks ship opt-in or as a warning. Flipping a default to refuse is a later owner step.

Out of scope: vulnerability scanning and publishing scan results; adding `publishers` to an entry of the production `ocx-sh/index` (the owner's act, after real releases are signed and backfilled); the index signature, its signing job and its alert (loop C); npm and PyPI releases of the theme, the catalog and the index bot (the owner's); merging.

## Requirements

Provisional prose. Repositories: `ocx-sh/indexbot` (local `/home/mherwig/dev/ocx-indexbot`), `ocx-sh/index` (`/home/mherwig/dev/index`, 329 commits behind its `origin/main` when read), `ocx-sh/ocx-catalog` (`/home/mherwig/dev/ocx-catalog`), `ocx-sh/website` (`/home/mherwig/dev/ocx-website`, theme source in `packages/theme`), `ocx-sh/ocx` for the docs site under `website/`, and the fixture `ocx-sh/e2e-index` (worktree `.agents/worktrees/e2e-index`).

**Index bot and index**

- The root schema admits `publishers` (`schema/root.schema.json` in `ocx-sh/index` sets `additionalProperties: false` today). The bot round-trips `publishers` byte-exactly and no bot-written root drops it (`src/ocx_indexbot/core/validate_entry.py`, `model.py`, `cli/validate.py`).
- Any change to `publishers` is classified for human review (`core/diff.py` `classify_change`). The bot checks shape, policy and that every listed tag still verifies under the new list. New or changed tag rows are verified against the base `publishers`.
- Signature enforcement is an optional setting of an index deployment.
- The bot writes the per-package security file (Decisions) at ingest and refreshes it in its reconcile job; it also records each package's dependency list.
- All of this is proven in `ocx-sh/e2e-index`, which uses nothing of the bot today (its roots are hand-built by `scripts/build_index.py`). `ocx-sh/index` receives pull requests only.

**Theme, catalog, website**

- The shared library owns the SBOM and dependency component (the unused `DependencyExplorer` is the starting point), the signature and SBOM badge, and the security matrix.
- The catalog shows a signature badge and an SBOM badge on cards and rows (`PackageCard.astro`, `PackageRow.astro`), a box on the details page with the publishers and the signature details (`MetaRail.astro`), a security matrix over tags and platforms beside the support matrix (`PlatformMatrix.astro`, `viewmodel/support_matrix.ts`), and the dependency list with hyperlinks to catalog entries across indexes. It still never contacts a registry.
- The OCX website's dependencies page (`website/` in `ocx-sh/ocx`, `/reference/dependencies/`, today generated Markdown from `scripts/sbom-to-markdown.py`) renders ocx's own SBOM with the shared component and links to the `ocx.sh/ocx/cli` catalog entry.

**Across repositories**

- A repository that needs an unreleased ocx consumes a pre-release from loop A's dev channel (`ghcr.io/ocx-sh/dev/cli`, pinned through `ocx.toml` and `ocx.lock`); one that needs an unreleased theme, catalog or index bot consumes a branch reference. The loop cuts no production release of any of them.

## Decisions

Working positions taken with the owner on 2026-11-13.

- **Index bot rule for a `publishers` change**: every add, replace or remove needs human review. The bot checks entry shape and policy (allowed kinds; a GitHub keyless entry must pin repository ID claims) and that every listed tag still verifies under the new list, so a change can never strand a published tag.
- **Catalog signature and SBOM state comes from a bot-written sidecar file** (owner: "sounds reasonable", on condition that it is content addressed and only updated when content changed, refreshed from time to time with a time-to-live, and usable later as the entry point for vulnerability scanning). The index bot verifies with a pinned ocx at ingest and in its scheduled reconcile job and writes a per-package file beside the root, outside the files a client hashes; the catalog renders it and never contacts a registry. Shape *(Claude's proposal; the owner asked to be grilled on it and did not object)*:
  - An entry is keyed by platform manifest digest and records the digest of that manifest's referrers list, a hash of the `publishers` list and the verifier version it was computed with. An unchanged key is reused with no verification and produces no commit; a backfilled signature or a corrected SBOM changes the referrers digest and re-checks that entry only.
  - A time-to-live governs only how often the referrers list is probed: recent tags on every reconcile run, older tags weekly. A verified signature does not expire by itself.
  - The file carries no timestamp that changes without a state change, so it is deterministic.
  - States: signature `verified` | `mismatch` | `none` | `unchecked` (the check could not run; never rendered as broken); SBOM `verified` | `unverified` | `none`. Per platform, aggregated per tag for the badge and the matrix.
  - The state is advisory. No client reads it; the client's own check stays the security boundary. The catalog words it as observed by the index.
  - Vulnerability scan results would be a sibling entry keyed by SBOM digest and scanner database date. Nothing is built for it now.

  Owner's wording of the change detection: the index repository keeps this content addressed. The SBOMs and signatures of a manifest are a list; the tag that points at that list moves when one is added, so its digest changes, and comparing that digest tells the repository whether anything must be updated.
- **Dependencies in the catalog are hyperlinks**: the details page lists a package's dependencies, and one that resolves to a package in any index the catalog renders links to that catalog entry, across indexes. The dependency list reaches the catalog through the bot-written file. A badge states the package's own state only *(Claude's position)*.
- **Scope includes the OCX website and the shared component library** (`@ocx-sh/theme`): one implementation, reused by the catalog and the website. The website's dependencies page embeds the shared component and links to the catalog entry (owner chose "embed and link").
- **Non-breaking order** *(Claude's reading)*: existing tags are backfilled before `publishers` appears in a production index entry, because a client that honours `publishers` refuses an unsigned tag of a declaring package (exit 79).
- **Loop shape** (owner, for every loop of this effort): build and integrate until the end-to-end cases work together, and fixing what integration turns up is not a review round and is not capped; then three to five Sonnet subagents act as end users in isolated environments and try to break things, each bug getting a regression test and a fix; only then the review rounds, at very coarse granularity, at most 4, with a pasted hard cap of 12. A blocker found in review is fixed even at the limit. Against drift: a round that finds only smaller or unrelated issues ends the reviewing; those are fixed and no further review starts.
- **Grants the owner stated** (they bind a run only when the owner's `/hex-loop` invocation repeats them): in the e2e repositories, "you are explicilty allowed to create, force push to, make tags, releases or change settings including gh secrets, same holds for other e2e repos". A feature branch and one pull request per repository was implied by the federated shape and not stated in those words. Not granted: merging, npm or PyPI releases, any `publishers` entry in `ocx-sh/index`.

## Threads

- **Dependencies** (raised by the owner: how are they covered, should the catalog show them, should a badge depend on them). Facts read: a package's metadata pins each dependency by digest, and every dependency is pulled through the same verification call as a root (`crates/ocx_package_manager/src/tasks/pull.rs:252`); a digest-pinned request is checked only against a `publishers` list already committed locally (item 4 of `.agents/loop-security-findings-release-signing.md`), so in a fresh home the dependency's own publisher is not checked, while its bytes are fixed by the verified parent's metadata. Settled in part (hyperlinks, own-state badge); the closure indicator is an open question below.

## Research

Committed copies under `.claude/artifacts/`, all dated 2026-11-13.

- `research_signing_dogfood_codebase.md` (sections 4 and 5). The index bot parses and re-serializes only the root keys it knows and byte-compares, so a root carrying `publishers` fails `validate` and a bot-written root drops the key; `classify_change` has no `publishers` term. `schema/root.schema.json` and `schema/c-index.schema.json` both set `additionalProperties: false`. The bot verifies no signature and never fetches referrers. It is released to PyPI and pinned exactly in `index/bot-tools/pyproject.toml`. The catalog reads index files only; its support matrix is built from each tag's image-index file and fetched per package as `data/catalog/support/<route key>.json`; cards render only deprecated and yanked stamps; `viewmodel/types.ts` defers any provenance flag by an ADR decision, which this work reverses.
- `research_signing_dogfood_theme.md`. `@ocx-sh/theme` 0.5.0 lives in `ocx-sh/website` (`packages/theme`), released to npm from a signed tag. It has shipped a `DependencyExplorer` component since v0.1.0 that reads a custom `dependencies.json` (not CycloneDX) and builds its rows in the browser; no consumer instantiates it. Pins: `ocx/website` 0.5.0 exactly, `ocx-catalog` `^0.5.0`; `index` depends on `@ocx-sh/catalog ^0.6.2`.
- `research_signing_dogfood_competitive.md`. Only aqua declares a signer per package in its registry entry. No package UI found shows both a verified and a failed state, and none shows security state across versions as a matrix; Artifact Hub's label is crawler-computed per version.
- `research_signing_dogfood_prior_art.md` (axes 5 and 6). PyPI's Security tab shows none, full, partial and changed provenance per release. npm trusted-publisher connections cannot be edited, only deleted and recreated, and bind to the immutable repository ID.
- `research_signing_dogfood_archaeology.md`. No signing or SBOM history in the bot, catalog, index or theme repositories.

## Related

- `.agents/discussions/release-signing-trust-default-2026-11-12.md`: the design this builds on (`publishers` vocabulary, the bot's ingest gate as "early feedback, not the security boundary"). Its client slice merged as [ocx-sh/ocx#696](https://github.com/ocx-sh/ocx/pull/696).
- `.claude/artifacts/adr_publisher_trust_distribution.md`, `.claude/artifacts/system_design_publisher_trust.md`.
- `.claude/artifacts/plan_issue_639_index_closure.md`: precedent for a plan federated over ocx, the index bot and the catalog.
- [ocx-sh/e2e-index](https://github.com/ocx-sh/e2e-index): serves `https://ocx-sh.github.io/e2e-index` as the logical registry `e2e.ocx.sh`, rendered by `@ocx-sh/catalog`; `TESTING.md` there is the hand-test guide.

## Open questions

- [NEEDS CLARIFICATION: Does the catalog show a closure indicator ("every dependency signed")?]
  Recommended: no — a badge that changes when someone else's package changes misstates whose state it is; each listed dependency shows its own badge.
- [NEEDS CLARIFICATION: What vocabulary does the bot's optional signature enforcement use?]
  Recommended: the client's three values (`off`, `if_declared`, `required`) as a deployment setting; `ocx.sh` starts at `if_declared`.
- [NEEDS CLARIFICATION: How does the catalog get SBOM content to render, given that it never contacts a registry?]
  Recommended: the bot stores a reduced component list (name, version, licence, package URL) per SBOM digest as a content-addressed index object, fetched by the page on demand as the support matrix is.
- [NEEDS CLARIFICATION: What does the shared SBOM component read?]
  Recommended: CycloneDX JSON, so every consumer passes a standard document; the custom `dependencies.json` shape of `DependencyExplorer` is retired.
- [NEEDS CLARIFICATION: In which order do the pull requests land, when a consumer's branch pins an unreleased producer?]
  Recommended: theme → catalog → index bot → index; each consumer's pin is swapped to the released version by a follow-up the owner triggers after releasing the producer.
- [NEEDS CLARIFICATION: Which ocx does the bot verify with, and where does it run?]
  Recommended: a pinned ocx from the project toolchain (`ocx.toml`), a dev-channel build until a release carries the publisher check; verification runs only in the bot's unprivileged jobs, never in the privileged governance job.
- [NEEDS CLARIFICATION: Which repository hosts the loop?]
  Recommended: `ocx-sh/ocx`, as the federated plan for issue 639 did; its own change is the website's dependencies page.
- Check on resume: what loop A changed in the release SBOM (format, one per platform) decides what the website page and the shared component receive.

## Verification

Each check is shown red on the failing case and green on the passing one. Everything that needs a real certificate or a served index runs in `ocx-sh/e2e-index` against the cases `ocx-sh/e2e-package` publishes, dispatched, never per pull request.

- **Index bot**: a root with `publishers` passes validation and survives a bot write byte for byte; a `publishers` change is routed to human review; a change that would strand a published tag is refused; with enforcement on, an unsigned new tag is refused, with it off it is admitted and recorded as unsigned; the security file is unchanged by a reconcile run that finds no new referrer and updated by one that finds a backfilled signature.
- **Catalog and website**: the rendered `e2e-index` site shows verified, mismatch, none and unchecked signature states and the SBOM states on cards and in the security matrix, lists dependencies with working links across two index sources, and the wire files stay byte-identical to the source. The website's dependencies page renders with the shared component.
- **Non-breaking**: the previous ocx release still installs from an index whose roots carry `publishers`; an index deployment that sets nothing new behaves as before.
- **Loop phases**: as under Decisions › Loop shape.
