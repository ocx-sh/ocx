---
title: C++ Under Bazel
summary: The BZL-CC family — toolchain choice and registration, layering_check enforcement, sanitizer configs, wrapped foreign builds, protoc, and the claims about all of them that go stale
---

# C++ Under Bazel

Owns `BZL-CC`: which hermetic C++ toolchain a workspace registers and how, what
proves `layering_check` is enforcing, sanitizer configs, `rules_foreign_cc`,
`include-what-you-use`, C++20 named modules, `protoc`, the sandbox base, and every
capability claim about a C++ ruleset that a reader will otherwise repeat from
training data. It does not own the autodetection off-switch, the sandbox strategy
taxonomy or the Windows `--output_user_root` mitigation (`BZL-HERM`), the explicit
`load()` every `cc_*` and `proto_library` needs on Bazel 9 (`BZL-LARK`),
transitions, `config_setting`, `platform_mappings` or the one-`proto_library`
model (`BZL-ARCH`), download modes and remote-cache economics (`BZL-CACHE`), or
coverage mechanics and the lcov flag (`BZL-TEST`). Those are cited by ID once and
never restated here.

**No row here was exercised against a live C++ build.** Every row is grounded on
upstream sources read at a tag, on the rulesets' own issue trackers,
and — where a row says so — on real Bazel binaries run in scratch workspaces. None
of it was exercised against a live C++ build, so a clean check here is
upstream-grounded, not locally rehearsed.

