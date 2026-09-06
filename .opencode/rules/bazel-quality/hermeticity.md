---
title: Hermeticity and Determinism
summary: The BZL-HERM family — what an action may write, sandbox strategy and the *_env flags, glob and generated-file drift, and the two-run proof a build is actually deterministic
---

# Hermeticity and Determinism

Owns `BZL-HERM`: sandboxing and its per-OS strategies, action-key stability
(`--action_env`, `--define`, absolute paths, timestamps, stamping), toolchain
hermeticity, `glob()` expansion, checked-in generated files, and the procedures
that prove a build reproducible. It does not own cache configuration — `BZL-CACHE`
owns the cache flags, Build-without-the-Bytes, eviction and execution-tag
cacheability — nor test flakiness and cached test results (`BZL-TEST`), module
extensions and lockfiles (`BZL-MOD`), buildifier warning names and deprecated
provider APIs (`BZL-LARK`), flag flip archaeology (`BZL-FLAG`), or per-language
toolchain mechanics (`BZL-CC`, `BZL-PY`, `BZL-JS`, `BZL-RUST`, which cite
BZL-HERM-24 rather than restating it).

Contents: [What an Action May Write](#what-an-action-may-write) ·
[Globs and Checked-In Generated Files](#globs-and-checked-in-generated-files) ·
[The Committed rc File](#the-committed-rc-file) ·
[Proving Determinism](#proving-determinism) ·
[Toolchains and the Loading Phase](#toolchains-and-the-loading-phase) ·
[Sandbox Strategy and What You May Claim](#sandbox-strategy-and-what-you-may-claim) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Every version-bound claim below was measured 2026-09-06 against real Bazel
8.7.0, 8.8.0 and 9.2.0 binaries on a **Linux** host with working unprivileged
user namespaces, where the selected strategy was `linux-sandbox`; a row whose
result could differ off Linux says so. Ruleset versions this file binds:
rules_python 2.3.3, rules_cc 0.2.22, rules_shell 0.8.0, protobuf 36.1.bcr.1,
bazel-contrib/bazel-lib 3.7.2, toolchains_llvm 1.9.0, hermetic_cc_toolchain
4.3.0. Before citing any flag, re-read it at **your** pinned version on **both**
help surfaces — `bazel help <command> --long` *and* `bazel help startup_options`
— because a flag lives on exactly one of them: `--output_user_root` is a startup
option, so a `build --output_user_root=…` line is mis-scoped, and
`--experimental_remote_repo_contents_cache` (8.8.0+) is invisible to
`help build --long` entirely. Replace `<ci-dir>` below with the directory
holding your CI workflow files. **Every grep in this file is blind to
generated-repository `BUILD` and `.bzl` text** — a repository rule that writes
Starlark into an external repo is out of reach of all of them, and a clean grep
says nothing about it.

## What an Action May Write

The check for this whole block: read every `cmd`, `cmd_bash` and
`ctx.actions.run_shell(command = …)` string the change touches, plus every
stamping site. The shortlist that finds them:
`grep -rn -e 'genrule(' -e 'run_shell(' -e 'ctx\.info_file' -e 'ctx\.version_file' --include='BUILD*' --include='*.bzl' .`
— EMPTY means this section does not bind to the repository at all. The
normative source is Bazel's [General Advice for
genrules](https://bazel.build/reference/be/general).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-10 | Never let a genrule or custom action write a timestamp, PID, UID/GID, hostname, username, or absolute path into a declared output. | Bazel's own General Advice names this the literal cause of "Bazel not rebuilding a genrule you thought it would". Measured mechanism for the absolute-path half: `$PWD` read at run time resolves inside the sandbox exec root, whose parent `output_base` is a hash derived from the workspace's absolute path — the same action emits different bytes at two checkouts while its key stays identical, and a cache hit then serves the other checkout's bytes verbatim. | `grep -rl -e 'genrule(' -e 'run_shell(' --include='BUILD*' --include='*.bzl' . \| xargs -r grep -n -e date -e '\$\$' -e whoami -e 'id -u' -e hostname -e '\$[(]pwd[)]' -e '\$PWD' -e /home -e /Users -e /root` over the `cmd` strings the first grep found. Empty `xargs` input (no genrule/run_shell action) or an empty grep result both read as pass, identically on 8.7.0 and 9.2.0. Generated-repository `BUILD` and `.bzl` text is out of this grep's reach. | MUST |
| BZL-HERM-11 | Sort every set, dict or map before it is serialised into a command line, a template, or a generated file. | `be/general` requires stable ordering for sets and maps by name. Starlark dicts iterate in insertion order, so the risk is not in the `.bzl` — it is in the tool the action runs, whose hash and set order is a process artifact that no Bazel input contract records and no rebuild reproduces. | `grep -rn -e '\.items()' -e '\.keys()' -e 'set(' <the tool sources the actions run>` — each hit feeding written output with no `sorted()` between is the violation. EMPTY = pass, but this is absence of evidence, not proof: say so when reporting it. | MUST |
| BZL-HERM-13 | Treat any action that shells into a foreign build system (`make`, `cmake`, `cargo build`, `npm run`, `go build`) as non-deterministic until both an execution-log diff and an output-digest diff pass, and pin its `PATH` with `--action_env=PATH=<fixed literal>`. This becomes MUST the moment its output feeds a cache another machine reads. | Nested build systems carry none of Bazel's purity obligations, and `be/general` states that any change to `PATH` re-executes the command on the next build — an unpinned `PATH` that drifts with the CI runner image is a whole-genrule-set cache-buster with no code change involved. A fixed literal is machine-stable, so this does not collide with BZL-HERM-06. | `grep -rn 'genrule(' --include='BUILD*' .`, then read each `cmd` for a foreign build binary. EMPTY on that grep = pass. If any exist, `grep -n 'action_env=PATH' .bazelrc*` — EMPTY there = finding. Same on both majors. | SHOULD |
| BZL-HERM-17 | Take `ctx.info_file` (stable status) only for a value that must force a rebuild, and `ctx.version_file` (volatile status) only for a value that may go stale. | Bazel deliberately does not invalidate dependents when only the volatile status file changes; picking the volatile file for a rebuild-worthy value reproduces bazelbuild/bazel#5573 exactly — the artifact simply does not rebuild, and nothing reports it. | `grep -rn -e 'ctx\.info_file' -e 'ctx\.version_file' --include='*.bzl' .`, then read each call site against "does this value need rebuild-on-change?". EMPTY = pass (no stamping in use). Unchanged 8.7.0 → 9.2.0. | MUST |
| BZL-HERM-18 | Never treat `--stamp` as a target's determinism control. | `--stamp` defaults `false` on both majors, but most `*_binary` rules ship `stamp = -1` ("defer to the flag") while `*_test` rules force `stamp = 0` — a target flips between stamped and unstamped from the invocation alone, with no change to its definition, and bazelbuild/bazel#14341 (open since 2021) confirms there is still no contract between Bazel and rulesets on what `--stamp` means. | `bazel query 'attr(stamp, "-1", //...)'` enumerates every target deferring to the flag — the value must be quoted, since Bazel's query lexer reads an unquoted leading `-` as a minus token and the query never evaluates; the same quoting applies to every numeric or dash-leading `attr()` value. Identical output on 8.7.0 and 9.2.0. EMPTY = pass; exit 2 with empty stdout is a broken command, never a pass. Otherwise check that no CI job builds a member of that set both with and without `--stamp` and compares the results. This row owns the `stamp = -1` / `--stamp` fact; BZL-RUST-13 cites it rather than restating it. | MUST |

```python
# wrong — the tool's own set order reaches the output bytes
for dep in deps_set:
    out.write(dep + "\n")
```

```python
# right — one sorted() between the set and anything written
for dep in sorted(deps_set):
    out.write(dep + "\n")
```

## Globs and Checked-In Generated Files

Two checks catch this block. For the glob half, `bazel query` before and after
the change — never a re-read of the Starlark, which cannot show it. For the
generated half, intersect `git ls-files` with every rule's `outs`/`out_file`.
The repository gate runs `bazel test //...`; every `diff_test` has to be inside
that pattern, which is exactly what BZL-HERM-21 protects. `glob()`'s
package-boundary semantics are normative in [the BUILD functions
reference](https://bazel.build/reference/be/functions).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-19 | Never conclude a `glob()`'s match set from its pattern; re-verify the expansion with `bazel query` whenever a change adds, moves or removes a `BUILD`/`BUILD.bazel` file anywhere under it. **pinned** | A glob silently stops matching inside any subdirectory that becomes its own package — no error, no warning, no diagnostic, just fewer sources from that moment on. The reference states plainly that the result of a glob depends on the existence of `BUILD` files. | `bazel query 'kind("source file", deps(//path/to:target))'` before and after the change, on either major. An unchanged result set = pass; a shrinking one = finding. EMPTY output from the query at all = the target has no source deps and the check did not run — re-scope the query, never read it as a pass. Neither backstop covers this: `--incompatible_disallow_empty_glob` catches the zero-match count only, and buildifier's `constant-glob` catches pattern shape only (measured disjoint; the warning taxonomy is `BZL-LARK`'s). | MUST |
| BZL-HERM-20 | Pair every checked-in generated file with a `write_source_files`/`diff_test`-style target whose failure message names the exact `bazel run` command that fixes it. **pinned** | `bazel build` and `bazel test` cannot write the source tree by construction, while `bazel run` sets `BUILD_WORKSPACE_DIRECTORY` — so a stale generated file surfaces only as an unexplained CI failure unless a paired target both detects the drift and prints the fix. | Intersect `git ls-files` with every rule's `outs`/`out_file`; every path in the intersection must be named by a `diff_test`/`write_source_files` target. EMPTY intersection, or every member covered, = pass. This is the mechanism, not the dependency: a hand-rolled `bazel_skylib` `diff_test` plus an updater `sh_binary` that copies into `$BUILD_WORKSPACE_DIRECTORY` satisfies it identically to bazel-lib 3.7.2. | MUST |
| BZL-HERM-21 | Never tag a `diff_test`/`write_source_files` target `manual`, and never exclude it from `//...`. | That test's failure *is* the safety mechanism the whole pattern rests on; excluding it silently reintroduces the "stale file, no signal" problem the pattern exists to close — and it is the first move an agent reaches for to turn a red CI green. | `grep -rn -A20 -e 'diff_test(' -e 'write_source_file' --include='BUILD*' . \| grep -E 'tags\s*=.*"manual"'` — EMPTY = pass, any hit = finding, on both majors. A platform exclusion via `target_compatible_with` is not a violation; `tags = ["manual"]` is. | MUST |
| BZL-HERM-22 | Name the Bazel major a golden-diff or snapshot test is authoritative for, in a comment adjacent to the CI condition that excludes the other legs. **pinned** | A golden pinned to one major leaves the rest of a multi-major matrix with zero signal on the surface most likely to drift across majors — stardoc emitting an extra `repo_mapping` row under Bazel 9 is the live shape. The bar is "name the gap", not "close the gap": one authoritative leg plus a comment naming the major is the correct answer for a deliberately held pin. | `grep -rn -B3 'matrix.bazel !=' <ci-dir>`, or the equivalent conditional-skip expression in your CI system. A guarded exclusion with no adjacent comment naming both the reason and the affected major = finding. EMPTY = pass only when the matrix is single-major; with two majors and no guard, the golden is running on both and drift is already failing legs. | MUST |

## The Committed rc File

One grep answers this whole block, and every hit is read against `.bazelversion`
and every Bazel version in the CI matrix:

```bash
grep -rn -e strict_action_env -e strict_repo_env -e sandbox_default_allow_network \
  -e sandbox_debug -e action_env -e repo_env -e BAZEL_DO_NOT_DETECT .bazelrc* <ci-dir>
```

The two environment defaults flip between the majors and nothing announces it,
so an unpinned matrix spanning both runs each leg under a different policy.
`BZL-CACHE` owns the cache flags these sit beside; a variable's effect on
*cacheability* is its row, the variable's effect on the *action key* is this one.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-01 | Pin `--incompatible_strict_action_env=true` in the committed `.bazelrc` whenever any Bazel 8.x version is in the supported range or the CI matrix, and remove the line only once the floor is ≥9.0.0 everywhere the rc file applies. Never quote one PATH value as "the" static PATH. | The flag defaults `false` across all of 8.0.0–8.8.0 and `true` only from 9.0.0, so an unpinned multi-major matrix runs each leg under a different inheritance policy with no configuration difference to explain it. Measured, the leak is exact and narrow: non-strict inherits the client's `PATH` and `LD_LIBRARY_PATH` and nothing else (an arbitrary exported variable never leaks in either mode, and `HOME` is absent from the action environment in every configuration), and 9.2.0's default `env` output is byte-identical to 8.7.0 run with the flag on. The static PATH strict mode installs is OS-shaped: `/bin:/usr/bin:/usr/local/bin` on POSIX, and on Windows the MSYS root parsed from the resolved `bash.exe` plus the Windows system directories — so the flip changes which PATH-construction branch every action takes there, not merely whether `LD_LIBRARY_PATH` is dropped. | `grep -rn incompatible_strict_action_env .bazelrc* <ci-dir>`, read against `.bazelversion` and every version in the matrix. EMPTY **with any Bazel-8 leg present = finding** (undocumented cross-major divergence); EMPTY with a ≥9.0.0 floor everywhere = pass. Live value at any pin: `bazel help build --long \| grep -A1 -- '--[no]incompatible_strict_action_env'`. | MUST |
| BZL-HERM-02 | Set `--sandbox_default_allow_network=false` for the build; never read a green Windows leg, or any leg running `processwrapper-sandbox` or `local`, as evidence that it held; and never cite this flag as a control over a repository-rule fetch. | The flag defaults `true` on 8.7.0, 8.8.0 and 9.2.0 — every sandboxed action may reach the network unless told otherwise. Enforcement is measured real under `linux-sandbox` (a `curl` genrule returns `NONET` with the flag, `HTTP/2 200` without) and source-settled under `darwin-sandbox`, whose profile emits `(deny network*)`; it is impossible on Windows, which has no strategy with a network namespace to revoke. It is also `execution`-tagged, so it never reaches `repository_ctx.download`/`.execute`, which run unsandboxed in the loading phase (bazelbuild/bazel#7764, open since 2019) — an agent pattern-matching "sandbox plus network" onto a fetch problem ships a flag that does nothing. | `grep -rn sandbox_default_allow_network .bazelrc* <ci-dir>` → EMPTY = **finding** (network silently allowed). Then `bazel build --sandbox_default_allow_network=false //...` on a Linux or macOS leg: the first failing target names the network-dependent action; a clean build there = pass, a clean build only on Windows = no signal at all. Before calling a green run a pass, read the per-action escape hatches: `tags = ["requires-network"]` overrides the block while staying sandboxed, and `tags = ["no-sandbox"]` or `["local"]` force the `local` runner where the flag is a no-op (all three measured on both majors). | MUST |
| BZL-HERM-03 | Never commit `--sandbox_debug` to a `.bazelrc`, a personal rc file, or a CI workflow. | It suppresses sandbox-directory cleanup by design, so leaving it on is an unbounded disk leak on every future invocation; it is a single-session inspection tool, not a setting. | `grep -rn sandbox_debug .bazelrc* <ci-dir>` — EMPTY = pass, any hit = finding. Default `false` on 8.7.0 and 9.2.0 alike. | MUST |
| BZL-HERM-04 | Use `--repo_env=NAME=VALUE`, never `--action_env=NAME=VALUE`, for anything a repository rule or module extension must read. | `--incompatible_repo_env_ignores_action_env` defaults `false` on all of 8.x and `true` from 9.0.0, so a repo rule reading an `--action_env` value works by accident on Bazel 8 and returns empty on Bazel 9 — no error, no warning, just a silent fallback to the default. | For every name set via `--action_env` in any rc file or workflow, `grep -rn 'getenv(' --include='*.bzl' .` and confirm no repository-rule or module-extension call site expects it. EMPTY (no overlap, or no `--action_env` at all) = pass. Generated-repository `.bzl` text is out of this grep's reach, and `environ=` on `repository_rule` is deprecated in favour of `getenv` on both majors. | MUST |
| BZL-HERM-05 | Never describe `--repo_env` as restricting what a repository rule may read. | Bazel's own flag help, verbatim on 8.7.0 and 9.2.0, says repository rules "see the full environment anyway"; `--repo_env` guarantees invalidation tracking for the named variables and builds no allowlist, so a hardened-environment claim resting on it is false. Only `--experimental_strict_repo_env` (added 8.6.0) restricts anything, and it defaults `false` on both majors with no announced graduation. | `grep -rn experimental_strict_repo_env .bazelrc*`. EMPTY reads: **the repository rules see the full client environment, full stop** — not a partial mitigation. Then read the repository's own prose for any "`--repo_env` restricts…" claim; a hit is a finding, corrected to "guarantees invalidation tracking for the named variables". | MUST |
| BZL-HERM-06 | Do not add `--action_env=<VAR>` for a variable whose value differs between machines, in any repository that shares a cache across users or machines. | The flag's own reference warns this "can prevent cross-user caching if a shared cache is used" — the exact opposite of the intended effect, because the value enters the action key. The 8.x default leak set is the same shape: a machine-local `LD_LIBRARY_PATH` reaches every action with nobody configuring it. | `grep -n action_env .bazelrc*`, then read each named variable for machine-stability — a `$HOME`-derived path or a `$PWD` capture fails, a fixed literal or a semantic version string passes. EMPTY (no `--action_env` at all) = pass. Both forms (`NAME=VALUE` and bare `NAME` to inherit) behave identically on 8.7.0 and 9.2.0; only Bazel 9 adds the `=NAME` form that unsets. | MUST |
| BZL-HERM-07 | In a repository with zero `cc_*` targets and no plan to add any, set `common --repo_env=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1`; never spell it as `--define`, `--action_env`, or a loadable Starlark symbol. | The C++ autodetection probe runs at repository and module-extension setup whether or not any `cc_*` target exists, and this environment variable — read via `repository_ctx.os.environ` in rules_cc's `cc/private/toolchain/cc_configure.bzl`, not in Bazel core — is its only documented off switch. The wrong spellings all parse and do nothing. | `grep -rn -e 'cc_binary(' -e 'cc_library(' -e 'cc_toolchain(' --include='*.bzl' --include='BUILD*' .` — EMPTY means no C++ targets and the rule applies. Then `grep -n BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN .bazelrc*` — EMPTY = finding. Confirm with `bazel clean --expunge && bazel query '@local_config_cc//...'`: an error or empty result = pass, on both majors. | SHOULD |

```bash
# wrong — both parse, neither reaches repository_ctx.os.environ
build --define=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1
build --action_env=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1
```

```bash
# right — the loading phase is the only phase that reads it
common --repo_env=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1
```

## Proving Determinism

Two independent runs, two independent comparisons. Neither one alone answers
the question, and the execution log cannot be made to answer the second:

```bash
bazel clean --expunge
bazel build --execution_log_compact_file=/tmp/a.log //...
sha256sum bazel-out/<config>/bin/*                       # capture, then repeat both
```

Build the parser once from a Bazel source checkout
(`bazel build src/tools/execlog:parser`), render each log with
`bazel-bin/src/tools/execlog/parser --log_path=… --output_path=…`, then `diff -u`
the two renders *and* the two digest captures. Bazel's own remote-caching page
publishes the same two-run and two-machine recipes, derived independently.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-15 | Diagnose an unexplained rebuild by diffing two `--execution_log_compact_file` runs through the execlog parser — never by re-reading the `BUILD` file, and never with `--explain` — working the causes in cost order: log diff, environment tier, command line, output ordering, local-versus-remote divergence, nested build systems, `/dev/urandom` last. Use the compact log for any new tooling, never `--execution_log_json_file`. **pinned** | The compact log records the action's actual resolved command line, environment and input digests — the only reliable view of an undeclared read — and the reference states it is significantly smaller and cheaper to produce than the JSON form. `--explain` cannot substitute: it reasons only from the local dependency checker and was measured reporting `no entry in the cache (action is new)` for actions that were confirmed disk-cache hits. | **An EMPTY diff reads: the action key is stable. It does NOT read "the build is deterministic"** — pair it with BZL-HERM-30 before concluding anything. A non-empty diff names the differing input, env var or argument directly. An action whose owning target hits the persistent local action cache is **absent from the log entirely**, so absence is a signal rather than a bug — identical on 8.7.0 and 9.2.0; the execlog parser/converter flag traps are BZL-CACHE-16's. | MUST |
| BZL-HERM-30 | Pair every execution-log diff with an independent two-run output-digest comparison, and never let an empty log diff stand as a determinism claim on its own. | Measured on 8.7.0: two clean builds whose `commandArgs`, `inputs[].digest` and `environmentVariables` were identical for every action still produced three output files with different SHA-256s. The log answers "did the declared inputs or the command change"; nothing in its fields can answer "did the output change for the same declared inputs", so a reviewer reading only a clean diff concludes the opposite of the truth. | The `sha256sum` capture above, run twice and diffed, in addition to the log diff. Both empty = pass. **A non-empty digest diff with an EMPTY log diff = finding, and the strongest kind** — the action is non-deterministic with a stable key, so the cache will serve one run's bytes forever. When diffing logs, strip the `metrics` object (`startTime`, `executionWallTime`, `totalTime`, volatile on every action) and **never** strip `actualOutputs[].digest`, which is exactly this signal. Cross-checked on 9.2.0. | MUST |
| BZL-HERM-31 | Never read a stable action key as proof that an action's output is portable across checkouts; name the ambient state the action reads at run time instead. | Measured on 8.7.0 and 9.2.0: two byte-identical checkouts at different absolute paths, sharing one `--disk_cache`, produced byte-identical action digests and the second served every action as a `disk cache hit`, because `$(location)`/`$(execpath)` resolve to exec-root-relative strings at analysis time and no absolute path enters the hashed material. Yet the served output contained the *other* checkout's `output_base` hash. Stable key, non-portable bytes — the failure a "the keys match, so we are fine" review misses. | Build the same target from two different absolute paths against one shared `--disk_cache`: a `disk cache hit` on the second proves the key is path-independent. Then `grep -rn -e /home -e /Users -e output_base <the served output>`. Cache hit **plus** a host path in the served bytes = finding, routed to BZL-HERM-10; EMPTY = pass. Two measured non-fixes: `--experimental_output_paths=strip` is opt-in per action through `supports-path-mapping`, which `genrule` never sets, and had zero effect on either major (9.2.0 also dropped its `content` mode); and `$$(pwd)` inside a genrule `cmd` is recorded literally and stays key-stable, while `--action_env=WS=$PWD` or `--define=WS_PATH=$PWD` destabilises the key for any action that consumes the value. | MUST |
| BZL-HERM-23 | Do not describe a deliberately uncached CI job as proving determinism. | It proves the build is correct starting from an empty store; it compares no action keys and no output digests, so it would pass every run with a fully non-deterministic action. Naming it a determinism job retires the real check before anyone writes it. | Reading heuristic over the job: does it compare two independent action-key or output-digest captures, or only assert an exit code and the presence of outputs? No comparison step = **finding** — re-scope the job's name and its documentation to "cold-store correctness". EMPTY (no uncached job at all) = not applicable, and the determinism question is simply unanswered. | SHOULD |

## Toolchains and the Loading Phase

Caught by reading `MODULE.bazel`'s registration order and by re-fetching from
cold: a warm fetch never re-invokes the calls this section hunts, so
`bazel clean --expunge` is load-bearing in every command here. On 9.x an
additional loading-phase trap sits beside these: `--incompatible_autoload_externally`
defaults empty, so a bare `cc_library`, `py_library`, `sh_binary` or
`proto_library` fails outright until an explicit `load()` is added — that row is
`BZL-FLAG`'s, and the pinned fixes are rules_cc 0.2.22, rules_python 2.3.3,
rules_shell 0.8.0 and protobuf 36.1.bcr.1.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-24 | Treat rules_cc's and rules_python's default toolchain configuration as host-installed and non-hermetic until an explicit hermetic toolchain is registered ahead of it — "the repo uses Bazel" does not imply "the repo is hermetic". | A peer-reviewed 150-million-syscall study across 70 real Bazel projects found zero with a fully hermetic build, and named the official rulesets' own defaults as the largest single documented cause: 38.1% of non-hermetic top-level toolchains come from the default configuration of official Bazel rules. | rules_python: `grep -n 'python.toolchain' MODULE.bazel` — a registration carrying an explicit `python_version` is the pass. EMPTY means "hermetic but unpinned", never "non-hermetic": rules_python 2.3.3 registers a prebuilt soft-default toolchain for any root module that never calls it, whose rolling version moved 3.11 → 3.14 in four months (that reading is `BZL-PY`'s). rules_cc: a hermetic toolchain module (toolchains_llvm 1.9.0, hermetic_cc_toolchain 4.3.0) registered; none found in a repository with real `cc_*` targets = finding. Not applicable where neither target kind exists. Same on both majors. | MUST |
| BZL-HERM-16 | Enumerate a repository rule's actual host-touching calls with `bazel clean --expunge` plus `--experimental_workspace_rules_log_file` before trusting a code read. | `execute`, unchecksummed `download`/`download_and_extract`, `which`, `.os` and unconstrained `.symlink` are exactly the operations Bazel's own [remote/workspace](https://bazel.build/remote/workspace) page names non-hermetic, and a code read misses dynamically constructed calls. | `bazel clean --expunge && bazel build --experimental_workspace_rules_log_file=/tmp/wsl.log //... && bazel-bin/src/tools/workspacelog/parser --log_path=/tmp/wsl.log > /tmp/wsl.txt`, then `grep -c -e '"which"' -e 'sha256: ""' /tmp/wsl.txt`. 0 matches (EMPTY) = pass; any match names the non-hermetic call. An EMPTY log file is not a pass: it means no repository rule ran under that target pattern — re-scope it, or check the expunge really happened. Flag name and `experimental_` status unchanged on 8.7.0 and 9.2.0. | SHOULD |
| BZL-HERM-28 | Reject a proposed fix that reintroduces `WORKSPACE`, `local_repository()`, or a top-level `cc_configure()` on a repository targeting Bazel 9. | Bazel 9.0.0 deleted the WORKSPACE support code outright rather than disabling it, and `--enable_workspace` is a no-op — measured, it is gone from both `help build --long` and `help startup_options` on 9.2.0. This is dead machinery, not legacy-but-working code, so the proposal cannot work at all; on a Bazel-8 pin the same hit is legacy-but-working and is a migration item, not a defect. | `ls WORKSPACE WORKSPACE.bazel 2>/dev/null` plus `grep -rn -e 'local_repository(' -e 'cc_configure(' --include='WORKSPACE*' --include='*.bzl' .` — EMPTY = pass; any hit outside an explicit, commented compatibility shim = finding on a 9.x target. Era grounding: the [9.0.0 release notes](https://github.com/bazelbuild/bazel/releases/tag/9.0.0). | MUST |

## Sandbox Strategy and What You May Claim

Caught by `--sandbox_debug` on one build plus the execution log's `runner` field
per action, and by reading the repository's own prose against what the strategy
actually does. The three strategies do not isolate the same things, and the
[sandboxing reference](https://bazel.build/docs/sandboxing) documents none of
Windows. Measured on Linux only.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-HERM-26 | Name which sandbox strategy actually ran, per action and not per build, before calling a build sandboxed; and never mount a real host directory writable (`--sandbox_writable_path`, `--sandbox_add_mount_pair`) where an empty `--sandbox_tmpfs_path` would serve. | `linux-sandbox` remounts the whole host root read-only (~130 mount points, measured) and allowlists only `/dev/shm`, `/tmp` and the execroot — host `/usr/lib`, `/home` and `/etc/hostname` stay readable, which is the mechanism behind GNU-versus-BSD tool mismatches. `darwin-sandbox` is not its equal on reads: its `sandbox-exec` profile begins `(allow default)` and denies only `file-write*` and, conditionally, `network*`. `processwrapper-sandbox`, the only cross-platform strategy, enforces nothing beyond "no undeclared-input read". A mount is worse than it looks: changing `--sandbox_add_mount_pair`'s source directory was measured to be a null build in-session and a stale disk-cache hit across `--expunge`, so the isolation level is not part of the action key. | `bazel build --sandbox_debug <target> 2>&1 \| grep -i sandbox` and read which strategy was selected — EMPTY (no strategy surfaced) = the isolation level is unconfirmed, treat as finding. Cross-check per action against the execution log's `runner` field, and `grep -rn -e no-sandbox -e '"local"' --include='BUILD*' .` for tags that opt individual actions out (both measured to force the unsandboxed `local` runner regardless of the build's strategy). Then `grep -rn -e sandbox_writable_path -e sandbox_add_mount_pair .bazelrc*` — EMPTY = pass; each hit must name a scoped, commented path. On a Windows-only leg the rule is **vacuous, not unmet**. | SHOULD |
| BZL-HERM-34 | Before diagnosing a "runfiles are stale or missing on Windows" report, name whether the entry is directory-shaped or file-shaped — they use different mechanisms and only one is gated on a flag. | `WindowsFileSystem.createSymbolicLink` is a three-way branch: a directory target always becomes an NTFS **junction** (no privilege needed, regardless of `--windows_enable_symlinks`); a file target becomes a real **symlink** only with that flag plus Developer Mode or Administrator, and otherwise **silently becomes a full copy**. A tool walking the tree with POSIX `is_symlink()` semantics gets inconsistent answers across the three, so "the flag is set, therefore the tree is symlinks" is wrong for every directory entry, and "the flag is unset, therefore there is no tree" is wrong too. | Reading heuristic on the failing entry: a directory (an external repo's tree, a sub-binary's `.runfiles`) or a single file? Then `grep -rn -e windows_enable_symlinks -e enable_runfiles .bazelrc* <ci-dir>` — read that against the pinned rulesets too, because a ruleset may force `--enable_runfiles` at the rule level and make an EMPTY grep a false finding (rules_python ≥1.9.0 does; that row is `BZL-PY`'s). **EMPTY grep alone is not a conclusion.** `--enable_runfiles` default `auto`, byte-identical on 8.7.0 and 9.2.0. Source-read only: no Windows runner was exercised. | MUST |
| BZL-HERM-29 | State the three environment tiers separately wherever a repository documents a hermeticity posture: build and host actions, repository rules and module extensions, and test actions. | A blanket "we run with a strict environment" is false for two of the three — repository-rule strictness is experimental and off by default, and test actions have no strict mode at all (bazelbuild/bazel#29472, open at 2026-09-05). On Bazel 8 there is not even a removal affordance: `--test_env=NAME=` is an assignment to the empty string, not an unset; the explicit unset `--test_env==NAME` arrives only on 9.x, uniformly across the four `*_env` flags. | Reading heuristic: does the hermeticity documentation distinguish the three tiers by name? A blanket claim with no distinction = finding. No such claim anywhere = not applicable, not a pass — the posture is simply undocumented. | CONSIDER |

## Gaps

- Every measurement here ran on Linux under `linux-sandbox`. `darwin-sandbox` and every Windows row are source-read only; `processwrapper-sandbox` was never exercised, and darwin's localhost carve-out shares the host's real loopback (bazelbuild/bazel#11325, open), so a port-binding test can behave differently there.
- The mount set a real rules_js or rules_python build needs under `--experimental_use_hermetic_linux_sandbox` — interpreter shared libraries beyond libc, `/etc/resolv.conf`, CA bundles — is unmeasured. The bare-shell set on a usrmerge host is all four of `/usr`, `/bin`, `/lib`, `/lib64`. This gap is why BZL-HERM-26 is SHOULD and not a gate.
- Action-key path stability was measured for genrule and native-substitution shapes only. An absolute path built by a custom rule into a *tool argument*, rather than read inside the command body, was never probed and could plausibly destabilise the key.
- `--sandbox_add_mount_pair`'s null-build and stale-cache-hit behaviour was measured on 8.7.0 and 9.2.0; 8.8.0, 9.0.0 and 9.1.0 were not probed.
- Whether `--incompatible_disallow_empty_glob` and buildifier's `constant-glob` together cover every glob defect is open: they are measured disjoint (zero-match count versus pattern shape) and neither sees the package-boundary shrink BZL-HERM-19 exists for.

## What Agents Get Wrong Here

1. **Asserting a flag default with no Bazel major attached** — above all
   "`--incompatible_strict_action_env` now defaults true", which holds only from
   9.0.0 and is false on every 8.x (BZL-HERM-01).
2. **Reading an empty execution-log diff as proof of determinism**, or reaching
   for `--explain`, which reported "action is new" for actions measured as
   disk-cache hits (BZL-HERM-30, BZL-HERM-15).
3. **Tagging a failing `diff_test`/`write_source_files` target `manual` to turn
   CI green**, which deletes the safety mechanism instead of regenerating the
   stale file (BZL-HERM-21).
4. **Editing a `glob()` pattern and stopping there**, never noticing that a new
   `BUILD.bazel` elsewhere in the same change shrank the match set
   (BZL-HERM-19).
5. **Believing the sandbox blocks the network by default** — wrong on the
   default, wrong again on Windows, and wrong a third time for a
   repository-rule fetch no sandbox flag reaches (BZL-HERM-02).
6. **Believing `--repo_env=X` restricts a repository rule's environment to `X`**
   rather than guaranteeing invalidation tracking for it (BZL-HERM-05).
7. **Spelling a flag that no longer exists or has moved**:
   `--experimental_strict_action_env`,
   `--define=BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN=1`, a `load()` of that symbol, or
   `--experimental_reuse_sandbox_directories` — silently aliased to
   `--reuse_sandbox_directories`, default `true` on both majors, and a sandbox
   cost knob owned by `BZL-CC`, never a hermeticity control (BZL-HERM-07).
8. **Proposing a `WORKSPACE`, `local_repository()` or `cc_configure()` fix on a
   Bazel-9 target**, where the machinery was deleted rather than deprecated
   (BZL-HERM-28).
