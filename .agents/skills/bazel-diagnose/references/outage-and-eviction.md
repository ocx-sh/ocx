# Cache outages, evictions and exit codes

Read this while running section C, or before writing any CI wrapper that reacts
to a cache failure. It holds what a cache outage and a blob eviction actually do
to a build, measured rather than documented, plus the flags involved and their
version boundaries.

Contents: [What an outage does](#what-an-outage-does) ·
[What an eviction does](#what-an-eviction-does) ·
[Exit codes, per configuration](#exit-codes-per-configuration) ·
[Rewind and its ceiling](#rewind-and-its-ceiling) ·
[Download modes and exposure](#download-modes-and-exposure) ·
[Flags removed at 9.0.0](#flags-removed-at-900) ·
[BES](#bes)

**Scope, stated once.** Everything below was measured on Bazel 8.7.0 and 9.2.0
on Linux against a real HTTP remote cache with **no `--remote_executor`
configured** (a cache-only build). A deployment with a live executor is a
materially different shape that was not measured, and saying so is part of the
receipt.

## What an outage does

With the cache server refusing connections, all four combinations of
`--remote_local_fallback` and `--incompatible_remote_local_fallback_for_remote_cache`
produced the identical result on both majors:

```
WARNING: Remote Cache: Connection refused: /127.0.0.1:9411
WARNING: Remote Cache: 3 errors during bulk transfer:
io.netty.channel.AbstractChannel$AnnotatedConnectException: Connection refused
INFO: 4 processes: 1 internal, 3 linux-sandbox.
INFO: Build completed successfully, 4 total actions
exit=0
```

An unreachable cache is a **WARNING and a green local build**, with zero fallback
flags set. This is structural, not flag-gated: those two flags govern falling
back from a failed *remote execution*, and a cache-only shape has no remote
execution to fall back from. Do not cite either as controlling outage behaviour
(BZL-CACHE-26).

Consequence for CI: **no Bazel mechanism turns a cache outage into a build
failure on a cache-only shape.** A lane that must fail on an unreachable cache
needs a check outside Bazel — a reachability probe before the build. Saying "we
will set a fallback flag" is not that check.

The confirming grep: `grep -n 'WARNING: Remote Cache' <build log>`. A hit with a
zero exit is the whole finding. EMPTY means the cache was reachable — the miss
you are chasing is a key or eviction question, not connectivity.

## What an eviction does

The mechanism is real and reproducible on both majors. Under the default
`toplevel` download mode a cache hit leaves non-top-level outputs as CAS
references; a later locally-executing action that needs one of those bytes hits
an evicted blob. What it does *not* do is surface a distinctive exit code.

Error texts to match, all observed:

| Text | Configuration |
|---|---|
| `lost inputs with digests: …` | 8.7.0, default retries |
| `Lost inputs no longer available remotely: <file>` | 9.2.0, default retries |
| `Found transient remote cache error, retrying the build...` | Both majors, default retries — the self-heal line |
| `Unexpected lost inputs (pass --rewind_lost_inputs to enable recovery)` | 9.2.0, retries `0` |
| `lost input too many times (#21) for the same action` | Rewind engaged and exhausted |

`grep -nE 'lost inputs|Lost inputs no longer available remotely|Found transient remote cache error|Unexpected lost inputs' <build log>`
EMPTY = not an eviction; look elsewhere. Any hit identifies it regardless of the
exit code, which is the point (BZL-CACHE-12).

## Exit codes, per configuration

Ten build invocations, two majors, five configurations. **Bare exit 39 was never
observed to reach the caller.**

| Configuration | Observed outcome | Outer exit |
|---|---|---|
| Default (`--experimental_remote_cache_eviction_retries=5`) | Prints the transient-error line, re-runs under a **fresh invocation ID**, re-executes the lost action locally | **0** |
| `--experimental_remote_cache_eviction_retries=0` | `Target //… failed to build`, no retry line | **1** |
| retries `0`, blob permanently discarded | Same as above | **1** |
| retries `3`, blob permanently discarded | Self-heals: the locally regenerated file satisfies the rest of that invocation | **0** |
| `--rewind_lost_inputs` on, retries `0` (9.2.0) | Skyframe rewinding genuinely engages and exhausts its ceiling | **1** |

Two readings follow, and both matter more than the code itself:

1. **Exit 0 with no eviction signal beyond a printed line** is the default. A CI
   lane that only records exit codes sees a clean build and learns nothing.
2. **Exit 1 with a generic failure message** is what "handled the eviction badly"
   looks like — indistinguishable from a compile error by exit code.

`ExitCode.REMOTE_CACHE_EVICTED = 39` is registered on both majors; it is real
plumbing that this reproduction shape never reaches. Whether remote **execution**
reaches it is untested here.

## Rewind and its ceiling

- `--experimental_remote_cache_eviction_retries` — default `5` on 8.7.0, 8.8.0
  and 9.2.0. This is a client-level whole-build retry. **Leave it at the
  default**; setting it to `0` converts a self-healing build into a generic
  failure, and raising it does not address the cause. (BZL-CACHE-12)
- `--rewind_lost_inputs` — a separate, lower-level Skyframe mechanism, default
  `false`. Present and empirically functional on **8.7.0** despite rendering in
  no help surface there (`UNDOCUMENTED` through 9.1.0, `REMOTE`-documented at
  9.2.0). A help-surface grep that finds nothing on 8.7.0 must not be reported as
  "the flag does not exist" (BZL-FLAG-11).
- The ceiling is **hard-coded**: `MAX_REPEATED_LOST_INPUTS = 20` in
  `ActionRewindStrategy.java`, byte-identical from 8.7.0 through 9.2.0. The 21st
  loss fails, which is why the text reads `(#21)` and never `(#20)`. The tunable
  `--experimental_max_repeated_lost_inputs` exists only after 9.2.0 on `main`. (BZL-CACHE-12)
- **Documented gap:** rewind together with the *default* retry budget of 5 was
  never exercised — only the two extremes. If a diagnosis depends on how those
  two layers compose, that is a gap, not an answer.

## Download modes and exposure

`--remote_download_outputs` defaults to `toplevel` on 8.7.0, 8.8.0 and 9.2.0.
Read it off the binary before writing anything about it:
`bazel help build --long 2>&1 | grep -A2 remote_download_outputs`.

| Mode | Materialises | Eviction exposure |
|---|---|---|
| `toplevel` (default) | Top-level target outputs only; intermediates stay CAS references | Exposed |
| `minimal` | Nothing, not even the top-level target's own output | **Identically exposed** — the distinction from `toplevel` is about the top-level output, not about intermediates a local action still needs |
| `all` | Everything, as soon as it is a cache hit | Not exposed — a later eviction has nothing left to reach. This is a side effect of the mode's definition, not an eviction feature |

Never describe Build without the Bytes as something to enable: it has been the
default since Bazel 7, and the three alias flags each expand to this one flag
(BZL-CACHE-10). Set the mode per role instead (BZL-CACHE-11).

## Flags removed at 9.0.0

| Flag | 8.7.0 | 8.8.0 | 9.2.0 |
|---|---|---|---|
| `--experimental_remote_merkle_tree_cache` | present, `false` | present, `false` | **absent** |
| `--incompatible_remote_use_new_exit_code_for_lost_inputs` | present, `true` | present, `true` | **absent** |
| `--rewind_lost_inputs` | present, `false`, undocumented | same | present, `false`, documented |
| `--experimental_remote_discard_merkle_trees` | `true` | `true` | `true` |
| `--experimental_remote_cache_eviction_retries` | `5` | `5` | `5` |
| `--experimental_remote_cache_ttl` | `3h` | `3h` | `3h` |

The correct phrasing everywhere is **"removed at 9.0.0"** — never "never
existed", never a bare "deleted". The consequence is a version-split failure, not
a uniform one: an rc line carrying either of the first two is dead configuration
on an 8.x leg and a hard `unrecognized option` on the 9.x leg of the same matrix.

Two citation disciplines this family cost the corpus twice (BZL-CACHE-23):

- A `bazelbuild/bazel` PR that is `CLOSED` with `mergedAt: null` is **not**
  evidence the change was rejected — that is the normal shape of a community PR
  superseded by an internal sync. Check whether the named commit exists in the
  tagged tree before demoting a citation to a proposal.
- A release's "flags removed" appendix does not enumerate every removed flag; one
  of the two above appears in the 9.0.0 appendix and the other does not.

`--experimental_remote_cache_chunking` is a plain boolean (default `false`) on
all three versions; there is no algorithm-selector flag, and no cache server in
this corpus advertises the capability it would use (BZL-CACHE-28).

## BES

Measured identical on 8.7.0 and 9.2.0: `--bes_backend` `""`, `--bes_results_url`
`""`, `--bes_upload_mode` `wait_for_upload_complete`, `--build_event_binary_file`
`""`, `--remote_build_event_upload` `minimal`.

Three consequences:

1. The default upload mode **waits**, so an unreachable BES backend can hold a
   lane even though an unreachable *cache* does not. Set `--bes_upload_mode` per
   lane deliberately and say why in a comment (BZL-CI-31).
2. BES shares one credential and one TLS stack with the cache and executor
   endpoints, by Bazel's own statement. A rotation that updates one surface and
   not the other breaks it silently (BZL-CACHE-33).
3. `--remote_build_event_upload` defaults to `minimal`, so a `bytestream://` URI
   appearing in a captured BEP stream is **not** proof the blob was uploaded. A
   tool that dereferences those URIs needs `=all` on the producing lane or a
   documented not-found fallback (BZL-CI-33).

Bazel's own exit-code table, for anything this file does not cover:
<https://bazel.build/run/scripts>.
