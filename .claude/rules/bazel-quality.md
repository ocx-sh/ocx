---
paths:
  - "**/BUILD.bazel"
  - "**/BUILD"
  - "**/*.bzl"
  - "**/MODULE.bazel"
  - "**/MODULE.bazel.lock"
  - "**/REPO.bazel"
  - "**/WORKSPACE"
  - "**/WORKSPACE.bazel"
  - "**/WORKSPACE.bzlmod"
  - "**/.bazelrc"
  - "**/*.bazelrc"
  - "**/.bazelrc.*"
  - "**/.bazelversion"
  - "**/.bazelignore"
  - "**/*.star"
  - "**/*.scl"
summary: The Bazel quality index — the gate, the non-negotiables, and where the depth lives
keywords: bazel,starlark,bzlmod,module-extension,repository-rule,lockfile,remote-cache,remote-execution,rbe,disk-cache,hermeticity,sandbox,determinism,buildifier,gazelle,bazel-diff,target-determinator,rules_rust,rules_python,rules_js,rules_ts,rules_cc,protobuf,monorepo,ci,bazelrc,bazelisk
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# Bazel Quality

Traps, not maps. Everything here names a mistake that gets made without it;
the shape of any particular workspace is discoverable by reading its
`MODULE.bazel` and its packages, so it is not in this file.

