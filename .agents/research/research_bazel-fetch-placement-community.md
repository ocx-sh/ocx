# Research: Bazel fetch placement — community reports

## Metadata

**Date:** 2026-11-23
**Lane:** community threads (/hex-discuss, .agents/discussions/bazel-rbe-rules-ocx.md)
**Basis:** every source below was read through a summarising fetch (WebFetch small-model summary) or a search-result snippet; none read in full verbatim. Search-only items are marked SNIPPET. Slack archives, BazelCon talks, Buildbarn/NativeLink/Buildfarm trackers, rules_js/rules_python/rules_go/rules_nixpkgs/rules_multitool trackers were NOT reached (see Negative).
**Expires:** 2027-05-23
**Caveat:** findings only, no recommendation; anecdotes are not measurements

Age flag: "OLD" = older than ~18 months before 2026-11-23 (before ~2025-05).

## Q1 Network use inside build actions

- Official doc on adapting rules for RE says nothing on network/`requires-network`; it says tools should be pre-installed in the execution environment (toolchain container) or built from source, and target-specific work belongs in build actions. https://bazel.build/remote/rules (undated, current docs)
- Aspect (Alex Eagle), undated: an action tagged `requires-network` "gave up Bazel's caching"; the fix was to move the download into a repository rule (`http_file`) and let the action run on the fetched file. Source: search-tool paraphrase, SNIPPET; the fetched article body did not contain this passage, so treat as UNVERIFIED against the article. https://aspect.build/blog/lazy-tool-fetching-under-bazel
- jmmv (Sep 2025): a `genrule` doing `curl` is the example of cache-poisoning risk: a tampered download only has to exist long enough to write a malicious result into the remote cache. States that disabling network on remote workers is "the only thing you can and must do", and still maybe "not good enough"; checksums are worthwhile, especially in repo rules, but insufficient alone. https://jmmv.dev/2025/09/bazel-remote-execution.html
- Tweag/Tereshenkov (2026-04-02): Bazel "by design lacks support and features for downloads to take place within build actions"; ad hoc curl/wget in genrules evade the downloader config and are invisible as fetch events. Mentions `block-network` / `requires-network` tags and that cached action results can still be served while network is blocked. https://tweag.io/blog/2026-04-02-making-bazel-builds-more-reliable/
- bazel-discuss (2025-02-28/03-03), "How to Stop Bazel From Accessing the Internet": replies point to `block-network` / `requires-network` tags and `--sandbox_default_allow_network`; another poster blocks all downloads by default with `--experimental_downloader_config` plus allow-list/rewrite rules. No resolution reported. https://groups.google.com/g/bazel-discuss/c/wrYU76i2dec
- Namespace RBE docs: sandboxed actions get loopback-only network by default; per-action `namespace_requires_network=true` opts in. Vendor-specific, not the Bazel tag. Docs do not say how such actions are cached. https://namespace.so/docs/bazel/execution
- Android Kleaf bazelrc: network disabled for actions by default; targets must tag `requires-network` to opt out (SNIPPET from search, not fetched). https://android.googlesource.com/kernel/build/+/main/kleaf/bazelrc/network.bazelrc
- Tweag rules_img (2025-12-18, Malte Poll): rules_oci's repository rule pulls all blobs; the author says this did not address bottlenecks at scale under RE; alternative fetches only manifest/config at build time and moves layer bytes at push/load. No benchmark given. https://tweag.io/blog/2025-12-18-rules_img/
- Not found: any report of cache poisoning or nondeterminism caused specifically by `requires-network` actions beyond the jmmv argument; any report of an executor without egress failing on such an action; policy-rejection reports.

## Q2 Repository-rule downloads under RE / shared cache

