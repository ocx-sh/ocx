---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.bazel-quality-depth-on-demand"
title: Swift under Bazel
summary: The BZL-SWIFT family, if you adopt Bazel for Swift - the scope probe, bzlmod-only rules_swift 4.x, host or hermetic toolchain and CC=clang, worker sandboxing, package-scoped feature lists, module_name, swift_test with Swift Testing, and the rules_swift_package_manager bridge
---

# Swift under Bazel

Owns `BZL-SWIFT`, and binds only if you adopt Bazel for Swift, meaning a `MODULE.bazel` already declares `rules_swift`:
the scope probe, bzlmod-only `rules_swift` 4.x and its load paths, the host versus hermetic Swift toolchain and
`CC=clang`, worker sandboxing, the package-scoped first-party feature list (warnings gate, Swift 6 mode, upcoming
features, `StrictLanguageFeatures`), `module_name`, `main.swift`, `private_deps` and `package_name`, `swift_test` over
Swift Testing and its fixture lookup, `layering_check_swift`, and the `rules_swift_package_manager` bridge. It does not
own `*.swift`, `Package.swift` or `Package.resolved`. The `swift-quality` and `swift-package` rule sets own those files,
and a rule here cites their SW-* IDs where the edit is Bazel-specific. Sibling families, cited never restated: BZL-MOD
owns `MODULE.bazel.lock`, module extensions and the `bazel mod tidy` diff gate (`bzlmod.md`). BZL-HERM owns
`--repo_env` versus `--action_env` and the C++ autodetection switch (`hermeticity.md`). BZL-CACHE owns cache wiring
(`caching.md`). BZL-TEST owns test sizing, timeouts, tags and how coverage is read (`testing.md`). BZL-FLAG owns
rc-file discipline, the `WORKSPACE` removal and ruleset version sourcing (`flags.md`). BZL-ARCH owns visibility and
`select()` (`architecture.md`). BZL-LARK owns BUILD and `.bzl` authoring and the buildifier gate (`starlark.md`).
BZL-CI owns matrix legs and target selection (`ci.md`). BZL-CC-02 owns the claim that a `CC` override is a probe
redirect and not a hermetic toolchain (`cpp.md`). BZL-CORE owns never weakening a check (the index).

