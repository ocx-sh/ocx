# Slow build and slow analysis

Read this while section A's profile is open, or before writing any profiling
command into a runbook. It holds the flag surface as it exists on 8.7.0 and
9.2.0, the per-culprit signatures, the memory knobs, and the traps that make a
build re-analyse with no source change.

Contents: [The flag surface](#the-flag-surface) ·
[Reading the trace](#reading-the-trace) ·
[Analysis-phase culprits](#analysis-phase-culprits) ·
[Memory and GC](#memory-and-gc) ·
[Session traps](#session-traps) ·
[Third-party analysis](#third-party-analysis)

Every flag below was read from the 8.7.0 and 9.2.0 binaries' own help output on
2026-09-06 unless a row says otherwise. Names and defaults move between majors;
re-derive them from the pinned binary before writing any of them down
(BZL-CACHE-23, BZL-FLAG-11).

## The flag surface

| Flag | Type / default | Version note |
|---|---|---|
| `--profile=<path>` | path, unset | Unset still writes `command-$INVOCATION_ID.profile.gz` under the output base, with a `command.profile.gz` symlink to the latest |
| `--generate_json_trace_profile` | TriState, `auto` | **Not a boolean.** Old spelling `--experimental_generate_json_trace_profile` still parses |
| `--slim_profile` | bool, `true` | Old spelling `--experimental_slim_json_profile` |
| `--experimental_profile_include_target_label` | bool, `false` | Unchanged both majors |
| `--starlark_cpu_profile=<path>` | string, `""` | pprof CPU profile across all Starlark threads |
| `--experimental_command_profile` | enum `{cpu,wall,alloc,lock}`, unset | Java Flight Recorder profile, written under the output base |
| `--memory_profile=<path>` | path, unset | Usage at phase ends plus stable heap at build end |
| `--heap_dump_on_oom` | bool, `false` | Writes `<output_base>/<invocation_id>.heapdump.hprof`; replaces `-XX:+HeapDumpOnOutOfMemoryError`, which has no effect for Bazel's manual OOMs |
| `--host_jvm_args` | repeatable, unset | **Startup** option; `--host_jvm_args=-Xmx2g` is the documented way to cap server heap |
| `--skyframe_high_water_mark_threshold` | int, `85` | Percent retained heap above which temporary Skyframe state is dropped on GC |
| `--gc_thrashing_threshold` | int, `100` | `100` **disables** the detector. `--experimental_oom_more_eagerly_threshold` is a **silent alias** for this name, and the algorithm behind it changed — it is not a rename of the old one-shot knob |
| `--gc_thrashing_limits` | `1s:2,20s:3,1m:5` | The flag that actually gates the detector |
| `--experimental_ui_debug_all_events` | bool, `false` | `UNDOCUMENTED` — present on both majors, rendered in neither help surface |
| `--noallow_analysis_cache_discard` | — | Turns a silent analysis-cache discard into a hard error |

**Commands and enum values that moved at 9.0.0**, both confirmed by diffing the
tagged source trees:

- `bazel analyze-profile` — present through 8.8.0, its implementing class deleted
  at 9.0.0. Bazel's own frozen **9.1.0** documentation snapshot still advertises
  it, so an agent reading the versioned docs for the right major gets it wrong.
  There is no replacement command; read the trace directly.
- `bazel dump --skyframe=working_set` / `working_set_frontier_deps` — renamed
  `active_directories` / `active_directories_frontier_deps` at 9.0.0. The *live*
  memory-optimisation page still prints the pre-9 spelling, so this one is wrong
  in the opposite direction: the prose is behind the release.

Both are BZL-FLAG-31's subject: a major bump removes commands and enum values,
not only flags. `grep -rn 'analyze-profile\|skyframe=working_set' docs/ .github/`
before and after a bump; any hit on a ≥9.0 pin is dead.

## Reading the trace

Open the `.json.gz` in `chrome://tracing` or Perfetto. The rows that carry the
answer:

| Row | What it tells you |
|---|---|
| `Main Thread` | Phase markers: `Launch Blaze`, `evaluateTargetPatterns`, `runAnalysisPhase` |
| `Critical Path` | One block per action on the critical path |
| `Action count` | Parallelism over time — a low flat line with high wall time is queuing, not work |
| `CPU usage (Bazel)` | Whether the server is compute-bound at all |
| `Garbage Collector` | Pause bands; if these dominate, go to [Memory and GC](#memory-and-gc) |

**Skymeld has no labelled lane.** `--experimental_merged_skyframe_analysis_execution`
defaults `true` on 8.7.0 and 9.2.0, so analysis and execution genuinely overlap
and no official documentation describes how that renders. Infer it from
concurrent analysis and execution activity on the Main Thread; never claim a
visual signature, and never write guidance that frames Skymeld as opt-in. The
one live tunable is `--experimental_skymeld_analysis_overlap_percentage`
(default 100).

## Analysis-phase culprits

Each row names what to look for and the rule that prevents the recurrence. None
of these is diagnosed from the profile alone — the profile points, the second
command confirms.

| Signature in the profile | Confirm with | Cause | Rule |
|---|---|---|---|
| Long unexplained span, no matching action count | `--starlark_cpu_profile` + `pprof -top` names a loop | Depset built in a loop with the accumulator in `transitive=` — O(N²) | BZL-LARK-07 |
| Elevated CPU inside analysis, no execution growth | Same, top frame calls `depset.to_list()` | Flattening on an action-construction path | BZL-LARK-17 |
| `PackageMetrics.packages_loaded` dominating wall time relative to `TargetMetrics.targets_configured` | Read both metrics from the BEP | One giant `BUILD` file, or a wide `glob()` re-walking a large tree | BZL-ARCH-01; re-verify glob correctness with BZL-ARCH-02 / BZL-HERM-19 |
| No Starlark hotspot, high configured-target count | `bazel cquery 'deps(//target)' \| awk '{print $1}' \| sort \| uniq -c \| sort -rn` — any label with count > 1 is built under more than one configuration | Transition-induced duplication (`2^n` down a depth-`n` tree) | BZL-ARCH-19 to measure, BZL-ARCH-21 before "fixing" it with a reset transition |
| A recently converted macro that was expected to be faster | Nothing to run — the premise is false | Symbolic macros fix typing, visibility and naming; lazy evaluation is still unshipped at 9.2.0 | BZL-LARK-22 |

**Never count `aquery` output lines as an action count.** Two actions whose
outputs share an `execPath` under different configurations render identically
(BZL-ARCH-20). EMPTY output from the `cquery` duplication command means the
query matched nothing — check the label before reading it as "no duplication".

There is **no published threshold** at which a wide `glob()` becomes a
performance problem as opposed to the correctness problem BZL-HERM-19 covers.
Treat `packages_loaded` dominating `targets_configured` as a reading heuristic,
and say so rather than inventing a number.

## Memory and GC

1. `bazel info used-heap-size-after-gc` — the aggregate.
2. To attribute heap to a rule class, memory tracking must be on for **every**
   invocation including the one that starts the server: two startup flags,
   `--host_jvm_args=-javaagent:<path-to-java-allocation-instrumenter.jar>` and
   `--host_jvm_args=-DRULE_MEMORY_TRACKER=1`. Forgetting either on one
   invocation restarts the server and loses the tracking state — the symptom is
   an empty or absent table, not an error.
3. `bazel dump --rules` prints `RULE / COUNT / ACTIONS / BYTES / EACH`.
   `bazel dump --skylark_memory=<path>` writes a pprof-compatible heap profile;
   `pprof -text -lines` or `pprof -flame` gives per-callsite byte attribution.
4. Cap with `--host_jvm_args=-Xmx<n>g`. On a repeat OOM add `--heap_dump_on_oom`.
5. Trading memory for speed, in increasing severity: `--discard_analysis_cache`,
   `--nokeep_state_after_build`, `--notrack_incremental_state`. The middle ground
   that keeps speed is `--experimental_enable_skyfocus` with
   `--experimental_working_set`, with
   `--experimental_skyfocus_dump_post_gc_stats` printing the heap delta.

## Session traps

A build that re-analyses everything with no source change is usually the session,
not the code:

- Mixing `bazel build -c opt` with `bazel cquery` in one session makes each
  discard the other's analysis cache. Documented behaviour, unrelated to sources.
- Many command-line flag differences between consecutive invocations discard the
  analysis cache the same way.

Prove it rather than guessing: add `--noallow_analysis_cache_discard` and re-run
the sequence. A hard error names the discard; **no error means the discard is not
happening** and the cost is elsewhere.

## Third-party analysis

EngFlow's Bazel Invocation Analyzer consumes the **JSON trace profile**
(`bazel run //cli -- /path/to/bazel_profile.json.gz`), not the execution log.
Its suggestion providers are themselves a checklist of mechanical symptoms worth
knowing by name: `Bottleneck`, `BuildWithoutTheBytes`, `CriticalPathNotDominant`,
`GarbageCollection`, `IncompleteProfile`, `InvestigateRemoteCacheMisses`, `Jobs`,
`LocalActionsWithRemoteExecution`, `MergedEvents`, `NegligiblePhase`,
`NoCacheActions`, `Queuing`, `UseRemoteCaching`, `UseSkymeld`.

Do not point it at an execution log, and do not point a log-diffing tool at a
trace profile — they read different artefacts and share nothing.

Primary reference for the lane names and the profile's own semantics:
<https://bazel.build/advanced/performance/json-trace-profile>.
