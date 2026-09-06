# Cache wiring and CI lanes

Read this when running steps 8 and 9. It holds the four cache stages in order
with the verification for each, the remote-execution readiness gate, the CI lane
taxonomy, and why target selection is not part of an adoption.

Contents: [Why the order](#why-the-order) · [Stage 1: disk cache](#stage-1-disk-cache) ·
[Stage 2: remote cache, read-only](#stage-2-remote-cache-read-only) ·
[Stage 3: writes](#stage-3-writes) ·
[Stage 4: remote execution](#stage-4-remote-execution) ·
[Download role](#download-role) · [CI lanes](#ci-lanes) ·
[Why selection is deferred](#why-selection-is-deferred)

## Why the order

Each stage is verifiable on its own, and each later stage adds a failure mode the
earlier one does not have. Wiring stage 2 before stage 1 is measured means the
first "it is still slow" report has two candidate causes instead of one. Wiring
stage 3 before stage 2 means the first credential incident and the first cache
correctness question arrive together.

Before any of it, know which cache you mean. There are seven, and conflating two
is the most common source of a wrong diagnosis: in-memory Skyframe, the
repository cache, the repo contents cache, the output tree plus local action
cache, the local disk cache, the remote action cache plus CAS, and remote
execution — which is a compute path, not persistent state (BZL-CACHE-15).

## Stage 1: disk cache

Local and on the runner. This answers most of the question for a single-runner
setup at a fraction of the operational surface of a remote cache.

Set an explicit size or age bound. Both GC flags default to `"0"` — unbounded —
**even on a version that supports them**; the feature is opt-in, not
automatic-by-version (BZL-CACHE-17). The classic `--repository_cache` has no
automated pruning on any version; prune a long-lived runner's copy on **mtime**,
never atime, because mtime is the signal Bazel actually maintains (BZL-CACHE-32).

Verification, which is also the thing that makes the disk cache worth having:

```sh
# warm it from one checkout, then build the same commit from a second path
bazel build --disk_cache=/some/shared/path //...   # checkout A
bazel build --disk_cache=/some/shared/path //...   # checkout B, different absolute path
```

The second build must report hits. An action key does not depend on the
workspace's absolute path — measured on both majors, a cache warmed at one path
served every action at another (BZL-CACHE-34). **A second build reporting zero
hits means the actions are not key-stable**, which is a hermeticity problem to
fix now, not a cache to configure harder.

That same property is the standing warning: a shared cache is never
checkout-scoped, so any action that reads ambient run-time state ships that state
verbatim to every other consumer of the cache.

## Stage 2: remote cache, read-only

Read-only everywhere first, on every lane, including the developer machine if it
gets a cache at all.

Give the URI an explicit `http://` or `https://` scheme. Both remote flags default
to `grpcs` when the URI carries none, so an unscoped URI against an HTTP-only
server speaks the wrong protocol.

```sh
grep -rn -e 'remote_cache=' -e 'remote_executor=' --include='*.bazelrc*' --include='*.yml' .
```

Every match must carry `://`. **Empty output = nothing remote configured = you
are still at stage 1**, which is the correct state until stage 1 is measured to
help — but a nonzero exit with empty stdout means "not searched," not a pass.

**An outage is not a failure, and this surprises people.** Measured on 8.7.0 and
9.2.0 with the endpoint refusing connections: all four combinations of
`--remote_local_fallback` and `--incompatible_remote_local_fallback_for_remote_cache`
produced the identical outcome — a `WARNING: Remote Cache: Connection refused`, a
full local build, exit **0**. Neither flag governs the cache-only shape at all. If
a lane must fail on an outage, implement that outside Bazel and say so; if a
silent local build is acceptable, say that too (BZL-CACHE-26).

The consequence for verification: **a green build proves nothing about whether the
cache was reachable.** Read the hit counters, not the exit code.

Before trusting a portability or hermeticity claim about the backend, read what
its `GetCapabilities` response advertises — `symlink_absolute_path_strategy` and
`digest_functions` are hardcoded properties of that server binary, not operator
settings, and a mismatch across a backend migration is a silent 100 percent cold
cache (BZL-CACHE-24).

## Stage 3: writes

Writes go on exactly one lane, and that lane is one untrusted content cannot
trigger.

- The write credential is **absent from every other lane's environment**, not
  merely unused. A write-capable untrusted lane can plant a backdoored tool that a
  later trusted build downloads and executes instead of compiling (BZL-CACHE-01).
- Write access is strictly narrower than read access — never symmetric, never held
  by a developer machine's default config (BZL-CACHE-02).
- The credential travels through `--credential_helper`, never a bare
  `--remote_header=authorization=…` or a static bearer token in an rc file,
  gitignored or not (BZL-CACHE-03).
- The `--credential_helper` flag itself is configured only from a file the
  repository does not ship, and never with a workspace-relative helper path
  (BZL-CACHE-04).

```sh
git ls-files | grep -E '\.bazelrc' | xargs -r grep -n -e credential_helper -e remote_header
```

**Empty output = pass; empty `xargs` input means no rc file is tracked.** Any
hit is a credential path reachable from a fresh clone.

If a build event service is added later, its credential and TLS trust surface is
the same one: rotate both together and name both in the runbook (BZL-CACHE-33).

## Stage 4: remote execution

Last, and only after the readiness gate passes in order, stopping at the first
failure (BZL-CACHE-07):

1. No generated launcher, wrapper or action output embeds a hardcoded absolute
   path rooted outside the workspace. An executor never ran the provisioning step
   that populated it. First filter, then read every site that *emits* launcher
   content, because a generated string is invisible to the grep:
   `grep -rn '"/home/\|"/Users/\|\$HOME\|/opt/' <generator .bzl files>` —
   **empty from the grep is necessary and not sufficient** (BZL-CACHE-06).
2. Every tool resolves through a declared toolchain or a `File` input, never the
   invoking shell's `PATH`. Bazel does not track tools outside the workspace, so
   swapping a host toolchain serves stale cached output rather than a miss
   (BZL-CACHE-18).
3. No repository rule or action reachable by an executed target writes outside
   the Bazel-managed tree or sets persistent environment.
4. No checked-in build-tool binary is used unconditionally across execution
   platforms.
5. A sandbox-clean build passes.

Never enable dynamic execution against a deployment that configures only a remote
cache with no executor: a cache miss under a cache-only backend is a failed
action, structurally (BZL-CACHE-08). When the gate fails structurally, record the
"no" in the repository's own docs so nobody rediscovers it in a debugging
session.

## Download role

Set the download mode explicitly per role rather than riding the default
(BZL-CACHE-11): a runner that never consumes build artifacts locally gets
`--remote_download_minimal`; an interactive machine keeps the `toplevel` default;
an IDE-feeding build gets `minimal` plus a regex naming the consumed paths. Never
a blanket `--remote_download_all`.

Build without the Bytes is not a feature to enable — `--remote_download_outputs`
has defaulted to `toplevel` since Bazel 7, measured on 8.7.0, 8.8.0 and 9.2.0
(BZL-CACHE-10). Guidance telling an adopter to "turn on BwoB" adds a flag that
changes nothing.

Do not key CI retry or alerting on exit code **39**. Measured across five
configurations on both majors: the eviction condition fires, but the caller sees
exit 0 (Bazel retries under a fresh invocation id and re-executes locally) or,
with retries at 0, a generic exit 1 — never 39. Recognise a lost evicted input by
its error text, and leave `--experimental_remote_cache_eviction_retries` at its
default of 5 (BZL-CACHE-12).

## CI lanes

The minimum job set is a lint gate plus a test matrix, both required checks. A
repository publishing to a registry adds a registry-parity job; one making any
offline or hermetic claim adds a cold-store job. Within this taxonomy only the
lint and test jobs may share the warm remote cache — the registry-parity and
cold-store jobs must not; the lockfile legs are BZL-MOD-02's gate and sit
outside it (BZL-CI-05).

| Lane | Trigger | Cache | Blocking |
|---|---|---|---|
| Lint gate | every PR | warm | yes |
| Test matrix, whole-repo `//...` | every PR | warm | yes |
| Lockfile freshness (`--lockfile_mode=error`) | every PR | warm | yes |
| Lockfile refresh (`--lockfile_mode=refresh`) | schedule | warm | no |
| Registry parity / cold store | release or schedule | **none**, with a comment saying why | yes |
| Rolling or `last_green` canary | schedule | warm | **no** |

A job whose purpose is proving something works without help runs with every
remote-cache flag absent **and** carries an adjacent comment naming what it proves
and why a cache hit would mask it. Without the comment a reader cannot tell a
deliberate omission from a forgotten step (BZL-CI-06).

Every gate job propagates the underlying command's own exit code. Nothing between
the command and the job result: no `|| true`, no stdout scrape, no
`continue-on-error: true` outside a declared canary (BZL-CI-07). And never read
the *number* as the tool's own — `buildifier_prebuilt` through 8.5.1.3 pipes
`find … | xargs`, which remaps 1-125 to **123**; 8.5.1.4 retired it. Pin
8.5.1.3 or later, 8.5.1.4 preferred, and assert non-zero either way
(BZL-LARK-31, BZL-CI-07).

```sh
grep -rn '|| true\|continue-on-error' .github/workflows/
```

**Empty output = pass.** Each hit needs an adjacent comment declaring that leg a
non-blocking canary.

Confirm CI actually invokes the build tool on a merge-gating path, never inferring
it from a badge, a `.bazelversion` file or an install step (BZL-CI-04):

```sh
grep -rn 'bazel\|bazelisk' .github/workflows/
```

At least one hit must sit in a `run:`/`script:` step of a required check. **Empty
output on a repository that believes it has adopted Bazel is the finding.**

Name the version only through the launcher's own resolution chain, never an OS
package install or a hand-pinned download outside it (BZL-CI-09).

## Why selection is deferred

Target selection is the last thing a repository should add and the first thing an
agent proposes.

- **No published target-count threshold exists.** Five primary sources were
  checked; none states one. Any guidance quoting a target count as an industry
  line is inventing provenance (BZL-CI-01).
- The default is whole-repo `bazel test //...`, and the switch point is
  wall-clock: consistently past ~40 minutes median on the widest runner, with
  ~300 rule targets as the tripwire that says *measure the wall-clock now*. Both
  numbers are this program's derived tripwire.
- Selection binds on the **determinism precondition**, not on scale. Run the
  five-step gate in order and stop at the first failure: environment tiers pinned
  per major, no undeclared non-determinism, non-hermetic steps isolated, eviction
  recognised by error text with the retry default left alone, and only then the
  tool's own miss class (BZL-CI-02). Convert every shared-service and
  unbounded-fan-out step into a cacheable target **before** the selection layer
  goes live, never alongside it.
- Both production tools document real false-negative classes. One is blind by
  default to any transitive dependency introduced only by *executing* a repository
  rule or module extension — pip, npm, most module extensions (BZL-CI-21). The
  other's results cache is unsafe across a changed home or system rc file, a
  changed environment variable, or a different host (BZL-CI-22).
- Whichever is adopted, keep at least one scheduled or on-merge whole-repo
  `bazel test //...` as the backstop; both miss classes are silent by
  construction (BZL-CI-27).

Record the deferral in the migration plan with the condition that reopens it, so
the next reader does not re-derive it.