Contents: [Scope, Versions and Loading](#scope-versions-and-loading) · [Toolchain and Workers](#toolchain-and-workers) ·
[The First-Party Feature List](#the-first-party-feature-list) · [Targets and Attributes](#targets-and-attributes) ·
[Tests and the SwiftPM Bridge](#tests-and-the-swiftpm-bridge) · [Reading Heuristics](#reading-heuristics) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-10-10 against Bazel 8.8.0 and 9.2.0, rules_swift 4.2.1, rules_swift_package_manager 1.25.0, Swift 6.4.0
(Swift 6.3 for the `swift_test` runner), on a Linux x86_64 host. **A `bazel` binary was run**: each command below was
watched fail on a planted violation and pass on a compliant twin, and a judgement no command can make is a named
reading heuristic. Bazel 7, 9.3.0, 8.8.1 and 10 were not run, and every Apple and Windows statement is `unverified: read
only`. Only 5 of 40 public Swift exemplars carry Bazel files and one builds with it as the main system, so whether to
adopt Bazel stays with the `bazel-adopt` skill, and the default answer for a SwiftPM repository is no. Read a ruleset's
current version from the BCR's `metadata.json` (BZL-ARCH-11, BZL-FLAG-28). Every command reads your own `MODULE.bazel`, BUILD, `.bzl`
and rc text and runs from the repository root, and each row says which way empty output reads. MUST = Block, SHOULD =
Warn, CONSIDER = Suggest. **pinned** marks a default an adopter overrides once, in their own rc file, `MODULE.bazel` or
feature-list `.bzl`, never per target. The name `SWIFT_FIRST_PARTY_FEATURES` and the file `tools/swift_features.bzl`
are pinned defaults the adopter renames.

## Scope, Versions and Loading

The `bazel mod graph` and `L1b` lines need a pipe or a chain, so they sit in the block. Cells cite them by label.

```bash
# 02 graph: empty output means the root does not declare rules_swift 4.x (the finding)
bazel mod graph --depth=1 | grep -e 'rules_swift@4\.'
lsf() { git ls-files -z --cached --others --exclude-standard -- "$@"; }
# 03 L1b: prints one sentence when it fires, empty output is the pass
lsf ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -l -e '@build_bazel_rules_swift' -- | grep -q . && ! lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e 'repo_name *= *"build_bazel_rules_swift"' -- | grep -q . && echo 'WORKSPACE-era repo name without repo_name'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-01 | Apply this file only when a `MODULE.bazel` declares `bazel_dep(name = "rules_swift", ...)`. In any other Swift repository build and test with SwiftPM and add no `MODULE.bazel`, `BUILD.bazel` or `.bazelrc`. | Bazel is secondary for Swift, so an agent that adds Bazel files to a SwiftPM-only repository forks the build graph and `Package.swift` stops being the source of truth. Adopting Bazel is `bazel-adopt`'s call. | see block BZL-SWIFT-01 **A hit means this file binds. Empty output means it does not and nothing below applies.** Watched: a hit on every tree that declares `rules_swift`, empty output on the SwiftPM-only tree. A nested test module also hits, so read the path. A commented-out `bazel_dep` does not hit. | MUST |
| BZL-SWIFT-02 | Configure rules_swift through `MODULE.bazel` only, on Bazel 8.x or 9.x. Declare `bazel_dep(name = "rules_swift", version = "4.2.1")` (the measured pin, any 4.x release) in the root module even when `rules_swift_package_manager` is declared, keep no `WORKSPACE*` file that mentions `rules_swift` (BZL-FLAG-12 owns the removal), and never use `rules_spm`. | rules_swift 4.2.1 is bzlmod-only (`bazel_compatibility = [">=8.0.0"]`) and ships no `repositories.bzl`. A root that declares only `rules_swift_package_manager` silently resolves `rules_swift` 3.6.0, the last Bazel-7 line, with no error. | see block BZL-SWIFT-02 and see block BZL-SWIFT-02-b and see block BZL-SWIFT-02-c **Empty output from each is the pass, a hit is the finding.** Then the 02 graph line. Watched: a `7.7.1` pin, a WORKSPACE `http_archive` and a `rules_spm` hit each caught, the compliant trees empty, the graph hit with the root dependency and empty without it. Bazel 10 is unverified. | MUST |
| BZL-SWIFT-03 | Load each rule from the file that defines it (`@rules_swift//swift:swift_library.bzl`, `swift_binary.bzl`, `swift_test.bzl`). Never load the deprecated aggregate `swift:swift.bzl` or anything under `swift/internal`, and use `@build_bazel_rules_swift` only when the `bazel_dep` sets `repo_name = "build_bazel_rules_swift"`. | The aggregate still loads at exit 0 but is deprecated, and the WORKSPACE-era repo name is invisible under bzlmod (`No repository visible as '@build_bazel_rules_swift'`, exit 1). Training data predates both. | see block BZL-SWIFT-03 and the 03 L1b line. **Empty output from both is the pass.** Never run the first on rules_swift itself, whose own `swift/internal` loads are legitimate. Watched: two lines on the violating tree, L1b fires there and stays empty on the twin that sets `repo_name`. | SHOULD |

```sh
# BZL-SWIFT-01
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/MODULE.bazel' | xargs -0 -r grep -H -n -e '^[^#]*name *= *"rules_swift"' --
# BZL-SWIFT-02
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.bazelversion' | xargs -0 -r grep -H -n -e '^[67]\.' --
# BZL-SWIFT-02-b
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/WORKSPACE' ':(glob)**/WORKSPACE.bazel' ':(glob)**/WORKSPACE.bzlmod' | xargs -0 -r grep -H -n -e 'rules_swift' --
# BZL-SWIFT-02-c
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/MODULE.bazel' ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e 'rules_spm' --
# BZL-SWIFT-03
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e 'swift:swift.bzl' -e 'rules_swift//swift/internal' --
```

## Toolchain and Workers

```bash
lsf() { git ls-files -z --cached --others --exclude-standard -- "$@"; }
# 04 K4: prints one sentence when it fires, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*swift\.toolchain(' -- | grep -q . && ! lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -l -e '^[^#]*repo_env[= ]CC=clang' -- | grep -q . && echo 'host toolchain without repo_env=CC=clang'
# 04 K4b: prints one sentence when it fires, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*swift\.toolchain(' -- | grep -q . && ! lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -l -e '^[^#]*enable_platform_specific_config' -- | grep -q . && echo 'CC line is inert without enable_platform_specific_config'
# 04 K5: hits only on a host-toolchain workspace that declares rules_swift, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*swift\.toolchain(' -- | grep -q . && lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -H -n -e '^[^#]*BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN' --
# 05 H2: a listed file declares the toolchain without one of the two exec registrations
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*swift\.toolchain(' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'swift_toolchain_exec_' --
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*swift\.toolchain(' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'cc_toolchain_exec_' --
# 06 K7: prints one sentence when it fires, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -l -e '^[^#]*--worker_sandboxing' -- | grep -q . && echo 'rules_swift without --worker_sandboxing'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-04 | With a host Swift toolchain on Linux, the checked-in rc carries `common:linux --repo_env=CC=clang` and `build --enable_platform_specific_config`. A `CC=clang` exported by a CI step or a developer shell does not count. While the workspace uses the host toolchain (no `swift.toolchain(`), never set `BAZEL_DO_NOT_DETECT_CPP_TOOLCHAIN`: BZL-HERM-07's zero-`cc_*` trigger does not apply, because Swift targets link through the CC toolchain. A workspace on the hermetic toolchain of rule 05 may set it. | gcc is Bazel's default C++ toolchain, so every `swift_*` target fails analysis with `Swift requires the configured CC toolchain use clang`. A shell export makes CI green and the next clean checkout red. The `common:linux` line is inert without `enable_platform_specific_config` (default false), and the build then fails loudly, so the K4b grep is the fast path. | The 04 K4, K4b and K5 lines. **K4 and K4b empty output is the pass. K5 empty output is the pass and a hit is the finding.** Watched on 8.8.0 and 9.2.0 with `CC` unset: without the rc line `bazel build` exit 1, with it exit 0, and with the off switch added on the host toolchain the same target exit 1 `No matching toolchains found for types`. A hermetic toolchain with the switch builds, tests and links on 8.8.0 and 9.2.0 (measured 2026-10-10). K4 fires with no rc file and passes with the flags in an imported rc. K4 and K4b also fire on a macOS-only workspace and on an rc imported under a name other than `*.bazelrc`: expected noise, not a finding there. | MUST |
| BZL-SWIFT-05 | **pinned**: for CI and release builds use the hermetic toolchain in the root module. Declare `swift.toolchain(name = "swift_toolchain", swift_version_file = "//:.swift-version")`, `use_repo` of `swift_toolchain` and one `swift_toolchain_<platform>` per build platform named in the bundled metadata of that version, and `register_toolchains` of both `@swift_toolchain//:cc_toolchain_exec_<platform>` and `@swift_toolchain//:swift_toolchain_exec_<platform>`. Never register only the `*_embedded_*` pair from `doc/standalone_toolchain.md`, and prefer the version file to an inline `swift_version`. | One compiler on every machine, no `CC`, no `--action_env=PATH`. Embedded-only or no registration fails `target 'linux-toolchain' not declared`, and a version outside the bundled list (4.2.1 carries 6.2.1 to 6.4.0) fails at extension evaluation (`6.4.1`, exit 1). A bare Ubuntu 24.04 needs `libxml2`, `libncurses6` and `libsqlite3-0` first. | see block BZL-SWIFT-05 **A hit is an inline version, allowed only when the CI image tag is the same string (BZL-SWIFT-22).** Then the two 05 H2 lines: **empty output from both is the pass**, a listed file lacks one of the two exec registrations. Watched: embedded-only exit 1, unregistered exit 1, only the Swift exec registered exit 1 (H2 lists the file), the exec pair exit 0 on 8.8.0 and 9.2.0. Local development may stay on rule 04's host form on one pinned image. | SHOULD |
| BZL-SWIFT-06 | The checked-in rc sets `build --worker_sandboxing`. | The default worker strategy is unsandboxed, so a `private_deps` leak and any undeclared input compile green. Sandboxed, the same target fails `no such module 'Lib'`. | The 06 K7 line, and run the build on a clean output base (a CI checkout, or `bazel clean` first): a warm cache replays the green result. **Empty output is the pass.** The pattern `--worker_sandboxing` does not match `--noworker_sandboxing`. Watched: an unsandboxed leak exit 0, sandboxed exit 1, K7 fires with no rc file. A Windows leg is in [Gaps](#gaps). | SHOULD |

```sh
# BZL-SWIFT-05
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/MODULE.bazel' | xargs -0 -r grep -H -n -e 'swift_version = ' --
```

## The First-Party Feature List

Gate and feature promotions are scoped to first-party packages, not the workspace. SwiftPM suppresses dependency
diagnostics and its guards are root-only (SW-PKG-02), and rules_swift does neither. Measured on 8.8.0 and 9.2.0, one
workspace-wide `swift.treat_warnings_as_errors`, `swift.upcoming.InternalImportsByDefault` or
`swift.werror.StrictLanguageFeatures` failed the build on a third-party `rules_swift_package_manager` package (exit 1),
while the package-scoped form built it and still failed the first-party plant. **pinned** default: the shape below. A
workspace with no external Swift code may use the rc form `build:ci --features=...` instead.

```starlark
# tools/swift_features.bzl, loaded by every first-party BUILD.bazel that writes
# package(features = SWIFT_FIRST_PARTY_FEATURES)
SWIFT_FIRST_PARTY_FEATURES = [
    "swift.enable_v6", "swift.treat_warnings_as_errors", "swift.werror.UnknownWarningGroup",
    "swift.werror.StrictLanguageFeatures", "swift.upcoming.ExistentialAny",
    "swift.upcoming.MemberImportVisibility", "swift.upcoming.InternalImportsByDefault",
]
```

```bash
lsf() { git ls-files -z --cached --others --exclude-standard -- "$@"; }
# 07 W1: prints one sentence when no Swift gate exists, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -l -e '^[^#]*swift\.treat_warnings_as_errors' -e '^[^#]*"-warnings-as-errors"' -- | grep -q . && echo 'rules_swift without a Swift warnings gate'
# 07 rc: a hit is a workspace-wide gate or promotion beside rules_swift_package_manager, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*rules_swift_package_manager' -- | grep -q . && lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -H -n -E -e '^[^#]*--features[= ]swift\.(treat_warnings_as_errors|upcoming\.|werror\.)' --
# 08 E6: prints one sentence when swift.enable_v6 is absent, empty output is the pass
lsf ':(glob)**/MODULE.bazel' | xargs -0 -r grep -l -e '^[^#]*name *= *"rules_swift"' -- | grep -q . && ! lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -l -e '^[^#]*swift\.enable_v6' -- | grep -q . && echo 'rules_swift without swift.enable_v6'
# 08 aquery: each printed line is a mode-5 compile target, empty output is the pass
bazel aquery "mnemonic('SwiftCompile', //...)" --output=text | awk '/^  Target:/ { t = $2 } /^    -swift-version/ { getline; if ($1 == "5") print t }'
# 10 W4: a listed file sets upcoming or experimental features without the guard, empty output is the pass; it matches rules_swift's own sources, never run it there
lsf ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -l -e '^[^#]*swift\.upcoming\.' -e '^[^#]*swift\.experimental\.' -e '^[^#]*enable-upcoming-feature' -e '^[^#]*enable-experimental-feature' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'swift\.werror\.StrictLanguageFeatures' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-07 | The Swift warnings gate is the feature `swift.treat_warnings_as_errors` (or `-warnings-as-errors` in one shared `copts` list) applied to first-party packages through the feature list. Never the unprefixed `treat_warnings_as_errors`, and never a workspace-wide `--features=` where the workspace compiles external Swift. Maps SW-GATE-13. | The unprefixed name is the C++ feature and leaves a planted Swift warning at exit 0. Bazel does not suppress dependency diagnostics, so a workspace-wide gate turned a third-party warning into a build error. | The 07 W1 and 07 rc lines and see block BZL-SWIFT-07 **Empty output from all three is the pass.** A hit on 07 rc means the gate or a promotion is workspace-wide beside external Swift: move the list to `package(features = ...)`, and the rc form is allowed only with no external Swift. A hit on the third is the C++ spelling, legitimate only beside a C++ target and with a Swift gate present. Canary: a planted unused `var` in a first-party target exits non-zero, and `bazel build //...` exits 0 with a third-party warning present. Watched: plain exit 0, unprefixed exit 0 (the false green), prefixed exit 1, clean twin exit 0, package-scoped plant exit 1 with the dependency exit 0. | MUST |
| BZL-SWIFT-08 | Every first-party Swift target is in Swift 6 mode: `swift.enable_v6` is in the feature list (or `features = ["swift.enable_v6"]` per target). A target kept in mode 5 carries a dated comment and a removal condition (SW-PKG-15). Maps SW-PKG-13, SW-PKG-14. | Without the feature rules_swift passes `-swift-version 5`, so Bazel's default is mode 5, and an invented name such as `swift.enable_swift6` builds green in mode 5. | The 08 E6 and aquery lines. **Empty output from both is the pass, and each aquery line names a mode-5 target.** The aquery covers first-party targets only: a global `--features=swift.enable_v6` did not change a `rules_swift_package_manager` package, which carries `-swift-version 5` in its generated `copts` (tools 5.9 manifest, a tools-6 manifest was not measured). Watched: 0 lines on the real feature, 1 on the typo target, 15 of 16 compiles on `//...`, 0 with the flag. Floor Swift 6.0. | MUST |
| BZL-SWIFT-09 | First-party targets enable `ExistentialAny`, `MemberImportVisibility` and `InternalImportsByDefault` as `swift.upcoming.<Name>` features in the feature list, or as two-element `copts` (`"-enable-upcoming-feature", "ExistentialAny"`). Never one joined string, and a dependency used in public API is then `public import`. Maps SW-PKG-07. | A joined string is one argv element and fails with `unknown argument`. The feature form is the one rule 10's guard can audit. | see block BZL-SWIFT-09 **Empty output is the pass.** Then `bazel build //...` under the feature list. Watched on Swift 6.4: each of the first two off exit 0, on exit 1 with `[#MemberImportVisibility]` or `function cannot be declared public because its result uses an internal type`, the explicit-import and `public import` twins exit 0. Floor Swift 6.2 (SW-PKG-07), measured on 6.4 only. | MUST |
| BZL-SWIFT-10 | A package that sets any `swift.upcoming.*` or `swift.experimental.*` lists `swift.werror.StrictLanguageFeatures` in the same package-scoped list, and `swift.werror.UnknownWarningGroup` whenever it promotes a `swift.werror.<Group>`. Group names start upper case, and a group absent from the oldest toolchain leg is not promoted. Maps SW-GATE-15, SW-GATE-16, SW-PKG-02. | A misspelt upcoming name exits 0 even with a warnings gate, and a lowercase group is dropped silently. `NoUsage` exists in 6.4 and not in 6.3. Workspace-wide, a third-party manifest's unknown feature failed the build (`'NoSuchFeatureX' is not a recognized upcoming feature`). | see block BZL-SWIFT-10 and the 10 W4 line. **Empty output from both is the pass**, and a file listed by W4 is the finding (one `.bzl` feature list holds the features and the guard together, so the check is per file, not per workspace). Watched: typo `ExistentialAnyy` with the guard exit 1 `[#StrictLanguageFeatures::UnrecognizedStrictLanguageFeatures]`, the right name exit 0, the same typo without the guard exit 0. Floor Swift 6.1 (SE-0443). | SHOULD |
| BZL-SWIFT-11 | New first-party code also enables `swift.upcoming.NonisolatedNonsendingByDefault` and `swift.upcoming.InferIsolatedConformances`, and no target lists `swift.upcoming.ApproachableConcurrency`. Maps SW-PKG-08, SW-PKG-16. | The bundle name compiles on 6.3 and 6.4, so only a rule stops it. With the two members off, a `nonisolated async` call with a MainActor value fails `[#RegionIsolation::SendingRisksDataRace]`, and with the first on it passes. | see block BZL-SWIFT-11 **Empty output is the pass.** Watched on 6.4 and 6.3: the first member off exit 1, on exit 0. `InferIsolatedConformances` compiled and had no behavioural flip planted. Floor Swift 6.2 (SE-0461). | SHOULD |
| BZL-SWIFT-12 | `copts = ["-default-isolation", "MainActor"]` appears only on app targets, never on a library, SDK, CLI or server target. SW-CONC-11 owns the prohibition. | It changes the isolation of every declaration, so a mutable global that fails Swift 6 mode passes under it. | see block BZL-SWIFT-12 **Empty output is the pass. Every hit must sit on an app target, a reading heuristic for the target kind.** Watched: off exit 1, on exit 0. Floor Swift 6.2 (SE-0466). | SHOULD |
| BZL-SWIFT-13 | Do not enable `swift.debug_module_path`, nor the `swift.use_c_modules` plus `swift.use_explicit_swift_module_map` pair, on a Linux workspace. Do not remove `-modulewrap` by hand, since the worker special-cases it. | The Xcode toolchain enables `swift.debug_module_path` by default from Xcode 27.0 (`xcode_swift_toolchain.bzl`) and the Linux toolchains never add it, so it is opt-in there. On Linux 6.4 `-c dbg` with the three features fails `unable to load standard library for target 'x86_64-unknown-linux-gnu'` (exit 1) while the plain target builds. The Apple behaviour is `unverified: read only`. | see block BZL-SWIFT-13 **Empty output is the pass.** | SHOULD |

```sh
# BZL-SWIFT-07
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e '--features[= ]treat_warnings_as_errors' -e '"treat_warnings_as_errors' --
# BZL-SWIFT-09
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e '"-enable-upcoming-feature [A-Za-z]' -e '"-enable-experimental-feature [A-Za-z]' --
# BZL-SWIFT-10
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -H -n -e 'swift\.werror\.[a-z]' --
# BZL-SWIFT-11
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -H -n -e 'swift.upcoming.ApproachableConcurrency' --
# BZL-SWIFT-12
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e '"MainActor"' --
# BZL-SWIFT-13
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/BUILD' ':(glob)**/*.bzl' ':(glob)**/.bazelrc' ':(glob)**/*.bazelrc' | xargs -0 -r grep -H -n -e 'swift.debug_module_path' --
```

## Targets and Attributes

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-14 | Every `swift_library` sets `module_name` to the `UpperCamelCase` name its dependents `import`. | The derived name replaces non-identifier characters (`//:my-lib` is `my_lib`, `//pkg/sub:thing` is `pkg_sub_thing`), so the dependent fails `no such module 'thing'` in a different target than the cause. | `bazel query "attr(module_name, '^$', kind(swift_library, //...))"` **Each listed target lacks the attribute, empty output is the pass.** Watched: it listed `//:unset-name` and nothing on an explicit target. `import MyLib` against `//:my-lib` exit 1, with an explicit `module_name` exit 0. | MUST |
| BZL-SWIFT-15 | Before relying on a rules_swift attribute default, feature or doc example, read the generated behaviour (`bazel query --output=xml --xml:default_values //:target`, `aquery` on `SwiftCompile`, or the 4.2.1 source). Where prose and behaviour disagree, behaviour wins. Leave `alwayslink` at its default `True`, and an `alwayslink = False` carries a comment naming the size measurement and the conformance-registration check. | Three drifts measured in 4.2.1: the hermetic quickstart registers the embedded pair a host build cannot use, `swift_test` is documented as XCTest-only and runs Swift Testing, and the doc platform list (`fedora39`, `amazonlinux2`) is not the 6.4.0 metadata. | `bazel query "attr(alwayslink, 0, kind(swift_library, //...))"` **Each listed target sets `alwayslink = False`, empty output is the pass.** Watched: it listed the planted target. The runtime loss from `alwayslink = False` is a reading heuristic from the rule docs, not run. | SHOULD |
| BZL-SWIFT-17 | Implementation-only imports go in `private_deps`. Targets that share `package`-level API share one `package_name`. Build under BZL-SWIFT-06 so a missed edge is a red build. A top-level-code file must be named exactly `main.swift` or the single source must use `@main` (`Main.swift` fails `expressions are not allowed at the top level`). | Unsandboxed workers let a missing dependency pass. A different or absent `package_name` makes `package` API invisible (`cannot find 'pkgFn' in scope`). | `bazel build //...` under the rc with `--worker_sandboxing`, on a clean output base (a CI checkout, or `bazel clean` first; a warm cache replays the unsandboxed green result), exits 0. Watched: `package_name` same exit 0, different exit 1, absent exit 1, a private leak exit 0 unsandboxed and exit 1 sandboxed. | SHOULD |
| BZL-SWIFT-20 | Enable `swift.layering_check_swift` (and `swift.layering_check_unused_deps` once the graph is clean) for first-party packages. | A Bazel-only gain with no SwiftPM twin: importing a module without a direct `deps` edge, or declaring a `deps` edge that is never imported, is a build error. | see block BZL-SWIFT-20 **Empty output means the feature is off, the finding.** Then `bazel build //...` exits 0 under the feature. Watched: off exit 0, on exit 1 `Layering violation in @@//:layer_viol`, an unused dependency exit 1. Workspace-wide flags built a two-module `rules_swift_package_manager` package at exit 0. | CONSIDER |

```sh
# BZL-SWIFT-20
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/BUILD.bazel' ':(glob)**/*.bzl' | xargs -0 -r grep -H -n -e 'layering_check_swift' --
```

## Tests and the SwiftPM Bridge

```bash
# 21: a non-zero exit from either line is the finding, exit 0 is the pass
swift package resolve && git diff --exit-code -- Package.resolved
bazel mod tidy && git diff --exit-code -- MODULE.bazel
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-18 | New tests are `swift_test` targets whose sources are Swift Testing files, with `discover_tests` left at `True`. `discover_tests = False` appears only on a target that owns `main` and exits non-zero on failure. Write no XCTest for new code because the rule docs mention only XCTest. Maps SW-TEST-01. | The generated runner runs XCTest and then Swift Testing. A failing `#expect` exits 3, a target with no tests exits 3 (`No tests were discovered.`), and `discover_tests = False` without a `main` fails to link. Sizing, timeouts and coverage reading stay with BZL-TEST. | `bazel test //...` exits 0, and `bazel query "attr(discover_tests, 0, kind(swift_test, //...))"` **lists targets that set `discover_tests = False`: each must justify its own `main`, empty output is the pass.** The SW-TEST-01 diff grep over new test files is unchanged. Watched on Swift 6.4 and 6.3: pass 0, fail 3, empty 3, no-main link 1, mixed XCTest plus Swift Testing 0. | MUST |
| BZL-SWIFT-19 | A `swift_test` that reads recorded fixtures lists them in `data` and finds them through one helper: `$TEST_SRCDIR/$TEST_WORKSPACE/<path>` under `#if BAZEL_BUILD` (set with `local_defines = ["BAZEL_BUILD"]` on the target, the bare name) and `Bundle.module` otherwise. A test that uses `Bundle.module` alone builds only under SwiftPM. Amends SW-TEST-21 for Bazel. | rules_swift does not synthesize `Bundle.module` (`type 'Bundle' has no member 'module'`). A cwd-relative path works only with the files in `data` (exit 3 `NSCocoaErrorDomain Code=260` without), and `#filePath` passes only because it is workspace-relative and the test cwd is the runfiles root. `local_defines = ["FLAG=1"]` is accepted here and defines nothing named `FLAG` under `swiftc -DFLAG=1`. | `bazel test //...` exits 0, and see block BZL-SWIFT-19 **Empty output is the pass. Each hit must sit in the `#else` arm of `#if BAZEL_BUILD`, a reading heuristic for the conditional, and a hit under `Sources` is SwiftPM-only resource code.** Watched on 8.8.0 and 9.2.0: `Bundle.module` alone exit 1, the helper with the define exit 0, without the define exit 1 (the define is not predefined), the `TEST_SRCDIR` lookup exit 0. A spawned fake CLI in `data` was not run. | SHOULD |
| BZL-SWIFT-21 | **MUST when `rules_swift_package_manager` is used.** The SwiftPM bridge is `swift_deps.from_package(resolved = "//:Package.resolved", swift = "//:Package.swift")` with the `use_repo` list written by `bazel mod tidy`, and CI fails when `swift package resolve` or `bazel mod tidy` changes a tracked file (BZL-MOD-10 owns the diff-gate shape). Third-party packages arrive with their own manifest's flags and are outside rules 07 to 10. | A stale `Package.resolved` builds green and silently omits a newly added dependency. Without the `use_repo` names the build fails `No repository visible as '@swiftpkg_nio'`. | The 21 lines. **A non-zero exit from either is the finding.** Watched on 8.8.0 and 9.2.0: an extra manifest dependency left `bazel build` at exit 0 (the false green) and the resolve diff at exit 1, an unchanged manifest diff exit 0, a missing `use_repo` exit 1 and exit 0 after `bazel mod tidy`. Floor rules_swift_package_manager 1.25.0 on rules_swift 4.2.1. | MUST when used, N/A otherwise |

```sh
# BZL-SWIFT-19
git ls-files -z --cached --others --exclude-standard -- ':(glob)**/*.swift' ':(exclude,glob)**/.build/**' | xargs -0 -r grep -H -n -e 'Bundle\.module' --
```

## Reading Heuristics

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-SWIFT-22 | `.swift-version`, the registered toolchain version and the CI image tag are one string. SW-GATE-08 pins the formatter and compiler the same way. | Diagnostics differ per compiler (`NoUsage` is a group in 6.4 and unknown in 6.3), so a host that drifts from CI changes what is red. | Reading heuristic, not run. Print the file with `cat .swift-version` and compare it with the image tag in the workflow or Dockerfile. **A mismatch is the finding.** No single grep exists because the tag lives in files this set never globs. | SHOULD |
| BZL-SWIFT-23 | Apple-platform targets build through `rules_apple` (`ios_*` and siblings), not by building a `swift_library` directly for iOS. This file asserts nothing about Windows legs. | A `swift_library` built directly for iOS imports host frameworks and fails `no such module 'UIKit'` (rules_swift FAQ). | Reading heuristic, `unverified: read only` (no macOS or Windows host). **A CI leg that builds a `swift_library` with an Apple `--platforms` value and no `rules_apple` target is the finding.** | SHOULD |

## Gaps

- **Static and cross builds are not covered.** Whether `swift_binary` can produce the static musl Linux binary that
  SwiftPM's static Linux SDK recipe ships, and what `swift.static_stdlib` does under `swift_test`, were not run. Release
  stays with SwiftPM (`swift-package`).
- Macros and compiler plugins (`swift_compiler_plugin` with swift-syntax through `rules_swift_package_manager`) and the
  attributes `swiftc_inputs`, `$(location)` in `copts`, `swift_import`, `swift_interop_hint`, `mixed_language_library`
  and `swift_proto_library` were not run, so none of the rules above is claimed for them.
- Test runtime is measured for fixture lookup only. A spawned fake CLI in `data` found through runfiles,
  `--test_filter` for Swift Testing ids, sharding, coverage and sanitizers were not run.
- `rules_swift_package_manager` was built against a tools-5.9 path dependency and swift-log only. Whether rules 07 to
  10 still isolate packages with tools-6 manifests (swift-nio, swift-subprocess, swift-argument-parser) is unmeasured.
- A Windows leg that needs `--noworker_sandboxing` is `unverified: read only`, and adding that flag is BZL-CORE-01's
  weakening list, so the diff states why.
- Bazel 10, 9.3.0 and 8.8.1 are `unverified`, not forbidden. `rules_swift_package_manager` is a separate project
  outside the ruleset, and it resolves against `rules_swift` 3.6.0 when the root omits the dependency (BZL-SWIFT-02).

## What Agents Get Wrong Here

1. **Writing a WORKSPACE** with `http_archive(name = "build_bazel_rules_swift", ...)`, `swift_rules_dependencies()` or a
   load of `@build_bazel_rules_swift//swift:swift.bzl`, because training data predates bzlmod (BZL-SWIFT-02, BZL-SWIFT-03).
2. **Forgetting `CC=clang` on Linux, or exporting it in a CI step only**, or applying BZL-HERM-07's C++ off switch to a
   Swift-only workspace on the host toolchain because it has zero `cc_*` targets (BZL-SWIFT-04).
3. **Promoting warnings or features workspace-wide**, which breaks any `rules_swift_package_manager` package that emits a warning or carries an unrecognised feature in its manifest, or
   writing the unprefixed C++ feature and getting a false green (BZL-SWIFT-07, BZL-SWIFT-09, BZL-SWIFT-10).
4. **Inventing feature names** (`swift.enable_swift6`, `swift.strict_concurrency`, `swift.warnings_as_errors`). They are
   ignored silently and the target stays in mode 5 (BZL-SWIFT-08).
5. **Copying the doc quickstart**: the `*_embedded_*` toolchains only, or a `swift_version` outside the bundled list
   (BZL-SWIFT-05, BZL-SWIFT-15).
6. **Omitting `module_name`** and importing the target name (BZL-SWIFT-14).
7. **Using `Bundle.module` or `resources:` in a `swift_test`**, or setting `discover_tests = False` to silence "no tests
   discovered" (BZL-SWIFT-18, BZL-SWIFT-19), or writing XCTest because the rule docs describe XCTest only (BZL-SWIFT-18).
8. **Editing `Package.swift` and running only `bazel build`**. The stale `Package.resolved` builds green (BZL-SWIFT-21).
9. **Adding Bazel files to a SwiftPM-only repository** to be helpful (BZL-SWIFT-01).
10. **Joining flags in `copts`** (`"-enable-upcoming-feature ExistentialAny"`) (BZL-SWIFT-09).
11. **Treating a green `bazel build //...` as proof tests ran.** `swift_test` runs only under `bazel test` (BZL-TEST).
12. **Enabling `swift.debug_module_path` "to speed up debugging"** on Linux (BZL-SWIFT-13), or building a `swift_library`
    directly for iOS (BZL-SWIFT-23).
