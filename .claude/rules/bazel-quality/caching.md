---
title: Caching and Remote Execution
summary: The BZL-CACHE family — cache flags and their version boundaries, the cache trust boundary, what enters an action key, eviction and outage behaviour, and the execution log as a cache-miss instrument
---

# Caching and Remote Execution

Owns `BZL-CACHE`: the disk and remote cache, remote execution readiness, Build
without the Bytes, eviction and rewinding, cache-outage behaviour, credentials
and credential helpers, and the execution log used as a cache-miss instrument.
It does not own test-caching semantics or the `no-remote-cache-upload` /
`external` tag rows — those are `BZL-TEST-08`, measured at the wire level there
and never re-derived here. CI lane and job design is `BZL-CI`'s, including
`--bes_upload_mode` and every BEP field a dashboard charts. Repository-rule and
sandbox hermeticity mechanics are `BZL-HERM`'s: two rules below gate on
`BZL-HERM-10`'s result rather than restating it. The general rc-file form of
this file's flag discipline is `BZL-FLAG-11`.

Contents: [The Flag Surface](#the-flag-surface) ·
[Credentials and the Trust Boundary](#credentials-and-the-trust-boundary) ·
[What Enters the Action Key](#what-enters-the-action-key) ·
[Outage, Eviction and Exit Codes](#outage-eviction-and-exit-codes) ·
[Before Remote Execution](#before-remote-execution) ·
[Download Mode and Cache Growth](#download-mode-and-cache-growth) ·
[Standing Up or Migrating a Cache](#standing-up-or-migrating-a-cache) ·
[Diagnosing a Miss](#diagnosing-a-miss) · [Gaps](#gaps) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Every version-bound claim below was measured 2026-09-06 against real Bazel
8.7.0, 8.8.0 and 9.2.0 binaries on a Linux host under `linux-sandbox`; a claim
that could read differently on darwin or Windows says so in its row or in
[Gaps](#gaps). No rule here is ruleset-versioned — rules_js 3.4.1,
rules_python 2.3.3, rules_rust 0.74.0 and rules_cc 0.2.22 change nothing in
this file. Re-check a flag on **both** help surfaces — `bazel help <command>
--long` **and** `bazel help startup_options` — at your own pinned version
before citing it: a flag can live in either, and the fetched CLI reference
tracks only the newest release. Every `grep` here reads the source tree; BUILD
and `.bzl` text a repository rule generates under the output base is out of its
reach, so reach that text with `bazel query --output=build` instead.

## The Flag Surface

The flag-existence check itself — both help surfaces, run per version in your
matrix and not the dev pin, plus a `bazel build --nobuild --<flag>`
unrecognized-option probe for the flag that shows on neither — is
`BZL-FLAG-11`'s; it is not re-derived here. The cache-specific instance:
`--rewind_lost_inputs` is exactly that `UNDOCUMENTED` case, rendering nothing
on either help surface while parsing fine, from 8.7.0 through 9.1.0 — a
zero-hit grep on it is not the stop signal, so probe it or read its `@Option`
block at the tag.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-23 | Re-derive a remote-cache flag's name, default and version boundary from your own pinned binary before writing it into an rc file, a rule or a review comment — never from memory, a blog post or a prior note — and confirm a cited PR by locating its commit in the tagged tree, not by its merge state. | This corpus made the error three times, all toward "the flag is gone". `--experimental_remote_merkle_tree_cache` and `--incompatible_remote_use_new_exit_code_for_lost_inputs` are present and default-active on 8.7.0 and 8.8.0 and removed only at 9.0.0, so an rc line carrying either is dead configuration on 8.x and a hard `unrecognized option` failure on the 9.x leg of the same matrix — a version-split failure, not a uniform one. On `bazelbuild/bazel` a `CLOSED` PR with `mergedAt: null` is the normal shape of a community PR superseded by the vendor's internal sync: one such change shipped anyway as a named commit, another never landed at all. | The two help greps above, per version. Then for a citation: `gh pr view <n> -R bazelbuild/bazel --json state,mergedAt` **first**, then `git log --oneline <old-tag>..<new-tag> -- <the option's own .java file>`. Unmerged **and** no commit demotes the claim to a proposal; a named commit means cite the commit. Empty grep output on both surfaces = the flag is absent on that version, which is the finding whenever you were about to cite it. Release notes are not a complete index: 9.0.0's removed-flags appendix omits one of those two removals. | MUST |
| BZL-CACHE-10 | Never write that Build without the Bytes needs enabling, and never add `--remote_download_minimal` "to turn BwoB on". | `--remote_download_outputs` has defaulted to `toplevel` since Bazel 7 (2023-12), measured `default: "toplevel"` on 8.7.0, 8.8.0 and 9.2.0, and the three alias flags each expand to that one flag. Guidance written against the pre-7 `all` default tells an adopter to add a flag that changes nothing, and buries the real topic, which is eviction. | `bazel help build --long 2>&1 \| grep -A2 remote_download_outputs` against the pin, and read the printed default. Empty output means a Bazel older than 0.25 — a much larger problem than stale guidance, not a pass. | MUST |
| BZL-CACHE-22 | Do not set `Action.salt` from a flag or a rule attribute — no such knob exists. Document a full cache miss immediately following a `no-remote-exec` / `no-remote` / `no-remote-cache` tag edit rather than "fixing" it. | `RemoteExecutionService.buildSalt()` derives salt entirely from the spawn's remote-executability bit (driven by those tags), the workspace name and an optional scrub config. A tag edit therefore moves every one of that target's actions into a different cache namespace, so the miss is expected behaviour, not a regression. Guidance offering a `salt=` parameter is fabricated. | `grep -c salt` over the CLI reference plus the `ctx.actions.run` / `run_shell` signatures in the Rules API. Empty on both **confirms the absence** — it is not a gap to fill. For a suspicious miss, correlate with a recent tag change through `git blame` or `bazel query --output=build <target>`. | MUST |
| BZL-CACHE-31 | Never write `--remote_symlink_absolute_path_strategy` or any client-side variant. State the observable behaviour instead: an action whose output is an absolute symlink fails at **upload** time, after local execution already succeeded, against any server that does not explicitly advertise `ALLOWED`. | The strategy is a server-declared REAPI capability with no client control surface — zero hits in the CLI reference and in `bazel help build --long` on 8.7.0, 8.8.0 and 9.2.0. `UploadManifest.checkAbsoluteSymlinkAllowed` treats `DISALLOWED` and the unset `UNKNOWN` zero-value identically, then throws `IOException: Spawn output … is an absolute symbolic link to …, which is not allowed by the remote cache`. Nothing on the client overrides it, and the failure lands on the caching step, not on scheduling or download. | `bazel help build --long 2>&1 \| grep -i symlink` against the pin — zero hits is the stop signal, never an invitation to guess a name. To diagnose the real failure, `grep 'is not allowed by the remote cache' <build log>`: a hit names the action and its absolute-symlink output, and the fix is the output's shape or the server, never a flag. Empty log = this failure is not in play. | MUST |

## Credentials and the Trust Boundary

One pass over the tracked rc files and the workflow tree catches every row:
`git ls-files | grep -E '\.bazelrc|^\.github/' | xargs -r grep -n -e credential_helper -e remote_header -e bes_backend -e remote_upload_local_results` — empty output, including when `xargs -r` receives no path at all, is the pass for this sweep.
The grep does not answer the ownership question that -02 asks — which actors
can hold the write credential — so read the result, then enumerate.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-04 | Configure `--credential_helper` only from an rc file that does not ship with the repository — never a committed `.bazelrc`, and never a `%workspace%`-relative helper path. | Bazel resolves and spawns the helper with the full client environment, before the sandbox, with no check on where the flag came from. A read-only `bazel query //...` on a fresh clone is enough to execute it — reproduced on 9.2.0 and closed by the vendor's own OSS vulnerability panel as intended behaviour ([#30439](https://github.com/bazelbuild/bazel/issues/30439)). This is a permanent property, not a bug awaiting a patch, so a naive token migration trades a leaked secret for arbitrary code execution on clone. | `git ls-files \| grep -E '\.bazelrc' \| xargs -r grep -n credential_helper` — any match in a **tracked** file is the finding. Empty output — whether `xargs -r` found no `.bazelrc` file at all or found one with no match — is the pass. | MUST |
| BZL-CACHE-01 | Every CI lane an untrusted contributor can trigger — any pull request, fork or same-repo — runs the cache read-only, with the write credential **absent from that lane's environment**, not merely unused. | A write-capable untrusted lane can plant a backdoored tool that a later trusted build downloads and executes instead of compiling ([#4276](https://github.com/bazelbuild/bazel/issues/4276), independently reproduced on the project's own discussion list). "The job does not upload" is not the control; an event untrusted contributors cannot trigger is. The portable shape is a composite action that appends the authorization header only when its secret input is non-empty and appends `--remote_upload_local_results=false` otherwise, with the workflow supplying that input only on a trusted event. | Confirm the write credential is gated on such an event (for example `github.event_name == 'push'`), and that `--remote_upload_local_results=false` is in the effective flags of every other path. Empty output from `grep -n remote_upload_local_results=false` scoped to the untrusted lane **is the finding** — that lane can write. Measured against a real HTTP endpoint: that flag produced zero `PUT` lines, and `--remote_accept_cached=false` produced zero `GET /ac/` lines while still uploading. | MUST |
| BZL-CACHE-02 | Keep write access to the Action Cache strictly narrower than read access — never symmetric, and never held by a developer machine's default configuration. | Read being public is not the vulnerability; write being as available as read is. The mitigation a Bazel maintainer names as actually working is that only the execution system writes the AC entry, after the action succeeded. A developer rc file holding the write token is the second un-gated actor that a CI-only review never sees. | Named reading heuristic — enumerate every actor that could plausibly hold the write credential: each CI role, the documented developer setup, any alternate path. More than one un-gated actor is the finding. An **empty enumeration means you have not looked**, not that you passed. | MUST |
| BZL-CACHE-03 | Carry a cache-write credential through `--credential_helper`, never a bare `--remote_header=authorization=…` or any static bearer token in an rc file, gitignored or not. **pinned** — a new setup takes the helper from its first line; an existing setup on a bare token takes a tracked migration item instead of an immediate rewrite. | A static token is long-lived, unscoped, readable by anything that reads the file, and visible in process argv. `--credential_helper` has been stable since Bazel 7.0 and is present on 8.7.0, 8.8.0 and 9.2.0. Migration costs a live secret rotation, which is the whole reason the existing-setup half does not block. | `grep -n remote_header *.bazelrc*` — a match on a **new** setup is the finding; on an existing one, the finding is the *absence* of a tracked migration item in the repository's own docs or tracker. Empty output is the pass. Run it in the same pass as -33's `bes_backend` term. | MUST (new) / SHOULD (existing) |
| BZL-CACHE-33 | Treat `--bes_backend`'s credential and TLS configuration as one trust surface with `--remote_cache` / `--remote_executor`: any credential rotation rotates both, and the rotation runbook names both. | Bazel's own BEP documentation states that the Build Event Service and the remote-execution endpoints "need to share the same authentication and TLS infrastructure". Nothing in the client distinguishes a BES credential from a cache credential at resolution time — both route through the same `--credential_helper` and the same gRPC stack — so rotating one leaves the other looking fine until it silently stops uploading. | The section grep, reading the `bes_backend` term. Where `bes_backend` and either credential mechanism both appear, a runbook or an adjacent comment must name both; its absence is the finding. Empty on `bes_backend` = the rule does not bind (no BES configured) — pass by absence, and it starts binding the day a results dashboard is stood up against the same credential. | MUST where `--bes_backend` is configured |

```bash
# wrong — tracked file; a plain `bazel query //...` on a clone runs the helper
# .bazelrc
build --credential_helper=%workspace%/tools/cache-auth.sh
```

```bash
# right — untracked, outside the workspace, invoked per host pattern
# ~/.bazelrc
build --credential_helper=cache.example.com=/opt/local/bin/cache-auth
```

## What Enters the Action Key

The check for this block is a read, not a command: for every action, name where
its tool, its environment and its stamped values come from. Two commands
support it. `grep -rn -e 'ctx.actions.run' -e 'ctx.actions.run_shell' --include='*.bzl' .`
enumerates the sites — empty output in a tree that has `.bzl` files means you
searched the wrong directory, not that there is nothing to read. And
`bazel clean --expunge; bazel build //...; sha256sum bazel-out/<config>/bin/*`,
run twice and diffed, is the **only** check that sees output non-determinism;
the execution log cannot (BZL-CACHE-16).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-18 | Never assume a host-resolved tool — a compiler, interpreter or linker invoked without a `File` from a declared toolchain or dependency — is covered by the cache key. | Bazel's own documentation states it "does not track tools outside a workspace": swapping the host toolchain serves stale cached output instead of producing a miss. Confirmed by contrast — actions whose `tools=` traced to a declared `File` had keys exactly as stable as pure genrules across two checkouts at different absolute paths, so it is specifically the undeclared, PATH-resolved executable that escapes the key. Under REAPI ≥ v2.3 the class deepens: `arguments[0]` may be PATH-resolved, so two workers can run two different binaries under an identical Action digest. | The section grep, then read each `executable=`: a bare string rather than a `File` from `ctx.executable.*` or a toolchain is the finding. An empty finding list — every executable traces to a declared `File` — is the pass. | MUST |
| BZL-CACHE-19 | Only environment variables declared through `--action_env` enter an action's digest; never add `--noincompatible_strict_action_env` to fix environment-related build flakiness, and fix it with a named `--action_env=SPECIFIC_VAR` instead. | The default is version-split (`BZL-FLAG-21` carries the measured per-version values). On an 8.x pin the negation looks like a no-op while silently pinning the permissive behaviour across a 9.x bump; on 9.x it reintroduces the ambient, cache-key-invisible dependency class the flip exists to prevent. Non-strict mode is a named allowlist (`PATH`, `LD_LIBRARY_PATH`), not a full client-environment passthrough — an arbitrary exported variable did not reach the action even on permissive 8.7.0, and `HOME` is unset on both majors. | `grep -rn 'incompatible_strict_action_env' *.bazelrc*` — any `--no…` match is the finding. Empty output is a pass on this rule, but on a matrix spanning both majors it means the two legs run **different** action environments with nothing announcing the split; route that to `BZL-HERM-01`. Re-read the default from the pinned binary per BZL-CACHE-23; do not carry it from this table. | MUST |
| BZL-CACHE-20 | Never put a value that changes on every build — a raw timestamp, an unpinned `git describe` with a dirty flag, a random build ID — behind a `STABLE_`-prefixed workspace-status key. | `bazel-out/stable-status.txt` is **not** exempt from action invalidation, while `bazel-out/volatile-status.txt` is; Bazel "pretends that the volatile file never changes". An unstable "stable" key therefore busts the cache on every invocation while its name advertises the opposite. In Starlark the pairing is inverted from the intuitive reading: `ctx.info_file` is the **stable** file, `ctx.version_file` the volatile one. | Run the `--workspace_status_command` script twice in immediate succession and diff its `STABLE_`-prefixed output lines. An empty diff is the pass — every stable key is actually stable; any differing `STABLE_` line is the defect. A sibling "looked stable, was not" mechanism with a different cause — the `linux-sandbox` instance slot number is not stable across invocations, so any action reading `$PWD` differs run to run — belongs to BZL-CACHE-34 and `BZL-HERM-10`, not here. | MUST |
| BZL-CACHE-21 | Code that hand-builds REAPI protos — a cache proxy, an RBE shim, a cache-warming script — sorts `Command.environment_variables` and `Command.output_paths` lexicographically, sorts a `Directory`'s files, directories and symlinks **each independently**, and sorts `Platform.properties` by name **then value**. | The spec makes all of these a MUST, and an unsorted repeated field is self-inflicted cache-key instability: two functionally identical inputs hash to two digests and every downstream consumer sees a false miss. A name-only comparator on `Platform.properties` looks sorted and is non-compliant the moment two properties share a name. Bazel's own client already does this correctly, so the rule binds only on hand-written REAPI code. | Read the tool's serialisation code for an explicit `sorted(...)` / `.sort()` immediately before the `SerializeToString` or hash call, and check the comparator carries a value tiebreaker. Absence of a sort ahead of the digest computation is the defect. Empty — no hand-built protos anywhere in the repository — means the rule does not bind, which is a pass. | MUST |
| BZL-CACHE-34 | Never treat a shared cache as checkout-scoped. An action key is independent of the workspace's absolute path, so a hit serves another checkout's — or another machine's — bytes verbatim, and any action that reads ambient run-time state ships that state to every consumer of the cache. | Measured on 8.7.0 and 9.2.0: a `--disk_cache` warmed at one absolute path served every genrule action at a second checkout under a different path, with identical compact-log `digest` fields, and the second checkout's output literally contained the **first** checkout's `output_base` hash. `$(location)` and `$(execpath)` resolve to exec-root-relative strings at analysis time, so the key carries no absolute path; divergence enters only through an action reading `$PWD`, `date` or an unsorted directory listing at run time. The consequence is not a slow build but a cached artifact carrying one machine's state into every other machine's outputs, invisible in the action key. | This rule gates on `BZL-HERM-10`'s check (no absolute path or host state in a declared output) rather than re-deriving it: a clean report there is the pass, and an empty finding list there passes this rule too. To reproduce the exposure, warm a `--disk_cache` from one checkout and build a byte-identical copy at a different absolute path against the same cache; every action reported as a disk-cache hit is now shared across checkouts. A **non**-empty finding at `BZL-HERM-10` is more urgent once a shared cache exists, because the cache is the distribution mechanism. Measured for genrule-shaped actions with native substitution on Linux; a custom action that shells `pwd` into a tool *argument* was not tested. | MUST |

## Outage, Eviction and Exit Codes

One grep over the rc files and the CI harness covers the block:
`grep -rn -e experimental_remote_cache_eviction_retries -e rewind_lost_inputs -e remote_local_fallback -e experimental_remote_cache_ttl *.bazelrc* .github/`.
Empty output is the pass for every row here except -13's tuning half. Exit
codes are not the signal: measured on 8.7.0 and 9.2.0, the eviction condition
surfaces as exit **0** (self-healed) or a generic exit **1**, and an
unreachable cache is a WARNING plus a green local build. Match the log text.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-26 | Do not cite `--incompatible_remote_local_fallback_for_remote_cache` or `--remote_local_fallback` as governing what happens when a cache-only deployment's cache goes down — neither has any effect there. If a lane must **fail** on a cache outage rather than silently build locally, implement that outside Bazel and say so; if a silent local build is acceptable, state that too. | Measured on 8.7.0 and 9.2.0 with the cache endpoint refusing connections: all four combinations of the two fallback flags produced the identical outcome — `WARNING: Remote Cache: Connection refused`, every action executed locally, exit 0. The mechanism is structural: with no `--remote_executor` there is nothing to fall back *from*, so cache connectivity is an optional accelerant whose loss is inherently non-fatal. No Bazel flag converts an outage into a failure on this shape. | Two checks. (a) `grep -rn -e incompatible_remote_local_fallback_for_remote_cache -e remote_local_fallback *.bazelrc* .github/` on a repository with `--remote_cache` and no `--remote_executor` — any **hit** is the finding (dead configuration presented as an outage policy); empty is the pass on this half. (b) Empty output from `grep -rn 'Remote Cache' <CI wrapper or log-gating logic>` **plus** no written statement anywhere means the outage behaviour is undecided by omission, which is the SHOULD-level finding. A lane that must fail needs a pre-flight reachability probe or a wrapper matching `WARNING: Remote Cache:` / `errors during bulk transfer`. Measured against an HTTP cache on Linux, cache-only shape; a downed cache alongside a live `--remote_executor` was not measured. | MUST (do not cite the fallback flags) / SHOULD (state the lane's outage policy) |
| BZL-CACHE-12 | Never key CI retry or alerting logic on exit code **39**, leave `--experimental_remote_cache_eviction_retries` at its default of 5, and never set it to `0` believing `--rewind_lost_inputs` covers the gap. Recognise a lost evicted input by its **error text**, not its exit code. | Measured on 8.7.0 and 9.2.0 across five configurations: the eviction condition fires reliably — a `toplevel` (or `minimal`) cache hit leaves intermediates as CAS references and a later locally-executing action needs an evicted blob — but the caller sees exit 0 (Bazel retries the whole build under a fresh invocation ID and re-executes locally) or, at `retries=0`, a generic exit 1 indistinguishable from a compile error. Bare 39 never reached the caller in ten invocations. Rewinding does not rescue `retries=0`: it engages genuinely, then exhausts a hard-coded `MAX_REPEATED_LOST_INPUTS = 20` (byte-identical 8.7.0 through 9.2.0; the 21st loss trips it, hence the `#21` in the message) and still exits 1. | Three checks. (a) `grep -rn '\b39\b' <CI retry or exit-code logic>` — a **hit** treating 39 as the eviction signal is the finding; the durable signals are `lost inputs with digests:`, `Found transient remote cache error, retrying the build...`, `Lost inputs no longer available remotely:` and `lost input too many times (#21)`. (b) The section grep on `…eviction_retries` — a hit with value `0` and no comment is the finding; empty is the pass (default 5 in force on all three versions). (c) `rewind_lost_inputs` present **alongside** `retries=0` is the finding. Do not verify that flag's availability with `bazel help build --long`: it is `UNDOCUMENTED` through 9.1.0 and renders zero hits while a real 8.7.0 binary accepts it. Measured on Linux under `linux-sandbox` against an HTTP cache with no `--remote_executor`. | SHOULD |
| BZL-CACHE-13 | Set `--experimental_remote_cache_ttl` to at most the deployed server's real advertised minimum blob TTL where that number is knowable, and never to a large static value such as `10000d` on a pin at or above 8.2.0 / 7.6.0. | Leaving the measured `3h` default when the server's real eviction window is shorter guarantees intermittent lost-input builds. The large-static workaround targeted a TTL-trust bug in long-lived JVMs that the vendor's own dated rc comment marks "Not needed after Bazel 8.2.0"; on a current pin it instead makes Bazel trust a TTL an LRU-evicting server does not honour, producing the same failure from the other direction. The PR cited elsewhere as upstream proof is closed unmerged — the vendor's dated account is the evidence for the floor. | `grep -n experimental_remote_cache_ttl *.bazelrc*`: a value at or above roughly `30d` with `.bazelversion` ≥ 8.2.0 / 7.6.0 is the finding. For the tuning half there is no command — read the cache server's own eviction or LRU configuration beside the flag. Empty output (unset, `3h` default measured on 8.7.0, 8.8.0 and 9.2.0) is the pass **only** where the server's real TTL is unknown; where it is known and shorter than 3h, empty is the finding. | MUST (the large-static prohibition) / SHOULD (the tuning) |

```bash
# wrong — 39 never reaches the caller on a cache-only build; the wrapper never fires
bazel build //... || { [ $? -eq 39 ] && bazel build //...; }
```

```bash
# right — the condition is text, and it may already have self-healed at exit 0
bazel build //... 2>&1 | tee build.log
grep -q -e 'lost inputs with digests:' -e 'Found transient remote cache error' build.log
```

## Before Remote Execution

Run the readiness gate in order and stop at the first failure, **before** any
`--remote_executor` flag reaches an rc file. Steps 1 and 5 are commands; steps
2 to 4 are reads. An empty finding list on every step is the pass; a red step 5
is fixed before remote execution, not after.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-06 | Before proposing `--remote_executor` for any repository, check whether any generated launcher, wrapper or action output embeds a hardcoded absolute path rooted outside the workspace — a package-manager store, a home directory, a machine-specific install prefix. | A remote worker never ran the provisioning step that populated that path, so the action either cannot find it or picks up another tenant's content. No flag closes this; the fix is architectural, and a workspace whose outputs reference their build-time location on the building machine fails the gate by construction. | First filter: `grep -rn -e '"/home/' -e '"/Users/' -e '\$HOME' -e '/opt/' <generator .bzl files>`. Then read every `.bzl` site that emits launcher or wrapper *content*, because string concatenation defeats the grep. An **empty grep is inconclusive, not a pass** — the manual read is mandatory. | MUST |
| BZL-CACHE-07 | Run the remaining gate steps in order: (2) every tool resolves through a declared toolchain or a `File` input, never the invoking shell's `PATH` or `JAVA_HOME`; (3) no repository rule or action reachable from a remotely-executed target creates files outside the Bazel-managed tree or sets persistent environment variables; (4) no checked-in build-tool binary is used unconditionally across execution platforms; (5) a sandbox-clean build passes. | All four are stated by upstream as correctness requirements, not tuning advice: a violation does not produce a slower remote build, it produces one that fails on a worker lacking the ambient state the local machine supplied. Sandboxing mimics remote execution, which makes step 5 the cheapest necessary — never sufficient — precondition. | Step 2: read every `ctx.actions.run` / `run_shell` `executable=`; a bare string literal not sourced from an attr or a registered toolchain is the finding. Step 3: gate on `BZL-HERM`'s non-hermetic-operation check rather than re-deriving it — a clean report there passes this step. Step 4: confirm a platform constraint or toolchain selects the matching binary per platform. Step 5: `bazel test //... --spawn_strategy=sandboxed` all green. Empty finding lists on steps 2 and 4 are the pass. | MUST |
| BZL-CACHE-08 | Never enable dynamic execution (`--dynamic_local_strategy` / `--dynamic_remote_strategy`) against a deployment that configures only `--remote_cache` with no `--remote_executor`. | Upstream states that a cache miss under a cache-only backend "would be considered a failed action" — the remote branch of the race has nothing to race against. This is structural, not an implementation gap, so it never becomes a free win on top of an existing cache. | `grep -n -e dynamic_local_strategy -e dynamic_remote_strategy *.bazelrc*` non-empty **and** `grep -n remote_executor *.bazelrc*` empty is the finding. Empty output on the first grep is the pass. | MUST |

## Download Mode and Cache Growth

Two commands cover the block: `grep -rn remote_download *.bazelrc* .github/`
for the mode, and
`du -sh $(bazel info repository_cache) $(bazel info output_base) <configured --disk_cache path>`
for growth. Equal or small `du` sizes mean the symptom is not a cache-capacity
problem — a routing answer, not a pass.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-11 | Set the download mode per role, explicitly: a CI runner that never consumes build artifacts locally gets `--remote_download_minimal`; an interactive developer machine keeps the `toplevel` default; an IDE-feeding build gets `minimal` plus `--remote_download_regex` naming the consumed paths — never a blanket `--remote_download_all`. **pinned** — the per-role table is a default an adopter overrides once, in their own rc file. | Riding the major's default downloads every top-level target's outputs onto a runner that reads none of them, and regressing to `all` to serve one IDE file reintroduces the exact bandwidth cost BwoB exists to avoid. Measured: under `toplevel`, three genrule actions all hit the remote AC and exactly one CAS blob was fetched, the two intermediates never materialising; under `minimal`, not even the top-level output materialises. | `grep -rn remote_download *.bazelrc* .github/`. Empty output **is the finding** whenever `--remote_cache` is configured at all — every role then inherits one undifferentiated default. A hit on `_all` with no comment justifying full-download intent is also a finding. Do not choose the mode for eviction safety: `minimal` and `toplevel` were measured to bite identically on an evicted intermediate (BZL-CACHE-12), and only `all` avoids it, incidentally. | SHOULD |
| BZL-CACHE-17 | A `--disk_cache` set on Bazel ≥ 7.4 without `--experimental_disk_cache_gc_max_size` or `_max_age` is unbounded growth; below 7.4 treat `--disk_cache` as having no automated GC at all. The classic `--repository_cache` has no automated pruning on any version; the separate repo contents cache does, age-only. | Both disk-cache GC flags default to `"0"` (unbounded) even on a version that supports them — the feature is opt-in, not automatic-by-version (measured `0` on 8.7.0, 8.8.0 and 9.2.0). The repo contents cache is a different feature with real shipped GC (`--repo_contents_cache_gc_max_age` `14d`, `--repo_contents_cache_gc_idle_delay` `5m`, measured present on all three) and **no** size cap — an asymmetry from `--disk_cache`, which has both. Repository-cache GC is an upstream issue open with zero comments since 2024-05-23 ([#22516](https://github.com/bazelbuild/bazel/issues/22516)). | `grep -n disk_cache *.bazelrc*` — `--disk_cache` present with neither GC flag nearby is the finding; on a pin below 7.4, any doc or comment claiming Bazel prunes it automatically is the finding. Empty output (no disk cache configured) is a pass **by absence**, not by configuration. Name which of the two repository-shaped caches your pin uses before asserting "no GC". | SHOULD |
| BZL-CACHE-32 | Prune a long-lived CI runner's classic `--repository_cache` with `find <cache>/repos/v1/ -type f -mtime +N -delete`, never with `-atime`, and never in the expectation that Bazel will do it. | Upstream states the classic repository cache "is never cleaned up automatically" and that "upon each cache hit, the **modification** time of the file in the cache is updated" — so mtime is the signal Bazel actually maintains, while atime depends on a mount option (`relatime` / `noatime`) most CI images do not guarantee. There is no dated upstream commitment to close the gap. | Read any pruning script beside the cache: an `-atime` predicate is the finding, as is a comment claiming Bazel prunes this cache. `du -sh $(bazel info repository_cache)` on a runner that has been up for days sizes the exposure. Empty (ephemeral runners recycled per job, so the cache never accumulates) is a pass **by architecture** — say so, because it stops being true the day a persistent self-hosted runner appears. Measured on Linux; the mount-option caveat is what varies elsewhere. | SHOULD |

## Standing Up or Migrating a Cache

The shared check is not a Bazel command: read the deployed server's own
capabilities handler **at source**, or query its Capabilities endpoint with a
REAPI-aware client. None of the five OSS servers surveyed exposes a
configuration knob for either capability field, and Bazel's documentation
describes the client, not the server. A server that does not implement the
endpoint reads as **unknown**, never as a default.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-24 | Before trusting any hermeticity or portability claim about a cache backend, read what its `GetCapabilities` response actually advertises — `symlink_absolute_path_strategy` and `digest_functions`. Treat both as fixed properties of that server binary and build, never as operator settings, and treat a mismatch across a backend migration as a silent 100 %-cold-cache event. | Both fields are hardcoded in every one of five OSS servers read at source, and the ecosystem is split: two ship permissive (`ALLOWED`), two ship `DISALLOWED`, and one sets neither field, which resolves client-side to the same "not allowed" outcome. Bazel's own reference worker also picks `DISALLOWED`. `ALLOWED` permits non-hermetic builds by spec, and a digest-function mismatch between two backends produces two disjoint namespaces rather than an error. | Read the deployed server's capabilities-handler source, or query the running server's Capabilities endpoint. After a backend migration, a 100 %-cold build rather than partial degradation points at `digest_functions`, not at client misconfiguration. Empty — the endpoint is unimplemented or the vendor publishes neither value — records as **unknown**, never as `DISALLOWED` and never as a pass. | MUST for anyone standing up, adopting or migrating a remote cache |
| BZL-CACHE-28 | Do not enable `--experimental_remote_cache_chunking` by default; enable it only against a backend whose `GetCapabilities` actually advertises `SplitBlob` / `SpliceBlob`. Never write `--experimental_remote_cache_chunking_function` or any `rep_max_cdc` selector — no such flag has ever shipped. | The base flag is a plain boolean, correctly still experimental, measured `default: "false"` with byte-identical help text on 8.7.0, 8.8.0 **and** 9.2.0, so it predates the measured range and 8.7.0 is merely the oldest binary probed. Its own help text names FastCDC-2020 as the only algorithm and requires the server to advertise the two methods, so on a backend that does not, the flag buys nothing. The selector shipped nowhere: its PR closed unmerged and `GrpcCacheClient.java` hardcodes `FAST_CDC_2020` at every call site at both the 8.8.0 and 9.0.0 tags. | `bazel help build --long 2>&1 \| grep -A4 remote_cache_chunking` against the pin: the boolean is the only hit, and a hit on any `_function` or `rep_max_cdc` spelling means a fork or a non-release build, not upstream. Cross-reference an rc hit against `.bazelversion` **and** the matrix's *oldest* pin; for a pin older than 8.7.0 re-derive presence per BZL-CACHE-23. Empty (flag absent from the rc file) is the pass. | CONSIDER |
| BZL-CACHE-30 | Do not enable `--remote_cache_compression` without first measuring the build's artifact-size distribution. | It defaults `false` (measured on 8.7.0, 8.8.0 and 9.2.0) and is a no-op below `--experimental_remote_cache_compression_threshold`'s 100-byte default, so a build dominated by small blobs pays CPU for zero transfer saving. The evidence is asymmetric and first-hand only on the cost side: a public reproducible repro measuring 3–5× higher JVM heap use, plus a maintainer's own 2 B → 15 B (750 %) zstd inflation on a tiny file, which is the mechanism behind the 100-byte threshold. No first-hand latency or bandwidth benefit benchmark exists in any published source. | Sample `bazel-out` output sizes (or the cache's own upload-size statistics) sorted descending before flipping the flag. A distribution with little mass above 100 bytes is the finding against enabling it; an empty or near-empty distribution above the threshold means do not enable. | CONSIDER |

## Diagnosing a Miss

Name the cache before running anything. The capacity read is
`du -sh $(bazel info repository_cache) $(bazel info output_base) <configured --disk_cache path>`;
the key-instability read is the two-run execution-log diff below. Diagnostic
decision trees and the per-cache discriminating commands live in this rule
set's diagnosis skill and are cited, never restated.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CACHE-15 | Before diagnosing any "why is this still slow" or "why did this rebuild", name which of the **seven** caches is implicated: in-memory Skyframe, repository cache, repo contents cache, output tree plus local action cache, local disk cache, remote action cache plus CAS, and remote execution (a compute path, not persistent state). Do not assume `--repository_cache` speeds up every repository rule. | Conflating two of them is the most common source of a wrong diagnosis. Only repository rules calling `rctx.download()` or `.download_and_extract()` benefit from the repository cache — several widely used rules bypass it by default. The repo contents cache is a separate, newer mechanism with its own GC and its own version gate. | The `du` read above, plus two signals that read backwards. A persistent **local** action-cache hit is never written to the execution log at all (the spawn proto's own contract), so an action's *absence* from a fresh `--execution_log_compact_file` is the positive signal, not a bug; and the terminal status line never reports local cache hits. For the build-wide local picture read the BEP's `ActionCacheStatistics` (`hits` / `misses` / `miss_details`), which measures the **local** cache only, never the remote one. Equal or empty `du` sizes mean the symptom is not a capacity problem — a routing answer, not a pass. | MUST |
| BZL-CACHE-16 | Diagnose a specific action's cache-key instability from the execution log, never from the aggregate cache-hit line and never from `--explain`: build twice with `--execution_log_compact_file=<path>`, build `//src/tools/execlog:parser` from a Bazel source checkout, run it against **both** logs in one invocation with matched `--output_path`s, diff the two text files, and read the **first** divergent action. Run a separate two-run output-digest diff for output non-determinism. | The parser *reorders* the second log to match the first's action order (matching by first output) — it does not diff, so a naive diff of raw logs is noise, and every action downstream of the first divergence also shows as changed. Measured limits: `--explain` reported `no entry in the cache (action is new)` for actions confirmed to be disk-cache hits, because it reasons from the local action-cache layer only; and two runs whose `commandArgs`, `inputs[].digest` and `environmentVariables` were byte-identical produced different output `sha256sum`s. The parser ships in no release archive, so this needs a source checkout and a local JDK. | The sequence is the check. Strip `metrics.startTime`, `metrics.executionWallTime` and `metrics.totalTime` before diffing — always volatile; do **not** strip `actualOutputs[].digest`, which is real non-determinism. Tooling traps: `--restrict_to_runner` belongs to `execlog:parser` and `--sort` to `execlog:converter`; `--execution_log_sort` never applies to the compact format; `--execution_log_json_file` emits concatenated pretty-printed objects with no separator or wrapping array, so only a raw-decode loop parses it. An **empty diff** means the two runs were execution-log-identical — a pass on Bazel-side reproducibility and a signal to investigate the server (eviction, instance name, auth scope, write authorisation). It is **not** proof the outputs match: run the clean-build `sha256sum` diff too. | MUST |

## Gaps

- Every measurement here ran on Linux under `linux-sandbox`; darwin and Windows
  tag, sandbox and eviction behaviour is unmeasured, and `-atime` versus
  `-mtime` (BZL-CACHE-32) turns on a mount option that varies by image.
- No shape with a live `--remote_executor` was measured: whether exit 39 is
  reachable at all, and how an outage behaves with an executor configured
  (BZL-CACHE-26), are both open.
- `--rewind_lost_inputs` at the **default** retry budget of 5 was never run;
  only the two `retries=0` extremes were, both exiting 1 (BZL-CACHE-12).
- `--experimental_remote_cache_chunking`'s runtime effect is unmeasured — no
  surveyed server advertises `SplitBlob` / `SpliceBlob` — and one commercial
  backend publishes neither capability field, so it records as unknown.
- No first-hand benefit-side `--remote_cache_compression` benchmark exists in
  any published source, and repository-cache GC has no dated upstream
  commitment; treat both as durable absences, not as pending answers.

## What Agents Get Wrong Here

1. **Citing a flag without the version it exists on, in either direction** —
   two flags written up here as never-existing are live and default-active on
   8.7.0 and removed only at 9.0.0 (BZL-CACHE-23).
2. **Reading a `bazelbuild/bazel` PR's `mergedAt: null` as the answer** — it is
   the normal shape of a community PR superseded by the internal sync, so check
   for the named commit at the tag before demoting a citation (BZL-CACHE-23).
3. **Writing a CI wrapper that retries on exit code 39** — measured, the
   eviction condition self-heals to exit 0 or fails generically at exit 1, so
   the wrapper never fires and gives false confidence (BZL-CACHE-12).
4. **Claiming a cache outage fails a cache-only build** — the two fallback
   flags have no effect without an executor, and an unreachable cache is a
   WARNING plus a green local build on both majors (BZL-CACHE-26).
5. **Recommending `--remote_download_minimal` "to turn on BwoB"** — it has been
   the effective default since Bazel 7, and the real topic is eviction
   (BZL-CACHE-10, BZL-CACHE-11).
6. **Suggesting `--noincompatible_strict_action_env` to fix environment
   flakiness** — on 8.x it looks like a no-op and pins the permissive default
   across a 9.x bump; the fix is a named `--action_env` (BZL-CACHE-19).
7. **Inventing a knob that does not exist** — `salt=` on an action, or a
   client-side symlink-strategy flag, which is exactly what an agent reaches
   for when told a server allows absolute symlinks (BZL-CACHE-22,
   BZL-CACHE-31).
8. **Migrating to `--credential_helper` by writing it into the committed
   `.bazelrc`** — strictly worse than the token it replaces, because a clone
   plus a read-only query executes it (BZL-CACHE-04).
