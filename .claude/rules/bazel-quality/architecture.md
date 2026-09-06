---
title: Bazel-Friendly Architecture
summary: The BZL-ARCH family — package boundaries, visibility, target granularity, select()/transition graph shape, generated code, and the layout decisions owed before the first BUILD file
---

# Bazel-Friendly Architecture

Owns `BZL-ARCH`: where package boundaries fall, who can see what, how fine the
targets get and what generates them, the graph shape a `select()` or a
transition produces, where generated code lives, and monorepo layout. It does
not own Starlark style or the Bazel 9 explicit-`load()` requirement
(`BZL-LARK`), flag defaults and rc-file layering (`BZL-FLAG`), `MODULE.bazel`
and its lockfile (`BZL-MOD`), CI target selection (`BZL-CI`), hermeticity and
the sandbox (`BZL-HERM`), or test rules and the Bazel 9 implicit `test` exec
group (`BZL-TEST`). The adoption procedure itself — the go/no-go checklist,
naming the narrower fix, sizing the rewrite — is the `bazel-adopt` skill, never
a rule here.

Contents: [Visibility and the Public Surface](#visibility-and-the-public-surface) ·
[Platforms and `select()`](#platforms-and-select) ·
[Package Boundaries and Globs](#package-boundaries-and-globs) ·
[Granularity and Generated Files](#granularity-and-generated-files) ·
[Transitions and Configured-Target Duplication](#transitions-and-configured-target-duplication) ·
[Cross-Cutting Checks and Execution Groups](#cross-cutting-checks-and-execution-groups) ·
[Migration State and the Bazel-Visible Tree](#migration-state-and-the-bazel-visible-tree) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-09-06 against Bazel 8.7.0, 8.8.0 and 9.2.0, with 9.0.0 and 9.1.0
added to settle the `transitive_visibility` boundary. Flag defaults are compiled
into the binary and carry no runner caveat; every load- and analysis-time probe
here ran on one Linux host. This file binds to bazel-gazelle 0.54.0,
rules_python 2.3.3 with its first-party `gazelle/` plugin, `aspect_gazelle_js`
1.2.1 with `aspect_gazelle_prebuilt` 0.0.25, `gazelle_rust` 0.1.0 (floor
rules_rust 0.40.0; 0.74.0 clears it), `gazelle_cc` 0.1.0 (floors gazelle 0.42.0
and rules_cc 0.1.1), rules_buf 0.5.4 and protobuf 36.1.bcr.1. Before citing any
flag default, read it off the pinned binary on **both** help surfaces —
`bazel help <command> --long` **and** `bazel help startup_options` — because a
flag lives in one or the other and checking one silently misses it. There is no
`bazel help package` subcommand on any version tested.

**Every grep in this file shares one blind spot: BUILD and `.bzl` text a ruleset
writes as a Starlark string into a generated repository is out of its reach.**
Only an integration test that actually builds from the generated repo sees that
text — a package that generates correct `constraint_values`-keyed
`config_setting`s into another repo reads as zero hits here.

## Visibility and the Public Surface

Target visibility, load visibility and `config_setting` visibility are three
separate systems and a public BUILD target unifies none of them
([concepts/visibility](https://bazel.build/concepts/visibility)). Four greps
over `BUILD*` and `*.bzl` plus one canary build cover this whole block; each row
names its own text because they target different files. Measured on 8.7.0 and
9.2.0: `--check_bzl_visibility` **true**,
`--incompatible_enforce_config_setting_visibility` **true**,
`--incompatible_config_setting_private_default_visibility` **false**,
`--incompatible_no_implicit_file_export` **false**.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-04 | Never set `default_visibility = ["//visibility:public"]` at package level, except in the package that *is* the module's public API surface — and write the reason at that site. | Bazel's own docs name it an anti-pattern, and **no buildifier warning checks it** (`WARNINGS.md` read in full: zero occurrences of `default_visibility`), so public surface grows with the codebase and nothing reports it. A compensating control living in another file is not a stated exemption. | `grep -rn 'default_visibility.*//visibility:public' --include='BUILD*' .` — **empty = pass; every hit needs an adjacent comment stating why it is intentional.** A value assembled through a Starlark variable or a macro parameter is not a literal and this grep never sees it. | MUST |
| BZL-ARCH-08 | Declare `visibility(...)` at the top of every `.bzl` file, non-public for anything under a `private/` or `internal/` directory, and never treat a BUILD target's visibility as covering it. | Load visibility defaults to **public**: a `.bzl` file with no `visibility()` call is `load()`-able from anywhere, however narrow its `bzl_library` target is. The two mechanisms gate different edges. `--check_bzl_visibility` is `true` on 8.7.0 and 9.2.0, so a violation is an error, not a warning. | `grep -rL '^visibility(' --include=*.bzl path/to/private` — **empty = pass (every file gated); any filename printed = finding.** On a change: `git diff --diff-filter=A -- '*.bzl'` and read each new file's first ten lines. | MUST under `private/`/`internal/`; SHOULD elsewhere |
| BZL-ARCH-09 | Write an `exports_files()` declaration for every source file consumed from outside its own package; never rely on implicit export. | Bazel's visibility page is imperative here, and the failure is **latent, not live**: `--incompatible_no_implicit_file_export` reads `default: "false"` on 8.7.0 and 9.2.0, so implicit export still works and the whole tree is one rc line — or one major — away from failing at analysis time at once. This rule shipped a wrong rationale for a day by quoting that default off a doc page instead of the binary. | Canary, which is also how you make the latent failure fire on demand: `bazel build --incompatible_no_implicit_file_export //...`. **A clean build with the flag on = pass; a `Visibility error: target '//pkg:f.txt' is not visible from …` naming a *source-file* label = finding** — the message names `exports_files()` as the fix, verbatim on both majors. Static proxy `grep -L 'exports_files' <pkg>/BUILD*` is incomplete on its own: the consumer side must be cross-referenced. | MUST |
| BZL-ARCH-34 | Never write `package(transitive_visibility = …)` on a pin below Bazel 9.0.0, and where it is available pass it a single `package_group` label **as a string** — not a list, and not `//visibility:public`. | 8.7.0 and 8.8.0 fail with `Error in package: unexpected keyword argument: transitive_visibility`; 9.0.0, 9.1.0 and 9.2.0 accept it and type-check the argument as `string`, so the list form the docs' own example suggests fails with `expected value of type 'string' for package() argument 'transitive_visibility', but got [...] (list)`. Both failures are loud at load time; the cost is an agent burning a build cycle per guess. | `grep -rn 'transitive_visibility' --include='BUILD*' .` cross-read against `.bazelversion` — **empty = pass**; any hit on a pin below 9 is a hard package-load error and any hit passing a list is a hard type error. To settle availability on an unfamiliar pin, write the two-line probe below into a throwaway package and run `bazel build --nobuild //...`; there is no `bazel help package` to grep (measured absent on 8.7.0, 8.8.0, 9.0.0, 9.1.0 and 9.2.0). | MUST |
| BZL-ARCH-10 | State a `config_setting`'s `visibility` explicitly wherever it is meant to be package-scoped. | `--incompatible_enforce_config_setting_visibility` is **true** while `--incompatible_config_setting_private_default_visibility` is **false** — both measured on 8.7.0 and 9.2.0 — so an unspecified-visibility `config_setting` is public today *even inside a package whose `default_visibility` is narrow*. "Visibility is enforced now" reads exactly backwards here. | `grep -rn -A8 'config_setting(' --include='BUILD*' .` and read each declaration. **An explicit `visibility =` on every declaration = pass; a missing one in a narrow-`default_visibility` package = finding.** **Empty output = no checked-in `config_setting` — a pass only where nothing generates one into another repository (see the blind-spot note above), never otherwise.** | SHOULD |
| BZL-ARCH-07 | Never assume a negated `packages` entry in one `package_group` filters a group it `includes`, and spell out `"public"` rather than relying on `//...`. | Each group's set is computed **independently** and the results are then unioned, so a group excluding `//foo/tests/...` does not remove them when it `includes` a group granting them ([reference/be/functions](https://bazel.build/reference/be/functions)); it reads backwards on a skim and no query surfaces the resulting semantic error. `--incompatible_fix_package_group_reporoot_syntax` and `--incompatible_package_group_has_public_syntax` are both **true** on 8.7.0 and 9.2.0, so `//...` means this repository only — the pre-Bazel-6 "that is public" reading is wrong. | Reading heuristic: a `package_group` carrying both a `-`-prefixed `packages` entry and a non-empty `includes` needs a comment stating the negation binds only its own list, or a hand-traced union — **no such group = pass**. For the syntax half, `grep -rn '"//\.\.\."' --include='BUILD*' .` — **empty = pass; a hit inside a `package_group` intended as a public allowlist = finding.** | CONSIDER |

```starlark
# wrong — load error through 8.8.0; type error on 9.x
package(transitive_visibility = ["//visibility:public"])
```

```starlark
# right — 9.0.0+ only, one package_group label as a string
package_group(name = "friends", packages = ["//app/..."])
package(transitive_visibility = ":friends")
```

## Platforms and `select()`

One grep over `BUILD*` and `*.bzl`, then a build under every platform the CI
matrix runs: `bazel build --platforms=//:<platform> //<target>`. The 8.7.0
snapshot of the platforms page
([versions/8.7.0/concepts/platforms](https://bazel.build/versions/8.7.0/concepts/platforms))
still carries the "biggest challenge" paragraph and the
`select()`-does-not-understand-`--platforms` sentence that the rolling page has
since dropped; cite the snapshot.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-16 | Key every `config_setting` on `constraint_values` once any rule in the graph resolves toolchains through `--platforms`; never leave it on `values = {"cpu": …}` alone. | `select()`s on `--cpu`/`--crosstool_top` do not understand `--platforms` — they silently stop tracking reality once toolchain resolution flips, which for C++ and Android is the default from Bazel 7.0. | `grep -rln 'values = {' --include='BUILD*' --include='*.bzl' . \| xargs grep -Ln 'constraint_values'`, restricted to files declaring `config_setting`. **Empty = pass; any file printed holds a legacy-only `config_setting`.** Runtime signal on 9.2.0 only: `WARNING: … select() on cpu is deprecated. Use platform constraints instead`. 8.7.0 prints nothing, so on an 8.x pin the grep is the whole check. | MUST |
| BZL-ARCH-35 | Never let one `select()` carry both a `values=`-keyed and a `constraint_values=`-keyed `config_setting` arm that can match the same build, unless one is unambiguously more specialized or both arms resolve to the same value. | Neither arm subsumes the other, so Bazel refuses the target: `Illegal ambiguous match on configurable attribute … Multiple matches are not allowed unless one is unambiguously more specialized or they resolve to the same value.` Reproduced byte-for-byte on 8.7.0 **and** 9.2.0. It fires only under a platform that makes both arms true, so it sits dormant through every local build and breaks exactly one CI leg. | No grep resolves the arms — read each `select()` for two arms keyed on different mechanisms, then build under each platform in the matrix. **A clean build under every platform = pass; the `Illegal ambiguous match` error = finding.** On 9.2.0 the `values=` arm emits a deprecation warning before the fatal error; 8.7.0 gives no early signal. | MUST |
| BZL-ARCH-17 | Never let a Starlark transition write a legacy flag key (`//command_line_option:cpu`, `:crosstool_top`, `:compiler`) in a repo where any consuming rule reads `--platforms`. | The transitioned value becomes invisible downstream — no error, just a rule that never sees it. Bazel's own migration page names this the biggest challenge of a platforms migration for exactly this reason. | `grep -rn '"//command_line_option:\(cpu\|crosstool_top\|compiler\)"' --include='*.bzl' .` — **empty = pass; any hit = finding.** During a `platform_mappings` bridge the hit is tracked tech debt with a named owner and an end date, not an accepted state. | MUST |

```starlark
# wrong — invisible to --platforms toolchain resolution, and no error says so
config_setting(name = "linux", values = {"cpu": "k8"})
```

```starlark
# right — tracks the resolved platform
config_setting(name = "linux", constraint_values = ["@platforms//os:linux"])
```

## Package Boundaries and Globs

Two checks, run at two moments: `find . -maxdepth 2 -type d \( -name dist -o
-name build -o -name out \)` before the first boundary is drawn, and
`bazel query 'kind("source file", //path/to/globbed/pkg:*)'` before and after
any `BUILD` file is added beneath a globbed package. The package-per-directory
rule lives on
[configure/best-practices](https://bazel.build/configure/best-practices)
verbatim — `concepts/build-files` does not contain it, and citing that page for
it cites the wrong page.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-03 | Decide, in writing, between one root `BUILD` file and a per-package output restructure **before** drawing any package boundary in a repo whose tooling writes into a shared top-level output directory; where a legacy build must keep working, redirect stale config references (a `tsconfig.json` `paths` entry, for instance) with a generated copy, never an in-place edit. | Bazel requires a package's outputs to live under that package's own output directory. A shared `dist/` above several intended `BUILD` boundaries has exactly two resolutions and no third; discovering that mid-migration means redrawing package boundaries twice. | `find . -maxdepth 2 -type d \( -name dist -o -name build -o -name out \)`. **Empty = pass, no decision owed; any hit above an intended per-package `BUILD` location = the decision is owed in writing before the next boundary is drawn.** | MUST |
| BZL-ARCH-01 | Give every directory that contains buildable source files its own `BUILD`/`BUILD.bazel`, and never list a bare relative path into a subdirectory in `srcs`/`hdrs`/`data`. | The day a `BUILD` file appears in that subdirectory, every such reference breaks and every reverse dependency must be updated; until then scope creep and inadvertent cycles accumulate with nothing to signal them. | Reading pass over `BUILD*`: any `srcs`/`hdrs`/`data` entry containing `/` that does **not** start with `:` or `//` is a bare path into a subpackage-to-be. Seed with `grep -rn -e 'srcs *= *\[' -e 'hdrs *= *\[' -e 'data *= *\[' --include='BUILD*' .` and read each list. **No bare path = pass; any hit = finding.** | SHOULD |
| BZL-ARCH-02 | Re-run `bazel query` over every globbed target under a directory before and after adding a `BUILD` file anywhere beneath it. | `glob()` never matches into a subpackage and the shrink is **silent** — no error, the files just stop being included, and the call site alone cannot show it. | `bazel query 'kind("source file", //path/to/globbed/pkg:*)'` before and after, then `diff`. **Empty diff = pass; any removed entry = finding.** | SHOULD |

## Granularity and Generated Files

Granularity is downstream of generator maturity, so it is decided before any
`BUILD` file is authored — and after BZL-ARCH-03. The shared checks:
`bazel test //:gazelle_test`, `bazel query 'kind("sh_test", //:gazelle*)'`, and
`grep -L 'diff_test\|write_source_files' <pkg>/BUILD*` for packages holding
checked-in generated output. **Empty output from the `sh_test` query in a repo
that believes it has a freshness gate is itself the finding.**

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-11 | Go fine-grained (one target per module) only for a language whose Gazelle-family generator is production-grade — Go, protobuf, and Python paired with a pytest wrapper; keep `BUILD` files coarse and hand-maintained at directory level for JS/TS ("usable with care") and Rust ("experimental"). Quote the maturity of the module you will actually depend on, never its most-mature sibling. **pinned** | Fine-grained targets are sustainable only with generation, and maturity is uneven: `gazelle_rust` is one maintainer at one tag (0.1.0), and `aspect_gazelle_js` is BCR 1.2.1 while `aspect_gazelle_prebuilt` — the distribution its own README recommends — is 0.0.25. Hand-fine-graining without a generator buys the maintenance cost and none of the tooling; adopting on a sibling's version number buys a semver promise nobody made. | Three answers, all required: (1) `curl https://bcr.bazel.build/modules/<module>/metadata.json` for the module you will actually depend on — a version recalled from memory is the failure here; (2) name the plugin's own default generation mode from its own directive docs (Go and rules_python default to package level, `gazelle_rust` to **one target per file**); (3) wire BZL-ARCH-12's gate yourself and watch it pass. **All three answered = fine-grained is supported; a pre-1.0 version, an empty or 404 metadata response, or no plugin from a maintained org = coarse is correct and hand-fine-graining is the finding.** The gate's absence is never evidence about the plugin — no ecosystem ships it. | MUST |
| BZL-ARCH-12 | Verify generated `BUILD` files with the `gazelle_test` rule from `@gazelle//:def.bzl`, hand-wired — never with a bare `gazelle -mode=diff` shell invocation, and never by assuming the plugin supplied a gate. | The CLI mode is undocumented at the invocation level and runs outside Bazel; `gazelle_test`'s own `mode` attribute defaults to `"diff"`, so the check is hermetic and part of `bazel test //...`. **No ecosystem wires it by default** — bazel-gazelle's own root `BUILD.bazel`, rules_python 2.3.3's install doc, `gazelle_rust`'s example and Aspect's `aspect_gazelle()` macro all ship only the `gazelle()`/`sh_binary` half. `aspect_gazelle(with_check = True)` is the false friend: its `<name>.check` target is an `sh_binary`, so `bazel test //...` never runs it and no test-result aggregation sees it. | `bazel test //:gazelle_test` — **passing (empty diff) = generation is in sync; non-zero exit with a printed diff = finding.** Confirm it is a test at all with `bazel query 'kind("sh_test", //:gazelle*)'` — **empty output in a repo that believes it has a freshness gate is the finding.** For Python a second, distinct target covers manifest freshness (`//:gazelle_python_manifest.test`); conflating the two is a finding. | MUST |
| BZL-ARCH-29 | Name the generator for every checked-in generated file and back it with a `diff_test` (or `write_source_files`) regeneration gate before treating that file as source. | A generator that silently diverges from its checked-in output is a correctness bug wearing the costume of a source-of-truth question, and the divergence stays invisible until someone regenerates. | `grep -L 'diff_test\|write_source_files' <pkg>/BUILD*` for every package containing a generated-output directory. **Empty = pass; any package printed has generated content with no gate.** Then check the gate's CI reach under BZL-HERM-22's carve-out: where the generated file's shape is Bazel-major-dependent, one authoritative leg plus a comment naming the major adjacent to the excluding condition is the correct shape, and "every leg" is wrong. **A gate excluded from legs with no such comment = finding; one leg carrying the comment = pass.** | SHOULD |
| BZL-ARCH-13 | Review the regenerated `BUILD` diff by hand on every version bump of a pre-1.0 third-party Gazelle plugin; never auto-merge it. | `gazelle_rust` 0.1.0, `gazelle_cc` 0.1.0 and `aspect_gazelle_prebuilt` 0.0.25 are all pre-1.0, so semver promises nothing, and none is first-party to the ruleset it generates for. `gazelle_rust` is additionally single-maintainer, with main-branch activity months past its only tag and a roadmap the maintainer describes as one they may change. | The `MODULE.bazel` diff bumping the plugin and the regenerated-`BUILD` diff land in the **same** change and are reviewed as code. **Bump-only change with no `BUILD` diff, or a squashed regeneration = finding.** | SHOULD |
| BZL-ARCH-31 | Declare one `proto_library` per `.proto` set and attach each consuming language through its own wrapper or aspect; never duplicate the `proto_library` per language. | Four rulesets' own source converges on one `proto_library` plus N attachments — a forwarding macro for `cc_proto_library`, protobuf's *own* `py_proto_library` aspect (not rules_python's), `rust_prost_library`'s single mandatory `proto=` attribute, and a plain `deps=` reference for the experimental JS path — and none documents a per-language duplicate. Two descriptor sets for one schema drift silently until a wire-format mismatch. No source states the prohibition outright, which caps this below MUST. | `bazel query 'kind(proto_library, //...)'`, then check that no two targets name the same `.proto` in `srcs`. **One `proto_library` per `.proto` set = pass; two naming the same source = finding.** `rules_buf`'s `buf` Gazelle language emits `buf_lint_test`/`buf_breaking_test`/`buf_dependencies` only and never `proto_library`, which stays bazel-gazelle core's `proto` language. The Bazel 9 explicit-`load()` requirement is BZL-LARK-10's row. | SHOULD |
| BZL-ARCH-15 | Never attribute the phrase "1:1:1" to Bazel in generated guidance, review comments or shipped rule text. | "1:1:1" is Pants' own coined idiom, in Pants' own words; neither `configure/best-practices` nor `concepts/build-files` contains the string. Bazel converges on the same practice without ever naming it that way, so the citation is a checkable misattribution. | `grep -n '1:1:1' <artifact>`. **Empty = pass; any hit must name Pants in the same sentence, or be removed.** | MUST |

## Transitions and Configured-Target Duplication

The shared check is the `cquery` one-liner in BZL-ARCH-19, run before adding or
debugging any custom transition. `aquery` cannot answer this question — two
actions whose outputs share an `execPath` under different configurations render
as separate entries, documented and not a bug.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-19 | Measure configured-target duplication with `cquery` before adding or debugging any custom transition. | A per-edge transition that never resets configuration produces `2^n` configured targets down a depth-`n` tree — the canonical docs show it and then leave their own mitigation section as a literal `TODO`. | `bazel cquery 'deps(//path/to:target)' \| awk '{print $1}' \| sort \| uniq -c \| sort -rn`. **Every label at count 1 = pass; any label with count > 1 is built under more than one configuration = finding.** Follow up with `bazel config <hash>` and `--transitions=full`. This measures *configured-target* duplication, not action duplication — confirm with `aquery` on the flagged label before calling it wasted work. **Empty output = the cquery did not resolve that target — re-scope it, never read it as a pass.** | MUST |
| BZL-ARCH-21 | Before shipping an outgoing reset transition as the fix for duplicate builds, trace every target downstream of the reset point for a reader of the value being reset. | A reset transition silently hands the **wrong** value to anything genuinely downstream — no error, no warning, a successful build with wrong output. | For each target reachable through the reset boundary, look for a `select()`/`config_setting` reading the same setting. **No downstream reader = pass; one found = the reset is not the fix.** | MUST |
| BZL-ARCH-20 | Never derive an action count from `aquery` output by counting lines or deduplicating on output path. | `aquery` documents that two actions whose outputs share an identical `execPath` under different configurations still render as **separate** entries, and that its output order is unspecified — so a naive line count silently under- or over-counts, and it is the first thing reached for to answer "did this transition double my build". | Negative check: `grep -rn 'aquery' <scripts-dir> .github` and read each hit. **No script parsing `aquery --output=text` positionally or counting its lines = pass; one that does = finding.** | MUST |
| BZL-ARCH-22 | Do not adopt `--experimental_output_paths=strip` as a production default, and never carry its value across a major upgrade unread. | Default `off` and labelled "highly experimental" on **both** 8.7.0 and 9.2.0, two LTS majors after 7.4.0 shipped the action dedup it depends on. Its accepted value set is not stable either: 8.7.0 takes `off, content or strip`, 9.2.0 takes `off or strip` — an rc file carrying `content` is a clean build on the old pin and a flag-parse failure on the new one. | `grep -rn 'experimental_output_paths' .bazelrc*`. **Empty = pass; presence outside an explicitly labelled experiment or CI canary leg = finding; the literal value `content` on a 9.x leg = finding regardless.** | CONSIDER |

## Cross-Cutting Checks and Execution Groups

Both rows are caught by a query diff taken before and after the check is wired —
the macro side with `query`, the aspect side with `cquery`, because plain
`query` rejects `--aspects` outright (`Unrecognized option`, both majors).

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-30 | Carry a cross-cutting check (lint, format, docs freshness, codegen consistency) with an `aspect`, not with a `BUILD`-file wrapper macro. Reserve a macro for a check that must be addressable by its own label — something another rule `deps=` on, or a result CI gates on by test-target name. | Measured on a live five-package graph on 8.7.0 and 9.2.0: a macro emitting three rules per call adds **two net-new query-visible labels per call** (rules per call minus the wrapped target, which already existed), and its `_test` joins `bazel test //...`'s closure, so five wrapped packages bill five extra tests on every invocation while the identical aspect-only tree has no test closure at all. An **aspect adds zero** labels under any invocation. One Bazel 9 cost only the macro path pays, reproduced live: a legacy macro calling a bare `native.sh_test(...)` dies with `Error: no native function or rule 'sh_test'` until an explicit `bazel_dep` plus `load()` is added — autoload is BZL-LARK-10's and BZL-FLAG-16's row, and an aspect touches no native rule symbol. | Three diffs. Macro side: `bazel query '//...' --output=label` before and after wiring the check — **empty diff = pass; a non-empty diff = the macro changed the graph**, and its added-line count is rules-per-call minus one, per call (10 lines for five three-rule calls, measured); a finding unless the label is genuinely needed. Aspect side: `diff <(bazel cquery '//...' --output=label) <(bazel cquery '//...' --output=label --aspects='<label>%<aspect>' --output_groups=<group>)` — **empty diff = pass, it really is an aspect.** Cheapest: a `bazel query 'tests(//...)'` diff — **empty = pass**; on a tree whose only would-be tests are the check itself, `bazel test //...` exits **4** with `No test targets were found, yet testing was requested`, and that exit 4 is the pass. Folding the check into the compile action (the `nogo` shape) is a third option, open only to whoever authors that action. | SHOULD |
| BZL-ARCH-33 | Scope every `exec_properties` key to its owning execution group (`<group>.<key>`) wherever two targets in one package can share a source file; never set a bare key on one of a pair and not the other. | A bare key applies to **every** exec group on the target, including an implicit one both targets share. Two `py_test`s over one `.py` file — one carrying `exec_properties = {"foo": "bar"}`, one on the rule's defaults — generate the identically-named precompiled output under two differing action configurations, and Bazel refuses the build with an action conflict ([rules_python#2445](https://github.com/bazelbuild/rules_python/issues/2445), filed 2024-11-26, widened by its maintainer 2025-09-17, no fix landed). The group-scoped key (`py_precompile.foo`, not `foo`) is the maintainer's own workaround; the ruleset cannot fix it, because a target-level `exec_properties` deliberately outranks anything the rule sets. | For each file named in more than one target's `srcs` in a package — `bazel query 'same_pkg_direct_rdeps(<file label>)'` — compare those targets' `exec_properties`. **Identical, or absent from all of them = pass; a bare key present on one and not another = finding.** No grep resolves the pairing. | SHOULD |

## Migration State and the Bazel-Visible Tree

Caught by reading four files — `.gitignore` against `.bazelignore`,
`.bazelversion`, `.gitmodules` — and by two `git log` passes over the manifest
and the Bazel-side lockfile.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-ARCH-26 | List in `.bazelignore` every directory a legacy build system, a parallel checkout or an agent worktree owns; never assume `.gitignore` covers it. | Bazel does not read `.gitignore` — the word does not appear in `.bazelignore`'s own definition. The failure is `bazel test //...` glob-expanding into a directory nobody meant it to see and failing on a tree that is not part of the build. | For every top-level directory `.gitignore` excludes that contains buildable files or is a parallel checkout location, confirm it also appears in `.bazelignore`. **Both lists agreeing = pass; a directory in one and not the other = finding.** | MUST |
| BZL-ARCH-24 | Keep the language's own manifest (`Cargo.toml`, `pyproject.toml` plus lockfile, `package.json` plus lockfile) as the sole hand-edited source of dependency truth and regenerate the Bazel-side lockfile (`cargo-bazel-lock.json`, `requirements_lock.txt`, an `npm_translate_lock` ingestion) from it; never hand-edit the Bazel side to fix a drift. | Two independent declarations is the failure. The IDE's language server reads the manifest, never the Bazel graph, and `crate_universe` ignores path dependencies regardless — so a hand-edit on the Bazel side reintroduces the second source of truth invisibly. | (a) `git log --name-only <manifest>` — a manifest change carries the Bazel-side lockfile in the **same** commit (the repin ran before commit). (b) `git log -p --follow <bazel-side-lockfile>` — a commit touching only it, with no manifest change and no repin marker in the message, is the smell. **No manifest-only and no lockfile-only commits = pass.** Scope is language lockfiles only; `MODULE.bazel.lock` is BZL-MOD's, and its schema version moves on a Maintenance patch bump. | MUST |
| BZL-ARCH-27 | On Bazel 8.0.0 and later, move any `.bazelignore` entry standing in for a wildcard into `REPO.bazel`'s `ignore_directories()`. | `.bazelignore` "does not permit glob semantics" by its own definition, and `ignore_directories()` was added at 8.0.0 explicitly as a migration path off it. Proposing it on a 7.x pin is a version-floor miss with no error until someone runs it. | Read `.bazelversion` first — the directive does not exist on 7.x. Then read `.bazelignore` for near-duplicate lines or a comment of the "and every directory like this" shape. **No such entries = pass, no migration owed.** | SHOULD |
| BZL-ARCH-28 | Treat a git submodule that is also a build dependency as an open Bzlmod question, not a settled pattern, and check the fork's own remote for a `MODULE.bazel` before proposing `git_override` at all. | Bzlmod has **no submodule-equivalent primitive**. `git_override`, the nearest analogue, requires the vendored fork to declare its own `MODULE.bazel` — which it carries only if its upstream is itself a Bazel module. Until it does, the whole lineage is invisible to `bazel mod`: the query is not the blocker, the missing module is. | `git config -f .gitmodules --get-regexp path`, then check each path against the `Cargo.toml`/`package.json`/`pyproject.toml` dependency graph; for each hit that is a real build dependency, probe the fork's own remote with `git ls-remote <remote>` plus a raw fetch of `MODULE.bazel` at its default branch. **No submodule that is also a build dependency = pass; one whose fork has no `MODULE.bazel` = `git_override` is not an option yet.** Once modules exist, `bazel mod graph --extension_info`, `bazel mod explain <repo>` and `bazel mod show_repo <repo>` make the lineage visible (identical descriptions on the 8.7.0 and 9.1.0 versioned command pages; no 9.2.0 snapshot exists). **Empty `bazel mod` output on a repo with no `MODULE.bazel` means the query is aspirational, never that the coupling is absent.** | CONSIDER |

## Gaps

- Every probe behind this file ran on one Linux host: darwin and Windows are unmeasured for sandbox, tag and toolchain behaviour, and no 9.x release after 9.2.0 was tested.
- **No source publishes a numeric adoption threshold, at any scale** — not repo count, not target count, not CI minutes. BZL-CI-01's ~40-minute whole-repo CI wall clock and ~300-rule-target tripwire are the closest sourced signals; a go/no-go number offered without a quotable source is invented.
- BZL-ARCH-30's label arithmetic is measured; its *analysis-time* cost at repository scale is not — the fixture's targets are noise-dominated, and Skymeld merges the loading/analysis phase marker so `--profile` cannot split them.
- No primary source publishes a `config_setting`-sprawl threshold or a rule-count-to-source-file target, so neither gates anything here; report the number, never fail on it.
- `ctexplain`'s `culprits`, `forked_targets` and `cloned_targets` analyses are "not yet implemented" stubs six years on and it ships only inside the Bazel source tree, so only its `summary` works — which BZL-ARCH-19's one-liner already computes.

## What Agents Get Wrong Here

1. **Writes `config_setting(values = {"cpu": "x86"})`** because it is shorter
   and was the only form for years; pre-2022 training data is dense with it
   (BZL-ARCH-16).
2. **Assumes Bazel honours `.gitignore`**, ports a repo with no ignore rules,
   and the first `bazel test //...` globs into a worktree or a vendor tree
   (BZL-ARCH-26).
3. **States a flag's default from a doc page instead of from the binary** — the
   error that made this file claim a guarantee already in force when
   `--incompatible_no_implicit_file_export` reads `false` on both majors
   (BZL-ARCH-09).
4. **Names a buildifier lint for public `default_visibility`**; no such category
   exists in `WARNINGS.md`, and the correct answer is the grep (BZL-ARCH-04).
5. **Cites "1:1:1" as Bazel's own recommended pattern**, conflating Pants'
   vocabulary with Bazel's (BZL-ARCH-15).
6. **Recommends a hand-written outgoing reset transition as "the fix"** for
   duplicate builds; it builds successfully and produces silently wrong output
   downstream (BZL-ARCH-21).
7. **Treats `aspect_gazelle(with_check = True)` or `gazelle -mode=diff` as a
   wired CI gate** and reports the job done; the first produces an `sh_binary`,
   the second runs outside Bazel (BZL-ARCH-12).
8. **Presents `transitive_visibility` as available on Bazel 8, or passes it a
   list** — a hard `unexpected keyword argument` through 8.8.0, and a type error
   on 9.x (BZL-ARCH-34).
