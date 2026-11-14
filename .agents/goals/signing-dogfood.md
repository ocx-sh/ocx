# Goal: "Signing dogfood, loop A — ocx and ocx-mirror sign their own releases"

Source: .agents/discussions/signing-dogfood.md · Written: 2026-11-13 by /hex-loop

## Definition of done

- [x] "GHCR dev channel" — evidence: dev run https://github.com/ocx-sh/ocx/actions/runs/43546383727 on `ee196e35e`: build, stamp, SBOM, sign and publish green; Docker images, Announce, GitHub release and ocx-sdkgen skipped; the job logs name `ghcr.io/ocx-sh/dev/cli` as the only push target, tag `0.7.0-dev_20261114022746`; production `ghcr.io/ocx-sh/ocx/cli` version list equal before and after (264), git tags and GitHub releases equal (36 / 36) (`.agents/m11-verify/report.md`); `deploy-dev.yml` carries no `dev.ocx.sh` (`git grep` exit 1); a repository selects the build by pin and lock: `ocx-sh/e2e-index` `eec0bf1d6` (`ocx.toml`, `ocx.lock` with six platform digests) and its run https://github.com/ocx-sh/e2e-index/actions/runs/43549464601 (`ocx exec -- ocx version --verbose`, success). Found by that run and fixed in `db8b08b61`: the dev prune job was skipped behind the skipped readiness gate; its first live run is the dev run on the final tip. Scope: the release workflow's dev mode runs build, stamp, SBOM, sign and publish to `ghcr.io/ocx-sh/dev/cli` and nowhere else (unannounced, no git tag, no GitHub release); `deploy-dev.yml` no longer points at `dev.ocx.sh`; a repository selects such a build through an `ocx.toml` pin and `ocx.lock`.
- [x] "Local publish task" — evidence: merge `d10ee5871` (P-PKG; `72dbfd892` adds `task release:local:publish`, `scripts/local_publish.py`); real run on `1e1cc7f11`: `task release:local:registry` exit 0, `task release:local:publish` exit 0 → `localhost:15000/ocx-sh/dev/cli:0.6.6-dev_20261113232933`; a second empty project with its own `OCX_HOME`: `ocx lock` exit 79 without the redirect, exit 0 with `OCX_MIRRORS='{"ghcr.io":"http://localhost:15000"}'`, and `ocx exec -- ocx version --verbose` prints commit `1e1cc7f1 (clean)`, built `2026-11-13T23:29:43Z`; the optional GHCR push is covered by `scripts/tests/test_local_publish.py` against a stub registry and was not run live (D-6: the workflow makes the dev package's first push). Scope: one local task builds a stamped host binary and publishes it as a package to a local registry, other checkouts on the machine resolve it through an `OCX_MIRRORS` redirect, and it can optionally push to the GHCR dev namespace.
- [x] "Bazel release replaces cargo-dist" — evidence: `release.yml` by hand-dispatch only, release and candidate refused off `refs/heads/main` (`release.yml:145`); dev run https://github.com/ocx-sh/ocx/actions/runs/43546383727 on `ee196e35e`: verify-version, plan, 8 stamped Bazel cross-builds, Release assets and the provenance scan green (the first attempt https://github.com/ocx-sh/ocx/actions/runs/43541745925 failed closed at the scan on a macOS test seam, fixed by `55b17316c`); the published binary reports version `0.7.0-dev+20261114022746`, commit `ee196e35eec05153954a5a1c7dba83d6c59b0621` (clean), branch `hex/signing-dogfood`, build `2026-11-14T02:28:31Z` and its run URL (`.agents/m11-verify/report.md`); asset names, checksums and `dist-manifest.json` held against the v0.6.6 listing by `scripts/tests/test_release_assets.py` (8 tests, `task scripts:self-test` exit 0). A production dispatch from `main` was not run (not granted). Scope: a release workflow dispatched by hand from `refs/heads/main`, binaries from the hermetic Bazel cross builds (8 triples), a released binary reports its real commit, branch, timestamp and version; asset names, checksums, `dist-manifest.json` and what the installers download by name stay as they are.
- [x] "Signed SBOM, signed manifest per platform" — evidence: dev run https://github.com/ocx-sh/ocx/actions/runs/43546383727: `ghcr.io/ocx-sh/dev/cli:0.7.0-dev_20261114022746`, six platforms (linux, darwin, windows × amd64, arm64); for each, `ocx package verify` of the signature and of the CycloneDX SBOM attestation exits 0 under `…/ocx/.github/workflows/release.yml@refs/heads/hex/signing-dogfood` and 77 under `…@refs/heads/main` (24 runs, `.agents/m11-verify/report.md`); sign-before-tag order: commit `3deaa0eec` (a failed `package push --sign` or `--sbom` writes no tag) and the floating-tag step as the last registry write of `publish` (`88bd23af1`). Scope: every platform's package carries its own CycloneDX SBOM as a signed attestation and a signed platform manifest, made in the run that pushed it; no tag is moved or announced before its signature exists.
- [x] "Backfill, both proven in e2e" — evidence: `backfill.yml` behind the `release-backfill` reviewer environment (`9a2505fbe`, `553ed394a`, `1c7b0098f`, `3cda319da`); in `ocx-sh/e2e-package` with GitHub-issued certificates: release https://github.com/ocx-sh/e2e-package/actions/runs/43532031981 (0.1.2, two platforms signed and attested), backfill https://github.com/ocx-sh/e2e-package/actions/runs/43504353050 (waited on the reviewer, then `v0.0.1 | linux/amd64 | signed | attested`), second backfill https://github.com/ocx-sh/e2e-package/actions/runs/43521493957 (`skipped | skipped`, referrer list unchanged at 2); `ocx package verify` of 0.0.1 exits 0 under the backfill identity and 77 under the release identity; https://github.com/ocx-sh/e2e-index/actions/runs/43533768626 (client-e2e on `161862927`, 25 passed). The ocx and ocx-mirror `release-backfill` environments do not exist yet (owner action); no production backfill was dispatched. Scope: a second dispatched workflow behind a reviewer environment backfills signatures and SBOMs onto existing tags per platform and skips what its own identity already signed; the release and backfill workflows both run in `ocx-sh/e2e-package` on real tags with real GitHub-issued certificates.
- [x] "Client carry-overs" — evidence: merges `3044d35a9` (P-HOST: `eff1818eb`, red exit 79 → green, `test_verify_tag_without_platform_verifies_the_host_platform_manifest`, `test_sbom_tag_without_platform_reads_the_host_platform_manifest`) and `2a1aade83` (P-NOVERIFY: `2bff20b42`, `test_no_verify_is_refused_under_a_system_locked_required`, red by disabling the guard); whole acceptance suite on `f4a515127`: exit 0, 4097 passed. Scope: `ocx package sbom` and `ocx package verify` select the host platform as `install` does; `--no-verify` and `OCX_NO_VERIFY` are refused under a system-locked `signatures = "required"` and no longer fail on an unreadable committed index root.
- [x] "prune on GHCR via --forge" — evidence: merge `0d3fc243a` (P-PRUNE; `c9770ddae`) with `1f50e3c39` (forge inferred for `ghcr.io`), `082b8f4f4`, `6690e755c`, `4d3c8d358`; live on `ghcr.io/ocx-sh/e2e-package`: https://github.com/ocx-sh/e2e-package/actions/runs/43510835871 attempt 1 (`--forge github`, exit 81, a shared version refused, nothing deleted) and https://github.com/ocx-sh/e2e-package/actions/runs/43532031981 (ocx built from `161862927`: four candidate tags deleted through the packages API, stable and unrelated tags kept); `--dry-run` on GHCR exit 0 with the version listing unchanged; `test/tests/test_package_prune.py` 39 passed (host inference, `--forge gitlab` refused, index-named host refused). Scope: `ocx package prune` deletes on GHCR through the GitHub packages API, selected by `--forge` or inferred from the registry host.
- [x] "ocx-mirror release cycle ported" — evidence: https://github.com/ocx-sh/ocx-mirror/pull/113 (branch `hex/signing-dogfood`): `release.yml` with release, candidate and dev modes, release and candidate refused off `main`, `backfill.yml`, per-platform signature and SBOM attestation; dev run https://github.com/ocx-sh/ocx-mirror/actions/runs/43549578414 on `84095c862` published `ghcr.io/ocx-sh/dev/mirror:0.7.0-dev_20261114030332` and nowhere else, six platforms, `ocx package verify` of signature and SBOM exits 0 under `…/ocx-mirror/.github/workflows/release.yml@refs/heads/hex/signing-dogfood` and 77 under `…@refs/heads/main` (24 runs), the dev prune job ran and deleted nothing, production `ghcr.io/ocx-sh/ocx/mirror` version list equal before and after (213), tags and releases equal (14 / 13) (`.agents/m16-verify/report.md`); the run published with the ocx this loop built: `packaging/release-tools/ocx.toml` pins `ghcr.io/ocx-sh/dev/cli:0.7.0-dev_20261114022746` (`760ad3705`, lock digest `sha256:32d36f39…`), verified under the ocx branch identity before use; the swap to the released ocx is https://github.com/ocx-sh/ocx/issues/718. Release, candidate and backfill modes were not dispatched in ocx-mirror (not granted); their shape is the one proven in `ocx-sh/e2e-package`. Scope: once the ocx side is green, the same workflow shape in `ocx-sh/ocx-mirror`: dispatched from `main`, release, candidate and dev modes, per-platform SBOM attestation and signature, backfill, dev builds to `ghcr.io/ocx-sh/dev/mirror`; no mirror feature work (SBOMs for mirrored packages, registry sync and re-signing stay in loop C); its release workflow uses the ocx this loop builds through an `ocx.toml` pin, and the swap to the released version is a recorded follow-up.
- [x] "Release candidates" — evidence: `4edffe389` (candidate mode of `release.yml`), spelling `X.Y.Z-rcN` (D-21); full cycle in the fixtures: candidate https://github.com/ocx-sh/e2e-package/actions/runs/43531108304 (0.1.2-rc2: two `ephemeral` rows, GitHub release `isPrerelease=true`, `latest`/`0`/`0.1` unchanged), release https://github.com/ocx-sh/e2e-package/actions/runs/43532031981 (0.1.2: its candidates pruned and their rows removed, `prune_after_days=0`), standing candidate https://github.com/ocx-sh/e2e-package/actions/runs/43532983178 (`releases/latest` stays `v0.1.2`); 30-day boundary `test_a_final_release_is_old_enough_at_exactly_prune_after_days` (`7b1862525`); pre-release flag `test_a_candidate_release_is_a_prerelease_and_a_release_is_not` (`4316c04a4`); install plus select to a candidate and back `test/tests/test_self_candidate_switch.py` (`ade1676e1`); the installer takes the first `channel == stable` row (`www-setup` `get_latest_version`), docs `51944758d`. No candidate was cut on the production package. Scope: version `X.Y.Z-rc.N` as a dispatch input from `main`, announced `ephemeral`, GitHub release marked pre-release, no floating tag moved, never what the installer picks, pruned 30 days after the final release, proven as a full cycle in the fixtures only; `ocx install` plus `ocx select` on the ocx package switch a terminal to a candidate and back; the install script stays stable-only.
- [x] "Registry docs measured" — evidence: `a3ee603e6`, `2103739a7`, `570ad52ca` in `website/src/content/docs/publishing/snapshot-tracks.md`: GHCR row measured (https://github.com/ocx-sh/e2e-package/actions/runs/43510835871 attempt 1 refusal exit 81; https://github.com/ocx-sh/e2e-package/actions/runs/43532031981 clean delete), gitlab.com row measured (`ocx package copy` to `registry.gitlab.com`: 400 `MANIFEST_INVALID`, exit 69, no tag), the GitLab CI recipe pushes to GHCR and links https://github.com/ocx-sh/ocx/issues/480; `--forge gitlab` refusal `test_forge_gitlab_is_refused_with_a_usage_error`; `task ci:docs` exit 0 on `161862927`. Scope: the registry table of `website/src/content/docs/publishing/snapshot-tracks.md` gains measured GHCR and gitlab.com rows, replacing the sentence that puts GHCR at exit 82 from documentation alone; the GitLab CI recipe keeps GitLab CI and a GitLab-hosted index, pushes packages to a registry that takes them, and the page says in one line that GitLab's registry is not a package host, linking https://github.com/ocx-sh/ocx/issues/480; `--forge gitlab` stays refused on `ocx package prune` and no GitLab fixture exists.
- [ ] PR merge-ready (DONE block only) — evidence: the PR URL, and the
  PR is not a draft.
- [ ] CI green per job (DONE block only) — evidence: every check run on
  the PR head SHA with its conclusion; every skipped job states its
  skip reason (its `if:` or path filter).
  A skip with no reason is not green.
- [ ] Deep verify passed (DONE block only) — evidence: the full documented verification result.

## Autonomy

- Prompting: never — no question waits for a human; a doubt runs
  § Issue resolution.
- Granted acts (authority: the pasted prompt): in ocx-sh/e2e-package and ocx-sh/e2e-index: "you are explicilty allowed to create, force push to, make tags, releases or change settings including gh secrets, same holds for other e2e repos"; one branch and one PR in ocx-sh/ocx-mirror: "pr for ocx-mirror is granted"; dispatch signed dev-channel runs of the release workflow in ocx-sh/ocx and ocx-sh/ocx-mirror, and push locally built pre-releases, publishing only to ghcr.io/ocx-sh/dev/*; never a production release, tag or backfill dispatch
- Forbidden or narrowed acts:
  - Never merge.
  - Never cut a production release, tag or backfill dispatch.
  - Never write to ocx.sh, ghcr.io/ocx-sh/ocx/cli or ghcr.io/ocx-sh/ocx/mirror.
  - Never add publishers to ocx-sh/index.

## Issue resolution

Every doubt — an ambiguous requirement, a design question, a failure
with an unclear cause — runs this protocol:

1. Delegate the research to a sub-orchestrator.
2. Record question → research → decision in this file or in the PR.
3. Defer to a GitHub issue only in hard cases — the research ends with no
   decision, or the decision needs an act outside the grants; the issue
   link is the recorded decision.
4. Every doubt not deferred ends in an action.
5. A pre-existing failure that blocks done is in scope and runs this
   protocol.
6. Findings outside this goal's scope become follow-up issues.
7. Secrets and credential-bearing logs are never written to any committed
   file, commit message, PR text, PR comment or review comment, or issue.
   Security findings are never filed as issues:
   this file records them by reference only — location and class, no
   secret value or exploit detail — and only the DONE block reports them
   in full.

## Loop shape

- Entry point: Run the /hex-plan skill on "Signing dogfood, loop A — ocx and ocx-mirror sign their own releases, per .agents/discussions/signing-dogfood.md".
- Refinement rounds: 12 — counts outer cycles: each review ⇄ execute
  pass is one, and so is every failed repair or retry cycle — a red
  gate's fix pass, an execute or finalize retry, a post-finalize CI fix ⇄
  re-finalize pass. Inner review-fix rounds do not count, and their limit
  is untouched.
  Past `12`, the DONE block reports every remaining criterion `not met`
  and the run stops.
- Inner loop: integrate until the end-to-end cases work together (fixes found there are not review rounds), then three to five Sonnet subagents act as end users in isolated environments and try to break it, each bug gets a regression test and a fix, then at most 4 coarse hardening review rounds, and a round that finds only smaller or unrelated issues ends the reviewing.
- Ticks: /hex-loop commits nothing. The session creates or switches to
  the branch the pasted prompt's I9 names and commits this file first on
  it. A box is ticked only between hex-mode runs, never during one, and
  each tick is committed at once; every tick lands before the final
  /hex-finalize, none after it. The `(DONE block only)` criteria are
  evidenced only in the closing DONE block, never ticked.

## Rules

- Run rules:
  - Secrets and tokens are read from the environment and never written to a file, a log or a commit.

## Emphasis

From the source, as pointers; the source's own text governs.

- Order: the dev channel is built first, the ocx release and backfill next, the `ocx-sh/ocx-mirror` port only once the ocx side is green. Existing tags are backfilled before any `publishers` entry reaches a production index, and that entry is not this run's act.
- Non-breaking as the source's `## Intent` defines it: no existing install, config file, published package, index entry, release asset name or install path stops working. The previous ocx release still installs every fixture case it installed before.
- The source's `## Intent` lists what is out of scope: loops B and C (`.agents/discussions/signing-dogfood-index-catalog.md`, `.agents/discussions/signing-dogfood-mirror.md`), index signing, vulnerability scanning, and every production release act.
- A candidate is a production write from `main`, so this run builds the candidate mode and proves it in the fixtures; it never cuts one on the production package.
- The source's `## Open questions` are this run's docket, each with a recommended answer (Bazel stamping, the SBOM generator, the `sdkgen-publish.yml` sign job, pruning dev builds, the packages-API owner type, who creates the dev package, shared or copied workflows, the `publishers` identities). A dev package that starts private is reported as an owner action, not worked around.
- The source's `## Threads` are closed positions; names such as `ghcr.io/ocx-sh/dev/cli` are working spellings open to a better shape with a recorded reason.
- The source's `## Verification` names the acceptance cases; each is shown red on the failing case and green on the passing one. Real-certificate and real-registry cases run in the fixture repositories or on the dev channel, dispatched, never per pull request.
- A new flag on `ocx package prune` is an interface change (`cli.json`, reference pages, exit-code table).

## Context

- Source discussion: `.agents/discussions/signing-dogfood.md` (untracked; read it from the main checkout).
- Parked sibling discussions: `.agents/discussions/signing-dogfood-index-catalog.md` (loop B), `.agents/discussions/signing-dogfood-mirror.md` (loop C).
- Design this builds on: `.agents/discussions/release-signing-trust-default-2026-11-12.md`, `.agents/goals/release-signing-trust-default-2026-11-12.md`, `.agents/loop-security-findings-release-signing.md`.
- Research: `.claude/artifacts/research_signing_dogfood_dev_channel.md`, `.claude/artifacts/research_signing_dogfood_codebase.md`, `.claude/artifacts/research_signing_dogfood_archaeology.md`, `.claude/artifacts/research_signing_dogfood_prior_art.md`.
- Rules and decisions: `.claude/artifacts/adr_bazel_build_adoption.md`, `.claude/rules/workflow-release.md`, `.claude/rules/subsystem-ci.md`.
- Satellite checkout: `/home/mherwig/dev/ocx-mirror` (https://github.com/ocx-sh/ocx-mirror).
- Fixture repositories: https://github.com/ocx-sh/e2e-package and https://github.com/ocx-sh/e2e-index (worktrees `.agents/worktrees/e2e-package`, `.agents/worktrees/e2e-index`).
- Related issues: https://github.com/ocx-sh/ocx/issues/694, https://github.com/ocx-sh/ocx/issues/402, https://github.com/ocx-sh/ocx/issues/403, https://github.com/ocx-sh/ocx/issues/480.