Contents: [Proving layering_check Enforces](#proving-layering_check-enforces) ·
[Claims About a Toolchain or protoc](#claims-about-a-toolchain-or-protoc) ·
[Registering the Toolchain](#registering-the-toolchain) ·
[Attributes That Do Not Do What Their Names Say](#attributes-that-do-not-do-what-their-names-say) ·
[Sanitizer Configs](#sanitizer-configs) · [Wrapped Foreign Builds](#wrapped-foreign-builds) ·
[Platform Legs That Cannot Deliver](#platform-legs-that-cannot-deliver) ·
[The Local Developer Surface](#the-local-developer-surface) · [Gaps](#gaps) ·
[What Agents Get Wrong Here](#what-agents-get-wrong-here)

Severity maps onto the house tiers: MUST = Block, SHOULD = Warn, CONSIDER =
Suggest. Version-bound claims were measured 2026-09-06 against Bazel **8.7.0**,
**8.8.0** and **9.2.0** binaries, with **7.3.0** named where a fix landed and
**9.1.0** where a doc snapshot is the newest that exists. The rulesets this file
binds to: `rules_cc` 0.2.22, `toolchains_llvm` 1.9.0 plus its unreleased `main`
after PR #843, `hermetic_cc_toolchain` 4.3.0, `rules_foreign_cc` 0.15.1,
`protobuf` 36.1.bcr.1, `rules_fuzzing` 0.8.0, `bazel_iwyu` 0.0.4, `aspect_rules_lint`
2.9.0. Before citing any flag, run it against the pinned binary and read **both**
help surfaces — `bazel help <command> --long` **and** `bazel help startup_options`
— because absence from one is not absence from the binary. Every grep below reads
checked-in source only: BUILD and `.bzl` text written into an external repository
by a repository rule or module extension is not on disk when the grep runs, so a
clean grep is a statement about the source tree, and reports must say so. Timings
here were taken on one Linux host and could differ on a CI runner or off Linux.

## Proving layering_check Enforces

One command decides this whole block, per target and per configuration leg:

```
bazel aquery 'mnemonic("CppCompile", //path/to:target)' --output=text | tee /tmp/aq.txt | grep -c -- '-fmodules-strict-decluse'; wc -l < /tmp/aq.txt
```

The `grep -c` count alone cannot tell "enforcing: no" apart from "no `CppCompile`
action matched at all" — both print `0`. Read the two numbers together: a `wc -l`
of `0` means no action was matched — wrong target or configuration, read it as
nothing, never as a pass. A nonzero `wc -l` with `grep -c` at `0` means the feature
is not enforcing for that target today; `grep -c` `>0` means it is, on the real
spawned command line. Nothing else is evidence — not a `.bazelrc` line, not a green
build, not a clean IWYU report. It works identically on 8.7.0 and 9.2.0.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-12 | Never read a build's success, a build's failure, a `.bazelrc` line, or a clean IWYU report as evidence that `layering_check` enforces for a target; confirm it on the real spawned command line with `aquery`, per target and per configuration leg. | The feature can sit in a config CI never selects, be unsupported by the resolved toolchain (Bazel's own toolchains support it with clang on Unix and macOS only), or be cancelled by a later negative feature, which always overrides a positive one. IWYU is a disjoint signal, not a proxy: `bazel_iwyu`'s aspect derives its command line from `CcInfo.compilation_context` and `cc_common`'s compile variables and never references a `.cppmap` or `-fmodule-map-file=`. | The section command, read both numbers it prints. `grep -c` `0` with a nonzero `wc -l` = not enforcing for this target today, and a finding for any target believed covered. `wc -l` `0` — no `CppCompile` action at all — means the target or the configuration is wrong; read it as nothing, never as a pass. Re-run per CI matrix leg; on a Windows leg never substitute BZL-CC-14's strategy grep for it. | MUST |
| BZL-CC-14 | Do not repeat "`layering_check` silently stops enforcing outside a sandbox". State the corrected fact: the reported symptom was a spurious *failure* in the legacy dotd-based undeclared-inclusion checker, fixed in Bazel **7.3.0**; what is permanent is that only **direct** dependencies' module maps are staged as inputs, and Clang silently skips an `extern module` reference it cannot resolve. | The inverted claim sends a reviewer hunting a false negative that does not exist, and implies enforcement can be restored by changing execution strategy. It cannot: enforcement completeness is a property of the declared BUILD graph. On Windows the strategy question is closed anyway — `windows-sandbox` exists in Bazel's source but defaults off, needs a `BazelSandbox.exe` Bazel does not ship, and is documented nowhere, so a Windows leg is always `processwrapper-sandbox` or `local`. Evidence: [bazelbuild/bazel#21592](https://github.com/bazelbuild/bazel/issues/21592), where sandboxed succeeded and unsandboxed failed. | `cat .bazelversion` plus the CI matrix: every leg enabling `layering_check` must be ≥ 7.3.0 (both live majors clear it). Then `grep -rn 'spawn_strategy\|strategy=CppCompile' --include=*.yml .bazelrc* .github/workflows` — empty with a ≥ 7.3.0 floor is a pass **on Linux and macOS only**; on a Windows leg empty says nothing, so read enforcement from BZL-CC-12's `aquery` instead. A pre-7.3.0 floor anywhere = the historical symptom is reachable. | MUST |
| BZL-CC-05 | Wherever `--features=cpp_modules` is enabled, re-verify `layering_check` enforcement on a real target **and** on a seeded violation instead of assuming it survived. | `toolchains_llvm` passed `-Xclang -fno-cxx-modules` unconditionally for LLVM ≥ 14 precisely because Clang's C++20-modules default breaks Bazel's `use_module_maps` feature, which is what `layering_check` uses — and [PR #843](https://github.com/bazel-contrib/toolchains_llvm/pull/843) gates that suppression off exactly when `cpp_modules` is on, with no replacement. `rules_cc` 0.2.22's `layering_check` fragment targets `compile_actions` while the module machinery uses the disjoint `cpp20_module_compile`/`cpp20_module_codegen`/`cpp_module_deps_scanning` action types; neither references the other, and no test anywhere combines the two features. | `grep -rn 'cpp_modules' .bazelrc* --include='BUILD*' --include='*.bzl' .` — any hit requires the section `aquery` re-run on that build plus a target that `#include`s an undeclared header and must still fail. Empty = the feature is not in use, rule not applicable. A green build with `cpp_modules` on and no seeded violation is evidence of nothing. Blind to generated-repo text. | MUST wherever `cpp_modules` is on |
| BZL-CC-11 | Roll `layering_check` out per package or per target (`package(features = ["layering_check"])`), gated on two preconditions in order: an include-what-you-use pass has run on that package, and the toolchain's system module map exists. Never adopt it as one repo-wide `.bazelrc` flip. | `-fmodules-strict-decluse` only ever sees a directly written `#include`, so a package that has had no IWYU pass fails on noise instead of on real violations; and most systems ship no Clang module maps for the C/C++ standard library, so every `#include <stdio.h>` errors until `tools/cpp/generate_system_module_map.sh` or the hermetic toolchain's equivalent has run. LLVM's own Bazel build enables it for three packages, not repo-wide. | Wire the pass with `bazel_dep(name = "bazel_iwyu", version = "0.0.4")` — confirm on the BCR (`modules/bazel_iwyu/metadata.json`) that it resolves to `RealtimeRoboticsGroup/bazel_iwyu`, never the stale `storypku/bazel_iwyu`, which carries the identical module name — plus `build:iwyu --aspects @bazel_iwyu//:iwyu.bzl%iwyu_aspect` and `build:iwyu --output_groups=report`, then read the generated `<target>.<src>.iwyu.txt` before enabling the feature. `aspect_rules_lint` 2.9.0 ships no IWYU linter; without the aspect, BZL-CC-28's compilation database feeds standalone `iwyu_tool.py`. Cross-check `grep -rn 'features.*layering_check' --include='BUILD*' --include='*.bzl' .` against `grep -rn 'bazel_iwyu\|iwyu_aspect' MODULE.bazel .bazelrc*`: a package carrying the feature with an empty IWYU grep = finding; both empty = not adopted anywhere, which is not a finding. Blind to generated-repo text. | SHOULD |
| BZL-CC-13 | Never state layering-check rollout coverage from `bazel query 'attr(features, "layering_check", //...)'` alone; pair it with a grep for `package()`-level defaults. | `attr()` reads a target's own rule attribute. A `package(features = [...])` default is inherited and never appears in that query's results, so an empty `attr()` result is not proof the feature is unused. | Run both: `bazel query 'attr(features, "layering_check", //...)'` and `grep -rn 'features.*layering_check\|--features[= ]layering_check' --include='BUILD*' --include='*.bzl' --include='.bazelrc*' .`. Both empty = not adopted here. Query empty with a non-empty grep = the package-level blind spot this row exists to catch. Blind to generated-repo text. | SHOULD |

## Claims About a Toolchain or protoc

Caught the same way in every row: the claim names the ref it was read at, and that
ref is the one the workspace pins. `gh api repos/<owner>/<repo>/releases --jq
'.[0].tag_name'` gives the newest tag; the BCR listing gives what a `bazel_dep`
can actually resolve; a README code fence gives neither. A claim whose ref is
unstated does not ship.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-10 | Re-read the project's own README **at the ref the workspace pins** before repeating any capability, compatibility or version claim about a C++ toolchain — never carry one across from a sibling project, from a README's own code fence, or from `main`. | Four demonstrated instances, in both directions. Stale-behind: the "tested with `rules_go`/`rules_rust`/`rules_foreign_cc`" sentence belongs to `toolchains_llvm` and gets mis-attributed to `hermetic_cc_toolchain`, whose README has no compatibility section at all; `hermetic_cc_toolchain`'s own README example pins `version = "3.1.0"` while the release is 4.3.0; the 2024 statement of intent to retire the autodetected default is unexecuted, and `rules_cc`'s README documents that default and its off-switch as live. Ahead-of-release: `toolchains_llvm`'s `main` documents a `cpp_modules` feature absent from every tag and from the BCR. | For a version, read the releases API or the BCR listing, never a README code fence. For a capability, quote the paragraph from the project actually being cited and name the ref it was read at; diff that tag's README against `main` before repeating a feature claim. There is no grep here: an absent record reads as unverified, never as verified. No re-read, or a re-read whose ref is unstated, = the claim does not ship. | MUST (claim accuracy) |
| BZL-CC-04 | Before choosing `toolchains_llvm` for C++20 named modules, confirm in order that the pinned ref carries the feature — as of 2026-09-06 it exists only on `main` ([PR #843](https://github.com/bazel-contrib/toolchains_llvm/pull/843), merged 2026-09-02), in no tagged release and in no BCR version — and that the floor is Bazel **9.2.0+** with an LLVM **22+** distribution containing `bin/clang-scan-deps`. Never infer support from Bazel's own `module_interfaces` attribute existing at an earlier version. | The ruleset states that Bazel 7 and 8 do not expose the required `cc_library` module API, while `--experimental_cpp_modules` and the `module_interfaces` plumbing demonstrably exist in Bazel's builtins at 8.7.0 — the missing surface is toolchain-internal, so a grep of Bazel source yields a floor wrong in the permissive direction. `v1.9.0`'s README has no named-modules section and its `cc_toolchain_config.bzl` still passes `-fno-cxx-modules` unconditionally, so `bazel_dep(name = "toolchains_llvm", version = "1.9.0")` gets zero named-modules support. | `grep -n 'llvm_version\|bazel_dep(name = "toolchains_llvm"\|git_override\|archive_override' MODULE.bazel` — a plain `bazel_dep` on a released version with named modules in use is a finding; only a commit-pinned override carries the feature today. Empty = named modules are not configured, rule not applicable. | MUST |
| BZL-CC-02 | Never present a bare `CC=` or `--repo_env=CC=<path>` override as a hermetic toolchain, or as an instance of adopting `hermetic_cc_toolchain` or `toolchains_llvm`. **pinned** | It redirects only the autodetected toolchain's compiler probe: it registers no `cc_toolchain`, resolves nothing ahead of the default, and carries none of a real toolchain's cross-compilation, sysroot or sanitizer-feature machinery. Conflating the two is the most likely misreading of a workspace that has one, and such an override is machine-local and gitignored in every case this program measured, so CI never sees it. | `grep -n 'repo_env=CC=\|action_env=CC=' .bazelrc*` paired with `grep -n 'bazel_dep(name = "hermetic_cc_toolchain"\|bazel_dep(name = "toolchains_llvm"' MODULE.bazel`. First non-empty with second empty = a probe redirect; document it as exactly that. Empty on the first = not applicable. BZL-HERM owns its severity ladder and the one committed off-switch line that retires the reason it exists. | MUST (claim accuracy) |
| BZL-CC-32 | Never claim a workspace's registered C++ toolchain builds every binary in its graph while a `protobuf` ≥ 34.0 dependency is in it — `protoc` arrives as a prebuilt download by default there — and name the resolved protobuf version alongside every `prefer_prebuilt_protoc` citation, using the current path `--@protobuf//bazel/flags:prefer_prebuilt_protoc`. | The label move and the `False` → `True` default flip are BZL-FLAG-34's; this row owns only the toolchain claim that a prebuilt `protoc` is not your toolchain's output. Bazel 9 enforces protobuf ≥ 33.4 as a graph minimum and the BCR publishes 34.0 through 36.1, so almost every current install resolves above the flip. A checksummed download is hermetic, but it is not your toolchain's output and carries none of your sysroot, sanitizer or `layering_check` features. | Read the resolved version first (`bazel mod graph \| grep protobuf`, or the `protobuf` entry in `MODULE.bazel.lock`), then `grep -rn 'prefer_prebuilt_protoc' .bazelrc* MODULE.bazel`. Empty with protobuf ≥ 34.0 = the prebuilt default is in effect; document that instead of repeating a whole-graph toolchain claim. Empty with 33.4 = `protoc` is built from source by the resolved toolchain. A `//bazel/toolchains:` spelling under ≥ 34.0 is stale-but-working, and a finding for any doc citing it without a version. Blind to generated-repo text. | MUST (claim accuracy) |
| BZL-CC-27 | Keep `--experimental_cpp_modules` and `module_interfaces` to an isolated single-`cc_library`, single-interface experiment with the experimental-flag risk accepted in writing, and never state "Bazel supports C++20 modules" without naming the flag, the Bazel version checked, and a date. | The flag's own help text promises no guarantees about incompatible changes or even keeping the support; the multi-interface case a real module graph needs was merged 2025-12-10 and reverted 2026-01-08 over a measured ~425s CPU regression, and the 2017 tracking issue was closed 2026-01-19 and reopened the next day over that revert. The name that shipped, identically at 8.7.0 and 9.2.0 with default `false` and `EXPERIMENTAL` metadata, is `--experimental_cpp_modules`; `--experimental_cpp20_modules` never existed. | `grep -rn 'experimental_cpp_modules' --include=*.yml .bazelrc* .github/workflows` and `grep -rln 'module_interfaces' --include='BUILD*' .` — both empty = pass. Non-empty = a written risk acceptance must exist rather than an inherited copied example. To settle whether a flag exists at all, run it against the pinned binary and read both help surfaces named in the era paragraph above. | MUST |

## Registering the Toolchain

One query decides whether the autodetected default is still in the picture, on
both live majors:

```
bazel query 'somepath(//your:target, @local_config_cc//...)'
```

A non-empty result means the autodetected toolchain is in the resolved graph.
Empty means it is not reachable from that target. Empty from a target that failed
to configure means nothing at all. `BZL-HERM` owns the one committed off-switch,
the "register a hermetic toolchain" rule, and the "confirm it resolves ahead of
the default" rule; none of the three is restated here.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-01 | Choose the hermetic C++ toolchain from a three-branch constraint tree — named modules or the newest sanitizer story → `toolchains_llvm`; cheap bring-nothing cross-compilation, absorbing UBSAN-on-by-default → `hermetic_cc_toolchain`; exactly one target platform with no external release cadence to track → `rules_cc`'s own modular `cc_toolchain()` API — record which constraint decided it beside the registration, and on the third branch record whether the toolchain references `rules_cc`'s built-in `cc/toolchains/args/*` fragments by label. **pinned** | No upstream source endorses a default: `rules_cc`'s README states it does not yet offer a hermetic toolchain distribution and names four projects while endorsing none, so a pick made because a project was most-starred or first in a training corpus is unreviewable later, and the three cost profiles are mutually exclusive rather than ranked. The third branch's cost is measured and two-tiered: the public macros took 3 optional additions and **zero renames or removals** across 0.1.1 → 0.2.22 (21 releases, 14 months) and the maintained example needed two small edits, while the built-in argument fragments (ThinLTO, PIC, `layering_check`) had their `select()` conditions rewritten three times in five weeks at 0.2.19-0.2.22 with no release-note line naming an affected label. | Reading heuristic: does a comment or a PR description beside the `bazel_dep`/`register_toolchains` call name one of the three constraints? No named constraint anywhere = finding. Enumerate candidates with `grep -n 'toolchains_llvm\|hermetic_cc_toolchain\|cc_toolchain(' MODULE.bazel`; empty there in a workspace with real `cc_*` targets is BZL-HERM's finding, not this one. On a `rules_cc` bump, `grep -rn '@rules_cc//cc/toolchains/args' --include='BUILD*' --include='*.bzl' .` and diff those exact fragment paths between the two tags — empty = the bump needs no fragment review; non-empty with no recorded diff = finding. A release-note keyword search is not a substitute: 85 PR titles across that span, none path-scoped. | SHOULD |
| BZL-CC-03 | Treat a build whose resolved graph reaches `@local_config_cc` as running on the autodetected default — non-hermetic, gcc-and-binutils-preferring, CI-versus-local divergent — and move its C++ flags into a toolchain definition instead of accumulating them in `.bazelrc`. | The autodetected toolchain assumes `host = exec = target`, which is why a native ARM64 Windows build silently emits x64 and a Linux-to-Windows cross-compile silently mis-resolves; its only flag surface is the command line or an rc file, with no typed API. Bazel's C++ autoconfiguration probes for a working compiler at workspace setup whether or not any `cc_*` target is ever requested. The gcc-preference and no-API symptoms are practitioner-argued while the non-hermetic half is normative, which is why this row is SHOULD and not MUST. | The section query. Non-empty = the autodetected toolchain is in the resolved graph, and a finding for any workspace claiming hermeticity. Empty = not reachable from that target, which is the pass. | SHOULD |
| BZL-CC-06 | Escape `zig cc`'s UBSAN-on-by-default by setting an optimization level (`-c opt`, or an explicit `--copt=-O2`/`-O3`/`-Os`), never by hunting for a switch that disables UBSAN — none is documented. | `zig cc` infers debug mode, with its safety checks, purely from the absence of an optimization flag; this is the Zig project's own stated design. A program that compiles clean under mainstream clang or gcc then crashes with `SIGILL` from the toolchain switch alone, at Bazel's default `-c dbg`. | `grep -rn 'compilation_mode\|-c opt\|copt=-O' --include=*.yml .bazelrc* .github/workflows` in a workspace registering a zig-cc toolchain. Empty = every target builds at the default compilation mode with UBSAN on: a finding wherever `SIGILL` reports exist, and a latent one otherwise. | MUST wherever `hermetic_cc_toolchain` is registered |
| BZL-CC-09 | Set `cxx_include_layout = "yocto"` and the matching `multiarch` override explicitly whenever a `toolchains_llvm` sysroot is Yocto-built. | The default `"debian"` expects `/usr/include/<multiarch>/c++/<ver>` while Yocto ships `/usr/include/c++/<ver>/<multiarch>`. The mismatch does not error; it silently resolves the wrong libstdc++ headers. | `grep -n 'cxx_include_layout\|multiarch' MODULE.bazel` for every `sysroot`-carrying `llvm_toolchain`/`llvm.toolchain` call, then read each sysroot's build provenance. A Yocto-sourced sysroot with no `cxx_include_layout = "yocto"` = finding. Empty with no Yocto sysroot in play = not applicable. | MUST wherever a Yocto sysroot is used |

## Attributes That Do Not Do What Their Names Say

Both rows are caught by one grep over checked-in BUILD and `.bzl` text, and both
are blind to generated-repo content:
`grep -rn 'hdrs_check\|includes\s*=' --include='BUILD*' --include='*.bzl' .` —
empty is the pass, every hit is read by hand.

```python
# wrong — one entry mutates every reverse dependency's compile line, for as long as the edge exists
cc_library(name = "foo", hdrs = ["inc/foo.h"], includes = ["inc"])
# right — relabels how this target's own headers are addressed, and never leaves the target
cc_library(name = "foo", hdrs = ["inc/foo.h"], strip_include_prefix = "inc")
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-16 | Use `strip_include_prefix`/`include_prefix` to relabel how a target's **own** `hdrs` are addressed, and `includes` only where the search path is genuinely meant to reach every reverse dependency. The three are not interchangeable, and their "only legal under `third_party`" restriction is a documented convention, not a verified Bazel-side check. | The Build Encyclopedia's own emphasis is that `includes` flags "are added for this rule and every rule that depends on it. (Note: **not** the rules it depends upon!)" — one entry silently mutates every reverse dependency's compile line for as long as the edge exists, while the prefix pair never leaves the declaring target. No source located an enforcement mechanism for the `third_party` restriction, so reporting a violation of it as a "Bazel error" would be unverified. | The section grep, then read each `includes` hit: is the intent "every consumer needs this path" (`includes`, correct) or "let this target's own headers resolve" (`strip_include_prefix`, correct)? Empty = pass. Any claim that Bazel itself rejects the prefix attributes outside `third_party` must cite a reproduced error message or be stated as a convention. Attribute text is byte-identical at 8.7.0 and 9.1.0. | SHOULD |
| BZL-CC-15 | Never set `hdrs_check` on a `cc_library` or `cc_binary`, and never accept it as evidence of header-inclusion checking. | The Build Encyclopedia's text is "Deprecated, no-op", byte-identical at 8.7.0 and 9.1.0. It predates `layering_check` entirely and any value written there has zero build effect, while its name reads exactly like the knob someone hunting for strict deps was looking for. | The section grep. Any `hdrs_check` hit is a finding — a cargo-culted dead attribute — regardless of its value. Empty = pass. | SHOULD |

## Sanitizer Configs

Caught by `grep -n '^build:asan\|^build:tsan\|^build:ubsan\|host_features' .bazelrc*`
and reading each block. Empty output means no sanitizer config is shipped and
neither row applies.

```
# wrong — no --strip=never and no linkopt: backtraces arrive with no symbols
build:asan --copt=-fsanitize=address
# right — the whole block, every time
build:asan --copt=-fsanitize=address --linkopt=-fsanitize=address
build:asan --copt=-O1 --copt=-g --strip=never
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-18 | Ship sanitizers as named `.bazelrc` configs, each pairing `-fsanitize=<san>` with `-fno-omit-frame-pointer`, an explicit optimization level, `-g`, `--strip=never` and the matching `linkopt`. **pinned** on `-O1` as the shipped level; an adopter who wants `-O0` changes it once, in their own config. | `rules_cc`'s own `_sanitizer_feature` already adds `-fno-omit-frame-pointer` and `-fno-sanitize-recover=all` but sets neither `--strip` nor an optimization level — and `--strip` defaults to `sometimes` (strip iff `--compilation_mode=fastbuild`), so a sanitizer build without `--strip=never` yields backtraces with no symbols, defeating the point. The primary sources disagree on the level: grpc uses `-O0`, google/xls uses `-O1 -g`. The pin is `-O1` for stack-trace readability, and the disagreement is recorded rather than hidden. | The section grep: confirm each sanitizer block carries `--strip=never` and a `linkopt=-fsanitize=` matching its `copt`. A block missing either = finding. Empty = no sanitizer config shipped, rule not applicable. | MUST wherever a sanitizer config exists |
| BZL-CC-19 | Never set `--host_features=<sanitizer>` alongside `--features=<sanitizer>` unless build tools are deliberately meant to run instrumented. | `--features` applies only to targets built in the target configuration, by design, so a code generator or `protoc` built in the same invocation is uninstrumented for free. Adding `--host_features` for symmetry actively defeats that and instruments the build tools. This is a scope property of the flag, not a reset by any toolchain. | The section grep: a hit pairing a sanitizer name with `--host_features`, and no comment naming a deliberate instrumented-tool need, = finding. Empty = pass, with the default scope in effect. | SHOULD |

## Wrapped Foreign Builds

Enumerate the wrapped builds first —
`grep -rn 'cmake(\|configure_make(\|make(\|ninja(\|boost_build(' --include='BUILD*' --include='*.bzl' .`
— and empty output means `rules_foreign_cc` is not in use and this whole section
is inapplicable. The grep is blind to generated-repo text. `BZL-HERM` owns the
determinism side of an action that shells into a foreign build.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-22 | Never enable, or plan to enable, `features = ["layering_check"]` on a `cc_library` whose `deps` reach a `rules_foreign_cc` target. | The ruleset hardcodes `layering_check` — with `module_maps`, `fdo_instrument`, `fdo_optimize` and `thin_lto` — into `FOREIGN_CC_DISABLED_FEATURES` and passes it as `unsupported_features` when configuring the flags handed to the external build. The request cannot reach it, a consuming `cc_library` cannot re-enable it for that edge, and the failure surfaces on the *consumer* as "module X does not depend on a module exporting <header>". The maintainers' only offered workaround opts the edge further out, not in. Open since 2024 with no fix. | `grep -rln 'features = \[.*layering_check' --include='BUILD*' --include='*.bzl' .`, then for each hit check whether any `deps` entry resolves to one of the wrapped-build rules the section grep enumerated. Empty on the first grep = pass; a hit whose deps reach a wrapped build = finding. | MUST |
| BZL-CC-23 | Hand-declare both directions of a wrapped build's I/O: `lib_source` as a filegroup over the whole wrapped tree (`glob(["**"])`, never a curated subset), and every artifact it produces named in `out_static_libs`/`out_shared_libs`/`out_interface_libs`/`out_binaries`/`out_include_dir`/`out_lib_dir`/`out_bin_dir`/`out_data_dirs`/`out_data_files`. | A CMake or Autotools project decides at configure time which files it reads, so a narrower glob drops one silently and the error surfaces deep inside the wrapped tool's own output rather than as a Bazel missing-input message. On the output side Bazel declares an output only for names present in those attributes: a produced file named nowhere never becomes a Bazel output at all, with no error, and the downstream failure points at the consumer instead. | Input side: read each `lib_source` filegroup and confirm its `srcs` glob carries no narrowing pattern — no narrowing found = pass. Output side: run the target once, then diff the wrapped build's own install-directory listing against the declared `out_*` names by hand; no Bazel query reaches inside a foreign build's log, and an empty diff is the pass. | SHOULD |
| BZL-CC-24 | Use `rules_foreign_cc` only to vendor third-party C/C++ you do not control and will not rewrite as native `cc_*`; never as the long-term build strategy for a workspace's own first-party CMake or Autotools project. **pinned** | The ruleset scopes itself to software "not built by Bazel and also not fully under their control". A first-party project under active development pays the hand-declared-I/O tax of BZL-CC-23 and the forced-off `layering_check` cost of BZL-CC-22 indefinitely, for code the team could port to `cc_library`/`cc_binary` instead. Argued from the ruleset's stated intent rather than a hard technical constraint, which is why this row is CONSIDER. | Reading heuristic: for each wrapped-build target the section grep found, does `lib_source` resolve to a path inside the same workspace (first-party) or to an external repository fetched by `http_archive` or a module extension (third-party)? A first-party hit is the finding; an external one is the pass; empty output from the section grep means no wrapped build exists and the row is not applicable. | CONSIDER |

## Platform Legs That Cannot Deliver

Caught by reading the CI matrix and, for every non-Linux leg, the exact command
that leg runs. Both rows describe a leg that runs, exits `0`, and delivers
nothing.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-29 | Before committing to Linux-to-Windows C++ cross-compilation, remote execution included, confirm the toolchain's tool paths resolve under the **execution** platform's path-absoluteness rules rather than the host's, and track bazelbuild/bazel#19208 as open rather than assuming a fix landed. | Path absoluteness is decided by the host's rules, so `C:/foo/bar` reads as *relative* on a Linux host and Bazel prepends the toolchain package path, emitting a silently broken compiler invocation of the shape `.../rbe_windows_.../C:/VS/VC/Tools/MSVC/.../cl.exe`. Open since 2023-08-09 with no fix; the discussion converged and stalled. It shares the autodetected-default root cause with BZL-CC-03. | Run the cross-compile action with `--subcommands` and read the emitted command line for a path where the toolchain package path precedes an absolute Windows path (the `.../C:/...` shape). Empty — no such malformed path — = pass for this bug; a hit = confirmed, with exec-platform-specific `cc_toolchain_config` paths the only documented workaround. The `--platforms`-versus-legacy-flag failure is `BZL-ARCH`'s, and the `MAX_PATH` mitigation is `BZL-HERM`'s. | MUST (know the limitation before committing) |
| BZL-CC-33 | Do not schedule or promise C++ coverage on a Windows leg, and set `GCOV_PREFIX_STRIP` explicitly on any macOS coverage leg before trusting a C++ coverage number. | Bazel's own coverage documentation describes the C++ path for **Linux and macOS only** — there is no Windows path at all — and states that the correct `GCOV_PREFIX_STRIP` value depends on your setup and that with a wrong value "no coverage data will be found". That is a second silent-empty cause on top of the raw-`.profdata` one `BZL-TEST` owns, and both leave the test PASSing and the command exiting `0`. The Windows half rests on documented absence, not on a measured failure — say so when reporting it. | `grep -rn 'GCOV_PREFIX_STRIP' --include=*.yml .bazelrc* .github/workflows` on any macOS coverage leg: empty = the value is unset, so an empty C++ report there is expected rather than mysterious, and a finding for a leg believed to produce coverage. A Windows leg running `bazel coverage` over `cc_*` targets is a finding on its own. Gate the result on `BZL-TEST`'s nonzero-`DA:`-record check, never on the exit code, and pass its lcov flag wherever the toolchain is clang or LLVM. | SHOULD |

## The Local Developer Surface

Caught by grepping `.bazelrc*` and `MODULE.bazel` and then reading the workspace's
own contributor documentation for the command a developer is expected to run.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-CC-28 | Generate `compile_commands.json` from an `aquery`-based extractor wired as a `refresh_compile_commands` target with clangd configured — never from a full build and never from `action_listener`/`extra_action` — and state in the workspace's own onboarding that freshness is a manual re-run. | `aquery` is the only query mode that reports the action graph's real compile commands without building (~30s cold against ~30m for the `action_listener` predecessor), while `query`, `cquery` or a hand-written aspect would mean re-implementing the toolchain's own flag assembly outside Bazel. The generator ships no watch mode and no CI drift check, so an unwarned developer files "autocomplete is wrong" as a mystery bug. The same artifact is the no-aspect IWYU path BZL-CC-11 needs: standalone `iwyu_tool.py` consumes exactly this compilation database. | `grep -rln 'action_listener\|extra_action' --include='BUILD*' --include='*.bzl' .` used for any compile-commands purpose = finding; empty = pass. Then confirm an `aquery`-based generator is present in `MODULE.bazel` — absence = finding — and read the contributor docs for the re-run command, whose absence = finding. Under build-without-the-bytes, `BZL-CACHE` owns the download-regex clause clangd needs. | SHOULD |
| BZL-CC-31 | Treat `--sandbox_base=<tmpfs path>` as a conditional, two-precondition knob and never a free win: confirm the current sandbox location is not already tmpfs before setting it, and never pair it with `--experimental_use_hermetic_linux_sandbox` when the base and the output root sit on different filesystems. | Measured on one Linux host (50 genrules, 3 clean rebuilds per condition, Bazel 8.7.0, `linux-sandbox` confirmed for all 50 actions): default base 26.2 ms/action against `--sandbox_base=/dev/shm` 24.1 ms/action, stdev 8-11 ms — no measurable effect, because that host's scratch was itself tmpfs, so the comparison was tmpfs-versus-tmpfs and not the tmpfs-versus-disk one the help text promises a win for. What holds regardless of host, read from 9.2.0 source: `/dev/shm`'s parent is mode `1777` where a `$HOME`-rooted default is `700`, so on a multi-user machine any local user can read the staged inputs; and the hermetic sandbox stages inputs as hardlinks, which fail `EXDEV` across a filesystem boundary — Bazel catches the exception and silently copies instead, logged only under `--sandbox_debug`. The flag carries `oldName = "experimental_sandbox_base"`, visible in source only and on no CLI reference page. | `grep -rn 'sandbox_base' --include=*.yml .bazelrc* .github/workflows` — empty (the `""` default, identical 8.7.0 through 9.2.0) = pass, nothing to review. For a hit, run `findmnt -T "$(bazel info output_base)"` (Linux only): `tmpfs` there means the flag buys nothing on that host, which is the finding. If `--experimental_use_hermetic_linux_sandbox` is also set, run one build with `--sandbox_debug` and grep that output plus `$(bazel info output_base)/java.log*` for `could not be hardlinked`; empty reads **weakly** there — either no downgrade happened or the line was not surfaced — so treat a base and an output root on different `findmnt` sources as the finding regardless. | CONSIDER |

## Gaps

- Every timing and sandbox measurement here ran on one Linux host; darwin and Windows legs are unmeasured, and Windows never runs a real sandbox at all.
- `layering_check` combined with `--features=cpp_modules` is untested at every version that exists, because no release of `toolchains_llvm` carries named modules. BZL-CC-05 is a guard, not an answer.
- `--sandbox_base` on a disk-backed output root is unmeasured; BZL-CC-31's null result is tmpfs-versus-tmpfs.
- `bazel_iwyu` 0.0.4 is two months old at one BCR version with a single maintainer; re-check its cadence before relying on the aspect rather than the compilation-database path.
- Sanitizer facts here all trace to the legacy `unix_cc_toolchain_config.bzl` path; whether the stock sanitizer features compose identically on a toolchain built with `rules_cc`'s modular API is unread.

## What Agents Get Wrong Here

1. **Reading a `layering_check` line in `.bazelrc` as proof a codebase is strict-deps-clean** — the feature may never reach the target through the wrong toolchain, the wrong OS, a later negative feature, or a config CI never selects (BZL-CC-12).
2. **Repeating the inverted claim that `layering_check` silently stops enforcing outside a sandbox**, which reverses the reported bug and implies a strategy change can restore enforcement (BZL-CC-14).
3. **Cross-attributing a capability between `toolchains_llvm` and `hermetic_cc_toolchain`, copying a `bazel_dep` version out of a README code fence, or citing a README on `main` for a feature no release carries** (BZL-CC-10, BZL-CC-04).
4. **Hallucinating `--experimental_cpp20_modules`**, the spelling the blog posts and the original PR discussion used, in place of the `--experimental_cpp_modules` that shipped (BZL-CC-27).
5. **Reaching for `includes` to make one target's own headers resolve, or setting `hdrs_check` expecting header checking** — both are the attribute whose name matches the intent, and both are wrong (BZL-CC-16, BZL-CC-15).
6. **Hand-writing sanitizer flags as `copts`**, which produces a partial set missing `--strip=never`, the matching `linkopt`, or `-fno-sanitize-recover=all` (BZL-CC-18).
7. **Proposing `features = ["layering_check"]` on a `cc_library` whose deps reach a wrapped foreign build "to improve include hygiene"** — plausible-sounding and structurally impossible (BZL-CC-22).
8. **Citing `prefer_prebuilt_protoc` with no protobuf version**, which is right for exactly one version and stale for every version after it (BZL-CC-32).
