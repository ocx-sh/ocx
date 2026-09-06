---
name: bazel-diagnose
description: Symptom-routed diagnosis procedure for a Bazel build that is already wrong, covering slow builds and slow analysis, unexpected cache misses, cache or BES outages, non-hermetic and nondeterministic actions, flaky tests, repository-rule and module-extension refetches, target-selection misses, and flags that appear not to exist. Use when someone says the build got slow, analysis takes forever, CI never hits the cache, this rebuilt for no reason, why did this action re-run, the remote cache is down, exit code 39, lost inputs, this test passes locally and fails in CI, the sandbox is not hermetic, Bazel keeps refetching an external repo, the diff tool skipped a target, or unrecognized option after a version bump. Not for deciding whether to adopt Bazel or for standing a workspace up, which is bazel-adopt, and not for the standards themselves, which the bazel-quality rules carry.
license: Apache-2.0
metadata:
  summary: Routes a wrong Bazel build by symptom to the one measurement that names its root cause, on 8.7.0 and 9.2.0 alike
  keywords: bazel,diagnose,debugging,slow-build,profile,cache-miss,execution-log,execlog,remote-cache,cache-eviction,exit-39,hermeticity,sandbox,nondeterminism,flaky-test,repository-rule,module-extension,lockfile,target-selection,bazel-diff,flags
---

# bazel-diagnose

A Bazel build is already wrong: too slow, missing the cache, rebuilding for no
reason, failing on one machine, or green when it should be red. This skill
routes the symptom to the one measurement that names the cause.

Every command below was checked against Bazel **8.7.0** and **9.2.0** on Linux;
where the two differ, the step says which, and where a result could differ off a
Linux host (macOS, Windows, a container without user namespaces) it says so
rather than generalising. This skill carries the procedure only: it never
restates a standard, it cites the `bazel-quality` rule by ID.

