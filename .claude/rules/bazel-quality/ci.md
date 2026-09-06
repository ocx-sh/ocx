---
title: CI and Target Selection
summary: The BZL-CI family — the job table, gate exit codes, matrix legs, release lockstep, the two selection tools' miss classes, and the build event stream
---

# CI and Target Selection

Owns `BZL-CI`: the lanes of a Bazel pipeline. Which jobs exist, which block a
merge, which must refuse a warm cache, how a command's exit code reaches the job
result, what a release verifies in lockstep, when target selection is worth its
documented false negatives, and what a Build Event Service stream may be used
for. It does not own the gate commands themselves — the loading-phase step is
`BZL-LARK-30` and the lint runner's own exit surface is `BZL-LARK-01`. Cache
flags, eviction and Build-without-the-Bytes are `BZL-CACHE`, and the BES
credential and TLS surface is `BZL-CACHE-33`; environment tiers, sandboxing and
generated-file freshness are `BZL-HERM`; lockfile freshness is `BZL-MOD-02`;
test `size`, `tags` and sharding are `BZL-TEST`; the ordered 8→9 flag-flip
checklist is `BZL-FLAG`. No row here restates a mechanism another family owns.

Contents: [The Gate Can Go Red](#the-gate-can-go-red) ·
[The Job Table](#the-job-table) ·
[Secrets, Pins and Dead Snippets](#secrets-pins-and-dead-snippets) ·
[Release Lockstep](#release-lockstep) ·
[Before Any Target Selection](#before-any-target-selection) ·
[Selection Tool Miss Classes](#selection-tool-miss-classes) ·
[The Build Event Stream](#the-build-event-stream) · [Gaps](#gaps) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Every version-bound claim below was measured 2026-09-06 against real Bazel
8.7.0, 8.8.0 and 9.2.0 binaries on Linux (`linux-sandbox`, working unprivileged
user namespaces); a claim that could differ on darwin or Windows says so in its
own cell. The rows bind `bazel-diff` v46.1.0 (2026-08-28; Bazel ≥6.2.0 for
`--useCquery`, ≥8.6.0 / ≥9.0.1 for default-mode `MODULE.bazel` diffing),
`target-determinator` v0.34.0 (2026-06-19; Bazel ≥4.0.0), `aspect_rules_lint`
2.9.0, `buildifier_prebuilt` 8.5.1.4 and bazelisk as released on 2026-09-06. Before citing any
flag, re-read it at your pinned version on **both** help surfaces —
`bazel help <command> --long` **and** `bazel help startup_options` — because
some flags live only on the startup surface and a search of the command surface
alone reports them absent. Every `grep` below reads committed workspace text
only: BUILD and `.bzl` content generated at fetch time into an external
repository is out of reach of all of them. Severity maps onto the house tiers:
MUST = Block, SHOULD = Warn, CONSIDER = Suggest.

## The Gate Can Go Red

The check for this whole block: plant one violation the gate exists to catch,
run the job's own command, and read `$?`. Never read a mode string, an attribute
name, or a specific number — `BZL-LARK-01` owns why the number lies.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-07 | Let every gate job propagate its command's own exit code — no `\|\| true`, no stdout scrape, no `continue-on-error: true` except on a leg an adjacent comment declares a non-blocking canary — and assert nonzero, never a specific number. | The linter, the test runner and the guard script already exit nonzero; the CI wrapper is the only place that can turn a gate into a report, and a `continue-on-error` leg reports the whole workflow run as passed. The number is not the tool's own: `buildifier_prebuilt` through 8.5.1.3 runs `find … \| xargs buildifier` under `set -euo pipefail`, and xargs remaps 1-125 to **123**, so buildifier's exit 4 never reaches `bazel run`; 8.5.1.4 switched to `find -exec … +` and retired the remap, which is exactly why the assertion must not name a number. | `grep -rn -e '\|\| true' -e 'continue-on-error' --include=*.yml .github/workflows` — a union over both spellings; every hit must sit on a leg whose adjacent comment declares it a canary; empty output is the pass. Then plant a violation and run the job's command on 8.7.0 and on 9.2.0: exit 0 is the finding. CI text, so the grep is version-independent. | MUST |
| BZL-CI-11 | On a repo with real per-language targets (`cc_*`/`py_*`/`js_*`/`rust_*`/`java_*`), run lint through `aspect_rules_lint`'s aspect — `--aspects=//tools/lint:linters.bzl%<linter>` **plus** `--@aspect_rules_lint//lint:fail_on_violation`, or a `lint_test` target — never a per-language CLI as a bare shell step outside the graph. | A shell-invoked linter is invisible to `bazel query`, gets no caching, and re-runs in full every time; the aspect flag **alone** only writes report files under `bazel-out` and fails nothing, so a repo can carry the whole mechanism and gate on none of it. | `grep -rn '\-\-aspects=' .bazelrc* --include=*.yml .github/workflows` and `grep -rn -e 'fail_on_violation' -e 'lint_test' .bazelrc* BUILD*` — an `--aspects=` hit with neither companion is report-only and the finding. A bare-shell per-language linter with no `--aspects=` on a repo with such targets is the finding. Empty output from both, on a repo with no per-language targets, is not applicable. Needs `aspect_rules_lint` ≥2.9.0 (`bazel_compatibility = [">=7.6.0"]`), unchanged 8.7.0 → 9.2.0. | SHOULD |
| BZL-CI-12 | Measure whether the lint gate reaches a `.bazelignore`d tree, in both directions, and never infer the answer from `.bazelignore` alone; where it does reach, read the printed paths to learn which tree failed, never the exit code. | A filesystem-walking runner has no concept of `.bazelignore`: measured on 8.7.0 with `buildifier_prebuilt` 8.2.0.2, a planted violation in an ignored tree and one in an in-graph `.bzl` file failed with the same exit code. A graph-scoped `lint_test` over `//...` cannot see targets that left the package graph at all. Which one a repo has is a property of its runner, not of Bazel, and one exit code covers every tree. | Plant a formatting violation inside an ignored directory and run the lint gate on 8.7.0 and on 9.2.0: a green run is the finding (the gate does not reach it), a red run proves reach and attributes nothing. Ask the same question separately for any graph-scoped lint target. An empty or absent `.bazelignore` is not applicable. Measured on Linux; a Windows runner's file walk is unmeasured. | SHOULD |

## The Job Table

The check for this whole block: enumerate every job in the CI config, classify
each as {lint, test, registry-parity, cold-store, other}, then read each job's
`name:` against the assertions its body actually makes. The lockfile-freshness
leg that lives in this taxonomy is `BZL-MOD-02`'s gate, not one of these rows.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-05 | Ship at minimum a lint gate and a test matrix as required checks; add a registry-parity job where the repo publishes to a registry, and a cold-store job where it makes any offline, airgapped or hermetic claim. Only the lint and test jobs may touch the warm remote cache. | The two prove-it-without-help job types exist specifically not to benefit from a cache; giving one a cache produces a green run that has stopped proving anything, and nothing in the output says so. | Classify every job. A registry-parity or cold-store job carrying a cache step is the finding; no lint job or no test job at all is a more basic finding. A classification that matches with no cache on those two job types is the pass. Identical on 8.7.0 and 9.2.0. | MUST |
| BZL-CI-06 | Run a job whose purpose is proving something works without help with every remote-cache flag absent **and** an adjacent comment naming what it proves and why a cache hit would mask it; give every other cache-omitted job the cache or the comment; and never let a job's `name:` claim more than its body asserts. | A reader cannot tell a deliberate omission from a forgotten step, so an oversight degrades silently to "job runs, proves nothing new". A job named for determinism that compares only exit codes and output presence — not two independent action-key captures — would pass unchanged with a fully non-deterministic action, and trains every later reader to over-trust a green run. | (a) For every cache-eligible job with no cache step, is there an adjacent comment stating the reason? Missing on a refuses-help job = finding (MUST); on any other = finding (SHOULD). (b) Read each `name:` against the body: a name claiming more than the body asserts = finding. Comments present and names matching = pass. The capture such a job needs is `BZL-CACHE-16`'s execution-log diff; the determinism claim itself is `BZL-HERM-23`. | MUST / SHOULD (other cache-omitted jobs) |
| BZL-CI-08 | On a module with external consumers spanning more than one Bazel major, make both the pinned major and the Active LTS blocking legs and reserve advisory framing for a rolling or nightly channel. On a single-consumer internal repo, an advisory Active-LTS leg is acceptable only with a comment stating the choice and naming who or what consumes a red run. | Advisory-only testing of the Active LTS lets a break reach a consumer on that major before CI sees it; gating only the Active major drops the signal for consumers still on the Maintenance pin. A stated choice with no named consumer is worth about as much as no leg: GitHub's own spec reports a `continue-on-error: true` job's workflow run as **passed**, so branch protection has no distinct state to act on, and Bazel's own `bazelci.py` excludes `soft_fail` steps from `try_update_last_green_commit`, its one automated result-consumer. | Read `continue-on-error:` (or the equivalent) against every `matrix.bazel` entry. Every leg blocking = pass. A non-blocking leg with no adjacent comment naming both the choice and a named owner, rotation or digest destination = finding. Era: at 2026-09-06, 9.2.0 is Active LTS and 8.8.0 Maintenance, so a matrix pinning only 8.7.0 has no Active-LTS leg at all. | MUST (multi-consumer) / SHOULD (single consumer, with the stated choice and consumer) |
| BZL-CI-04 | Confirm CI invokes `bazel`/`bazelisk` inside a build or test step on a path that gates a merge — never infer it from a badge, a `.bazelversion` file, or an install step. | 31.23% of Bazel projects **with a CI service configured** never invoke Bazel inside it, and 27.76% of those that do need extra tooling to make it work (383-project study; the denominator is the CI-adopting subset). Adoption that never reaches CI bought nothing, and every other row here then binds to nothing. | `grep -rn 'bazel' --include=*.yml .github/workflows` (or the CI system's equivalent — the pattern matches `bazelisk` too), then confirm at least one hit sits in a `run:`/`script:` step of a **required** check. Empty output, or hits only in setup and install steps, is the finding: Bazel is local-only here. | MUST |

## Secrets, Pins and Dead Snippets

The check for this whole block: greps over the committed configuration —
`.bazelrc*`, `WORKSPACE*`, `MODULE.bazel*`, `.bazelversion`, `.bazeliskrc` and
`.github/`. Each reads the same on 8.7.0 and 9.2.0 because each reads text, not
Bazel.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-20 | Reject on sight any CI or RBE setup instruction that adds a `bazel-toolchains` dependency to a `WORKSPACE` file or calls `rbe_autoconfig`, however authoritative the source — Bazel's own remote-CI page still teaches it. | Bazel 9.0.0 (2026-01-20) deleted the WORKSPACE support code outright: measured, `--enable_workspace` and `--enable_bzlmod` are absent from both `bazel help build --long` and `bazel help startup_options` on 9.2.0, while both are still present on 8.7.0 and 8.8.0. The snippet cannot be typed into a Bzlmod-only repo at all, and the official page has not been updated to say so. | `grep -rn -e 'rbe_autoconfig' -e 'bazel-toolchains' WORKSPACE* MODULE.bazel* .bazelrc* .github/` — a union over both names; any hit in a Bzlmod-only repo is the finding, including one freshly pasted from official docs; empty output is the pass. On an 8.x repo that still loads WORKSPACE the same hit is a portability finding, not a syntax error. | MUST |
| BZL-CI-09 | Name the Bazel version only through bazelisk's own resolution chain — `USE_BAZEL_VERSION`, `.bazeliskrc`, `.bazelversion` — never an OS package install or a hand-pinned Bazel download outside it, and never write `last_downstream_green`. | A side-channel install stops tracking what a developer's local `bazelisk` would pick, so CI and local dev drift with nothing changing on either side. `last_downstream_green` was removed from bazelisk (re-derive the release on your own pin); a model trained before the removal still reaches for it, and [the README](https://github.com/bazelbuild/bazelisk/blob/master/README.md) says to use `last_green` instead. | `grep -rn 'last_downstream_green' .bazelversion .bazeliskrc .github/` — any hit is the finding, empty output is the pass. Then read every Bazel-provisioning step: `apt-get install bazel`, a pinned Bazel download URL, or an unversioned "latest" package is the finding. Downloading a **pinned bazelisk** and letting it resolve Bazel from the chain is not a violation. | MUST |
| BZL-CI-19 | Gate every privileged secret a CI job could expose to an untrusted-triggerable lane behind an event only a trusted actor can produce, and spell that gate identically in every job that touches the secret. | Identical spelling turns a secret-flow audit into a diff; a divergent expression is immediately visible as suspect instead of needing a line-by-line trace. The cache half — a write-capable untrusted lane planting an artifact a trusted build later executes — is `BZL-CACHE-01`, and a BES endpoint reached with the same credential is `BZL-CACHE-33`. | `grep -rn 'secrets\.' --include=*.yml .github/workflows .github/actions` and diff the surrounding conditional across every hit naming the same secret. All identical = pass; any hit with a different or absent gate = finding. Empty output means no secret is in reach of CI at all — confirm that before reading it as a pass. | MUST |

```yaml
# wrong — two spellings of one gate; the audit becomes a line-by-line trace
job-a: auth: ${{ secrets.CACHE_AUTH }}
job-b: auth: ${{ github.event_name == 'push' && secrets.CACHE_AUTH || '' }}
```

```yaml
# right — one spelling everywhere; every pull-request lane evaluates to ''
job-a: auth: ${{ github.event_name == 'push' && secrets.CACHE_AUTH || '' }}
job-b: auth: ${{ github.event_name == 'push' && secrets.CACHE_AUTH || '' }}
```

## Release Lockstep

The check for this whole block: read the release and publish workflows against
everything else that must move with a version — the git tag, `MODULE.bazel`'s
`version`, every repeated tool pin, and the human runbook.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-15 | Make the release job verify, in the same run, that every place the version must appear agrees — git tag, `MODULE.bazel`'s `version`, any in-repo pin moving in lockstep — and where an automated refresh path exists for one of those pins, have the check and the refresh call **one** implementation. | A version-mismatched release either ships silently wrong or fails mid-publish. Two independent implementations of "does this pin match" diverge even when each looks correct on its own, and the divergence surfaces only at the release that needed them to agree. | The release workflow must contain a step comparing the tag ref (`GITHUB_REF_NAME` on GitHub Actions) against a parsed `MODULE.bazel` version and failing on mismatch: `grep -rn 'MODULE.bazel' .github/workflows/release*.y*ml` returning empty is the finding. For a repeated pin, confirm the CI check invokes the same script or function the scheduled refresh calls, not a second copy. | MUST |
| BZL-CI-16 | Move a tool-version pin repeated across N job definitions with exactly one script, and run that script as the CI verification — never hand-edit it across N files. | A partial hand-edit leaves some jobs on the old version with no error anywhere; a bump script that treats a row it cannot rewrite as a hard failure is the only shape that cannot half-apply. | `PIN=2.9.0; grep -rln -- "$PIN" --include=*.yml .github/workflows` before a bump (substitute the repeated tool-version string itself, e.g. an `aspect_rules_lint` pin like `2.9.0`, for `PIN`); the set of files the bump commit touches must equal the set that matched. A bump commit touching fewer files is the finding. Empty output means the pin is not repeated and the row does not bind. | MUST |
| BZL-CI-13 | Read a registry-parity job's shard count from the registry's own presubmit schema — its `platform` list **×** its `bazel` list — and state the resulting number in the job's comment. | The Bazel Central Registry's presubmit format requires a `bazel` field per task alongside `platform`, so the real cost and coverage are the cross-product; counting platforms alone understates it by the major-version factor, which is how a four-platform, two-major matrix gets reported as four shards. | Multiply the job's matrix arrays and compare against any stated count. A stated count that does not match the product is the finding; no count stated anywhere means recompute and add one. Era: with 8.8.0 in Maintenance and 9.2.0 Active LTS, a parity job covering both majors doubles whatever the platform list costs. | SHOULD |
| BZL-CI-17 | Keep the human-facing release runbook — an `AGENTS.md` section, a rules file, a CONTRIBUTING step list — in lockstep with the release workflow, and delete every step describing a manual action the workflow has since automated. | An agent or a new maintainer follows the stale instruction in good faith and performs by hand what the pipeline already did, or waits for a step nobody is running. A stale runbook is worse than no runbook. | Read the runbook's steps against the release and publish jobs, step for step. Any runbook step describing behaviour the workflow no longer requires by hand, or a workflow step the runbook contradicts, is the finding; full agreement is the pass. | SHOULD |

## Before Any Target Selection

The check for this whole block: the median wall-clock the CI system already
reports for the whole-repo job, then the five-step determinism gate below. None
of the [Selection Tool Miss Classes](#selection-tool-miss-classes) rows bind
until every step here passes.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-01 | Default every pipeline to whole-repo `bazel test //...` / `bazel build //...`. Cross into "consider target selection" only when the whole-repo job's median wall-clock on the widest CI runner is consistently past **~40 minutes**; treat **~300 rule targets** as the tripwire that says measure the wall-clock now, not as the trigger itself. Present both numbers as a derived tripwire, never as a published threshold. **pinned** | Both selection tools in production use document real false-negative classes (BZL-CI-21/-22); below the one evidenced switch point, that correctness risk is unpaid-for. Wall-clock leads because it is measurable before the repo is in Bazel and is the cost actually being bought. No source publishes a target-count threshold: the numbers derive from one team's stated pre-adoption context (300+ targets, 1.5M lines, 40-60 minute builds), and a counter-anchor of 2M SLOC and 500 engineers still on whole-repo green argues no such line exists at fleet scale. | The CI system's own median duration for the whole-repo job, plus `bazel query 'kind(rule, //...)' \| wc -l` (identical on 8.7.0 and 9.2.0). Empty query output — no Bazel targets yet — means not yet applicable. A count and duration below the tripwire **with a selection tool already wired** is the finding (premature adoption), as is any document citing either number as an industry threshold. | MUST |
| BZL-CI-02 | Run the five-step determinism gate in order before trusting any target-selection tool, and stop at the first failing step: (1) environment tiers pinned per major, (2) no undeclared non-determinism in the actions the tool would call unchanged, (3) every shared-service or unbounded-fan-out CI step converted to a self-contained cacheable target, (4) cache eviction recognised by its **error text** with the retry default left alone, (5) only then the tool's own miss class. | A tool's "unchanged" verdict is only as good as the action-key stability of what it diffs; skipping to tool choice fuses two independent risk sources into one nobody can diagnose separately. Step 4 keys on the error text because **exit 39 never surfaces**: measured across ten invocations on both majors, an evicted blob self-heals to exit **0** under the default five eviction retries and fails as a generic exit **1** with retries at 0. | Named reading heuristic — confirm each cited rule's own verification passes, in that order: `BZL-HERM-01`, `-04`, `-05`, `-10`, `-11` and `-19` for steps 1-3, `BZL-CACHE-12` and `-16` for step 4. A gate step nobody has run reads exactly like a failed one, so "no findings" from an unrun step is the finding. Measured in the cache-only shape on both majors; a live `--remote_executor` alongside a downed cache is unmeasured. | MUST |
| BZL-CI-28 | Run the execution-log diff that measures action-key stability once per distinct Bazel major in the CI matrix, never once for the whole matrix. | `--incompatible_strict_action_env` (the per-version default is BZL-FLAG-21's and the rc pin is BZL-HERM-01's; read it off your own pin) and `--incompatible_repo_env_ignores_action_env` default `false` on 8.7.0 and 8.8.0 and `true` on 9.2.0 — read from the binaries themselves — so a stability measurement on one leg says nothing about a leg running the other major under a different environment-inheritance policy. Measured on Linux: 8.7.0 additionally leaks `LD_LIBRARY_PATH` and the client `PATH` into the action environment. | Confirm `BZL-CACHE-16`'s execution-log diff runs against every Bazel version named in the matrix, not only the pinned default. A single-major matrix means the row does not bind; a multi-major matrix with one captured log is the finding. `bazel help build --long \| grep incompatible_strict_action_env` at each pinned version prints the default that leg actually runs under. | MUST |

## Selection Tool Miss Classes

The check for this whole block: read the CI wrapper's flags, then run
`bazel-diff -v generate-hashes …` once against a live checkout and read the
literal `Executing Query:` line it prints.

```
Executing Query: '//external:all-targets' + '//...:all-targets'   # default mode — the repo-rule miss is live
Executing Query: deps(//...:all-targets)                          # --useCquery — that class is already closed
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-21 | Treat `bazel-diff`'s **default** mode as blind to any transitive dependency introduced only by *executing* a repository rule or module extension — pip, npm, Maven, most Bzlmod extensions. Pass `--useCquery`, keep an exhaustive `--fineGrainedHashExternalRepos` allow-list, or state in the wrapper that the miss class is live and unmitigated. | Measured across four flag combinations on 8.7.0 with a `use_repo_rule`-generated repo whose implementation reads a file: in default mode the dependent target's hash was **byte-identical** before and after, and only the input file itself was reported. `--fineGrainedHashExternalRepos=@<repo>` caught it; `--useCquery` also caught it, with no allow-list at all. Maintainer-confirmed and closed as expected behaviour. Since Bazel 8.6.0+/9.0.1+, default mode *does* catch a declared `bazel_dep` version bump via `mod show_repo` diffing — a different, narrower miss. | Read the wrapper for `--useCquery` or `--fineGrainedHashExternalRepos`/`--fineGrainedHashExternalReposFile`. Both absent, on a repo whose dependency resolution runs through a repository rule or module extension, is the finding: the class is live. On a live invocation, `deps(//...:all-targets)` on the `Executing Query:` line means cquery mode is on and the class is closed; `'//external:all-targets' + '//...:all-targets'` means it is not. An allow-list is a second, quieter miss class — nothing verifies it stays exhaustive. | MUST |
| BZL-CI-22 | Treat `target-determinator`'s results cache as invalid across a change to a home or system `.bazelrc`, an environment variable affecting `cquery` output, or a different host machine; never share its cache directory across CI runners of differing OS or arch, and pass `--nocache_results` whenever any of the three changed since the cache was populated. | None of the three enters the results-cache key — [the tool's own README](https://github.com/bazel-contrib/target-determinator/blob/main/README.md), re-checked against its current `--help` with zero drift — so a stale "before" compared against a fresh "after" "may produce spurious differences". Options passed via `-bazel-opts`/`-bazel-startup-opts` **are** in the key. Note the direction: this errs toward over-reporting, the opposite of BZL-CI-21's silent miss. | `grep -n -e 'cache-dir' -e 'nocache_results' <the CI wrapper>` and confirm the cache directory is not a volume shared across a multi-OS or multi-arch matrix. Empty output means no cache is configured — a pass by absence. A shared cache path across heterogeneous runners is the finding. v0.34.0 against Bazel ≥4.0.0; behaviour unchanged 8.7.0 → 9.2.0. | MUST |
| BZL-CI-23 | On a Bzlmod-only repo, never feed a raw `//external:...` label from `bazel-diff get-impacted-targets` into `bazel build`/`test`; rely on the Bzlmod auto-detection of `--excludeExternalTargets` or set it explicitly, and know it means two different things per subcommand. | `//external:*` labels are synthetic accounting devices `generate-hashes` needs to express repo-rule dependencies. They are not buildable in Bzlmod-only mode and fail downstream. On `generate-hashes` the flag skips querying `//external:all-targets`; on `get-impacted-targets` it drops `//external:*` from the **output**. This is a crash class, not a staleness class — never cite it as a BZL-CI-21 mitigation. | `grep -n 'excludeExternalTargets' <the CI wrapper>`. On a Bzlmod-only pin — every 9.x repo, since 9.0.0 deleted WORKSPACE support — the flag must be absent, meaning auto-detected via `bazel mod graph`, or explicitly `true`. Empty output on an 8.x repo that still loads WORKSPACE is not applicable. A downstream build failing on an `//external:` label is the live symptom. | MUST |
| BZL-CI-24 | Tag every target that reads undeclared workspace state at execution time — a repo-scanning linter run as a test — and pass that tag to `bazel-diff --alwaysAffectedTags`. | `bazel-diff`'s own flag documentation names this class: such targets "would otherwise hash as unchanged and be wrongly skipped", and a repo-wide gate is precisely what selection must never skip. `target-determinator` publishes no equivalent flag, so the same target class needs a different answer there. | `grep -rn -e 'buildifier' -e 'eslint' -e 'gofmt' --include='BUILD*' .` — a union over the usual repo-scanning linters — for a test target of that shape, then confirm its tag appears in the wrapper's `--alwaysAffectedTags` list. Empty output from the grep means no such target exists and the row does not bind; a hit with no matching tag is the finding. | MUST where such targets exist |
| BZL-CI-26 | Re-derive a selection tool's flag surface from its **current** `--help` or README before writing guidance about it, pair any accuracy or absolute-correctness claim with a check of that tool's issue tracker for a named exception, and never cite `--includeTargetType` as a correctness mitigation. | `bazel-diff` alone gained `--useCquery`, `--fineGrainedHashExternalRepos`, `--excludeTargetsQuery`, `--alwaysAffectedTags`, `mod show_repo`-based `MODULE.bazel` diffing, a persistent `serve` mode with an S3 cache tier and snapshot warmup since the 2022 critique most training-data summaries repeat verbatim — read [the release list](https://github.com/Tinder/bazel-diff/releases), not a summary. `--includeTargetType` only changes the output schema (`{target: sha256}` → `{target: "type#sha256"}`) and closes no miss class. Bazel core has no native answer either: the tracked request for a target-diffing primitive ([bazelbuild/bazel#7962](https://github.com/bazelbuild/bazel/issues/7962)) has been open since 2019-04-05. | `bazel-diff --help` and `bazel-diff generate-hashes --help`, or a fresh README fetch, against the specific flag being cited — empty output for a flag name means it is renamed or absent at that version, which is the finding, not a pass. For a native primitive, `bazel help query 2>&1 \| grep -i affected` prints nothing on 8.7.0 and 9.2.0 alike. | MUST (flag re-derivation) / CONSIDER (the accuracy-claim clause, vendor-sourced) |
| BZL-CI-25 | Read an empty diff from either tool as "no evidence of a difference under this tool's own blind spots", never as "nothing changed", and name which flags were in effect for that specific invocation before acting on it. | `target-determinator`'s `-filter-incompatible-targets` defaults `true` and silently drops platform-incompatible targets before the caller sees them; its `-before-query-error-behavior` defaults `ignore-and-build-all`, so a failed "before" query degrades silently to build-all and makes "the tool ran" indistinguishable from "the tool selected". `bazel-diff`'s default query mode over-approximates `select()` rather than resolving it. All three change what empty means. | Named reading heuristic — before trusting a zero-target diff as a skip-everything signal, state which of `-filter-incompatible-targets`, `-before-query-error-behavior`, `--useCquery` and `--excludeExternalTargets` were set, and name a real change this invocation could have missed. A zero-target diff acted on with no flag list stated is the finding. | SHOULD |
| BZL-CI-27 | Keep at least one scheduled or merge-to-main whole-repo `bazel test //...`, whichever selection tool gates pull requests. | Both documented miss classes are silent by construction; a periodic full run is the only backstop that does not itself depend on the tool's correctness. | `grep -rn -e 'schedule:' -e 'cron' --include=*.yml .github/workflows` for a job running the unrestricted target pattern. Empty output on a repo already using target selection is the finding; on a repo with no selection tool the row does not bind. | SHOULD |
| BZL-CI-30 | Choose `target-determinator` when repository-rule and extension-introduced transitive dependencies must be tracked exactly with no per-repo configuration to maintain; choose `bazel-diff` **with `--useCquery`** when the same class must close without an allow-list and query cost is acceptable; choose `bazel-diff` default mode only where that class is separately mitigated or genuinely absent. State which trade-off was taken. | `target-determinator` was built for this class by its maintainer's own account in the same issue thread that documents the miss — the strongest first-hand comparison available, and still informal and unbenchmarked. `--useCquery` has since closed the same class for `bazel-diff`, so the decision is a query-cost trade-off, not a tool switch. | Named reading heuristic — count the module extensions and repository rules the repo's dependency resolution runs through, then price one `cquery` per revision against an allow-list somebody keeps exhaustive. A wrapper with no stated trade-off is the finding. `--useCquery`'s cost at scale is unmeasured (see [Gaps](#gaps)). | CONSIDER |

## The Build Event Stream

The check for this whole block: `grep -rn -e 'bes_backend' -e 'bes_upload_mode' -e 'remote_build_event_upload' -e 'build_event' .bazelrc* .github/`
— a union over the four names. Empty output means no BES lane exists and none of
these rows bind. All five flag
names and defaults were read from both binaries' own `bazel help build --long`
and are identical on 8.7.0 and 9.2.0. Whose credential the endpoint uses is
`BZL-CACHE-33`; `BZL-CI-19` folds that endpoint into the secret audit.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CI-32 | Name which cache a "cache hit rate" means wherever the Build Event Protocol feeds a dashboard, and never source a build-wide **remote** figure from `BuildMetrics.ActionSummary.remote_cache_hits`. | That field is `[deprecated = true]` in Bazel's own [`build_event_stream.proto`](https://github.com/bazelbuild/bazel/blob/9.2.0/src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto). The live substitutes measure something else: `action_cache_statistics.hits/misses` is the **local**, on-disk action cache and can read 100% with zero remote traffic, and `ArtifactMetrics.output_artifacts_from_action_cache` versus `.output_artifacts_seen` separates local from local-plus-remote, never remote alone. The only clean remote-scoped boolean in the schema is test-only: `TestResult.execution_info.cached_remotely`, rolled up as `TestSummary.total_num_cached`. | Read the dashboard's own query against the proto field it reads. A chart labelled "remote cache hit rate" fed by `action_cache_statistics`, by `ArtifactMetrics`, or by the deprecated counter is the finding. Empty (no BEP-derived cache chart exists) means the row does not bind. Schema identical on 8.7.0 and 9.2.0. | MUST |
| BZL-CI-33 | Run the producing lane with `--remote_build_event_upload=all`, or carry a documented not-found fallback, wherever a tool dereferences `bytestream://` URIs out of a captured BEP stream. | The flag defaults to `minimal` on 8.7.0 and 9.2.0, and its own help text states that local outputs referenced by BEP are not uploaded except files important to BEP consumers, while "bytestream:// scheme is always used for the uri of files even if they are missing from remote cache". Under the default the URI is emitted for blobs that were never pushed, and the consumer gets a not-found for a file BEP said exists. | `grep -rn 'remote_build_event_upload' .bazelrc* .github/` read alongside the consumer's fetch path. Empty output **plus** a consumer that fetches by URI is the finding — the `minimal` default is in effect. Empty with no such consumer means the row does not bind. | MUST where a consumer dereferences BEP URIs |
| BZL-CI-31 | Set `--bes_upload_mode` deliberately per lane and say why in an adjacent comment: `fully_async` where the lane's pass/fail must not depend on the BES backend being reachable, `wait_for_upload_complete` only where a missing upload is itself worth failing on. | The default is `wait_for_upload_complete` on both majors, so an unreachable or slow BES endpoint blocks build completion on a lane whose cache outage is already tolerated silently — measured, a refused cache endpoint produces a WARNING, a full local build and exit 0 in the cache-only shape. Two best-effort decisions that disagree leave the stricter one deciding the lane's reliability, with nothing saying so. | `grep -rn -e 'bes_backend' -e 'bes_upload_mode' .bazelrc* .github/` — a `bes_backend` hit with no `bes_upload_mode` alongside it is the finding: the blocking default is in effect, unexamined. Empty output on `bes_backend` means no BES lane exists and the row does not bind. | SHOULD |

## Gaps

- No source publishes a target-count threshold; BZL-CI-01's numbers are this
  program's derived tripwire, and quoting them as an industry line invents
  provenance.
- `--useCquery`'s cost at scale is untested — the measurement closing the
  repository-rule miss class ran on one small scratch repro on 8.7.0.
- Exit-39 and cache-outage behaviour were measured only in the cache-only
  shape; a live `--remote_executor` alongside a downed cache may differ.
- A `bazel-diff` allow-list has no verification that it stays exhaustive, and
  nobody has measured whether an *owned* advisory leg is acted on.
- Every measurement here ran on Linux under `linux-sandbox`: darwin and Windows
  lint-runner reach, tag and sandbox behaviour are unmeasured, as is a full
  changelog audit of `buildifier_prebuilt` 8.2.0.2 → 8.5.1.4.

## What Agents Get Wrong Here

1. **Reading a mode string or a job name as the gate's behaviour** — a `warn`
   mode still exits nonzero, and a job named for determinism may compare no
   action keys at all (BZL-CI-06, BZL-CI-07).
2. **Pasting an `rbe_autoconfig` / `bazel-toolchains` snippet from Bazel's own
   remote-CI page into a Bzlmod repo**, where it cannot be typed at all
   (BZL-CI-20).
3. **Proposing target selection before whole-repo green costs anything**, and
   quoting "~300 targets" as an industry threshold rather than a derived
   tripwire (BZL-CI-01).
4. **Assuming `bazel-diff` defaults to cquery** because its most co-mentioned
   peer does, then reaching for `--fineGrainedHashExternalRepos` when cquery
   mode is already on and the allow-list is pure maintenance surface
   (BZL-CI-21).
5. **Treating an empty diff or "zero impacted targets" as proof nothing
   changed**, with no flag list stated for that invocation (BZL-CI-25).
6. **Matching retry or alerting logic on exit code 39 for an evicted cache
   blob** — measured over ten invocations on two majors, the caller sees 0 or a
   generic 1, never 39 (BZL-CI-02, step 4).
7. **Charting `action_cache_statistics.hits` as a remote cache hit rate**; that
   message is the local on-disk action cache and reads 100% with zero remote
   traffic (BZL-CI-32).
8. **Adding `--aspects=` with no `fail_on_violation` and no `lint_test`** — the
   build writes report files under `bazel-out` and fails nothing (BZL-CI-11).
