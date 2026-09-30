# Research: Multi-Stage Artifact Promotion Standards

<!-- Technology Landscape Research. Handoff to: adr_snapshot_lifecycle.md. Artifacts decay — check dates before trusting findings. -->

## Metadata

**Date:** 2026-09-29
**Domain:** release-engineering
**Triggered by:** adr snapshot lifecycle
**Expires:** 2027-03-29

## Direct Answer

Living practice converges on **"build once, promote many"**: an artifact is built and
digest-addressed exactly once, then a *reference* to that same digest is advanced
through stage gates (retag in place, or copied to a stage-scoped location) —
never rebuilt per stage. Retention systems that respect this model protect any
digest a stage tag or a promotion record currently points at, and let
per-stage generations (dev builds, canary rolls) expire independently and much
faster than the stage tag itself. For OCX: model "stage" as a channel/tag
convention on top of the existing snapshot rolling-tag design, not a new
first-class resource; make `prune` digest-aware so any digest still carried by
a live channel tag (or referenced from the index) is structurally
unprunable, never merely policy-excluded.

## Technology Landscape

### Established (proven, widely accepted)

| Tool/Pattern | Status | Notes |
|---|---|---|
| Build once, promote by retag/copy | Dominant | The default advice across CI vendors: rebuilding per environment breaks the "tested = deployed" guarantee ([Steve Smith, "Pipeline antipattern: Artifact Promotion"](https://www.stevesmith.tech/blog/pipeline-antipattern-artifact-promotion/); [CircleCI changelog](https://circleci.com/changelog/artifact-promotion/)) |
| JFrog Artifactory build promotion | Established | Copies/moves artifacts between repos using build properties from the CI run; only promotes builds that already exist, never rebuilds ([Artifactory cleanup/retention docs](https://docs.jfrog.com/administration/docs/cleanup-policies)) |
| Nexus staging repos + promote/drop | Established | Staging repositories hold a candidate; "promote" moves it into a release repo, "drop" discards it — same build-once model, Maven-centric |
| Harbor replication + retention | Established | Replication copies images between Harbor instances/projects (push- or pull-based); retention is local per instance and does **not** cascade — a tag deleted by retention on one site is not deleted on a replicated site ([Harbor tag retention docs](https://goharbor.io/docs/1.10/working-with-projects/working-with-images/create-tag-retention-rules/), [Harbor issue #16746](https://github.com/goharbor/harbor/issues/16746)) |
| ECR cross-account/cross-region copy | Established | No native "promotion" primitive; teams script `crane`/`skopeo`/`oras` copies or use replication rules between repos/accounts |
| crane / regctl / oras cp | Established, dominant for raw OCI | Daemonless registry-to-registry copy without pull+rebuild; `crane cp`, `regctl image copy`, `oras cp` cover the "move a digest, don't touch the bytes" step in most OCI pipelines ([regctl vs crane vs skopeo comparison](https://alexandre-vazquez.com/skopeo-crane-regctl-container-image-tools/), [oras cp docs](https://oras.land/docs/commands/oras_cp/)) |
| kubernetes-sigs/promo-tools | Established, most OCX-relevant precedent | k8s.io's own image promotion pipeline: builds once to a staging registry, promotes by manifest-list copy to the canonical registry, and (since 2023) attaches a signed SLSA Verification Summary Attestation per promoted digest as an OCI referrer ([promo-tools #1954](https://github.com/kubernetes-sigs/promo-tools/issues/1954), [#1997](https://github.com/kubernetes-sigs/promo-tools/pull/1997)) |

**Copy-between-repos vs retag-in-place.** Retag-in-place (one repo, tags
`:dev`/`:staging`/`:prod` or channel names) is simpler and is what package
managers with a single index (rustup, Homebrew, npm dist-tags) use.
Copy-between-repos (Artifactory staging repos, Nexus, Harbor replication, ECR
cross-account) is preferred when stages need separate access control or
compliance boundaries. For OCI specifically, copy is cheap (digest-addressed
blobs dedupe, only the manifest and tag move), so the choice tracks
namespace/ACL boundaries, not storage cost. Dominant pattern for OCI overall:
retag within one registry for same-trust-boundary stages, cross-registry copy
at trust boundaries.

### Design Patterns Worth Considering

- **Digest-first, tag-second promotion.** Every promotion tool in this survey
  (Artifactory, Kargo, promo-tools) promotes a *digest*, and a stage tag is
  just a movable pointer to that digest. This is the same shape as
  `0.5.0-canary_<build>` → `0.5.0-canary` in the ADR: the digest is immutable,
  the rolling tag moves. [k8s promo-tools](https://github.com/kubernetes-sigs/promo-tools)
- **Freight as an immutable promotion unit** (Kargo) — bundles a digest +
  provenance into one object that either is or isn't in a stage, instead of
  tracking "current tag per stage" as loose state.
  [Kargo concepts](https://release-1-0.docs.kargo.io/concepts/)
- **Retention exemption by "still referenced," not by age.** Every mature
  system (Artifactory "keep" flag, ECR high-countNumber prefix rule, Harbor
  retain-matching-pattern) special-cases the artifact a later stage currently
  points at, so the general N-generations rule never deletes something live.
  [ECR lifecycle parameters](https://docs.aws.amazon.com/AmazonECR/latest/userguide/lifecycle_policy_parameters.html)

## Key Findings

1. **"Build once, promote many" is the near-universal norm**, not a debated
   choice — rebuilding per stage is explicitly called an anti-pattern because
   it breaks the "what was tested is what ships" guarantee.
   [Pipeline antipattern: Artifact Promotion](https://www.stevesmith.tech/blog/pipeline-antipattern-artifact-promotion/)
2. **Orchestrators model a stage as a gated pointer to a digest, not a build.**
   Kargo: Warehouse (watches upstream) → Freight (immutable bundle of digests)
   → Stage (gated promotion target with `promotionTemplate`).
   [Kargo Explained](https://burrell.tech/blog/kargo/), [Kargo docs](https://release-1-0.docs.kargo.io/concepts/)
   Google Cloud Deploy: a `deliveryPipeline` defines an ordered `targets`
   sequence; each target can set `requireApproval`, and promotion walks the
   sequence with Pub/Sub-driven approval gates.
   [Cloud Deploy: promote and approvals](https://docs.cloud.google.com/deploy/docs/promote-release)
   Octopus Deploy: a `Lifecycle` is 1..N `Phases`, each phase 0..N
   environments; the lifecycle also carries **per-environment retention**.
   [Octopus lifecycles](https://octopus.com/docs/best-practices/deployments/lifecycles-and-environments)
   GitHub environments: no ordered pipeline primitive, but `deployment_tier`-like
   semantics via named environments plus required-reviewer/wait-timer
   protection rules per environment.
   [GitHub environment protection rules](https://docs.github.com/en/actions/concepts/workflows-and-actions/deployment-environments)
   GitLab: environments declare a `deployment_tier` (`production | staging |
   testing | development | other`), inferred from name or set explicitly —
   this is the closest thing to a standard *vocabulary* for stage identity.
   [GitLab environments docs](https://docs.gitlab.com/ci/environments/)
3. **Retention never deletes what a later stage still references** — the
   load-bearing invariant across every tool surveyed:
   - Artifactory: cleanup policies do **not** check whether a package is part
     of a promoted build or release bundle by default — a documented gap, not
     a solved problem. Release bundles instead get their own explicit "keep"
     flag, ignored only if the retention policy itself is disabled.
     [Artifactory cleanup policies](https://docs.jfrog.com/administration/docs/cleanup-policies), [Release bundle retention](https://docs.jfrog.com/artifactory/docs/manage-release-bundle-retention-and-cleanup)
   - ECR: `tagStatus: tagged` + `tagPrefixList` + `countType:
     imageCountMoreThan` with a high `countNumber` is the standard idiom to
     make a prefix (e.g. `prod`) effectively unprunable, since rules apply
     only within the matched prefix set.
     [ECR lifecycle parameters](https://docs.aws.amazon.com/AmazonECR/latest/userguide/lifecycle_policy_parameters.html), [makandra: protect production tag](https://makandracards.com/operations/612166-protect-container-images-production-tag-ecr-lifecycle)
   - Harbor: retention rules are pattern/count/age based and evaluated *per
     registry instance* — a replicated copy in another project/instance is
     invisible to the source rule and needs its own policy. Nothing
     cross-references "is this tag also a promoted stage pointer elsewhere."
     [Harbor retention rules](https://goharbor.io/docs/1.10/working-with-projects/working-with-images/create-tag-retention-rules/)
   - Octopus: retention is a first-class field *of the lifecycle phase*, so
     "dev keeps 10, prod keeps forever" is native configuration, not bolted on.
     [Octopus lifecycles](https://octopus.com/docs/best-practices/deployments/lifecycles-and-environments)

   Net: no surveyed system automatically derives "protected" from "is this
   digest promoted" without an explicit rule or flag — Artifactory's own docs
   flag this as a real gap. A prune design assuming "the tool figures out
   what's protected" is optimistic; the correct pattern is a structural
   protection check (walk live tags/index refs), not a separate retention
   policy that can drift out of sync with what's actually promoted.
4. **Provenance of promotion is an emerging but real practice, gated by
   digest not by tag.** k8s promo-tools' `--verification-summaries` flag
   writes one signed SLSA VSA per promoted digest as an OCI referrer,
   attesting "this digest was verified/promoted from staging" — attached once
   per digest regardless of how many tags later point at it.
   [promo-tools #1997](https://github.com/kubernetes-sigs/promo-tools/pull/1997)
   SLSA's VSA spec frames this generally: a verifier attests an artifact met a
   policy/level so downstream consumers skip re-evaluating raw provenance.
   [SLSA VSA spec](https://slsa.dev/spec/v0.1/verification_summary)
   Implication for deletion: an attestation referrer is itself an OCI artifact
   attached to a digest — deleting the digest orphans the attestation unless
   the registry cascades referrer deletion, so prune must delete subject and
   referrers together and must never delete a digest a VSA still attests.
5. **Version-naming-as-channel is the norm in package-manager-shaped tools,
   and it is SemVer-legal.** SemVer 2.0's prerelease precedence
   (`1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-beta < 1.0.0-rc.1 < 1.0.0`) is exactly
   a stage ladder, and build metadata (`+build`) is explicitly precedence-inert
   — the right place for a build/commit id that must not affect ordering.
   [SemVer 2.0.0](https://semver.org/)
   Real channel systems map stage → identifier directly:
   - rustup: `stable` / `beta` / `nightly` as toolchain names, not
     SemVer prerelease strings — a separate channel axis outside the version.
     [rustup channels](https://rust-lang.github.io/rustup/concepts/channels.html)
   - Chrome: four channels (`canary` daily, `dev` weekly, `beta` ~monthly,
     `stable`), each a *separate release train*, not a prerelease suffix on
     one version. [Chrome release channels](https://developer.chrome.com/docs/web-platform/chrome-release-channels)
   - Deno: `deno-version: stable | lts | rc | canary`, where `canary` is
     "one build per commit on main" — structurally identical to OCX's
     per-build canary tag. [Deno release schedule](https://docs.deno.com/runtime/contributing/release_schedule/)
   - Node.js: moving to an explicit `alpha` channel using literal SemVer
     prerelease syntax (`27.0.0-alpha.1`) — the one surveyed tool that puts
     the channel *inside* the SemVer prerelease field, matching OCX's own
     `0.5.0-canary_<build>` shape. [Node.js release schedule announcement](https://nodejs.org/en/blog/announcements/evolving-the-nodejs-release-schedule)
   - Homebrew: no native prerelease channel concept; prerelease/nightly builds
     go through a separate tap (e.g. `homebrew-prerelease`), not a version
     suffix. [Homebrew prerelease tap example](https://github.com/jrnl-org/homebrew-prerelease)

## Recommendation

Keep this general — it applies to any snapshot/prune design, not just OCX:

- **Stage as a naming/tag convention, not a first-class resource.** No
  surveyed tool that OCX's shape resembles (rustup, Deno, Node's new alpha
  channel) needed a dedicated "Stage" object; that machinery (Kargo Stages,
  Cloud Deploy targets, Octopus phases) exists to solve *approval gating
  between humans/services*, which is out of scope for a package registry.
  OCX's SemVer prerelease identifier (`-canary`, per-PR channel) already *is*
  the stage vocabulary — don't duplicate it as config.
- **Per-channel retention (different N per channel) is necessary but not
  sufficient.** Every mature system does this (Octopus phase retention, ECR
  per-prefix count, Artifactory per-build-name policy) — replicate it. But
  every surveyed failure mode traces to treating "retained by policy" and
  "referenced by a promoted pointer" as two separate, driftable concepts
  (Artifactory's cleanup-vs-promotion gap; Harbor's non-cascading retention).
  Don't repeat that: derive protection structurally.
- **`prune` must treat any digest still carried by a live rolling/stage tag,
  or referenced by the index, as unprunable by construction** — not as a
  policy exemption a config file can omit. Walk the current tag set (and any
  promotion/verification attestation referrers on a digest) before computing
  what "keep N" would delete, the same way k8s promo-tools treats a VSA'd
  digest as load-bearing. A digest with zero remaining tag/index references
  is the only thing safe to delete — this is stronger and simpler than
  emulating Artifactory's "keep flag can be globally overridden" design.
- **Promotion itself (retag vs. copy) should stay out of `ocx package`'s verb
  surface for now.** The ecosystem split (retag-in-place for same-registry
  channels, cross-registry copy at trust boundaries) tracks a distinction OCX
  doesn't have yet — one registry, one trust boundary, one index. `ocx
  package copy` as a raw digest-copy primitive (crane/regctl/oras shaped) is
  reasonable scope; an opinionated "promote" workflow with approval gates is
  Kargo/Cloud Deploy territory — that belongs to the package's own CI, not
  the package manager.

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [Pipeline antipattern: Artifact Promotion — Steve Smith](https://www.stevesmith.tech/blog/pipeline-antipattern-artifact-promotion/) | Blog | — | Build-once-promote-many rationale |
| [JFrog Artifactory: Cleanup Policies](https://docs.jfrog.com/administration/docs/cleanup-policies) | Docs | current | Cleanup-vs-promotion gap |
| [JFrog: Manage Release Bundle Retention and Cleanup](https://docs.jfrog.com/artifactory/docs/manage-release-bundle-retention-and-cleanup) | Docs | current | "Keep" flag semantics |
| [Harbor: Create Tag Retention Rules](https://goharbor.io/docs/1.10/working-with-projects/working-with-images/create-tag-retention-rules/) | Docs | 1.10 | Retention rule model, OR logic |
| [Harbor issue #16746](https://github.com/goharbor/harbor/issues/16746) | Issue | — | Retention doesn't cascade across replication |
| [AWS ECR: Lifecycle policy parameters](https://docs.aws.amazon.com/AmazonECR/latest/userguide/lifecycle_policy_parameters.html) | Docs | current | `tagStatus`/`tagPrefixList`/`countType` |
| [makandra: Protect production tag from ECR lifecycle](https://makandracards.com/operations/612166-protect-container-images-production-tag-ecr-lifecycle) | Blog | — | High-countNumber protect idiom |
| [Kargo: Key Concepts](https://release-1-0.docs.kargo.io/concepts/) | Docs | v1.0 | Freight/Stage/Warehouse model |
| [Kargo Explained — Burrell Technology](https://burrell.tech/blog/kargo/) | Blog | — | Warehouse→Freight→Stage flow |
| [Google Cloud Deploy: Promote and approvals](https://docs.cloud.google.com/deploy/docs/promote-release) | Docs | current | `requireApproval`, promotion sequence |
| [GitLab CI: Environments](https://docs.gitlab.com/ci/environments/) | Docs | current | `deployment_tier` vocabulary |
| [GitHub Docs: Deployment environments](https://docs.github.com/en/actions/concepts/workflows-and-actions/deployment-environments) | Docs | current | Environment protection rules |
| [Octopus Deploy: Lifecycles and Environments](https://octopus.com/docs/best-practices/deployments/lifecycles-and-environments) | Docs | current | Phases, per-environment retention |
| [kubernetes-sigs/promo-tools issue #1954](https://github.com/kubernetes-sigs/promo-tools/issues/1954) | Issue | 2023 | VSA-per-promoted-digest design |
| [kubernetes-sigs/promo-tools PR #1997](https://github.com/kubernetes-sigs/promo-tools/pull/1997) | PR | 2023 | Implementation of promotion VSA |
| [SLSA: Verification Summary Attestation spec](https://slsa.dev/spec/v0.1/verification_summary) | Spec | v0.1 | VSA definition and purpose |
| [Semantic Versioning 2.0.0](https://semver.org/) | Spec | 2.0.0 | Prerelease precedence, build metadata |
| [rustup book: Channels](https://rust-lang.github.io/rustup/concepts/channels.html) | Docs | current | stable/beta/nightly channel model |
| [Chrome for Developers: Release channels](https://developer.chrome.com/docs/web-platform/chrome-release-channels) | Docs | current | canary/dev/beta/stable cadence |
| [Deno Docs: Release Schedule](https://docs.deno.com/runtime/contributing/release_schedule/) | Docs | current | stable/lts/rc/canary channels |
| [Node.js: Evolving the Node.js Release Schedule](https://nodejs.org/en/blog/announcements/evolving-the-nodejs-release-schedule) | Blog | 2026 | New alpha channel, SemVer prerelease syntax |
| [ORAS: `oras cp`](https://oras.land/docs/commands/oras_cp/) | Docs | current | Registry-to-registry copy primitive |
| [regctl vs crane vs skopeo comparison](https://alexandre-vazquez.com/skopeo-crane-regctl-container-image-tools/) | Blog | 2026 | Copy tool landscape |