Contents: [Stop condition](#stop-condition) ·
[Never edit the check](#never-edit-the-check) ·
[Step 0: ground truth](#step-0-ground-truth) ·
[Route by symptom](#route-by-symptom) ·
[A. Slow build or slow analysis](#a-slow-build-or-slow-analysis) ·
[B. Unexpected cache miss](#b-unexpected-cache-miss) ·
[C. Cache or BES outage](#c-cache-or-bes-outage) ·
[D. Nondeterministic or non-hermetic action](#d-nondeterministic-or-non-hermetic-action) ·
[E. Flaky test](#e-flaky-test) ·
[F. Repository-rule or module-extension refetch](#f-repository-rule-or-module-extension-refetch) ·
[G. Target-selection miss](#g-target-selection-miss) ·
[H. A flag that "does not exist"](#h-a-flag-that-does-not-exist) ·
[The receipt](#the-receipt) ·
[What agents get wrong](#what-agents-get-wrong) ·
[References](#references)

## Stop condition

Stop when all four hold. Not before, and do not keep measuring after.

1. The **root cause is named** as a mechanism, not a category. "The action key
   changed because the `.bazelrc` passes `--action_env=WS=$PWD` and this action
   reads `WS`" is a cause. "Cache instability" is a category.
2. The **measurement that proved it is pasted** into the report — the command
   and its real output, verbatim. Narrating a measurement is not making one.
3. Either the **fix was watched to make the symptom go away** (re-run the same
   measurement after the change and paste both readings), or the cause is a
   **named documented gap** with the version and the surface that would settle
   it.
4. Nothing outside the cause was changed to reach step 3.

A diagnosis that ends at "probably X" is not finished; it is a hypothesis that
needs one more measurement or an explicit gap.

## Never edit the check

Making the symptom disappear by weakening the thing that detected it is the
failure this skill exists to prevent. Each of these is a violation, not a fix:

| Edit | Why it is a violation | Rule |
|---|---|---|
| Adding `--noincompatible_strict_action_env` to stop "environment flakiness" | On 8.x it looks like a no-op and silently pins permissive behaviour across a 9.x bump; on 9.x it reintroduces the cache-key-invisible class | BZL-CACHE-19 |
| Tagging an action `no-cache` / `local` so the miss stops being visible | The action now re-executes every build forever; the key instability is untouched | BZL-CACHE-15, BZL-TEST-08 |
| Adding `--sandbox_debug` to a committed rc file | An unbounded disk leak by design; it is a one-shot investigation flag | BZL-HERM-03 |
| Dropping to `--spawn_strategy=local` or `no-sandbox` to make a sandbox failure go away | `local` has no namespace to revoke, so every isolation guarantee the build had is gone | BZL-HERM-26 |
| Tagging a failing `diff_test` / `write_source_files` target `manual` | Deletes the regeneration guard instead of regenerating the file | BZL-HERM-21 |
| Setting `flaky = True` or `--flaky_test_attempts` on a red test | Both retry paths stop at the first success — they manufacture a green, they do not measure | BZL-TEST-05, BZL-TEST-06 |
| Widening a `glob()` until the missing file reappears | The pattern was never the mechanism; a `BUILD` file appearing beneath it was | BZL-HERM-19, BZL-ARCH-02 |
| Inventing a knob (`salt=`, `--remote_symlink_absolute_path_strategy`) | Neither exists in any client control surface | BZL-CACHE-22, BZL-CACHE-31 |

If the only available fix is one of these, it is a documented gap under stop
condition 3 — recorded with its reason, never applied silently.

## Step 0: ground truth

Run these three before the first hypothesis. They cost seconds and they are what
keeps the rest of the procedure from being fiction.

1. **Name the version.** `bazel --version`, and read `.bazelversion` and any
   `USE_BAZEL_VERSION` in CI. A matrix spanning 8.x and 9.x is two diagnoses,
   not one: `--incompatible_strict_action_env` flips its default at 9.0.0
   (BZL-FLAG-21 owns the per-version default, BZL-HERM-01 the rc pin), so an
   action-key measurement on one leg says nothing about the other; measure
   once per major (BZL-CI-28).
2. **Read the flag surface you are about to cite**, on that binary:
   `bazel help <command> --long` **and** `bazel help startup_options`. A flag can
   live in only one. `bazel help all --long` is not a command — it prints
   `ERROR: 'all' is not a known command`, and reading that error as "the flag is
   absent" inverts the answer (BZL-FLAG-11).
3. **Decide whether a remote executor exists at all**:
   `grep -rn 'remote_executor' .bazelrc* .github/ 2>/dev/null`. Nearly every
   cache and exit-code behaviour below differs between a cache-only build (no
   `--remote_executor`) and a remote-execution build, and every measurement in
   this skill was taken cache-only. EMPTY = cache-only; read the outage and
   eviction sections as measured. Non-empty = the measurements do not bind, and
   that limitation goes in the receipt.

## Route by symptom

| The symptom, in the reporter's words | Go to | Depth |
|---|---|---|
| "the build got slow", "analysis takes forever", "it re-analyzes everything" | [A](#a-slow-build-or-slow-analysis) | [references/slow-build.md](references/slow-build.md) |
| "CI never hits the cache", "why did this action re-run", "it rebuilt for no reason" | [B](#b-unexpected-cache-miss) | [references/cache-miss.md](references/cache-miss.md) |
| "the cache is down", "exit code 39", "lost inputs", "the build event upload failed" | [C](#c-cache-or-bes-outage) | [references/outage-and-eviction.md](references/outage-and-eviction.md) |
| "same inputs, different output", "works on my machine", "the sandbox isn't hermetic" | [D](#d-nondeterministic-or-non-hermetic-action) | [references/nondeterminism.md](references/nondeterminism.md) |
| "this test passes locally and fails in CI", "it's flaky", "coverage is empty" | [E](#e-flaky-test) | [references/flaky-test.md](references/flaky-test.md) |
| "Bazel keeps refetching", "the lockfile check crashed", "the extension re-runs" | [F](#f-repository-rule-or-module-extension-refetch) | [references/fetch-and-selection.md](references/fetch-and-selection.md) |
| "the diff tool skipped a target that was broken" | [G](#g-target-selection-miss) | [references/fetch-and-selection.md](references/fetch-and-selection.md) |
| "unrecognized option", "that flag doesn't exist any more" | [H](#h-a-flag-that-does-not-exist) | — |

B and D share one measurement — the execution-log diff. Run it once, read it for
both.

## A. Slow build or slow analysis

1. **Capture a profile.**
   `bazel build <target> --profile=/tmp/prof.json.gz --generate_json_trace_profile=yes`
   (`--generate_json_trace_profile` is a TriState defaulting to `auto`, not a
   plain boolean; with `--profile` unset Bazel still writes
   `command-$INVOCATION_ID.profile.gz` under the output base with a
   `command.profile.gz` symlink to the latest).
   Confirms: the file exists and is non-empty. A missing or zero-byte file means
   the invocation died before Bazel wrote the profile — fix that failure first.
2. **Read it.** Open `/tmp/prof.json.gz` in `chrome://tracing` or Perfetto.
   **Do not run `bazel analyze-profile` on a 9.x pin**: the command was deleted
   at 9.0.0, while Bazel's own frozen 9.1.0 documentation snapshot still lists
   it (BZL-FLAG-31). On 8.7.0 and 8.8.0 it still exists.
   Read the `Main Thread` row's phase markers (`Launch Blaze`,
   `evaluateTargetPatterns`, `runAnalysisPhase`), plus the `Critical Path`,
   `Action count` and `Garbage Collector` rows. Skymeld overlaps analysis and
   execution and **defaults on** for both 8.7.0 and 9.2.0, so the two phases are
   concurrent by design; no lane is labelled for it. Never write guidance that
   frames Skymeld as something to enable.
3. **Split analysis-dominated from execution-dominated.** If wall time is spent
   before or overlapping `runAnalysisPhase`, go to step 4. If it is in execution
   or in GC pauses, go to step 5.
4. **Analysis-dominated — profile Starlark.**
   `bazel build --nobuild --starlark_cpu_profile=/tmp/cpu.pprof <target>` then
   `pprof -top /tmp/cpu.pprof`. The top frame names the `.bzl` function.
   A depset built in a loop with the accumulator in `transitive=` is
   BZL-LARK-07; `depset.to_list()` on an action-construction path is
   BZL-LARK-17. EMPTY (no Starlark hotspot) sends you to package loading and
   configured-target duplication — both in
   [references/slow-build.md](references/slow-build.md).
5. **Execution- or GC-dominated.** `bazel info used-heap-size-after-gc`, and read
   the profile's `Garbage Collector` row. Cap the server heap with
   `--host_jvm_args=-Xmx<n>g`; on a repeat OOM add `--heap_dump_on_oom`, which
   writes `<output_base>/<invocation_id>.heapdump.hprof`. Tune
   `--gc_thrashing_threshold` / `--gc_thrashing_limits`, never
   `--experimental_oom_more_eagerly_threshold` — that name is a **silent alias**
   for the first and its default of `100` disables the detector outright.
6. **Before blaming the code, rule out the session.** Mixing `bazel build -c opt`
   with `bazel cquery` in one session makes each discard the other's analysis
   cache — a full re-analysis with no source change. `--noallow_analysis_cache_discard`
   turns that silent discard into a hard error, which is how you prove it.

Read [references/slow-build.md](references/slow-build.md) when the profile is
open and you need the per-culprit table, the memory-tracking startup flags, or
the `bazel dump --skyframe=` enum whose values were renamed at 9.0.0.

## B. Unexpected cache miss

1. **Read the status line of the build in question.**
   `INFO: N processes: X remote cache hit, Y linux-sandbox`.
   This line **never reports local action-cache hits** — a build showing few
   processes is not evidence of a remote miss.
2. **Name which cache.** There are seven (in-memory Skyframe, repository cache,
   repo contents cache, output tree plus local action cache, local disk cache,
   remote action cache plus CAS, and remote execution — a compute path, not
   persistent state), and conflating two is the most common wrong diagnosis
   (BZL-CACHE-15). The one-command discriminator per cache is the table in
   [references/cache-miss.md](references/cache-miss.md).
3. **Never use `--explain` for this.** Measured on 8.7.0 and 9.2.0, it reported
   `no entry in the cache (action is new)` for actions that were confirmed
   disk-cache hits: it reasons from the local dependency checker only and cannot
   see a disk or remote hit at all (BZL-CACHE-16, BZL-HERM-15).
4. **Find the action in the execution log.**
   `bazel build --execution_log_compact_file=/tmp/e.log <target>`, then read the
   entry for the label. `runner` is the discriminator:
   - `"disk cache hit"` / `"remote cache hit"` — that tier served it.
   - a real strategy name (`"linux-sandbox"`, `"worker"`, `"local"`) with
     `cache_hit: false` — a genuine miss; go to step 5.
   - **the action is absent from the log entirely** — that is a *persistent
     local action-cache hit*, which by `spawn.proto`'s own contract is never
     logged. Absence is the positive signal, not a broken log.
   The top-level `digest` field is present **only** when a disk or remote cache
   is configured; without one, its absence says nothing.
5. **Diff two runs to find what busted the key.** Build `execlog:parser` in a
   bazelbuild/bazel source checkout at the same tag as your pin (the parser
   ships in no release archive and needs a local JDK — BZL-CACHE-16):
   ```
   bazel build --execution_log_compact_file=/tmp/a.log //t
   bazel build --execution_log_compact_file=/tmp/b.log //t
   bazel build //src/tools/execlog:parser
   bazel-bin/src/tools/execlog/parser \
     --log_path=/tmp/a.log --log_path=/tmp/b.log \
     --output_path=/tmp/a.txt --output_path=/tmp/b.txt
   diff -u /tmp/a.txt /tmp/b.txt
   ```
   A differing `environment_variables`, `command_args` or input digest names the
   cause. **EMPTY diff reads as "Bazel-side action keys are stable here"** — not
   as "nothing is wrong": it says the divergence is downstream (server eviction,
   auth scope, a different `--remote_instance_name`) or in the *outputs*, which
   this log structurally cannot see (section D, step 4).
6. **Two machines, not two runs.** Same recipe, run after `bazel clean` on each
   machine, diffing the two files (BZL-CACHE-16). The action key does **not**
   depend on the workspace's absolute checkout path — measured on both majors, a
   `--disk_cache` warmed at one path served every action at another — so a
   "different directory" theory needs the specific mechanism from
   [references/cache-miss.md](references/cache-miss.md), not the directory.

Read [references/cache-miss.md](references/cache-miss.md) when you need the
seven-cache discriminator table, the execution-log format traps, the BEP
`ActionCacheStatistics` read, or the list of constructions that really do
destabilise an action key.

## C. Cache or BES outage

1. **Establish that an outage even fails the build.** With no `--remote_executor`
   configured, an unreachable remote cache is **not fatal**: measured on 8.7.0
   and 9.2.0, across all four combinations of `--remote_local_fallback` and
   `--incompatible_remote_local_fallback_for_remote_cache`, a refused cache
   produced `WARNING: Remote Cache: Connection refused`, a full local build and
   **exit 0**. Do not cite either fallback flag as governing this (BZL-CACHE-26).
   `grep -n 'WARNING: Remote Cache' <build log>` — a hit with exit 0 is the
   whole story.
2. **Never key retry or alerting on exit code 39.** `ExitCode.REMOTE_CACHE_EVICTED
   = 39` is registered on both majors, but across ten invocations, two majors and
   five configurations on a cache-only build it **never surfaced to the caller**.
   Match the error text instead (BZL-CACHE-12):
   `grep -nE 'lost inputs|Lost inputs no longer available remotely|Found transient remote cache error|Unexpected lost inputs' <build log>`
   EMPTY = this is not an eviction; look elsewhere. A hit plus **exit 0** is the
   default self-heal (retry budget 5, new invocation ID, local re-execution). A
   hit plus **exit 1** with a generic `Target //… failed to build` is the same
   condition with recovery disabled or exhausted — indistinguishable from a
   compile error by exit code alone.
3. **Do not "fix" it by turning retries off or up.** Leave
   `--experimental_remote_cache_eviction_retries` at its default of `5` on both
   majors. The rewind path has a **hard-coded ceiling** of 20 losses per action
   (`MAX_REPEATED_LOST_INPUTS`, byte-identical 8.7.0 through 9.2.0), which is why
   the failure text reads `lost input too many times (#21)` (BZL-CACHE-12).
4. **Decide the exposure, not the flag.** `--remote_download_outputs` defaults to
   `toplevel` on 8.7.0, 8.8.0 and 9.2.0; `minimal` has the identical eviction
   exposure for intermediates; only `all` removes it, and only because it
   materialises everything locally. Never describe Build without the Bytes as a
   feature to enable (BZL-CACHE-10, BZL-CACHE-11).
5. **BES is a separate endpoint with a shared credential.** `--bes_backend`
   defaults empty and `--bes_upload_mode` to `wait_for_upload_complete` on both
   majors, so an unreachable BES backend *can* hold a lane. BES shares one
   credential and TLS stack with the cache and executor endpoints — a rotation
   that updates one and not the other breaks it silently (BZL-CACHE-33,
   BZL-CI-31).

Read [references/outage-and-eviction.md](references/outage-and-eviction.md) for
the measured exit-code table per configuration, the rewind flags and their
version boundaries, and the two remote flags that were removed at 9.0.0.

## D. Nondeterministic or non-hermetic action

1. **Run section B step 5's execution-log diff first.** It answers "did the
   declared inputs, args or environment change".
2. **Read a differing `environment_variables`** against the version split:
   non-strict mode on 8.7.0 leaks exactly `PATH` and `LD_LIBRARY_PATH` from the
   client (a named allowlist, not a full passthrough); 9.2.0's default is
   byte-identical to `--incompatible_strict_action_env=true` on 8.7.0 (`PATH`
   pinned to `/bin:/usr/bin:/usr/local/bin`, no `LD_LIBRARY_PATH`). `HOME` is
   unset in the action environment on both, in every configuration
   (BZL-HERM-01, BZL-HERM-06).
3. **Read a differing `command_args`** for a literal date, PID, hostname or
   absolute path (BZL-HERM-10, BZL-HERM-11).
4. **An empty log diff is not a determinism result.** The log cannot see output
   non-determinism at all. Run the independent pass (BZL-HERM-30):
   ```
   bazel clean --expunge && bazel build //... && sha256sum bazel-out/<config>/bin/* > /tmp/run1
   bazel clean --expunge && bazel build //... && sha256sum bazel-out/<config>/bin/* > /tmp/run2
   diff /tmp/run1 /tmp/run2
   ```
   EMPTY diff here **plus** an empty log diff is the determinism result. A diff
   here with an empty log diff means the action reads ambient state at run time —
   the sandbox instance slot, wall clock, an unsorted directory listing.
5. **Name the strategy that actually ran, per action.**
   `bazel build --sandbox_debug --subcommands <target> 2>&1 | grep -i sandbox`
   Confirms a sandboxed spawn and its mount lines; EMPTY means nothing sandboxed
   ran. Read the execution log's `runner` field for the per-action strategy,
   `local` included — both are required (BZL-HERM-26). Never commit the flag
   (BZL-HERM-03). On Linux
   with working user namespaces the default sandbox remounts the whole host root
   **read-only** — `/usr/lib`, `/home` and `/etc/hostname` are all visible to the
   action. "Sandboxed" is not "isolated from the host".
6. **If a repository rule is suspected instead**, the execution log will not show
   it — repository rules run in the loading phase. Go to section F.

Read [references/nondeterminism.md](references/nondeterminism.md) for the
hermetic-sandbox mount set, the mount-pair staleness trap (a changed
`--sandbox_add_mount_pair` busts nothing at any cache layer), the network flags
and tags as measured, and the glob failure family.

## E. Flaky test

1. **Detect, do not mask.** `bazel test --runs_per_test=100 --runs_per_test_detects_flakes //the:test`
   is the detection command. `flaky = True` and `--flaky_test_attempts` both stop
   at the first success — a 50%-flaky test "passes" `--flaky_test_attempts=10`
   roughly every time (BZL-TEST-05, BZL-TEST-06). A mixed PASSED/FAILED summary
   confirms the flake. **All 100 green is not a refutation** — it is evidence
   about *this host*; re-run on the lane that reported it.
2. **Compare the two environments before the two runs.** The test contract is
   identical on Bazel 8 and 9 except for one thing: 9.0.0 adds a mandatory
   implicit toolchain requirement on
   `@bazel_tools//tools/test:default_test_toolchain_type` (BZL-TEST-28). Any
   other "Bazel 9 changed tests" claim is fabricated.
3. **Check the cheap structural causes in order** — each has a distinct
   signature, listed with its confirming output in
   [references/flaky-test.md](references/flaky-test.md): a timeout at the
   `size`-derived default; `shard_count` on a runner that never touches
   `TEST_SHARD_STATUS_FILE`; writes outside `$TEST_TMPDIR`; a mutated runfiles
   tree; a `local`/`no-sandbox` tag that removed the isolation the test relied on.
4. **Tag semantics, measured, both majors.** `no-sandbox` forces the `local`
   runner but stays cacheable and hits on rebuild; `local` also marks the action
   uncacheable and re-executes every build; `no-remote-cache` and `no-remote`
   leave the disk cache untouched; `external` is **test-only** — with
   `--cache_test_results=yes` the tagged test re-runs while its untagged sibling
   reports `(cached)` — and has zero effect on a build action. Probing either
   against `--disk_cache` alone reads **blind**, never "confirmed" (BZL-TEST-08).
5. **An empty coverage report is four different bugs.** `DA:` record count in the
   produced `_coverage_report.dat` is the separator, never `bazel coverage`'s exit
   code, which is 0 on an empty report (BZL-TEST-25). The differential is in
   [references/flaky-test.md](references/flaky-test.md).

## F. Repository-rule or module-extension refetch

1. **Enumerate what the rule actually touched.**
   ```
   bazel clean --expunge
   bazel build --experimental_workspace_rules_log_file=/tmp/wsl.log //...
   bazel build src/tools/workspacelog:parser
   bazel-bin/src/tools/workspacelog/parser --log_path=/tmp/wsl.log > /tmp/wsl.txt
   grep -nE '"which"|sha256: ""' /tmp/wsl.txt
   ```
   Any hit names a non-hermetic call (BZL-HERM-16). EMPTY means no host probe and
   no unchecksummed download on this run — it does **not** clear the rule for a
   different host or a different `--repo_env`.
2. **Re-fetch one repo, on the right command for the version.**
   `bazel fetch --repo=@<repo>` works on both majors. `bazel sync --only=<repo>`
   was **deleted at 9.0.0**, and several rulesets' own live documentation still
   prints it (BZL-FLAG-31). `bazel fetch --force --configure` re-runs only rules
   declaring `configure = True` — measured, a non-configure rule's fetched state
   is left byte-identical (BZL-MOD-26).
3. **Check purity before blaming the cache.** A module extension's impl must not
   read `ctx.os`/`getenv` (BZL-MOD-14); an extension that is a pure function of
   its tags returns `extension_metadata(reproducible = True)` (BZL-MOD-15). The
   dynamic check is to re-run `bazel mod deps --lockfile_mode=update` under a
   changed `--repo_env=<VAR>=…` and diff the extension's lock entry: identical
   `bzlTransitiveDigest`, `usagesDigest`, `recordedRepoMappingEntries`,
   `generatedRepoSpecs` and `envVariables` confirms purity. EMPTY diff is the
   pass here — the inverse of most steps.
4. **Know which cache is in play at your version.** `--repo_contents_cache` is
   opt-in (`""`) on 8.7.0 and 8.8.0 and **on by default at 9.2.0**, deriving to
   `{--repository_cache}/contents`. The observed local-cache gate is an explicit
   `repo_metadata(reproducible = True)`; a `getenv()` call does **not** exclude a
   rule from the local cache (BZL-MOD-16).
5. **A lockfile check that "crashed" has three shapes, all exit 37** — an
   existing dependency's locked version moved (internal `IllegalStateException`),
   a brand-new `bazel_dep` (the clean documented message), or an unsupported
   `lockFileVersion`. Gate CI on the exit code, never on the message text
   (BZL-MOD-02).

Read [references/fetch-and-selection.md](references/fetch-and-selection.md) for
the lockfile schema versions per binary, the repository-rule invocation error
text that differs by major, and section G's tool flags.

## G. Target-selection miss

A change broke something the selection tool did not schedule.

1. **Read what the tool actually ran.** `bazel-diff -v generate-hashes …` prints a
   literal `Executing Query:` line. `deps(//...:all-targets)` means `--useCquery`
   is on. Its absence means the default plain-`query` mode, which is **blind to
   any transitive dependency introduced only by executing a repository rule or
   module extension** — pip, npm, Maven, most Bzlmod extensions (BZL-CI-21).
2. **Close the class or state it.** `--useCquery` (Bazel ≥6.2.0) closes it with no
   list to maintain; `--fineGrainedHashExternalRepos` closes it with an allow-list
   nothing verifies. Choosing neither is allowed only if the wrapper says the miss
   class is live and unmitigated (BZL-CI-30).
3. **Never read an empty diff as "nothing changed."** Read it as "no evidence of a
   difference under this tool's blind spots, with these flags"; name the flags
   that were in effect (BZL-CI-25). `target-determinator`'s
   `-filter-incompatible-targets` defaults true and silently drops targets, and
   `-before-query-error-behavior` defaults to `ignore-and-build-all`.
4. **Confirm the backstop exists.** `grep -rn --include=*.yml 'schedule:\|cron' .github/workflows`
   for an unrestricted `bazel test //...`. EMPTY on a repo that already uses
   target selection is itself the finding (BZL-CI-27) — both miss classes are
   silent by construction, so a periodic full run is the only check that does not
   depend on the tool being right.

## H. A flag that "does not exist"

1. `bazel help <command> --long | grep -- '--<flag>'` **and**
   `bazel help startup_options | grep -- '--<flag>'`, on the pinned binary.
   A flag can live in only one surface: `--experimental_remote_repo_contents_cache`
   is a real **startup** option from 8.8.0 and is invisible to `help build --long`.
2. **EMPTY on both surfaces means "not rendered", never "never existed."** An
   option marked `UNDOCUMENTED` renders in neither — `--rewind_lost_inputs` is
   present and functional on 8.7.0 while rendering nowhere until 9.2.0. The
   escalation is to read the `@Option` annotation in the tagged source, then to
   run the flag once and read whether Bazel says `unrecognized option`
   (BZL-FLAG-11, BZL-CACHE-23).
3. **Check the 8→9 removals before rewriting anything.**
   `--experimental_remote_merkle_tree_cache` and
   `--incompatible_remote_use_new_exit_code_for_lost_inputs` are present and
   default-active on 8.7.0 and 8.8.0 and **removed at 9.0.0** — so one rc line
   carrying either is dead configuration on the 8.x leg and a hard failure on the
   9.x leg of the same matrix. `--enable_workspace` and `--enable_bzlmod` are
   gone from every help surface at 9.2.0; neither is a no-op there (BZL-FLAG-13).
4. **A load error is not a flag problem.** On 9.2.0 a bare `py_library`,
   `sh_binary` or `proto_library` fails with `name 'X' is not defined` plus a
   *wrong* "did you mean" suggestion, while `cc_library` alone hits a purpose-built
   stub naming `buildifier --lint=fix`. The fix is the `bazel_dep` plus an explicit
   `load()`, never a non-empty `--incompatible_autoload_externally`
   (BZL-LARK-10, BZL-FLAG-14, BZL-FLAG-16).
5. **When the flag name is right but nothing happens**, check the loading phase:
   `bazel query //...` (exit 7) and `bazel build --nobuild //...` (exit 1) catch
   undefined names, bad `load()` targets and bad builtin arguments that
   `buildifier` is structurally blind to (BZL-LARK-30).

## The receipt

Write four blocks, in this order. Anything else is padding.

1. **Symptom** — one line, in the reporter's words.
2. **Root cause** — the mechanism, with the Bazel version it holds on and the
   host class it was measured on.
3. **Evidence** — the command and its verbatim output, pasted. Two readings when
   a fix was applied: before and after.
4. **Fix, or gap** — the change that made the symptom go away and the rule ID it
   satisfies, or the named gap with the surface that would settle it.

Name every rule ID you relied on. A diagnosis citing none either found something
genuinely new — say so — or skipped the standard that already covered it.

## What agents get wrong

Ranked by how often the measurements caught it.

1. **Citing `bazel analyze-profile` on a Bazel 9 pin.** It is in five years of
   posts and in Bazel's own frozen 9.1.0 docs snapshot; it was deleted from the
   source at 9.0.0.
2. **Writing a CI retry that keys on exit code 39.** Measured, the eviction
   condition self-heals to exit 0 or fails generically at exit 1. Match the error
   text.
3. **Using `--explain` to answer a cache question.** It reported "action is new"
   for confirmed disk-cache hits.
4. **Reading an empty execution-log diff as proof of determinism.** The log cannot
   see output bytes at all.
5. **Reading an action's absence from the execution log as a broken log.** It is
   the signature of a local action-cache hit.
6. **Claiming a downed cache fails a cache-only build.** It is a WARNING and a
   green local build on both majors.
7. **Concluding a flag is gone because one help surface had no hit** — or because
   `bazel help all --long` printed an error.
8. **Answering "the disk is filling up" with "the remote cache."** Name which of
   the seven caches, then `du -sh` that directory.
9. **Recommending `--flaky_test_attempts` to investigate a flake.** It manufactures
   a green.
10. **Assuming a selection tool's default mode is `cquery`-based.** `bazel-diff`
    defaults to plain `query`; `--useCquery` is opt-in.
11. **Blaming the workspace's absolute path for a cross-machine miss.** The action
    key does not depend on it; a build-flag value that *carries* the path does.
12. **Fixing the check.** See [Never edit the check](#never-edit-the-check) — this
    is the one failure that leaves the build worse than when the diagnosis started.

## References

Read one level down, on demand. These files do not link each other.

| File | Read it when |
|---|---|
| [references/slow-build.md](references/slow-build.md) | Section A: the profile is open and you need the per-culprit signatures, the memory-tracking startup flags, the `dump --skyframe` enum rename, or the analysis-cache discard traps |
| [references/cache-miss.md](references/cache-miss.md) | Section B: the seven-cache discriminator table, execution-log formats and their tooling traps, the BEP `ActionCacheStatistics` read, and what really destabilises an action key |
| [references/outage-and-eviction.md](references/outage-and-eviction.md) | Section C: the measured exit-code table per configuration, rewind and retry flags with their version boundaries, download modes, and the flags removed at 9.0.0 |
| [references/nondeterminism.md](references/nondeterminism.md) | Section D: sandbox strategies and what each isolates, the hermetic-sandbox mount set, the mount-pair staleness trap, network flags and tags, and the glob family |
| [references/flaky-test.md](references/flaky-test.md) | Section E: detection versus masking, the sizing and sharding contract, environment invariants, and the empty-coverage differential |
| [references/fetch-and-selection.md](references/fetch-and-selection.md) | Sections F and G: repo-rule and extension diagnosis, repo-contents-cache and lockfile behaviour per version, and the two selection tools' flag surfaces and blind spots |