Contents: [The Gate](#the-gate) · [Non-Negotiables](#non-negotiables) ·
[Rules This File Owns](#rules-this-file-owns) ·
[Where the Depth Is](#where-the-depth-is) · [Severity](#severity) ·
[Siblings](#siblings)

**Before trusting any rule below, read `.bazelversion` and know which major
you are on.** Four behaviours flip at 9.0.0 — `WORKSPACE` support is gone,
`--incompatible_autoload_externally` is empty so every `cc_*`, `py_*`,
`sh_*`, `java_*` and `proto_library` symbol needs an explicit `load()`,
`--incompatible_strict_action_env` defaults on, and `--repo_contents_cache`
defaults on — and every rule set here was measured on 8.7.0, 8.8.0 and 9.2.0
(2026-09-06). Where a rule names a version, that is the version it was
watched on. Where it names none, it held on both majors. Bazel's own docs,
ruleset READMEs and release notes were each measured wrong at least once
during that work; the binary's two help surfaces are the only authority for
whether a flag exists.

## The Gate

Run it after every change, cheapest first, and through the pin — `bazelisk`
reading `.bazelversion`, never a `bazel` on `$PATH`.

```bash
bazel build --nobuild //...            # loading + analysis; catches what no linter can (exit 1)
bazel run //:buildifier.check          # format + lint; gate on nonzero, never on a number
bazel mod deps --lockfile_mode=error   # lockfile fresh; exit 37 with no text is still the finding
bazel test //...                       # the CI verb; `bazel build` alone skips every typecheck test
```

The first line is not optional and not a substitute for the second. The
loading phase catches an undefined name in dead code, a `load()` of a symbol
the file does not export, and a bad builtin argument — buildifier exits 0 on
all three, and so does a gate that only formats. Neither line sees a
generated `.bzl` until some package loads it: a generator with no consumer
is checked by nothing.

Name the lint target whatever your `buildifier_prebuilt` pin generates and
chain all four into one named target CI invokes; a hand-copied step list
drifts from the local gate on day one. A task is done when a command, its
exit code and the Bazel version it ran under are all named. Narration is not
evidence.

## Non-Negotiables

Every line below blocks a merge at the severity of the depth row it cites;
where that row is SHOULD, the depth row governs and this line warns. IDs
resolve to the depth files in
[Where the Depth Is](#where-the-depth-is), where each rule carries its
rationale and verification.

| # | Rule | ID |
|---|---|---|
| 1 | Load every `cc_*`, `java_*`, `py_*`, `sh_*` and `proto_library` symbol explicitly, from its ruleset — Bazel 9 removed all of them from the global namespace, and a bare rule that builds on 8.x fails at load time on 9.x. | BZL-LARK-10, BZL-PY-20 |
| 2 | On a Bazel 9 pin no `WORKSPACE*` file, no `--enable_workspace` or `--enable_bzlmod`, and no 8-era autoload list; reject any snippet that configures through `WORKSPACE`, adds `bazel-toolchains` or `rbe_autoconfig`, or targets Bazel 6 — each fails the invocation, not the feature. | BZL-FLAG-12, BZL-FLAG-13, BZL-FLAG-14, BZL-CI-20, BZL-JS-02 |
| 3 | Gate on a nonzero exit status, never on a specific number and never as `buildifier_test`; run the loading phase as its own step, and let every CI job propagate its command's exit code — no `\|\| true`, no stdout scrape. | BZL-LARK-01, BZL-LARK-31, BZL-LARK-30, BZL-CI-07 |
| 4 | Never write a top-level `if` or `for` in any Starlark file, never merge depsets with `+`, `\|` or `.union()`, and declare `visibility(...)` in every `.bzl` — load visibility defaults to public whatever the BUILD target says. | BZL-LARK-15, BZL-LARK-11, BZL-ARCH-08 |
| 5 | Every generated `.bzl` or BUILD string has a consumer package the gate loads and one test that builds a target out of the generated repository. | BZL-LARK-25, BZL-LARK-30 |
| 6 | Never hand-type a digest in `MODULE.bazel.lock`; regenerate it with `bazel mod deps`, keep a JSON-aware merge driver on it, and run CI at `--lockfile_mode=error`. | BZL-MOD-02, BZL-MOD-03, BZL-MOD-04 |
| 7 | A repository rule reads the environment through `repository_ctx.getenv()` never `ctx.os.environ`, watches every `attr.label()` it reads, and passes `sha256` or `integrity` to every `ctx.download()`. | BZL-MOD-20, BZL-MOD-21, BZL-MOD-22 |
| 8 | Prove a flag exists on the pinned binary's two help surfaces (`bazel help <command> --long` and `bazel help startup_options`) before writing it anywhere; empty output is a stop signal, never a pass. Pin `.bazelversion` to an exact three-component version. | BZL-FLAG-11, BZL-FLAG-01, BZL-FLAG-03, BZL-CACHE-23 |
| 9 | No `--credential_helper` path, cache token or other executed or secret value in a tracked rc file; the personal rc is imported as the last non-comment line, and every lane an untrusted contributor can trigger runs with the cache write credential absent, not merely unused. | BZL-FLAG-19, BZL-CACHE-03, BZL-CACHE-04, BZL-CACHE-01, BZL-CACHE-02 |
| 10 | Never write a timestamp, PID, hostname, username or absolute path into a declared output, and pin `--incompatible_strict_action_env=true` while any 8.x leg exists. | BZL-HERM-10, BZL-HERM-31, BZL-HERM-01, BZL-HERM-04 |
| 11 | A test writes only under `$TEST_TMPDIR` or `$TEST_UNDECLARED_OUTPUTS_DIR`, resolves inputs through runfiles, and is checked for flakiness with `--runs_per_test=N --runs_per_test_detects_flakes` — never `flaky = True` or `--flaky_test_attempts`. | BZL-TEST-12, BZL-TEST-13, BZL-TEST-06, BZL-PY-25 |
| 12 | Never set `default_visibility = ["//visibility:public"]` outside the package that is the module's API surface, and key every `config_setting` on `constraint_values`, never on `values = {"cpu": ...}` alone. | BZL-ARCH-04, BZL-MOD-34, BZL-ARCH-16, BZL-ARCH-35 |
| 13 | The default pipeline is whole-repo `bazel test //...`; adopt a target-selection tool only after that job's median wall-clock is measured past roughly forty minutes, and list every legacy, worktree or parallel-checkout directory in `.bazelignore` — Bazel does not read `.gitignore`. | BZL-CI-01, BZL-CI-02, BZL-ARCH-26 |
| 14 | Never key CI retry logic on exit code 39 and never claim a cache outage fails a cache-only build — both were measured false on 8.7.0 and 9.2.0; match the log's error text instead. | BZL-CACHE-12, BZL-CACHE-26 |
| 15 | Rust: an explicit `lockfile =` on every crate-universe instance in every module, `rust.toolchain(versions = [...])` pinned in the root module, and repins through `CARGO_BAZEL_REPIN=1 bazel fetch --repo=@<repo>` — `bazel sync` was deleted at 9.0.0 and the ruleset's docs still print it. | BZL-RUST-01, BZL-RUST-31, BZL-RUST-03 |
| 16 | Python: pin the interpreter with `python.defaults` and `python.toolchain` and bind every `pip.parse` to it; never point a `py_test` at a pytest-style file without a pytest entrypoint — it passes forever having run zero tests. | BZL-PY-01, BZL-PY-03, BZL-PY-21 |
| 17 | TypeScript: name a `transpiler` on every hand-written `ts_project`, never list one `.ts` path in two targets' `srcs` (`TS5033: EPERM` is that collision), and run the generated `_typecheck_test` under `bazel test`. | BZL-JS-13, BZL-JS-16, BZL-JS-11, BZL-JS-12 |
| 18 | C++: confirm `layering_check` on the real compile command with `aquery`; a `.bazelrc` line, a green build and a clean IWYU report each prove nothing. | BZL-CC-12, BZL-CC-14 |
| 19 | Never reach green by weakening the check, and never ship a verification nobody has watched go red. | BZL-CORE-01, BZL-CORE-02 |

## Rules This File Owns

Three cross-cutting rules that belong to no single depth file. Everything
else is defined in a depth file and only cited here.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CORE-01 | Never reach green by weakening the check: no new `no-sandbox`, `no-cache`, `no-remote`, `local`, `manual`, `external` or `requires-network` tag, no `flaky = True`, no raised `timeout` or `size`, no widened `.bazelignore`, no `--lockfile_mode=off` or `--no<flag>` added to an rc file, and no edit to the gate's own config as part of a functional change. | The gate's whole value is that it can go red. A change that edits both the code and the check that judges it reports nothing and looks identical to a passing change; under Bazel the cheapest such edit is one tag, and a tag never warns. | `git diff --stat -- '.bazelrc' '*.bazelrc' '.bazelversion' '.bazelignore' 'MODULE.bazel'` — any hit in a change that is not itself a build-config change is the violation. Then `git diff -U0 -- '*BUILD*' '*.bzl' \| grep -nE '^\+.*("(no-sandbox\|no-cache\|no-remote\|local\|manual\|external\|requires-network)"\|flaky = True\|timeout = "(long\|eternal)"\|size = "(large\|enormous)")'` — a line added by this change is the finding. Then `git diff -U0 -- '.bazelrc' '*.bazelrc' '.bazelrc.*' \| grep -nE '^\+.*(lockfile_mode=off\|--no[a-z_]+)'` — any added line is the finding. Empty output from all three is the pass. Generated-repository text is out of their reach. | MUST |
| BZL-CORE-02 | A verification enters a rule table, a CI lane or a review only after it has been watched go red against a deliberately planted violation. | A check that cannot fail launders an unchecked change as a checked one, and reads exactly like a passing one forever. Bazel's failure modes here are specific and were each measured: a `buildifier_test` that passes a broken file at a pin below 8.5.1.3, a `--disk_cache`-only run that cannot see what a cache tag does, a `bazel query` over a generated repository no package loads that prints `INFO: Empty results` and exits 0, a flag declared absent because only one of the two help surfaces was read. | Copy the subject, break the thing the rule forbids, run the verification. A pass on the broken copy is the violation. | MUST |
| BZL-CORE-03 | State whether empty output means a pass or means the finding in every verification that is not self-evidently one or the other, and name the Bazel version it was run on. | Half the checks in this rule set are inverted — a missing lockfile mode, an absent public target on a published module, a `bazel help` that omits a startup option, an empty execution log are each *the finding*, not the pass; and a command that exists on 8.7.0 may be gone at 9.0.0. | Read each verification cell: one whose empty output is ambiguous, or that names no version where the majors differ, is the violation. | SHOULD |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is
filed under. One level deep; these files do not point at each other.

| Doing… | Read |
|---|---|
| Writing or editing a `.bzl`, a BUILD file, a macro, a rule, or the lint gate — including Starlark generated as a string | [bazel-quality/starlark.md](bazel-quality/starlark.md) |
| Touching `MODULE.bazel`, its lockfile, a module extension, a repository rule, or a registry submission — including a lockfile merge conflict | [bazel-quality/bzlmod.md](bazel-quality/bzlmod.md) |
| Writing a genrule, action or generated file, editing a glob, setting a sandbox or `*_env` flag, or claiming a build is deterministic | [bazel-quality/hermeticity.md](bazel-quality/hermeticity.md) |
| Wiring a remote or disk cache, a cache credential, a download-mode or eviction flag, an RBE readiness check, or reading a cache miss | [bazel-quality/caching.md](bazel-quality/caching.md) |
| Writing a test target, choosing its size, timeout, tags or shard count, faking a `ctx` in a Starlark test, or reading a coverage report | [bazel-quality/testing.md](bazel-quality/testing.md) |
| Adding or editing a CI job, matrix leg, release lockstep check, target-selection wrapper, or build event stream | [bazel-quality/ci.md](bazel-quality/ci.md) |
| Adding or moving a BUILD file, setting visibility, writing a `select()` or transition, or wiring a code generator | [bazel-quality/architecture.md](bazel-quality/architecture.md) |
| Bumping the Bazel version, writing a flag into an rc file, workflow or `PROJECT.scl`, or checking a ruleset's Bazel floor | [bazel-quality/flags.md](bazel-quality/flags.md) |
| Adding a crate dependency, repinning a lockfile, porting a `build.rs`, or wiring `rust_test`, clippy or prost under Bazel | [bazel-quality/rust.md](bazel-quality/rust.md) |
| Writing a `py_*` target, a `python.toolchain()` or `pip.parse()` call, a pytest suite, or running Gazelle for Python | [bazel-quality/python.md](bazel-quality/python.md) |
| Writing a `ts_project`, `npm_translate_lock`, a pnpm lockfile entry, a JS test target, or running Gazelle for JS | [bazel-quality/typescript.md](bazel-quality/typescript.md) |
| Writing a `cc_*` target, registering a C++ toolchain, enabling `layering_check` or a sanitizer, or wrapping a CMake or Autotools build | [bazel-quality/cpp.md](bazel-quality/cpp.md) |
| Editing `Cargo.toml`, `pyproject.toml`, `package.json` or `tsconfig*.json` themselves | `rust-cargo`, `python-packaging`, `typescript-packaging` (sibling sets — see below) |
| Deciding whether to adopt Bazel, or migrating a repository onto it | the `bazel-adopt` skill |
| A build that is already slow, missing the cache, nondeterministic, flaky or refetching | the `bazel-diagnose` skill |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in
the commit body. CONSIDER = Suggest: never blocks, never re-raised after a
decline.

Rules marked **pinned** in a depth file — the version pin, the lockfile
mode, the cache wiring order, a tag policy — encode an agreed decision
rather than a derivable fact. They are defaults an adopter may override,
once, in their own rc file or `MODULE.bazel`, never per target. Overriding
one is a decision; ignoring one is a violation.

Keep the Block list short enough that a blocked change is unusual. A rule
set where everything blocks teaches the reader to negotiate with all of it.

## Siblings

- **`rust-cargo`, `python-packaging`, `typescript-packaging`** — own
  `Cargo.toml`, `pyproject.toml`, `package.json`, `tsconfig*.json` and the
  lockfiles beside them. The language depth files here say what a
  *Bazel-specific* edit to one of those files must look like and name the
  file; this set never loads on it.
- **`rust-quality`, `python-quality`, `typescript-quality`** — own the
  source files a target here compiles. This set never loads on `*.rs`,
  `*.py` or `*.ts`, so the two never load together.
- **`bazel-adopt`** (skill) — the go/no-go gate and the migration order,
  run once per repository. **`bazel-diagnose`** (skill) — a build that is
  already wrong, routed by symptom. Bundled with this rule as
  `bazel-essentials`.
