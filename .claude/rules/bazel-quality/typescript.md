---
title: TypeScript and JavaScript Under Bazel
summary: The BZL-JS family — why `bazel build` cannot fail on a type error, pnpm as the only ingestion surface, and the rules_js/rules_ts traps that read green
---

# TypeScript and JavaScript Under Bazel

Owns `BZL-JS`: `rules_js` and `rules_ts`, pnpm as the lockfile of record,
`npm_translate_lock` and `npm_link_all_packages`, `ts_project` and its
transpiler, JS test runners and bundlers under Bazel, the editor-host and
`@vscode/test-electron` shape, Aspect's JS/TS Gazelle plugin, and JS coverage.
It does not own `bazel_dep`, registry or lockfile-mode mechanics (BZL-MOD), the
remote-cache and download flags (BZL-CACHE), test tag and result-caching
semantics (BZL-TEST), macro and aspect graph shape or the Gazelle freshness gate
(BZL-ARCH), or the buildifier and loading-phase gate (BZL-LARK). Non-Bazel
hygiene for `package.json`, `tsconfig.json` and lockfile kind belongs to the
`typescript-packaging` set (TS-PKG-13 owns lockfile kind); a rule below touches
one of those files only where the edit exists for Bazel, and names the file.

Contents: [The Typecheck Gate](#the-typecheck-gate) ·
[Transpiler Selection](#transpiler-selection) ·
[pnpm, the Lockfile, and the Directory Walk](#pnpm-the-lockfile-and-the-directory-walk) ·
[Ported Snippets and Retired Names](#ported-snippets-and-retired-names) ·
[Failure Signatures and the Reflex Fix](#failure-signatures-and-the-reflex-fix) ·
[Coverage](#coverage) ·
[Test Runners, the Editor, and Ruleset Health](#test-runners-the-editor-and-ruleset-health) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-09-06 against Bazel 8.7.0, 8.8.0 and 9.2.0 on a Linux host; rows
that could read differently on darwin or Windows say so. Binds `aspect_rules_js`
3.4.1 (floor 3.0.0), `aspect_rules_ts` 3.10.1 (behaviour floor 2.0.0), pnpm ≥ 9,
TypeScript ≥ 5.5 for the isolated-declarations rows, and `aspect_gazelle_js`
1.2.1. Before citing any Bazel flag named here, re-check it at your pinned
version on **both** help surfaces — `bazel help <command> --long` and
`bazel help startup_options` — because a startup option is invisible to the
first. Every grep and buildozer check below reads the repository's own
checked-in BUILD, `.bzl`, rc and JSON files: BUILD or `.bzl` text generated into
an external repository by `npm_translate_lock`, `npm_link_all_packages` or any
module extension is out of their reach, and a clean grep says nothing about it.

## The Typecheck Gate

`bazel build` is not a gate for TypeScript, and the whole family hangs on that
one fact. With `transpiler`, `declaration_transpiler`, `no_emit` or
`isolated_typecheck` set — and a transpiler is mandatory since rules_ts 2.0.0,
so this is every target — the default output group is JavaScript only and the
typechecker never runs. The check for this section: enumerate the gates with
`bazel query 'filter("_typecheck_test$", //...)'`, then run `bazel test //...`
over a target set that contains them. Empty output from that query in a
repository with `.ts` sources under BUILD files is the finding, never a pass.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-11 | Never report a `ts_project` as clean on the strength of `bazel build`; run `bazel test //pkg:name_typecheck_test`, or `bazel build --output_groups=typecheck //pkg:name`, before making the claim. | The build goes green with a live type error, and a green build is what an agent reports. This is the sharpest failure mode in the family: nothing in the output says the typechecker did not run. | The section query, then the named test per target. **Empty query output in a repository holding `.ts` sources under BUILD files is a FINDING, not a pass** — either no target generates the gate, or the wrong query was used. Never `bazel query 'kind(ts_project, //...)'`: it matches rule kinds and `ts_project` is a macro wrapping a private rule, so it returns empty on a repository full of `ts_project` calls, and empty reads as pass. `--output_groups` present on 8.7.0 and 9.2.0. | MUST |
| BZL-JS-12 | Make `bazel test` the CI verb of record for every tree containing `ts_project` targets, over a target set that includes every `_typecheck_test`. | A `bazel build //...`-only pipeline is structurally incapable of failing on a type error, so BZL-JS-11's per-target gate is inert and the adoption bought nothing. Build-only smoke legs are correct until the first `ts_project` lands, and become a false green that hour. | `grep -rn "bazel test" .github/workflows/` scoped to the migrated tree, then diff its target patterns against the section query. **Empty output — no `bazel test` anywhere — is the finding: the tree is ungated; a pattern narrower than `//...` needs the diff run explicitly.** | MUST |
| BZL-JS-15 | Do not set `--@aspect_rules_ts//ts:validation_typecheck` in a checked-in rc file without an adjacent comment naming the cost it accepts. | It turns type-checking into a validation action on *every* `bazel build`, reintroducing exactly the cost a custom transpiler and `isolated_typecheck` exist to avoid; the ruleset calls it discouraged. It is the first flag an agent reaches for after learning BZL-JS-11. | `grep -rn "validation_typecheck" --include='*bazelrc*' .`. **Empty is the pass — flag unset, fast path intact.** A hit with a comment is an accepted exception; a hit without one is the finding. | MUST |
| BZL-JS-20 | Never set `isolated_typecheck = True` on a target whose `tsconfig.json` lacks `"isolatedDeclarations": true`; that tsconfig key is the prerequisite edit, and it is the `typescript-packaging` set's file. | The action-graph split without the tsconfig option removes no sequential cost at all, so the flag buys nothing; and the option may be illegal until every exported symbol carries an explicit annotation, which is a source change, not a build change. | `grep '"isolatedDeclarations"' <the tsconfig the target names>`, and confirm `tsc --noEmit` is green under it before touching the BUILD file. **Absence of the key is the finding; present plus a green `--noEmit` is the pass.** | MUST |

```bash
# wrong — both go green while a type error is live in the sources
bazel build //pkg:app
bazel query 'kind(ts_project, //...)'            # empty: ts_project is a macro
```

```bash
# right — the label regex finds the generated gate, and the gate is a test
bazel query 'filter("_typecheck_test$", //...)'
bazel test //pkg:app_typecheck_test
```

## Transpiler Selection

The check for this whole section is two commands:
`buildozer 'print transpiler' //pkg:%ts_project` per package — buildozer matches
the call name written in the file, so it survives the macro — plus
`grep -rn "default_to_tsc_transpiler" --include='*bazelrc*' .`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-13 | Name a `transpiler` on every hand-written `ts_project`; do not adopt `--@aspect_rules_ts//ts:default_to_tsc_transpiler` repo-wide as a way of never deciding. **pinned** | [rules_ts 2.0.0](https://github.com/aspect-build/rules_ts/releases/tag/v2.0.0) removed the default because none is universally right, and a target without one hard-`fail()`s with "Required Transpiler Selection". The flag is legitimate as a migration step and a defect as a permanent state — it silences that `fail()` for targets nobody has decided about yet. | The section commands. **Empty from both — every call site names one, flag unset — is the pass.** A `ts_project` printing an empty `transpiler` is the finding. Where BUILD files come from Aspect's Gazelle plugin, BZL-JS-33 owns the case; this row binds hand-written targets. | SHOULD |
| BZL-JS-14 | Start every target at `transpiler = "tsc"`; move one to SWC only after confirming its `tsconfig.json` sets no `emitDecoratorMetadata`, that no runtime code mutates a named CJS export, and that a profile shows `tsc` on the critical path. **pinned** | SWC's two documented output gaps — type-only import elision and ESM→CJS export mutability — fail at *runtime*, not at build time, while the reward is only build speed. The evidence for both is an upstream discussion thread, so this pin takes the conservative side of an asymmetry rather than settling a measurement. An adopter reverses it in this row, never per call site. | `grep -rn --include='tsconfig*.json' "emitDecoratorMetadata" .`; read the package for named-export mutation across a CJS boundary; `bazel build --profile=<file>` for the `tsc` share. **An empty grep makes SWC the lower-risk option for that target; a hit means stay on `tsc`, never silently keep SWC.** | CONSIDER |
| BZL-JS-33 | Never assume Aspect's JS/TS Gazelle plugin sets `transpiler=` on a generated `ts_project` — it never emits the attribute. Resolve it one of exactly two ways and record which in the same commit: a `# keep`-anchored `transpiler=` on each generated target, or the repo-wide flag as a reviewed, commented exception. | `transpiler` is absent from the plugin's own kinds map and from its whole generator source at `aspect_gazelle_js` 1.2.1, and the generated call loads the native macro, so nothing downstream injects a default; rules_ts ≥ 2.0 hard-`fail()`s without one. The plugin's own smoke test papers over this with the repo-wide flag, and no open issue tracks the gap. This is the row that makes "BUILD files are generated, never hand-written" unreachable without a named exception. | The section commands. **A `ts_project` with an empty `transpiler` and no adjacent `# keep`, or a flag hit with no comment, is the finding; empty from both is the pass.** Do not substitute `bazel query 'kind(ts_project, …)'` — see BZL-JS-11. | MUST |

## pnpm, the Lockfile, and the Directory Walk

pnpm is not a preference, it is the ingestion surface: the virtual store is the
only `node_modules` layout that decomposes into discrete cacheable Bazel
actions. Every row here reads two things — `head -1 pnpm-lock.yaml`, and the
repository's ignore configuration.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-01 | Confirm a real `pnpm-lock.yaml` at `lockfileVersion` 9 or higher exists before wiring any `npm_translate_lock`; convert the repository to pnpm first (`pnpm import`) rather than pointing the rule at an npm or yarn lockfile "for now". | rules_js's linker only decomposes into cacheable actions for pnpm's virtual store, so an npm- or yarn-locked repository faces a one-shot conversion, not a partial path. Wiring the rule at the wrong lockfile spends the migration budget and produces no cacheable graph. | `test -f pnpm-lock.yaml && head -1 pnpm-lock.yaml`. **Empty output (no such file), or a lower version, is the finding — an unmigrated repository; `lockfileVersion: 9` or higher is the pass.** With Aspect's Gazelle plugin in play, BZL-JS-34 narrows the accepted range further. | MUST |
| BZL-JS-03 | Never point a lockfile attribute at a `bun.lock` and never write `bun_lock =` — the attribute does not exist; a bun-locked package converts fully to pnpm before rules_js can see any dependency. | `npm_translate_lock` defines exactly `pnpm_lock`, `npm_package_lock` and `yarn_lock`; no feature request for a bun lockfile has ever been filed. An agent extrapolates the attribute because bun "is basically npm", and the error arrives as an unknown-attribute failure far from the assumption. | `grep -n 'attr\.' npm/private/npm_translate_lock.bzl \| grep -i lock` against [the source at v3.4.1](https://github.com/aspect-build/rules_js/blob/v3.4.1/npm/private/npm_translate_lock.bzl). **The absence of a `bun_lock` line is the confirmation, not a gap in the check.** | MUST |
| BZL-JS-04 | Keep pnpm's `node_modules` out of Bazel's directory walk with `ignore_directories(["**/node_modules"])` in `REPO.bazel` (Bazel 8+), or an entry in `.bazelignore`; never rely on `npm_translate_lock(verify_node_modules_ignored = …)` as the guard. | That attribute is the deprecated Bazel 7.x path. A snippet carried forward keeps it, gets no protection on 8 or 9, and Bazel's walk then collides with pnpm's tree on the first workspace added. | `grep -n "ignore_directories" REPO.bazel; grep -n node_modules .bazelignore`. **Both empty in a repository that has a `pnpm-lock.yaml` is the finding; a hit in either is the pass.** | MUST |
| BZL-JS-34 | Read `pnpm-lock.yaml`'s `lockfileVersion` before adopting Aspect's JS/TS Gazelle plugin: only majors 5, 6 and 9 resolve at `aspect_gazelle_js` 1.2.1. | The plugin's pnpm parser reads the first `lockfileVersion:` line and dispatches on the major, erroring on anything else. pnpm 12's multi-document lockfile is an open upstream bug with two competing unmerged fixes as of 2026-09-05, so npm-import resolution fails outright on a pnpm-12-locked workspace. | `head -1 pnpm-lock.yaml`. **A major outside {5, 6, 9} with this plugin pinned is the finding; empty output (no lockfile) is BZL-JS-01's finding, not this row's pass.** | SHOULD |
| BZL-JS-36 | Before running Aspect's JS/TS Gazelle plugin, confirm that every directory holding a `package.json` it will visit appears as an `importers:` key in the configured `pnpm-lock.yaml`; a directory that `pnpm-workspace.yaml` globs but pnpm never installed becomes a real pnpm workspace member first — that edit is to `pnpm-workspace.yaml` and the lockfile, and it exists for Bazel. | The plugin decides pnpm-project membership **solely** from the lockfile's parsed `importers:` map, never from `pnpm-workspace.yaml`'s globs, while recording every `package.json` directory regardless. A non-importer directory gets neither a public package target nor `npm_link_all_packages`, and every bare-specifier import returns `Resolution_NotFound` — under the plugin's default `js_validate_import_statements = error` that is a hard, named generation-time failure. No `# gazelle:` directive fixes it; the alternatives are `off`/`warn` validation or hand-maintaining that package's BUILD file. | Against the lockfile the `# gazelle:js_pnpm_lockfile` directive names: `awk '/^importers:/{f=1;next} /^[^[:space:]]/{f=0} f&&/^  [^[:space:]]+:/{sub(/:[[:space:]]*$/,"");gsub(/^ +/,"");print}' pnpm-lock.yaml \| sort > imp.txt`, then `find . -name package.json -not -path '*/node_modules/*' \| xargs -n1 dirname \| sed 's#^\./##' \| sort > pkg.txt`, then `comm -13 imp.txt pkg.txt`. POSIX `awk`/`find`/`comm` only, so a CI runner reads the same. **Empty `comm` output is the pass; every line it prints is a directory Gazelle will refuse to resolve — a will-fail-generation finding. An empty importer extraction in a repository that HAS a lockfile means that lockfile carries no `importers:` block at all: a wrong query, never a clearance.** | MUST |

## Ported Snippets and Retired Names

One grep each, over the repository's own BUILD, `.bzl`, `MODULE.bazel`, rc and
tsconfig files, for a name that no longer means what training data thinks it
means. rules_js 3.0.0 and rules_ts 2.0.0 are the two boundaries.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-02 | Reject any rules_js snippet that configures the ruleset through `WORKSPACE`, targets Bazel 6, assumes pnpm below 9, or passes `prod=`, `dev=`, `defs_bzl_filename=`, `link_workspace=`, `additional_file_contents=`, `root_package=` or `link_packages=`. | [rules_js 3.0.0](https://github.com/aspect-build/rules_js/releases/tag/v3.0.0) deleted Bazel 6, `WORKSPACE` and pnpm < 9 support outright and removed those attributes, so a training-data-era snippet hard-errors. Bazel 9.0.0 deleted the `WORKSPACE` code path entirely, which turns the same snippet into an unrelated-looking load failure. | `grep -rn "aspect_rules_js" WORKSPACE* 2>/dev/null`, then `grep -n -e 'prod *=' -e 'dev *=' -e 'defs_bzl_filename' -e 'link_workspace' -e 'additional_file_contents' -e 'root_package' -e 'link_packages' MODULE.bazel`. **Empty from both is the pass.** On a Bzlmod-only repository the first is trivially empty, so the second grep is the one that bites. | MUST |
| BZL-JS-22 | Replace any ported `ts_project(supports_workers = True/False)` with the tri-state `-1`/`0`/`1`, or drop the attribute. | The attribute changed type at rules_ts 2.0.0 and persistent-worker mode is no longer the default, so a boolean either errors or means something the author did not intend. | `grep -rn "supports_workers" --include=BUILD.bazel --include=BUILD --include='*.bzl' .` and read each value. **Empty is the pass, and is now the common case.** | MUST |
| BZL-JS-35 | Never name `ts_proto_library` as the current TypeScript protobuf path. The replacement is rules_js's `js/proto.bzl` — one ordinary `proto_library` referenced from an ordinary `js_library`'s `deps`, with a `js_proto_toolchain` behind it — and it ships marked EXPERIMENTAL, so name that status every time you recommend it. | [`ts_proto_library`'s own docstring at v3.10.1](https://github.com/aspect-build/rules_ts/blob/v3.10.1/ts/proto.bzl) reads "This API has been replaced by rules_js", and the replacement's header warns it "is subject to breaking changes outside our usual semver policy". Citing the old rule ships a migration the ruleset abandoned; citing the new one as stable ships a dependency on an unstable API. | `grep -rn "ts_proto_library" --include=BUILD.bazel --include=BUILD --include='*.bzl' .` — **empty is the pass.** Before recommending the replacement, re-read the header of `js/proto.bzl`: removal of the EXPERIMENTAL banner, not a version number, is what makes it citable as stable. | MUST |
| BZL-JS-18 | Do not edit `outDir` or `declarationDir` in a `tsconfig.json` feeding a `ts_project` expecting the output to move. | Bazel ignores both and always writes under `bazel-out/…/bin`. `validate = True` checks alignment, but nothing stops a plausible, inert value from misleading the next reader into a second wrong edit. | `grep -n -e '"outDir"' -e '"declarationDir"' <the tsconfig the target names>`, in packages holding a `ts_project`. **Empty means nothing to check; a hit is not automatically wrong, but it reads as decoration, never as configuration.** | CONSIDER |

## Failure Signatures and the Reflex Fix

Each row names a build- or run-time failure and the one-line fix that makes it
worse. The check is per row; where the row says reading heuristic, an empty
grep is never a clearance.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-16 | Never let two `ts_project` targets list the same `.ts` path in `srcs`, and read `TS5033: EPERM` on a `bazel-out` path as that collision, never as a filesystem permission problem to fix with `chmod`, a retry, or `--sandbox_writable_path`. | Bazel allows one producer per output: built together, the two targets error on conflicting `.js`; built separately — two CI shards, or a developer building one target at a time — whichever ran last wins the output path with no error at all. It is the family's only silent-corruption defect, and `EPERM` is its downstream signature, which pattern-matches OS permission errors in training data. | Per package holding more than one `ts_project`: `buildozer 'print srcs' //pkg:%ts_project` and intersect the lists. **An empty intersection is the pass.** On a live failure, `--listFiles` plus `--explainFiles` (TypeScript ≥ 4.2) shows whether a `.ts` resolved where the sibling `.d.ts` belonged. `--sandbox_writable_path` exists on 8.7.0 and 9.2.0 and is still the wrong reach. | MUST |
| BZL-JS-08 | Classify every "module not found" into exactly one of three branches before editing: a first-party `require` (add the package to the target's `data` and to `package.json#dependencies`), a genuine upstream under-declaration (add `pnpm.packageExtensions` **and re-run `pnpm install`**, because rules_js reads only the lockfile), or a plugin-discovery tool (`public_hoist_packages` — which still needs its own explicit `//:node_modules/<pkg>` dep on the target). | It is the most common rules_js error, and guessing the branch — the reflex is hoisting when the fix is a `data` dep — widens the hoist surface and buries the real under-declaration. Hoisting changes the tree layout, not the dependency graph, so a hoist without the dep edge fails a second time and the second failure looks like the first. | Reading heuristic: does the failing `require` originate in first-party `srcs` (branch a), or inside `node_modules/<pkg>` — then read that package's own `package.json` to separate (b) from a runtime string or glob plugin walk (c)? For branch (b), check yarn's extensions database first; rules_js does not consume it. **No grep substitutes and there is no empty-output reading.** For every entry in `public_hoist_packages`, grep the BUILD files for a matching `//:node_modules/<pkg>` dep: an empty dep list for a hoisted package is the finding. | MUST |
| BZL-JS-05 | Route every custom rule, macro or `genrule` that runs a Node tool through `js_run_binary` or `js_binary_lib.run_binary_action`; never call `ctx.actions.run` on a JS binary directly. | rules_js runs Node tools with cwd inside `bazel-out`. A raw call sets no `BAZEL_BINDIR` and re-paths nothing, producing a spurious "file not found" at action time that reads as a missing dependency. | Scoped to `.bzl` files that load a rules_js symbol: `grep -rl "aspect_rules_js" --include='*.bzl' . \| xargs grep -n "ctx.actions.run(" \| grep -v run_binary_action`. **Empty is the pass.** Unscoped, this grep fires on every Starlark repository and returns only false positives — the `aspect_rules_js` filter is what makes it a check. | MUST |
| BZL-JS-06 | Never state that the `BAZEL_BINDIR` tax, the ESM sandbox escape, or automatic consumption of yarn's extensions database has been fixed without checking the issue state first; and read a vitest ruleset's "vite, vitest, react and jsdom must be installed at root" workaround as a symptom of the same ESM bug, not as a separate mystery. | All three are open — bazelbuild/bazel#15470 (2022-05-11), [rules_js#362](https://github.com/aspect-build/rules_js/issues/362) (2022-08-05, on the ruleset's own Known Issues) and rules_js#1215 (2023-08-14) — and their age is exactly what makes an agent assert closure. The vitest ruleset's own troubleshooting doc says "we have not figured out why yet" and links #362, so the same bug reappears wearing a different costume and gets a fresh investigation. | `gh issue view 15470 -R bazelbuild/bazel --json state -q .state`, and the same for `362` and `1215` in `aspect-build/rules_js`. **`OPEN` is the pass and the constraint holds as written; `CLOSED` means re-verify the whole finding before repeating it; empty or errored output is a failed check, never a pass.** | MUST |
| BZL-JS-19 | Answer `TS2786` or `TS7016` by fixing the offending package's declared dependencies, or with a scoped `pnpm.packageExtensions` entry, never by turning on `skipLibCheck` repo-wide. | The error is a real defect that npm's flat hoisting used to hide — a package exposing a type from its own `devDependency`. A blanket `skipLibCheck` disables checking inside *every* dependency to silence one, and it is a plausible one-line diff. | Reading heuristic on the named package's own `package.json`: does the `@types/*` entry belong in `dependencies`? **No grep substitutes and there is no empty-output reading.** A new repo-wide `skipLibCheck` appearing in the same diff as the error is the finding. | SHOULD |
| BZL-JS-26 | Read a framework's own config for output-layout assumptions — Next.js `output: "standalone"`, Astro, SvelteKit dev codegen — before wiring it to `js_binary` or `js_run_binary`. | These tools write into their own idea of `node_modules` and `src`, and fail Bazel's tree-artifact validation with a symlink-resolution error rather than a recognisable import error. No blanket fix exists, so the diagnosis has to happen before the target is written. | Reading heuristic: grep the framework config for `output: "standalone"`, an `outDir` inside `src`, or documentation requiring a real `node_modules`. **No single grep covers every framework, so empty output is not a clearance.** | SHOULD |

```python
# wrong — the same .ts in two targets: conflicting outputs, or last-writer-wins
ts_project(name = "lib", srcs = glob(["src/**/*.ts"]), transpiler = "tsc")
ts_project(name = "cli", srcs = glob(["src/**/*.ts"]) + ["bin/main.ts"], transpiler = "tsc")
```

```python
# right — disjoint srcs, one edge between them
ts_project(name = "lib", srcs = glob(["src/**/*.ts"]), transpiler = "tsc")
ts_project(name = "cli", srcs = ["bin/main.ts"], deps = [":lib"], transpiler = "tsc")
```

## Coverage

Both rows are caught by one run, `bazel coverage //pkg:target`, read three ways:
the log for `code coverage requires a runfiles tree`, the
`--instrumentation_filter` Bazel prints as an INFO line, and
`grep -c '^DA:'` on the `_coverage_report.dat` path it names. Enumerate
candidates with `bazel query 'kind("js_test\|vitest_test", //...)'` —
`kind(js_test, …)` alone misses every vitest-ruleset target, and an empty result
from the narrow query is a wrong query, not a clearance. Measured on Linux;
Windows additionally needs `--enable_runfiles` for the runfiles tree to exist.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-24 | Give a `js_test` or `vitest_test` with many instrumented files a larger `size`/`timeout` for `bazel coverage` than for `bazel test`, and prove the run instrumented something — a runfiles tree **and** a non-zero `DA:` count — before quoting any coverage number. | The lcov conversion runs inside the test's own spawn and budget. That is a Bazel-core property, not a rules_js quirk: `collect_coverage.sh` is substituted for the test binary for every ruleset, and the coverage post-processing shares the test's own spawn and budget (BZL-TEST-02 owns the flag and its per-version default). With `--instrumentation_filter` unset, Bazel auto-computes it from the **test target's own package**, so an implementation in `//lib/foo` exercised from `//tests/foo` is instrumented not at all — a silent 0% with no error. | The section run, all three readings. **Absence of a timeout and absence of the runfiles string is NOT a pass; a zero `DA:` count is exactly the failure this row exists to catch.** A CI runner that flips either coverage flag on splits post-processing into its own spawn, which still belongs to the same `TestRunnerAction` and can still TIMEOUT. | SHOULD |
| BZL-JS-25 | Depend on first-party code under test as a `js_library`, never as a repackaged `npm_package`, wherever that code's coverage is required. | `npm_package` produces a store copy with no link back to sources, so the report comes back empty by design, not by bug — and an empty report beside a passing test reads as a tooling glitch. | `bazel query 'kind("js_test\|vitest_test", //...)'`, then per test `bazel query 'deps($t) intersect kind(npm_package, //...)'`. **Empty is the pass — but only from the widened kind pattern above.** | MUST |

## Test Runners, the Editor, and Ruleset Health

Caught by reading what a target is actually tagged, what a module name resolves
to on the Bazel Central Registry, and what a repository's tag list says about
the ruleset behind it. Tag semantics themselves belong to BZL-TEST; `bazel_dep`
resolution belongs to BZL-MOD.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JS-32 | Never judge a JS ruleset's health by grepping its README for "maintained" or "deprecated", and never read GitHub's `/releases/latest` as the current version — read the tag list sorted by semver and compare the newest tag's date against today. | None of six surveyed bundler and test rulesets uses either word, so the grep is empty for a five-week-old release and an 18-month-stale one alike. And `/releases/latest` returns rules_js **v2.9.3**, a 2.x-line patch published 110 minutes after v3.4.1 on the same day, because "latest" is newest by clock, not by semver: an agent trusting it reports the wrong major. The usable second signal is doc rot — a `WORKSPACE` snippet sitting beside an existing `MODULE.bazel`. | `gh api repos/<org>/<repo>/tags --jq '.[].name' \| sort -V \| tail -5`, plus `gh api repos/<org>/<repo> --jq .pushed_at`. **An empty README grep reads as nothing at all — never as "unknown, proceed with caution", and never as a clearance.** | MUST |
| BZL-JS-31 | Prescribe exactly two things for editor and tsserver resolution — a real `node_modules` at the source root from an ordinary, non-Bazel `pnpm install`, and a `paths` entry in a committed root `tsconfig.json` for first-party workspace imports (the `typescript-packaging` set's file, edited here because `node_modules` lives under `bazel-out`). Never offer `npm_link_all_packages()`, a `bazel run` link step, or a symlinked `node_modules` as the editor mechanism. | rules_js's own FAQ prescribes exactly those two and nothing else. `npm_link_all_packages()` generates `//:node_modules/<pkg>` **Bazel-graph** targets that tsserver never sees, so offering it produces a confident fix that changes nothing in the editor. rules_ts's own example config carries the inline comment that `paths` is for the editor and "isn't needed for Bazel". | `test -d node_modules` at the source root, and `jq '.compilerOptions.paths' tsconfig.json` on the committed root config. **Empty output from `test -d` (no source-root `node_modules`), or `null` from `jq` in a repository whose sources import a first-party workspace package, is the finding.** An inline-dictionary `tsconfig` attribute with no on-disk file is a finding for the same reason: editors then skew from the build. | MUST |
| BZL-JS-29 | Never wire a `bun test` suite to a Bazel target on the assumption a ruleset exists — none does, and the package converts to pnpm first (BZL-JS-01, BZL-JS-03) before the test question is reachable. A `@vscode/test-electron` suite does have a path — a plain `js_test`, no ruleset — but wire it only with `tags = ["e2e", "no-remote-exec", "requires-network"]`, a host-installed `Xvfb` on every runner image, the editor binary re-downloaded on each clean run, and the user-data socket created under the host tmpdir; never present that target as cacheable, remote-executable or hermetic. | There is no bun-test ruleset and no lockfile ingestion path, which is the stronger half. The editor-host half was once called a structural non-starter by mechanism, and that verdict was wrong: the shape ships in a public monorepo today. Its four costs are read from that BUILD file — `requires-network` because the pinned editor build downloads per invocation with no Bazel-level caching of the ~100 MB binary; `no-remote-exec` because remote workers have no Xvfb; the `xvfb` npm package shells out to a host-installed binary, so the action is non-hermetic by construction; and the user-data lock socket fails `EROFS` under `TEST_TMPDIR`. | `buildozer 'print tags' //pkg:target` must list all three tags, and the runner image must answer `command -v Xvfb`. **A missing tag, or empty output from `command -v Xvfb`, is the finding — an empty tag list is never a pass.** Xvfb is Linux-only; no darwin or Windows equivalent was measured. Before declaring any other JS tool a structural non-starter, run `gh api search/code` for its own import string plus `BUILD.bazel` and record the query — that search is what overturned this row. | MUST |
| BZL-JS-30 | Route a Playwright suite to the BCR module `rules_playwright`, and confirm from BCR metadata that the name resolves to `github:mrmeku/rules_playwright` before adding the `bazel_dep`. | Two unrelated GitHub projects carry that display name. Only one is BCR-published (v0.5.4, pushed 2026-03-06) and provides sha256-pinned browser downloads through a module extension composed with `npm_link_all_packages()` and a plain `js_test`; the other has no BCR entry, pins Playwright 1.49, and lists macOS support as validation pending. A GitHub name search surfaces the wrong one. | `gh api repos/bazelbuild/bazel-central-registry/contents/modules/rules_playwright/metadata.json --jq .content \| base64 -d`, then confirm `"repository": ["github:mrmeku/rules_playwright"]`. **A different repository string, empty output, or a 404 means stop — never fall back to a GitHub name search.** | MUST |

## Gaps

- Every command here was measured on Linux. Windows needs `--enable_runfiles`
  for coverage, and the `Xvfb` path in BZL-JS-29 has no measured darwin or
  Windows equivalent.
- `rootDirs` (plural) appears in neither ruleset's documentation as of
  2026-09-05; a repository that needs the virtual-merge behaviour is unguided.
- No organisation publishes cache-hit rate or CI time as a function of
  percentage-of-codebase-under-Bazel for a JS monorepo — the partial-adoption
  payoff point is unmeasured on both sides, so no row claims one.
- The rows that read `package.json`, `pnpm-lock.yaml`, `pnpm-workspace.yaml`,
  `tsconfig.json` or a source-root `node_modules` reach an agent only through
  index routing; this rule set globs none of those files.
- `aspect_gazelle(with_check = True)` emits an `sh_binary`, so `bazel test //...`
  catches no BUILD drift for a JS/TS tree. BZL-ARCH-12 owns the real gate.

## What Agents Get Wrong Here

1. **Reports "the build passed" after `bazel build //pkg:my_ts_project` and
   stops** — the default output group is JavaScript only (BZL-JS-11).
2. **Ports a 1.x/2.x-era snippet**: no `transpiler=`, removed
   `npm_translate_lock` attributes, `supports_workers = True` (BZL-JS-02,
   BZL-JS-22).
3. **Silences `TS2786`/`TS7016` with a repo-wide `skipLibCheck`** instead of
   fixing the one package's declared dependencies (BZL-JS-19).
4. **Treats `TS5033: EPERM` as an OS permission problem** and reaches for
   `chmod`, a sandbox flag, or a retry loop (BZL-JS-16).
5. **Invents `bun_lock =` on `npm_translate_lock`** because bun "is basically
   npm" (BZL-JS-03).
6. **Assumes an old open issue was surely fixed by now** — #362, #15470, #1215
   are all still open (BZL-JS-06).
7. **Reports the current rules_js version from `/releases/latest`** and names
   2.9.3, a 2.x patch published after 3.4.1 (BZL-JS-32).
8. **Runs Aspect's Gazelle plugin, sees BUILD files appear, and reports
   success** — the generated `ts_project` carries no `transpiler=`, and the
   `fail()` arrives later or never (BZL-JS-33).
