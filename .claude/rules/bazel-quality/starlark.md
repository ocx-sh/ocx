---
title: Starlark and BUILD Shape
summary: The BZL-LARK family — the buildifier gate and how far it reaches, the loading-phase step, macro and rule authoring, and the generated Starlark nothing static ever sees
---

# Starlark and BUILD Shape

Owns `BZL-LARK`: the Starlark lint gate and what it does and does not reach, the
loading-phase step, macro/rule/aspect choice, symbolic-macro authoring, `.bzl`
load visibility, generated Starlark, and BUILD-file shape. It does not own how a
module extension or repository rule is declared, resolved or locked (`BZL-MOD`) —
only the Starlark text one writes into a generated repository, which is this
file's. Test rules and test-tree conventions are `BZL-TEST`, the CI jobs that
invoke the gate are `BZL-CI`, target and package visibility is `BZL-ARCH`, and
glob emptiness and sandbox mounts are `BZL-HERM`. None of it is restated here.
`.bzl` load-visibility gating is BZL-ARCH-08's row, cited never restated.

Contents: [The Gate](#the-gate) ·
[Warnings the Gate Already Blocks](#warnings-the-gate-already-blocks) ·
[What Only a Build Catches](#what-only-a-build-catches) ·
[Generated Starlark](#generated-starlark) ·
[Macros and Rules](#macros-and-rules) ·
[Depsets, Runfiles and Action Keys](#depsets-runfiles-and-action-keys) ·
[Load Visibility and Failure Tests](#load-visibility-and-failure-tests) ·
[Claims About Versions and Performance](#claims-about-versions-and-performance) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn. Every
version-bound claim below was measured 2026-09-06 against Bazel 8.7.0, 8.8.0 and
9.2.0 on Linux, and binds `buildifier_prebuilt` 8.2.0.2 through
[8.5.1.4](https://github.com/keith/buildifier-prebuilt/releases) (latest),
buildifier 8.2.0 and bazel_skylib 1.9.0; the Bazel-9 `load()` fixes are
rules_python 2.3.3, rules_shell 0.8.0, rules_cc 0.2.22 and protobuf 36.1.bcr.1.
9.2.0 is Active LTS and 8.8.0 is Maintenance, so a repository still pinned at 8.x
has not yet run this file's Bazel-9 rows against anything. Before citing any flag,
read it on **both** help surfaces at the pinned version — `bazel help <command>
--long` and `bazel help startup_options`. A flag lives in one or the other, the
command surface silently misses a startup option, and `bazel help all --long` is
not a subcommand.

**Reach limit on every grep in this file, stated once.** A grep sees the `.bzl`
that *builds* a string, never the string. BUILD and `.bzl` text that a repository
rule or module extension writes into a generated repository is out of reach of
every grep, of buildifier and of stardoc, so a clean grep never proves a generated
repository is clean. [Generated Starlark](#generated-starlark) is the section that
reaches it.

## The Gate

The shared check for this whole section: seed one violation into an
already-formatted `.bzl` **outside the build's load graph** — a file Bazel loads
fails the build first and teaches you nothing about the gate — then run
`bazel run //:buildifier.check; echo $?`. Gate on `$?` being nonzero, never on
stdout and never on a specific number. A target that stays green with the
violation planted is the finding.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-01 | Invoke the lint gate as a `buildifier()` target run through `bazel run` and propagate its exit status — never behind `\|\| true`, `continue-on-error` or a stdout scrape, and **never as `buildifier_test`**. | `mode = "diff"` with `lint_mode = "warn"` already exits nonzero on a pure lint finding with no diff to print, so an attribute name is no evidence a gate is soft. `buildifier_test` is worse than soft: its runner template's `${WORKSPACE+x}` test asks whether the variable is *set*, not whether it is non-empty, so `find -type f` walks a symlink-only runfiles tree, buildifier receives zero file arguments, and it reads empty stdin as valid Starlark. It passes with a deliberately broken `srcs`, measured identically on 8.7.0 and 9.2.0. | The section drill. Nonzero exit with EMPTY stdout still reads **pass for the gate**. A green run with the violation planted reads **finding**, and is the tell for a `buildifier_test`. | MUST |
| BZL-LARK-31 | Gate the lint target on its exit status being nonzero, never on a literal number, and pin `buildifier_prebuilt` at 8.5.1.3 or later — 8.5.1.4 preferred. **pinned** | Two defects live in `runner.bash.template`, not in buildifier and not in Bazel, and both are properties of the pin. `${WORKSPACE+x}` is what makes `buildifier_test` false-pass, fixed at **8.5.1.3**. `find … -print \| xargs` remaps any invoked command's 1-125 exit to **123**, so a check for buildifier's documented 4 goes green on every finding; retired at **8.5.1.4**, where `find … -exec … {} +` lets the native 4 through. Below the floor the `test` form checks nothing; above it a `-eq 123` comparison passes everything. | `grep -n 'buildifier_prebuilt' MODULE.bazel` — a version below 8.5.1.3 is the finding; EMPTY output reads **rule does not apply** (not a consumer), never pass. Then `grep -n -e 'WORKSPACE+x' -e 'xargs' bazel-bin/<target>.bash` on the generated runner: a hit means that defect is live, EMPTY means both are gone. Then grep CI and task files for `123` or `-eq 4` compared against this gate; any hit is the finding. | MUST |
| BZL-LARK-29 | Never treat `.bazelignore` as a lint or format exclusion, and never report a tree as unlinted because it is listed there; exclude a tree with the lint target's own `exclude_patterns`. | The runner `cd`s to `$BUILD_WORKSPACE_DIRECTORY` and runs a plain shell `find .`; `.bazelignore` prunes Bazel's package graph and has no bearing on a `find`. Measured: a violation planted in a `.bazelignore`d tree and one planted in an in-graph `.bzl` produce the same nonzero exit, with nothing distinguishing them. Two failures follow — shipping lint debt into a tree believed unchecked, or "excluding" a tree by editing a file the gate never reads. | Plant one unused `load` inside a `.bazelignore`d tree and run the gate. Nonzero reads **the tree is in scope**; EMPTY output with exit 0 reads **the runner does not reach that tree** — confirm why before relying on it. Read the generated runner under `bazel-bin` for the `exclude_patterns` → `\! -path` expansion that decides reach. | MUST |
| BZL-LARK-04 | Every `# buildifier: disable=<category>` names a category that still exists in `warn.go` and carries a one-line reason on the same or the preceding line; never write `load-on-top`, `out-of-order-load`, `same-origin-load` or `attr-package-metadata`. | Those four are gone from all three warning maps — the first three are unconditional formatter rewrites now — so the suppression is inert cruft that reads as protection and survives review forever. | `grep -rn 'buildifier: disable=' --include='*.bzl' --include='BUILD.bazel' .`, then check each category against the four dead names and against `warn.go`'s three warning maps, which are ground truth where the rendered `WARNINGS.md` table drifts. EMPTY output reads **pass**. | MUST |

## Warnings the Gate Already Blocks

Every row here is a default-set buildifier category the same
`bazel run //:buildifier.check; echo $?` already reports, and a default-set
warning is a build-breaking finding rather than advice: 99 categories exist, 98
are on by default, exactly one (`unsorted-dict-items`) is off. These five are
promoted out of the 98 because each names a distinct failure an agent repeats.
Buildifier parses and lints; it never evaluates Starlark, so a clean run is
evidence about format, style and the structural lints only.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-11 | Never merge depsets with `+`, `\|` or `.union()`; construct one `depset(direct = …, transitive = […])`. | A hard load-time error today, not a deprecation: `depset([1]) + depset([2])` fails with `Error: unsupported binary operation: depset + depset`, exit 1, identically on 8.7.0 and 9.2.0, before any action is analysed. Its migration flag was deleted from Bazel's source years ago, so no flag re-enables it. The idiom reads as natural set algebra and dominates pre-2020 training data. | buildifier `depset-union` catches it before a build; a build catches it at load time and reds the whole package. Backstop `grep -rn -e 'depset\s*+\s*depset' -e '\.union(' --include='*.bzl' .`. EMPTY output reads **pass**. | MUST |
| BZL-LARK-05 | Never return a bare `struct(...)` from a rule or aspect implementation function; return a list of declared providers. | Deprecated on Bazel 8 and **removed at 9.0.0**, where `--incompatible_disallow_struct_provider_syntax` is a permanent no-op. A CI matrix that runs 8.x and 9.x therefore has one green leg and one dead leg, which is worse than code that never worked. Upstream's "Migrating from legacy providers" section still reads as current and mentions Bazel 9 nowhere. | buildifier `rule-impl-return` (default set, no autofix); backstop `grep -rn 'return struct(' --include='*.bzl' .` — a hit is a finding **only** where the enclosing function is a rule or aspect implementation, not a helper returning a value object, and test fake-`ctx` constructors match without being findings. EMPTY output reads **pass**. | MUST |
| BZL-LARK-07 | Never build a depset inside a loop with the accumulator itself in `transitive =`; collect a plain list across the loop and call `depset(transitive = collected)` once, after it. | Produces an unbounded chain of single-element nodes — silent O(N²) traversal that no `--incompatible_*` flag will ever catch, because it is a performance defect and not a compatibility break. | buildifier `overly-nested-depset` (default set, no autofix); EMPTY output reads **pass for source `.bzl` only**. Where the static check cannot reach — a generated `.bzl`, or a suspected hotspot with no source hit — run `bazel build --nobuild --starlark_cpu_profile=<file> <target>` then `pprof -top <file>`: a hot frame in a loop building `depset(transitive = accumulator)` is the same finding. | MUST |
| BZL-LARK-09 | Pass every argument at a rule or macro call site by keyword, never positionally. | Positional arguments block migration to symbolic macros, which reject them at the interpreter level (`accepts no more than N positional argument(s) but got M`), and the warning has no autofix, so every site is a hand edit. | buildifier `positional-args` (default set; exempts `load()`, `vardef()`, `exports_files()`, `licenses()`, `print()`). EMPTY output reads **pass for bare-name call sites in BUILD files only**: the check returns immediately on any file that is not a BUILD file and skips dotted calls even inside one, so a call site living in a `.bzl` — a legacy macro body, a helper — reads **not checked**, and those are read by hand. Measured identical on 8.7.0 and 9.2.0. `unittest.make(_impl)` and `analysistest.make(_impl, …)` are doubly exempt and are not this rule's target. | MUST |
| BZL-LARK-06 | Every `provider()` call declares both `fields` and a documentation string; retrofitting an existing provider is a warning, declaring a new one without them blocks. | Provider identity is symbol plus shape. An undeclared field set makes the first later field addition a silent contract change for every consumer that pattern-matches with `hasattr`. | buildifier `provider-params` (default set). EMPTY output reads **pass**. | MUST |

```starlark
# wrong — one depset node per iteration, O(N²) traversal (BZL-LARK-07)
for d in deps:
    acc = depset(d.files, transitive = [acc])
```

```starlark
# right — one node, built once after the loop
parts = [d.files for d in deps]
acc = depset(transitive = parts)
```

## What Only a Build Catches

The shared check: `bazel build --nobuild //...` on the repository's Bazel 9 leg.
Buildifier reports neither row reliably, and on an 8.x-only CI matrix both stay
invisible until an adopter upgrades.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-10 | Every `cc_*`, `java_*`, `py_*`, `sh_*` and `proto_library` call carries an explicit `load()` from its Starlark ruleset, and a clean `native-py` or `native-sh-*` buildifier result is never read as Bazel-9 readiness. | `--incompatible_autoload_externally` is a full allowlist on 8.7.0 and 8.8.0 and `""` on 9.2.0 — the native symbols are gone from the global namespace. Measured on fresh workspaces: all five kinds build bare on 8.7.0 and all five fail on 9.2.0, in two error shapes — `name 'py_library' is not defined (did you mean 'cc_library'?)` for `py_*`, `sh_*` and `proto_library`, and a purpose-built `_removed_rule_failure` traceback naming `buildifier --lint=fix` for `cc_*`. buildifier's `native-py` doc text still says the disabling "has been postponed" and `native-sh-*` cites no flag, so the linter lags reality for those two families. | `bazel build --nobuild //...` on Bazel 9 is ground truth for all five. EMPTY buildifier output reads **pass for `cc_*`, `java_*` and `proto_library` only** and **not checked** for `py_*` and `sh_*`. For the symbol-to-ruleset mapping read [`AutoloadSymbols.java` at `release-9.0.0`](https://github.com/bazelbuild/bazel/blob/release-9.0.0/src/main/java/com/google/devtools/build/lib/packages/AutoloadSymbols.java) — no prose page enumerates it. | MUST |
| BZL-LARK-15 | Never write a top-level `if` or `for` **statement** in any Starlark file — `.bzl`, `BUILD`, `MODULE.bazel`, `.star`, `.scl` alike — and never write `while` or `assert` anywhere: they are reserved words with no grammar production at all. Use an if-*expression* for a value, or a `def` called as a bare top-level expression. | The restriction is in the [Starlark specification](https://github.com/bazelbuild/starlark/blob/master/spec.md)'s own grammar and prose, not in Bazel, and independent engines enforce it with the same wording — it reaches `.star` and `.scl` files no Bazel ever loads. It is a **parse-time** error, so it reds every target on every platform at once, before a single assertion runs, and the whole-file redness invites edits to unrelated nearby code. | `grep -rn -e '^if ' -e '^for ' -e '^while ' -e '^assert ' --include='*.bzl' --include='*.star' --include='*.scl' --include='BUILD*' --include='MODULE.bazel' .` — a column-0 triage grep, not a proof; EMPTY output reads **pass**, since an indented occurrence inside a `def` is legal. On an already-red build, grep the log for the phrase "cannot be used outside" before touching anything else. | MUST |

## Generated Starlark

The reach limit above is why this section exists. Three layers, cheapest first,
and each one's green means less than it looks: the loading-phase step
(`bazel query //...`, exit 7 on a defect, or `bazel build --nobuild //...`, exit
1), the raw buildifier binary over a rendered sample, and a real build of a
target out of the generated repository.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-30 | Every repository rule or module extension that generates a `.bzl` or `BUILD` file has a consumer package in the repository that `load()`s or references it, and the gate runs a **loading-phase step** over that consumer on every push, as its own step — never folded into or replaced by the buildifier step. | Buildifier never evaluates Starlark, so it is blind to an undefined name, a `load()` of a symbol the target module does not export, and a bad builtin argument, and exits 0 on all three. Bazel's loading phase catches every one at the same reliability as a syntax error, in two sub-phases its own error text names: *compilation of module* (static resolution, which rejects an undefined name even when the only reference sits in a function that is never called) and *initialization of module* (which rejects the bad `load()` and `attr.string(default = 123)` when the top-level statement executes). Measured identical on 8.7.0 and 9.2.0, at 0 actions, no binary, 0.057 s. | `bazel query //<consumer>:<target>; echo $?` — 0 reads **the generated Starlark compiled and initialised**; 7 prints the generated file's own `path:line:col`, and `bazel build --nobuild //...` gives the same diagnosis at exit 1. EMPTY output with exit 0 over a generated repository **no package loads** reads **not checked**, not pass: `bazel query @gen//:all` on an unconsumed generated repository returns `INFO: Empty results`, exit 0. A generator with no consumer package at all is the finding. | MUST |
| BZL-LARK-25 | Any repository rule, module extension or macro that writes Starlark text as a string — `ctx.file("BUILD.bazel", …)`, `ctx.file("*.bzl", …)` or equivalent — has at least one test that **builds a target out of the generated repository**, exercised on every push. A substring assertion over the rendered string is not validation. | Generated Starlark is invisible to buildifier, stardoc and every grep-based audit: those tools see the `.bzl` that builds the string, never the string. The cheaper layers reach further than they look — BZL-LARK-30 catches undefined names, bad `load()` targets and bad builtin arguments — but only once some package loads the file, and never a render that compiles, initialises and analyses cleanly and names the wrong thing when the target is built. That class is what this rule alone covers. | For each `ctx.file(` whose content argument is a computed string, confirm a corresponding example or e2e target exists **and** runs in CI on push, not on a schedule — a reading heuristic cross-referenced against the CI config, not one grep. EMPTY output ("every generator site has an exercised integration test") reads **pass**. A green BZL-LARK-30 or BZL-LARK-26 check on a site with no such test reads **finding**, not pass. | MUST |
| BZL-LARK-26 | Funnel generated-Starlark string building through a small number of named `render_*` helpers, and have at least one test pipe a rendered sample through the raw `buildifier` binary at `-mode=check -lint=warn` — as a plain `sh_test`, never `buildifier_test`. | Shrinks BZL-LARK-25's blind spot from every implementation site to a handful of tested helpers, and catches unbalanced parens and bad quoting without a live build. A substring assertion cannot: `asserts.true(env, "x" in content)` passes over an unbalanced paren, a bad attribute name and a truncated render alike. | Does a `sh_test` with `data = [<rendered target>, "@buildifier_prebuilt//:buildifier"]` capture a helper's output and run the binary over it? Absence reads **finding**; a green `buildifier_test` over the same `srcs` reads **not checked**, not pass. EMPTY buildifier output on a rendered `.bzl` reads **pass on format, style and the structural lints only** — never as a semantic check, and it layers above BZL-LARK-30 rather than in place of it. On Bazel 9 the `sh_test` itself needs an explicit `load("@rules_shell//shell:sh_test.bzl", "sh_test")` (BZL-LARK-10). | SHOULD |

## Macros and Rules

The shared check: read the definition, then build one target the macro
instantiates and read the error text. Symbolic macros are the default for new
macro code on Bazel 8.0.0 and later; `inherit_attrs` ships at 8.0.0 too, despite
the 9.0.0 release notes re-listing it.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-18 | Choose by capability: a `rule()` when the logic must register an action or return a provider; otherwise a symbolic `macro()`; a legacy `def` macro only when the body needs `glob()`, an untyped parameter, or `native.existing_rules()` outside a finalizer — and move the `glob()` to the calling BUILD file rather than keep the macro legacy. **pinned** | Macros compose rule calls at loading time and can do neither of the first two things — a hard Starlark API boundary. Legacy macros are transparent to the visibility system and permit silent argument mutation, so porting later is strictly harder than starting symbolic; a `native.existing_rules()` macro's correct destination is `macro(finalizer = True, …)`, which restores macro-aware visibility. | Reading heuristic per macro definition: does the body call `ctx.actions.*` (→ it must be a `rule()`), `glob()`, take a parameter with no `attr.*()` type, or call `native.existing_rules()` outside `finalizer = True`? A plain `def` on a Bazel-8+ repository with none of those is the finding; EMPTY output (no such `def`s) reads **pass**. | MUST |
| BZL-LARK-19 | Name every target and submacro a symbolic macro creates exactly the macro's `name`, or `name` followed by `_` (preferred), `.` or `-`. | A violating target "can be declared, but cannot be built and cannot be used as dependencies" — it surfaces only when something depends on it, as `Target //src:X declared in symbolic macro 'Y' violates macro naming rules`, far from the macro that produced it. | Read every `name = ` construction in the macro implementation against the `name` parameter, or build the suspect target and grep the log for `violates macro naming rules`. EMPTY output reads **pass**. | MUST |
| BZL-LARK-20 | Inside a symbolic macro, never mutate a received argument: no `kwargs["x"]["k"] = v`, no `.append()` on a received list or attribute. Copy first with `dict(x)`, and extend a configurable attribute with `+=`. Mark an attribute `configurable = False` only where the macro genuinely cannot handle a per-configuration value. | All macro arguments arrive frozen (`Error: trying to mutate a frozen dict value`), and every attribute not marked `configurable = False` arrives wrapped in a trivial `select()` even when the caller passed a plain list — so `.append()` fails with `'select' value has no field or method 'append'` while `+=` succeeds. Both patterns were silent no-ops in a legacy macro, which is why a rename-the-`def` port breaks here first. | `grep -n -e 'kwargs\[[^]]\+\]\s*\[[^]]\+\]\s*=' -e 'kwargs\[[^]]\+\]\.append(' -e '\.append(' over the macro's `.bzl`, then confirm each target of a hit is a macro argument rather than a local. Confirm a hit by building a target the macro instantiates and reading for the two exact error strings. EMPTY output reads **pass**. | MUST |
| BZL-LARK-21 | Guard every `inherit_attrs`-sourced attribute for `None` before using it as a list, dict or string, and declare defaults in the macro's own `attrs` dict — never on the implementation function's `def` line, where they are silently ignored. | Inheritance overrides every non-mandatory attribute's default to `None` regardless of the original attribute definition's default: `target_compatible_with` is `[]` on the rule and `None` on the inheriting macro, so `if not kwargs["x"]` cannot distinguish unset from explicitly empty, and `None + ["x"]` raises where `[] + ["x"]` works. | For each name in `inherit_attrs`, read the implementation for a use of that parameter and confirm an `or []` or `== None` guard precedes it; EMPTY output (no unguarded read) reads **pass**. For a suspected ignored default, call the macro without the argument and read the value back with `bazel cquery //<target> --output=build`. | MUST |
| BZL-LARK-23 | Inside a symbolic macro, set each created target's visibility explicitly — omitted (macro-private) or forwarded as `visibility = visibility` — and never hardcode `visibility = ["//visibility:public"]` in a macro body. | The package's `default_visibility` does not apply inside a symbolic macro, the opposite of legacy-macro behaviour, so an omitted visibility means macro-private rather than the package default. Hardcoding public makes the target unconditionally visible to every package even when the caller specified something narrower. | `grep -n 'visibility = \["//visibility:public"\]'` in every `.bzl` containing `macro(` — a mechanical proxy for the worst case. EMPTY output reads **pass for the hardcoded-public case only**; the "set it explicitly" half needs a read of each rule call in the implementation. | MUST |

```starlark
# wrong — declares, then cannot be built or depended on (BZL-LARK-19)
def _impl(name, visibility, **kwargs):
    native.genrule(name = "genrule_" + name, **kwargs)
```

```starlark
# right — the macro's own name, then a separator
def _impl(name, visibility, **kwargs):
    native.genrule(name = name + "_genrule", **kwargs)
```

## Depsets, Runfiles and Action Keys

The shared check: two greps over `*.bzl`, plus a two-build execution-log diff
where the grep cannot decide. Nothing in this section has a buildifier category
or an `--incompatible_*` flag behind it, so a clean gate says nothing about any
of it.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-17 | Never call `depset.to_list()` to build action arguments; pass the depset to `ctx.actions.args().add_all()` or `add_joined()` and let expansion happen at execution time. Flattening is permitted only at a genuinely terminal target or in debugging code. | `to_list()` is O(N²) across overlapping dependency chains, and the "safe at top-level targets" exemption is false the moment tests or an IDE import build overlapping target sets. Bazel's own guidance reports `args()` cutting rule memory by 90% or more. | `grep -rn '\.to_list()' --include='*.bzl' .`, excluding test and debug paths; each remaining hit must be terminal or replaceable by `add_all`. EMPTY output reads **pass**. Dynamic backstop: elevated CPU inside `runAnalysisPhase` with no matching execution-phase growth, with `--starlark_cpu_profile` plus `pprof -top` naming the calling `.bzl` function. | MUST |
| BZL-LARK-16 | Never let a `default`-order depset's traversal reach an action's command line, an action input manifest, or a generated file without either an explicit `order =` chosen for a stated requirement or a canonicalisation step such as `sorted()`. | `default` order guarantees only "deterministic for this graph shape". Adding an unrelated transitive edge elsewhere silently reorders the flattened list, changing the command line and therefore the action key — a spurious cache miss that presents as "the build is non-deterministic". | Build twice with `--execution_log_compact_file`, editing an unrelated part of the graph between the runs, and diff with the execlog parser — the execlog parser/converter flag traps are BZL-CACHE-16's. EMPTY diff **on an action present in both logs** reads **pass**. An action **absent from both** was served by the persistent local action cache and never written to the log at all, which reads **inconclusive**, not pass — `bazel clean` and re-run, or read the build event protocol's `ActionCacheStatistics`. | MUST |
| BZL-LARK-27 | Never use `ctx.runfiles(collect_data = …)` or `collect_default = …`, and never pass `data_runfiles =` or `default_runfiles =` to the `DefaultInfo` constructor; use `DefaultInfo(runfiles = …)` and read a dependency's runfiles as `DefaultInfo.default_runfiles`. New and edited code blocks; an existing estate is a migration backlog. | Bazel's own "Runfiles features to avoid" names all of these — the collect modes gather runfiles across hardcoded dependency edges in confusing ways, and the data/default split is legacy-only. No buildifier warning and no `--incompatible_*` flag exists for any of them, so nothing surfaces it on its own. | `grep -rn 'collect_data\|collect_default\|data_runfiles' --include='*.bzl' .` — every hit is suspect; a `.default_runfiles` **read** is correct and does not match the pattern. EMPTY output reads **pass**. | MUST |

## Load Visibility and Failure Tests

The shared check: two greps, then one `bazel test` run of the harness pair.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-24 | An `analysistest` built with `expect_failure = True` asserts against a fragment held in a constant that the test's own call site cannot spell, drives a throwaway harness `rule()` whose implementation does nothing but call the guarded path, and is paired with a sibling plain `analysistest.make()` over the same harness on a valid input. | `asserts.expect_failure` is `actual_errors.find(msg) < 0` — a raw substring search over the concatenated cause messages, which include the traceback's echo of each frame's *source line*. A fragment that also appears at the call site matches that echo, and the test passes with the guard deleted. Documented nowhere upstream. The pairing makes the guarantee structural: the passing sibling proves the harness has no other latent failure, so the failing sibling's failure can only be the guard's, and deleting the guard makes the harness return normally, at which point `expect_failure` reports `Expected failure of target_under_test, but found success`. | Three checks. (1) `grep -rn 'asserts.expect_failure(env, "'` — any hit with a quoted literal rather than a bare identifier is the finding; EMPTY output reads **pass**. (2) Read the harness rule's implementation: anything beyond the single call into the guarded path is a second possible failure source and reads **finding**. (3) `bazel test` the pair — a green `expect_failure` test with **no** passing sibling over the same harness reads **finding**, not pass. A bare `unittest.make()` is never the driver here: an uncaught `fail()` aborts that target's analysis with no PASS/FAIL and `bazel test` reports "No test targets were found, yet testing was requested". Mechanism confirmed against [`unittest.bzl` at 1.9.0](https://github.com/bazelbuild/bazel-skylib/blob/1.9.0/lib/unittest.bzl). | MUST |

## Claims About Versions and Performance

The shared check: fetch a source with a version attached before the claim is
written — the reference docs at the git tag, the release notes at the tag, a code
search, or the pinned binary's own help output.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-LARK-22 | Never justify writing or converting a macro as a performance improvement without a dated citation that symbolic-macro lazy evaluation has shipped. | Lazy expansion is the only mechanism that would make symbolic macros cheaper at equal call volume, and it is explicitly unshipped: upstream `macros.md` still reads "We are in the process of implementing lazy macro expansion and evaluation. This feature is not available yet", and no 9.0, 9.1 or 9.2 release note says otherwise. The cost is unchanged by the conversion — a legacy `def` macro re-executes its whole body at loading time for every call site, and a symbolic macro pays it identically. | `curl -sL https://raw.githubusercontent.com/bazelbuild/bazel/master/site/en/extending/macros.md \| grep -A2 'Laziness'` — a match containing "not available yet" means the performance claim is still false; a match describing shipped laziness re-opens this rule. EMPTY output means the section moved: re-read the page, never assume it shipped. | MUST |
| BZL-LARK-28 | Verify every Bazel version floor, flag availability and "still supported" claim against a source with a version attached before writing it into code, a doc, a rule or a commit message — never a release-notes bullet alone and never a prose page alone. | Four misdatings surfaced in one research program: the [9.0.0 notes](https://github.com/bazelbuild/bazel/releases/tag/9.0.0) re-list `inherit_attrs`, an 8.0.0 feature; buildifier's `WARNINGS.md` flag column cites seven flags Bazel has deleted; the legacy-provider migration page never mentions Bazel 9 at all; and `depset + depset` was reported "deprecated but working" by inference when a two-line build shows it is a hard error. | `curl -sL https://raw.githubusercontent.com/bazelbuild/bazel/<claimed-floor-tag>/site/en/<page>.md \| grep '<api>'` at the claimed floor **and one major earlier** — a hit one major earlier contradicts the release notes, and the versioned docs win. For a flag: a code search over `bazelbuild/bazel` for existence, plus `bazel help <command> --long` **and** `bazel help startup_options` on the pinned binary for the live default. EMPTY search result for a flag reads **historical — do not cite as flippable**, never pass. | SHOULD |

## Gaps

- Every measurement ran on Linux with `linux-sandbox`. The mechanisms here are shell-, parser- and Go-source-level rather than kernel-level, but no row is confirmed on darwin or Windows.
- `buildifier_prebuilt` 8.2.0.2 → 8.5.1.4: only the two template fixes named in BZL-LARK-31 are audited across those nine releases. `exclude_patterns` expansion, `lint_warnings` handling and the runfiles layout are unaudited across that span.
- One BUILD-level generated shape — a rule call with a nonexistent keyword argument — is expected to fail at package construction by the same mechanism as a bad builtin argument. Expected, not measured.
- The class BZL-LARK-25 alone covers — a render that compiles, initialises and analyses cleanly and is wrong only when the target is built — has no fixture, so how much a real consumer build adds over `bazel build --nobuild` is reasoned rather than measured.
- Nothing measures what converting an existing legacy-macro estate costs, or when the visibility and typing wins pay for the port.

## What Agents Get Wrong Here

1. **Writing a legacy `def foo(name, **kwargs)` macro on a Bazel-8+ repository**, because training data over-represents WORKSPACE-era and pre-2024 macros and "write a macro" returns the pattern the model has seen most — BZL-LARK-18.
2. **Treating `lint_mode = "warn"` as report-only by analogy to a compiler warning, or reaching for `buildifier_test` because it is the rule with `test` in the name.** One ships a blocking gate believed soft; the other ships a green test that passes with a broken `srcs` — BZL-LARK-01, BZL-LARK-31.
3. **Reading `.bazelignore` as a lint exclusion** — then reporting a tree unlinted when it is linted, or "excluding" a tree by editing a file the gate never reads — BZL-LARK-29.
4. **Writing a bare `cc_library(…)` or `py_test(…)` with no `load()`**, because it built on every Bazel through 8.x and the symbol is simply gone on 9 for all five families — BZL-LARK-10.
5. **Copying Python module-level structure into a `.bzl`** — a top-level `if` to define a target conditionally, a top-level `for` to generate several — which fails at parse time and reddens the whole file — BZL-LARK-15.
6. **Merging depsets with `+` or `|`, or calling `.to_list()` to build a command line.** The first is a hard load-time error, not a deprecation; the second is the readable form and the O(N²) form — BZL-LARK-11, BZL-LARK-17.
7. **Extending a generated-BUILD or generated-`.bzl` string and assuming the lint gate covered the diff.** The gate did not look inside the string, and substring assertions over the same text stay green through a syntax error — BZL-LARK-25, BZL-LARK-30.
8. **Trusting a green `expect_failure` test** — the exact signal an autonomous agent is built to trust, and the one that passes with the guard deleted — BZL-LARK-24.