- bazelbuild/bazel#6359 "Allow using remote cache for repository cache": opened 2018-10-11 (OLD): `--repository_cache` does not help stateless CI, downloads cost minutes each run; commenter described tarring `external/` to an internal file server (search snippet, date not confirmed). Closed completed 2025-11-03 by "Add a remote repo contents cache"; in 8.8.0 RC1 per the 2026-08-07 comment. 2025-08-06: `--repo_contents_cache` (local, 8.3.0) and experimental `--experimental_repo_remote_exec` / `remotable` for `rctx.execute`, "without vouching for its quality". https://github.com/bazelbuild/bazel/issues/6359
- EngFlow known-issues page: before Bazel 9.3.0, `--experimental_remote_repo_contents_cache` can hang after a remote cache read timeout; enable only on 9.3.0+ (refs bazelbuild/bazel#30780). A search snippet quoted an Aug 2026 version of the page not recommending the flag for production (memory-related hangs); the fetched page did not show that sentence, so the two renderings disagree. https://docs.engflow.com/re/client/bazel-known-issues.html
- Aspect (Alex Eagle, undated): repository cache "often misunderstood": it caches only hash-pinned fetches through Bazel's downloader, not the installed repo; install computation (npm/pip) is never cached and reruns on invalidation. Advises profiling first; says external repos are locally cached and shouldn't be invalidated often. https://aspect.build/blog/lazy-tool-fetching-under-bazel
- Tweag (2026-04-02): Bazel 9 adds remote repository caches; suggests `--repository_disable_download` to verify them; `--module_mirrors` (8.4+) covers registry sources only. https://tweag.io/blog/2026-04-02-making-bazel-builds-more-reliable/
- magma/magma#12458 (2022-04 to 2022-07, OLD): team moved off GitHub Actions cache; fetched-but-unbuilt deps not cached remotely (cites #6359); `--experimental_remote_downloader` showed no gain in their test (621.7s vs 556.1s cold); a reviewer called the test flawed (needed expunge, two warm runs, `--repository_cache` unset). https://github.com/magma/magma/issues/12458
- Bazel docs: WORKSPACE rules that build binaries, pip-install or mutate the host are incompatible with RE; do host probing conditionally or via `--override_repository`. https://bazel.build/remote/rules
- Not found: first-hand CI numbers for `bazel vendor`, `--distdir` in CI at scale, or a fresh runner downloading a large toolchain despite warm cache (only the #6359 complaint in general terms).
- toolchains_llvm README (SNIPPET): host-OS detection options "not suited for docker based sandboxed builds or remote execution builds"; toolchains_llvm_bootstrapped claims RE works out of the box. https://github.com/Snowflake-Labs/toolchains_llvm and https://github.com/cerisier/toolchains_llvm_bootstrapped

## Q3 Stated norms and dissent

- For "downloads in repository rules, not actions": Tweag 2026-04-02 (tracking/visibility, downloader config, checksum), Aspect (cacheability), jmmv 2025-09 (poisoning; but says checksum checks in repo rules are fine), Kleaf (network off in actions by default). URLs above.
- Qualifications of that norm: repo rules are not sandboxed either; Bazel cannot enforce offline builds through them (Bazel external/advanced docs, SNIPPET) https://bazel.google.cn/external/advanced?hl=en ; repo-rule work is never cached beyond hash-pinned downloads (Aspect); repo rules run on the client, so under RE the client pays (Bazel docs, rules page).
- Dissent in the opposite direction (downloads should be deferred to actions to be lazy/cacheable): Aspect advises deferring *install work* from repository rules to actions, noting it "generally requires changes to the rules you're using". Same article. Tweag rules_img likewise moves layer bytes out of the repository rule. This is about computation/bytes, not about network in actions.
- Not found: a maintainer statement of "never use the network in actions" as policy; a defender of `requires-network` actions with stated reasoning.

## Q4 Avoidance patterns reported

- Pre-install tools in executor image/toolchain container (Bazel RE rules doc).
- Remote Asset API / `--experimental_remote_downloader` (Buildbarn bb-remote-asset, bazel-remote partial Fetch support; Bazel 3.0+, experimental): https://github.com/buildbarn/bb-remote-asset ; https://bazel.virtuslab.com/book/4~9~10/ ; EngFlow https://docs.engflow.com/re/client/remote-asset-api.html (SNIPPET only)
- Downloader config rewrite/block (`--downloader_config`), internal repo managers, pull-through proxies (Tweag 2026-04-02).
- Tar `external/` to internal file server (#6359, 2018-2019, OLD); persistent runner cache; rsync of repository cache.
- Split metadata fetch from byte movement (rules_img, Tweag 2025-12-18).
- Check in generated outputs / vendor (Bazel RE rules doc; Aspect advises vendoring pip_parse output with a freshness test).
- Chaos-test by blocking hosts in staging (Tweag 2026-04-02).

## Q5 Executors reaching only an internal mirror

- No first-hand practitioner report found. Closest: Virtuslab book chapter on air-gapped Bazel (SNIPPET) https://bazel.virtuslab.com/book/6~7~2/ ; JFrog says an Artifactory remote repo is a proxy not a pre-filled mirror, offline needs pre-warm or local repo upload https://jfrog.com/blog/using-artifactory-with-an-air-gap/ ; bazel-discuss thread above (host allow-list with downloader config).
- Inference, UNVERIFIED by a primary source: repository-rule evaluation stays on the client unless remote repo exec flags are used, so executor egress matters only for actions.

## Negative

- No BuildBuddy, NativeLink, Buildfarm, rules_js, rules_python, rules_go, rules_nixpkgs, rules_multitool, rules_oci issue-tracker thread read; the search tool returned none on topic.
- Bazel Slack archive and BazelCon talks not searched successfully (no public mirror hit).
- GitHub MCP unavailable (auth error); GitHub issue search was not possible beyond web fetches.
- No source found stating `requires-network` is honored or ignored on remote workers in vanilla Bazel; the one search summary said no confirmation.
- Contradiction: EngFlow page wording differs between the fetch and the search snippet (see Q2).
- Aspect article has no date in the fetch.

## Leads

- bazelbuild/bazel#30780 and the remote repo contents cache design/PRs: real behaviour and failure modes on 9.x.
- bazelbuild/bazel issue search for `requires-network` + remote: confirm tag semantics on RE (needs working GitHub access).
- rules_multitool, rules_oci, rules_python issue trackers: download-placement decisions by maintainers.
- BuildBuddy and EngFlow docs on executor egress policy and Remote Asset fetch.
- bazel-discuss/bazel-dev threads on `--experimental_repo_remote_exec`.
