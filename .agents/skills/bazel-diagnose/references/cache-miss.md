# Cache misses and the execution log

Read this while running section B. It holds the seven-cache discriminator table,
the execution-log formats and their tooling traps, the one BEP field that answers
"did the local action cache work", and the constructions that genuinely
destabilise an action key.

Contents: [Name the cache first](#name-the-cache-first) ·
[The execution log](#the-execution-log) ·
[The diff recipes](#the-diff-recipes) ·
[What the log cannot see](#what-the-log-cannot-see) ·
[What really busts an action key](#what-really-busts-an-action-key) ·
[Cache-scoping tags, measured](#cache-scoping-tags-measured)

Measured on Bazel 8.7.0 and 9.2.0, Linux, `linux-sandbox` strategy, against a
local `--disk_cache` and a real HTTP remote cache. Nothing here was exercised
against a remote **executor**; a build with `--remote_executor` set may route
output verification through different code, and that limitation belongs in the
receipt.

## Name the cache first

Seven caches, one discriminating command each. Conflating two of them is the most
common wrong diagnosis in this whole domain (BZL-CACHE-15).

| Cache | Discriminator | How to read it |
|---|---|---|
| In-memory Skyframe graph | `bazel dump --skyframe=summary` (or `=count`) | Node counts per SkyFunction; growth across incremental builds with no source change is state retention, not a miss |
| Repository cache | `du -sh $(bazel info repository_cache)` | Only `ctx.download()`/`.download_and_extract()` populate it. A small directory is not evidence of breakage if the repo rule never calls those |
| Repo contents cache | Version check first: opt-in (`""`) on 8.7.0 and 8.8.0, **on by default at 9.2.0** under `{--repository_cache}/contents` | No `bazel info` key names it. A populated directory is not proof — confirm a hit by fetching twice with `clean --expunge` between, with a `print()` in the rule body: no DEBUG line on the second fetch is the hit (BZL-MOD-16) |
| Output tree + local action cache | The action is **absent** from a fresh execution log | Absence is the hit signal, by `spawn.proto`'s own contract. For a build-wide figure read the BEP (below) |
| Disk cache | `du -sh <the configured --disk_cache path>` | A bare path the invoker chose; no `bazel info` key. Unbounded unless `--experimental_disk_cache_gc_max_size` or `_max_age` is set (BZL-CACHE-17) |
| Remote AC + CAS | The status line's `N remote cache hit` count | Cross-checked with `--remote_grpc_log` traffic — `FindMissingBlobs`/`GetActionResult` calls; an AC hit with a CAS miss is the eviction shape, see the outage reference. The canonical flag name is `--remote_grpc_log`; `--experimental_remote_grpc_log` is a deprecated alias |
| Remote execution — a compute path, not persistent state | N/A — no cache to name | Executes actions on a remote worker and persists nothing between builds: no `bazel info` key, log signal or directory to inspect. Unmeasured against a live `--remote_executor` in this reference — see the scope note above |

The status line — `INFO: 7 processes: 3 remote cache hit, 4 linux-sandbox` —
**never includes local cache hits**. A low process count is not a remote-cache
finding. Primary source: <https://bazel.build/remote/cache-local>.

**The BEP discriminator for the local action cache.** `BuildMetrics`'
`action_cache_statistics` carries `hits`, `misses` and `miss_details` keyed by
`MissReason`. Only four of that enum's values are live —
`NOT_CACHED`, `DIGEST_MISMATCH`, `CORRUPTED_CACHE_ENTRY`,
`UNCONDITIONAL_EXECUTION`; the remaining four are marked "currently not used" in
the proto itself, so citing them as reasons is fabrication. Capture with
`bazel build --build_event_text_file=/tmp/bep.txt <target>` and read that
message. It is the **local, on-disk** cache only. Never source a *remote* hit
rate from `BuildMetrics.ActionSummary.remote_cache_hits` — that field is
`[deprecated = true]` in Bazel's own proto (BZL-CI-32).

## The execution log

Three mutually exclusive formats:

| Flag | Shape | Use it when |
|---|---|---|
| `--execution_log_compact_file` | Length-delimited `ExecLogEntry` protos, whole file zstd-compressed | Default choice — smaller and cheaper to produce, and what the parser expects |
| `--execution_log_binary_file` | `SpawnExec` protos | You already have tooling for it |
| `--execution_log_json_file` | Newline-free **concatenated pretty-printed JSON objects** | Scripting without building the parser — but see the trap below |

`--execution_log_sort` (bool, default `true`) affects the binary and JSON formats
only; **the compact format is never sorted**.

**JSON trap.** `--execution_log_json_file` emits concatenated pretty-printed
objects with no separator and no wrapping array. `json.loads()` on the file — or
on any single line — fails. Only a `JSONDecoder().raw_decode()` loop reads it. A
script that "found nothing in the log" has usually hit this and swallowed the
exception.

**Two tools, two flag sets.** `//src/tools/execlog:parser` takes `--log_path`
(repeatable), `--output_path` (repeatable, paired positionally) and
`--restrict_to_runner`. `//src/tools/execlog:converter` takes `--input`/`--output`
as `format:path` and `--sort`. `--sort` is the converter's, never the parser's;
`--restrict_to_runner` is the parser's, never the converter's.

**Fields that answer the question** (`SpawnExec`): `runner` — the strategy name,
or literally `"disk cache hit"` / `"remote cache hit"`; `cache_hit`; `cacheable`;
`remotable`; `remote_cacheable`; `target_label`; and the top-level `digest`.

Two readings that are easy to invert:

- **`digest` is present only when a disk or remote cache is configured.** Its
  absence in a log taken with no cache configured is expected, not a finding.
- **`cacheable`/`remotable` describe the action's own eligibility**, never
  whether an endpoint received anything. They cannot confirm an upload.

## The diff recipes

Same machine, two runs — the action-key question:

```
bazel build --execution_log_compact_file=/tmp/exec1.log //t
bazel build --execution_log_compact_file=/tmp/exec2.log //t
```

Build and run the parser in a bazelbuild/bazel source checkout at the same tag
as your pin (the parser ships in no release archive and needs a local JDK —
BZL-CACHE-16):

```
bazel build //src/tools/execlog:parser
bazel-bin/src/tools/execlog/parser \
  --log_path=/tmp/exec1.log --log_path=/tmp/exec2.log \
  --output_path=/tmp/exec1.log.txt --output_path=/tmp/exec2.log.txt
diff -u /tmp/exec1.log.txt /tmp/exec2.log.txt
```

Two machines — the "why did this miss between hosts" question: `bazel clean` on
each, run the same build with `--execution_log_compact_file` on each, diff the
two files (BZL-CACHE-16). Both recipes are Bazel's own, at
<https://bazel.build/remote/cache-remote>.

**How EMPTY reads.** An empty diff means Bazel-side action keys are stable for
this pair of runs. It does **not** mean the build is deterministic and it does
not mean the cache is healthy — it moves the investigation to the server side
(eviction, auth scope, a different `--remote_instance_name`) or to the outputs.

**Volatile fields to strip before diffing a JSON log by field**:
`metrics.startTime`, `metrics.executionWallTime`, `metrics.totalTime`. These
differ on every run and are safe to drop unconditionally. Do **not** strip
`actualOutputs[].digest` — a difference there is real output non-determinism,
which is a finding, not noise.

`bb explain --old {FILE|INVOCATION_ID} --new {FILE|INVOCATION_ID} [--nondeterministic_only]`
is the packaged, invocation-ID-addressable version of the same compact-log diff,
if that CLI is already available. It reads execution logs only.

## What the log cannot see

The execution log answers "did this action's declared inputs, arguments or
environment change". It cannot answer "did the output change for the same
declared inputs" — measured directly: two runs whose `commandArgs`,
`inputs[].digest` and `environmentVariables` were byte-identical produced
different `sha256sum` output for three targets. The companion check is the
two-run output-digest comparison in section D, and neither substitutes for the
other (BZL-HERM-30, BZL-CACHE-16).

`--explain` / `--verbose_explanations` reason only from the **local dependency
checker**. Measured: `--explain` reported `no entry in the cache (action is new)`
for actions confirmed as disk-cache hits. It never discriminates a cache tier.
Do not use it for any cache diagnosis.

## What really busts an action key

Measured on 8.7.0 and 9.2.0, cross-checkout, against a shared `--disk_cache`:

| Construction | Key stable across checkouts? | Note |
|---|---|---|
| `$(location …)` / `$(execpath …)` substitution | **Yes** | Resolved to exec-root-relative strings at analysis time; no absolute path enters the hashed material |
| `$$(pwd)` or `$PWD` read inside a genrule `cmd` body | **Yes** | Bazel records the un-evaluated literal. The *output content* is still not portable (BZL-HERM-10, BZL-HERM-31) |
| `ctx.bin_dir.path` in a custom rule's command | **Yes** | Exec-root-relative; there is no analysis-time route to a true absolute path through `ctx.*` |
| `--action_env=WS=$PWD` (shell-expanded before Bazel sees it) | **No**, for any action that reads `WS` | The value is baked into the recorded environment. An action that never reads it keeps its hit |
| `--define=WS_PATH=$PWD` read via `ctx.var.get()` or `$(WS_PATH)` | **No**, same scope | Substituted at analysis time, before hashing |
| A host-resolved tool invoked bare off `PATH` | **Not covered by the key at all** | Only a declared `File` from a toolchain or dependency enters it (BZL-CACHE-18) |

Grep signatures for the two destabilising forms, over rc files, workflows and
wrapper scripts:
`grep -rnE -- '--(action_env|define)=[A-Z_]+=\$\(pwd\)|--(action_env|define)=[A-Z_]+=\$PWD' .bazelrc* .github/ scripts/`
A hit means every checkout at a different absolute path builds a private,
unshareable cache entry for every action consuming that variable. EMPTY clears
only the files you searched — a value injected by a CI wrapper you did not read
is out of this grep's reach, as is anything generated into a repository's own
`BUILD`/`.bzl` text.

A shared cache is **not** checkout-scoped: a hit serves another checkout's — or
another machine's — bytes verbatim. Measured, the second checkout's output
literally contained the first checkout's output-base hash (BZL-CACHE-34).

## Cache-scoping tags, measured

Both majors, warm `--disk_cache`, cross-read against the execution log:

| Tag | Runner | Disk-cache hit on rebuild | `cacheable` | `remotable` |
|---|---|---|---|---|
| *(none)* | `linux-sandbox` | hit | true | true |
| `no-cache` | `linux-sandbox` | **miss** | false | true |
| `no-remote-cache` | `linux-sandbox` | hit | true | true |
| `no-remote` | `linux-sandbox` | hit | true | false |
| `no-sandbox` | `local` | hit | true | true |
| `local` | `local` | **miss** | false | false |

Two tags a disk cache cannot probe at all (BZL-TEST-08): `no-remote-cache-upload`
suppresses exactly the `PUT /ac/` result mapping while the action's `PUT /cas/`
content upload still fires — visible only in a real remote cache's request log;
and `external` is **test-only**, with zero observable effect on a build action.
Against `--disk_cache` alone both read identically to the default row, so such a
probe reads **blind**, never "confirmed".
