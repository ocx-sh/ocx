---
title: Java and Kotlin under Bazel
summary: The BZL-JAVA family: the four JDK version flags, strict deps and Error Prone wiring, maven_install pinning and repin, the rules_kotlin traps, deploy jars, JUnit 5 and coverage
---

# Java and Kotlin under Bazel

Owns `BZL-JAVA`: the four JDK version flags and the bytecode-below-toolchain
split, strict-deps semantics and Error Prone wiring under javac,
`default_java_toolchain` definition and registration order, `rules_jvm_external`'s
`maven.install` and `maven_install.json` pinning, repin and merge discipline,
`rules_kotlin`'s `java_import` trap and its `x_lambdas` divergence,
`java_binary`'s `_deploy.jar` implicit output, JUnit 5 through
`contrib_rules_jvm`, and the Java half of coverage. It does not own `pom.xml`,
`build.gradle.kts`, a version catalog, `*.java` or `*.kt` hygiene. The
`java-quality`, `kotlin-quality`, `gradle-build` and `maven-build` rule sets own
those files, and a rule here touches one only where the edit is Bazel-specific.
Sibling families, cited never restated: BZL-MOD owns `MODULE.bazel.lock`, the
`--lockfile_mode` values and module extensions (`bzlmod.md`). BZL-HERM owns the
action environment and `--repo_env` versus `--action_env` (`hermeticity.md`).
BZL-CACHE owns credential helpers, download-mode and eviction flags
(`caching.md`). BZL-TEST owns test sizing, timeouts, `manual`-tag semantics and
how a coverage report is read once it exists (`testing.md`). BZL-FLAG owns
`--incompatible_autoload_externally`, rc-file discipline and ruleset Bazel
floors (`flags.md`). BZL-ARCH owns visibility, `select()` and generator wiring
(`architecture.md`). BZL-LARK owns `.bzl` and BUILD authoring and the buildifier
gate (`starlark.md`). BZL-CI owns matrix legs and target selection (`ci.md`).
`rust.md` carries the crate_universe lock-file rows this family's pinning rows
invert.

Contents: [What the Root rc File Pins](#what-the-root-rc-file-pins) ·
[Strict Deps and Error Prone](#strict-deps-and-error-prone) ·
[Toolchain Definition and Registration](#toolchain-definition-and-registration) ·
[maven.install and Its Lock File](#maveninstall-and-its-lock-file) ·
[Kotlin under Bazel](#kotlin-under-bazel) ·
[What Ships, and Which Build Is Canonical](#what-ships-and-which-build-is-canonical) ·
[Running Tests and Reading Coverage](#running-tests-and-reading-coverage) ·
[Gaps](#gaps) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Measured 2026-09-12 against Bazel 8.7.0, 8.8.0 and 9.2.0, `rules_jvm_external`
7.1 (2026-07-23), `rules_kotlin` v2.4.10 (2026-08-20) and `contrib_rules_jvm`
0.31.0 (pre-1.0, no tagged releases). **No `bazel` binary was run**: every flag
name below carries the documentation page or the public exemplar it was read
from, and two of those pages contradict each other (see
[Gaps](#gaps)). Before writing any Bazel flag anywhere, confirm it on **both**
help surfaces at your pinned version, `bazel help <command> --long` **and**
`bazel help startup_options`, because a flag lives in either and a reader who
checks one surface misses the other. Empty output from `bazel help` for a flag
name means the flag does not exist at that version, which is an answer and not a
pass. Every grep here reads your own `.bazelrc`, `MODULE.bazel`, `BUILD.bazel`
and `.bzl` text. The `java_import` targets `rules_jvm_external` renders into
`@maven` are out of a grep's reach by construction, so where a rule's subject is
generated, its verification reads the configuration that produced it. MUST =
Block, SHOULD = Warn, CONSIDER = Suggest. **pinned** marks a default an adopter
overrides once, in their own rc file or `MODULE.bazel`, never per target.

## What the Root rc File Pins

Bazel compiles with one JDK and JVM pair and executes tools with a second,
independently configured pair. One read of the root rc file covers this whole
block, on Bazel 8.x and 9.x alike:

```bash
grep -n -e '--java_language_version' -e '--java_runtime_version' \
  -e '--tool_java_language_version' -e '--tool_java_runtime_version' \
  -e '--javacopt' .bazelrc
```

Empty output is not "nothing to check". It is BZL-JAVA-01's finding, and the
most common one in the family.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-01 | **pinned**: Pin all four version flags in the root rc file: `--java_language_version`, `--java_runtime_version`, `--tool_java_language_version` and `--tool_java_runtime_version`. Never write `--java_version` or `--tool_java_version`, which do not exist. | Unset, the four take `11`, `local_jdk`, `11` and `remotejdk_11`, so an unpinned repository is one Bazel upgrade away from a silent floor change that appears in no diff. A made-up combined flag name does not error, it no-ops, so the build reads as configured and is not. The four values are the adopter's decision, the requirement that all four are written is not. | The block grep. **All four present is the pass, any absent is the finding.** Then `grep -n -e '--java_version' -e '--tool_java_version' .bazelrc`. **Empty output is the pass here, and any hit is the harder finding**, because that line does nothing at all. Identical on 8.7.0 and 9.2.0. | MUST |
| BZL-JAVA-02 | Give both runtime flags a `remotejdk_*` value. Treat `local_jdk`, written or defaulted, as a finding unless an adjacent comment says why local compilation is deliberate. | `local_jdk` resolves through `JAVA_HOME` and `PATH`, so the resulting binaries depend on what is installed on the machine. Two developers on two JDK vendors get two different test runs out of one identical BUILD graph, and nothing in the build output says so. | The block grep, reading the **values** rather than the keys. `local_jdk` or an absent runtime flag is the finding. The documented opt-in escape hatch is `--extra_toolchains=@local_jdk//:all`, which must appear together with the comment naming why. | MUST |
| BZL-JAVA-03 | Express a bytecode target below the toolchain as `--javacopt="-source N -target N"` on top of an unchanged, modern `--java_language_version`. Never by lowering `--java_language_version` itself. | `--javacopt` is applied after Bazel's built-in javac defaults, and the last specification of any javac option wins, so it changes only the emitted class-file version. Lowering the language flag also re-selects *which* `java_toolchain` resolves, through `source_version` matching, silently dropping any `package_configuration` wired to the higher toolchain. | `grep -n -B2 -A2 -e 'javacopt.*-source' -e 'javacopt.*-target' .bazelrc`. **A downward target present as a `javacopt` beside a high `--java_language_version` is the pass. A low `--java_language_version` beside a high `--tool_java_language_version` is the finding.** Public exemplar: dagger's rc file pins the toolchain and tool pair high and drops only the emitted bytecode. | MUST |

```bash
build --java_language_version=8   # wrong: also re-selects the toolchain
build --java_language_version=17 --javacopt="-source 8 -target 8"  # right
```

## Strict Deps and Error Prone

Both rows below are about reading a value rather than a presence. Two reads
cover them:

```bash
grep -n -e strict_java_deps -e '-Xep' -e java_package_configuration .bazelrc
grep -rn -e strict_java_deps -e explicit_java_test_deps -e java_package_configuration \
  --include='*.bzl' --include='BUILD.bazel' --include='BUILD' .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-04 | Never conclude "strict deps is not enforced here" from an absent flag, and never add `=off` or `=warn` to make a build pass. `default`, `strict` and `error` are three names for one behaviour. | Bazel's user manual states that all three mean javac generates errors instead of warnings, and that this is also the behaviour when the flag is unspecified. An agent pattern-matching "no flag means feature disabled" inverts a review finding, then repairs the real missing-`deps` bug it found by disabling the check that found it. | The block greps, read for the **value**. **Absence, or a value in `default`, `strict` or `error`, is the pass. `off` or `warn` is the finding to justify in the diff.** Five of the six public Bazel-carrying exemplars measured leave the flag unset and are all enforcing, so absence is the normal shape of a correct repository. | MUST |
| BZL-JAVA-05 | Write the flag as `--experimental_strict_java_deps`, the only spelling with exemplar evidence, and confirm it against the pinned binary before relying on it. | Bazel's own two pages disagree. The command-line reference lists `--experimental_strict_java_deps`, the user manual calls it `--strict_java_deps`. A model trained on one page and asked after the other produces the wrong name confidently, and a misspelled flag is rejected at the command line rather than silently. | `bazel help build --long` at your pin must list the flag, and `bazel help startup_options` is the second surface to read before declaring it absent. **Empty output on both surfaces is the finding, not a pass.** Where no binary is reachable, write the exemplar-evidenced spelling and leave this row a SHOULD. Verified against documentation only, 2026-09-12. | SHOULD |
| BZL-JAVA-06 | Wire Error Prone severity through `--javacopt="-Xep:CheckName:SEVERITY"` build-wide, or through `java_package_configuration` plus a `package_group` for a scoped override. Never through a `bazel_dep`, a `maven.install` coordinate on `error_prone_core`, or a patched `java_toolchain`. | Error Prone is baked into JavaBuilder's compilation toolchain, not pulled in through `deps`. The upstream Error Prone repository ships no `MODULE.bazel`, no `WORKSPACE` and no BUILD file at all, so there is no Bazel integration surface to depend on and an agent sent to copy its wiring finds nothing and invents a coordinate instead. Which checks to promote is settled by the `java-quality` lint gate. This row owns only the plumbing. | `grep -rn -e error_prone_core -e error-prone-core MODULE.bazel maven_install.json`. **Empty output is the pass, and any hit that is not a transitive `error_prone_annotations` is the finding.** Then confirm a `-Xep:` override actually reaches `--javacopt`, `--host_javacopt` or a `java_package_configuration`'s `javacopts`. **None of the three is the finding**, because the severity was then never applied. | MUST |

## Toolchain Definition and Registration

No grep settles this block. The check is reading every
`default_java_toolchain(...)` and `register_toolchains(...)` in **evaluation
order**, root module first, then each `bazel_dep` in `MODULE.bazel` declaration
order, because a dependency's module extension can register before the root
module's own call.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-07 | Never set `forcibly_disable_header_compilation = True` without a recorded reason, and never describe Bazel's header compilation as "ijar-based". The default toolchain's header compiler is **turbine**. | `ijar` strips an already-compiled jar down to its API surface. `turbine` parses Java **source** and emits a header jar without invoking javac's back end at all, which is why a downstream target's header compile does not wait on its dependencies' bodies. Disabling it reverts every dependent to full recompilation of every upstream body on every rebuild. | `grep -rn forcibly_disable_header_compilation --include='*.bzl' .`. **Empty output is the pass, a hit with no comment naming why is the finding.** Then, at your pin, `bazel aquery 'mnemonic("Turbine", //your/package:target)'` should list one Turbine action per Java compilation unit. **Empty output there, on a toolchain that has not disabled header compilation, is itself a finding to escalate and never a pass.** | SHOULD |
| BZL-JAVA-08 | When more than one `local_java_repository` or `remote_java_repository` matches the same operating-system and CPU pair, register the intended default **first**, and say so in a comment beside the registration. Never rely on "more specific wins". | Bazel's Java documentation states it flatly: when multiple definitions for the same operating system and CPU architecture are given, the first one is used. Nothing in the Starlark syntax signals that a registration loses to an earlier one, and specificity, which decides platform constraints elsewhere in Bazel, decides nothing here. | Read every `register_toolchains(...)` targeting `@rules_java//java:runtime_toolchain_type` or `:toolchain_type` in evaluation order. **Two registrations matching the same operating-system and CPU pair with no ordering comment is the finding. One registration, or an explicit comment, is the pass.** | SHOULD |
| BZL-JAVA-09 | Give a first-party annotation processor or javac plugin that reads `com.sun.tools.javac` internals its own `--jvmopt="--add-exports=jdk.compiler/the.package=ALL-UNNAMED"`. Never try to add to or change JavaBuilder's `--patch_module` invocation. | `--patch_module` is computed and applied by JavaBuilder itself, from the JDK major version, for *its own* internal access: strict deps, header compilation, Error Prone. No attribute appends to it. `--add-exports` is what JDK 16 and later strong encapsulation actually exposes. | `grep -rn 'com.sun.tools.javac' --include='*.java' .` for the processor's own sources, then `grep -n -e '--add-exports' -e jvmopt .bazelrc` for a matching line. **A processor reading those internals with no matching line is the finding**, because it either already fails on a strongly-encapsulated JDK or is quietly riding an older one. Empty output from the first grep is nothing to check. Floor JDK 16. | MUST where such a processor exists, N/A otherwise |

## maven.install and Its Lock File

The whole block reads `MODULE.bazel` plus the committed lock file, and binds
`rules_jvm_external` 5.x and later, read at 7.1 (2026-07-23), verified
2026-09-12:

```bash
grep -n -A8 'maven.install(' MODULE.bazel
git ls-files '*_install.json'
```

**Empty output from the first means this module declares no `maven.install` and
the whole block is N/A.** The inverse of `rust.md`'s crate_universe gate applies
here: there the thing that must be *absent* is an environment variable, here the
gate is opt-in and the thing that must be *present* is a boolean. Prefer the
literal `@maven//:group_artifact` label over the `artifact("group:artifact")`
macro in any BUILD file a `buildozer`-driven refactor or bump job will touch,
because the macro hides the target label at the syntax level. That is a tooling
cost rather than a correctness bug and carries no rule.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-10 | Set `lock_file = "//:maven_install.json"`, or another explicit path, on every `maven.install` call, and commit the file. Each `install` tag needs its own `name` and its own lock file. | Without `lock_file` there is no freshness check to gate at all. Every clean checkout re-resolves online with no checksum verification, no integration with Bazel's downloader, no cross-workspace artifact sharing and no offline build after a fetch. Sharing one lock file across two `install` tags is invalid, not merely discouraged. | The block grep. **A `maven.install(` block carrying no `lock_file =` is the finding. No `maven.install` at all is nothing to check.** First pin is `touch maven_install.json BUILD.bazel` followed by `REPIN=1 bazel run @maven//:pin`. | MUST |
| BZL-JAVA-11 | **pinned**: Set `fail_if_repin_required = True` on every `maven.install` that carries a `lock_file`. | The default is `False`, so **both** of the lock file's self-checks are advisory. An input-artifacts hash mismatch, meaning the artifact list was edited without a repin, and a resolved-artifacts hash mismatch, meaning the file was hand-edited or merge-desynced, each print a warning and the build **continues** against whatever the stale lock says. Only this boolean turns either into a hard failure. The ruleset that ships the flag does not set it on its own primary resolution, so its own `MODULE.bazel` is not the thing to copy. | `grep -n fail_if_repin_required MODULE.bazel`. **Absent, or `False`, beside a `lock_file` is the finding, `True` is the pass.** Self-verifying once set: stale the artifact list on a scratch copy and confirm the documented failure reproduces before trusting the gate. | MUST |
| BZL-JAVA-12 | Never hand-edit `maven_install.json`, and never resolve a merge conflict in it by keeping one side, unioning or splicing JSON. Discard both sides, resolve the conflicting `artifacts` or `boms` list in `MODULE.bazel`, and regenerate with `REPIN=1 bazel run @maven//:pin`. | The file declares itself autogenerated and carries two generator-computed hashes over its own content, with no human-editable safe region. A line-based merge produces syntactically valid JSON whose hashes no longer match its body, and Git's textual conflict detector never fires. No schema-aware merge driver ships for this format at all. | `git check-attr merge maven_install.json`. **Any driver claiming to merge this file safely is the finding**, and pointing Bazel's own `bazel-lockfile-merge` at it is itself a finding, because that driver parses `MODULE.bazel.lock`'s schema and not this one. BZL-JAVA-11 is the mechanical backstop for whatever a bad merge produces. | MUST |
| BZL-JAVA-13 | Set `strict_visibility = True` on `maven.install`. | Without it every transitive dependency is visible to every target, so pruning the declared artifact list can silently remove a jar that some other target depended on without declaring it. The failure surfaces later, elsewhere, with no link back to the edit that caused it. Measured: set by three of the four public exemplars carrying a real external Maven graph. | `grep -n strict_visibility MODULE.bazel`. **Absent is the finding to raise, present is the pass.** The per-repository override is `strict_visibility_value`, which is a narrowing decision to record rather than a way to turn the flag off wholesale. | SHOULD |
| BZL-JAVA-14 | Set `version_conflict_policy = "pinned"` wherever the build claims deterministic resolution, and document the choice once per `maven.install` rather than per artifact. Use `maven.artifact(force_version = "true")` for a deliberate single exception. | The default is Coursier's highest-wins across the whole graph, so someone else's transitive request can outrank your explicit declaration. That is the Bazel analogue of unmanaged Maven mediation, and `pinned` forces your declaration to win. `GRADLE-DEP-12` makes the same point on the Gradle side: pinning locks **resolution**, never legitimacy. Neither setting vets any version, and the checksum in the lock file is the actual integrity control. | `grep -n version_conflict_policy MODULE.bazel`. **Absent means the default applies**, which is a finding only where the repository claims deterministic resolution somewhere else without qualifying the claim. Present and `pinned` is the pass. | SHOULD |
| BZL-JAVA-15 | Scope every exclusion to the narrowest correct mechanism: `maven.artifact(exclusions = [...])` for one top-level artifact's transitive closure, `excluded_artifacts` on `maven.install` only when the coordinate must be banned from the entire resolved graph. | The two are not interchangeable. A global `excluded_artifacts` entry can starve an unrelated target that legitimately needed the jar, and a per-artifact `exclusions` entry leaves the coordinate reachable through any other top-level artifact that also pulls it in. | Bind the target name first, then query after a build the exclusion is meant to affect: `EXCLUDED=commons_logging_commons_logging` then `bazel query "somepath(//..., @maven//:$EXCLUDED)"`. **Empty output is the pass. A returned path the exclusion was supposed to sever is the finding.** Same query syntax on 8.7.0 and 9.2.0. | SHOULD |
| BZL-JAVA-16 | Mark a codegen-only or annotation-processor-only coordinate `neverlink = "true"`, and a test-only one `testonly = "true"`, through `maven.amend_artifact`. Never hand-edit the generated `java_import` target to add either. | `neverlink` keeps an artifact off the runtime classpath and out of any deploy jar, and `testonly` restricts it to `testonly` consumers. A hand-edit to the generated target is silently overwritten on the next repin, so the attribute quietly disappears and a codegen-only jar leaks onto a production classpath with no build-time signal. | `grep -n -e neverlink -e testonly MODULE.bazel` against `bazel query 'kind(java_import, @maven//...)'` for a target you know should carry one. **A mismatch between the amend list and the intent is the finding.** The generated target's own text is out of a grep's reach, so the amend list is the reachable subject. | SHOULD |

## Kotlin under Bazel

Two reads, one of your BUILD and `.bzl` text and one of any Gradle build beside
it. Rows bind `rules_kotlin` v2.4.10 (2026-08-20), verified 2026-09-12:

```bash
grep -rn -e 'java_import(' -e kt_kotlinc_options -e x_lambdas \
  --include='BUILD.bazel' --include='BUILD' --include='*.bzl' .
grep -rn -e 'kotlin("jvm")' -e kotlinJvm --include='*.gradle.kts' --include='*.gradle' .
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-18 | Never let a Maven-resolved Kotlin jar reach a hand-rolled `java_import`. Resolve Kotlin coordinates only through `rules_jvm_external`'s `maven.install`, whose generated import detects `kotlin_module` entries and skips `ijar` automatically. | In the ruleset's own words, the native `ijar` does not know about Kotlin metadata with respect to inlined functions and will remove method bodies inappropriately. An `inline fun` is spliced into the *caller's* bytecode, so the callee's body must be present at every downstream compilation. A codebase that happens not to call the affected function compiles clean while carrying a corrupted classpath entry, and the bug lands months later with the first new caller. | The first block grep. **A hand-written `java_import` whose `jars` names a jar containing a `META-INF` `kotlin_module` entry, which `unzip -l` on the jar shows, is the finding. Empty output, meaning no hand-written `java_import` at all, is the pass.** Coordinates reached through `@maven//:...` are already covered. **No version floor applies**: the Kotlin-aware import path landed 2019-03-26 and predates every supported release line. | MUST |
| BZL-JAVA-19 | Set `kt_kotlinc_options(x_lambdas = "indy")` explicitly wherever the same Kotlin sources are also compiled by Gradle. | The attribute defaults to `"class"`, anonymous inner classes, while Kotlin 2.x's own compiler default and Gradle's are both `"indy"`. Identical source therefore produces different bytecode depending on which build ran it: different class counts, different stack frames, different results out of any shrinker, bytecode-size budget or reflection-based harness. Leaving it unset in a dual build is not "using each tool's default", it opts the Bazel leg alone into the outlier. `x_sam_conversions` already defaults to `"indy"` and needs nothing. | Both block greps together. **`x_lambdas` unset, or set to `"class"`, in a repository whose Gradle build also applies a Kotlin plugin over the same source tree is the finding. A Bazel-only Kotlin repository is not a finding either way.** The public confirmed violation is dagger, where the same `.kt` tree is globbed by a `kt_jvm_library` wrapper and compiled again by a Gradle Kotlin module, and `x_lambdas` appears in zero files. | MUST with a Gradle leg over the same sources, SHOULD otherwise |

`KotlinCompile`, `KotlinKsp2` and `JdepsMerge` already run under persistent,
multiplexed workers, with no flag turning them on, so `--strategy=KotlinCompile=local`
is the first diagnostic step for a flaky Kotlin build (see [Gaps](#gaps)).

## What Ships, and Which Build Is Canonical

Three decisions a repository takes once, each read from a different place: the
CI lane that names an artifact, the top-level prose, and `MODULE.bazel`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-21 | Name every `//package:target_deploy.jar` you ship explicitly, in the build and in the CI lane that is supposed to produce it. Never treat a green `bazel build //...` or `bazel test //...` as evidence that the deploy jar builds. | `name_deploy.jar` is an implicit output of `java_binary`, built only if explicitly requested, so it appears in no BUILD file and in no wildcard build. Measured: 5 of 32 corpus repositories declare a `java_binary`, and 1 of 32 carries any `_deploy.jar` path anywhere, which is what a target nobody names looks like. Its single-jar merge also inherits the whole shading taxonomy the `gradle-build` set's `GRADLE-DIST` rows own, dropped `ServiceLoader` provider files and a class-load failure from stale signature files among them, so a deploy jar that is never built is never tested against any of it. | `grep -rn '_deploy\.jar' --include='*.yml' --include='*.yaml' --include='*.bzl' --include='BUILD.bazel' --include='BUILD' .` in a repository that declares `java_binary(` and ships an executable. **Empty output beside a shipped binary is the finding, not the pass.** Manifest additions go through `deploy_manifest_lines`. | MUST for CLI and application consumers, N/A for libraries |
| BZL-JAVA-22 | In a repository building the same code under both Gradle and Bazel, require one prose line naming which build is canonical. Do not require, and do not build, a generator or verifier proving the two dependency graphs agree. | Nothing measured checks parity. Neither of the two public dual-build exemplars generates one build's declarations from the other, and neither documents why it carries two. The stronger answer available, a CI job ordering that builds Bazel before Gradle, proves both builds succeed and never that their artifact sets agree, so asserting a parity requirement would encode a practice that exists nowhere it was measured. | `grep -rniE -e 'canonical build' -e 'built with bazel' -e 'built with gradle' CONTRIBUTING.md README.md`. **A hit is the pass. Empty output in a genuinely dual-build repository is the finding.** Parity between the two graphs is out of scope for any mechanical check, and a claim of parity with no mechanism behind it is its own finding. | SHOULD |
| BZL-JAVA-23 | Treat `contrib_rules_jvm`'s checkstyle, PMD and SpotBugs **wrapper macros** as a project decision to record, never a default to reach for. Adopt only where linting need already exceeds Error Prone plus the `java-quality` lint gate. The thing being decided is the `lint_setup()` or `linter.register()` call, not the `bazel_dep`. | Zero of the 32 corpus repositories reference `contrib_rules_jvm` or `apple_rules_lint`, and the wrappers layer a **third** linting framework on two this program already answers. The ruleset's own README states that linting is opt-in and that with no `lint_setup` call everything continues working with no lint tests generated, so the wrappers are inert until configured. | `grep -n -e 'lint_setup(' -e 'linter.register(' -e 'linter.configure(' MODULE.bazel`. **Absence needs no justification and is the pass.** Presence with no comment naming the gap it closes beyond Error Prone and the `java-quality` gate is a review finding, never a mechanical failure. **A bare `bazel_dep` on `contrib_rules_jvm`, or a transitively fetched `apple_rules_lint`, is not evidence of adoption and must not be flagged** (BZL-JAVA-25). | CONSIDER |

## Running Tests and Reading Coverage

There is no first-party JUnit 5 rule. `rules_java` and `@bazel_tools` ship none,
and `java_test`'s runner contract is JUnit-4-shaped. One grep plus one read of
the CI lane covers this block, which binds `contrib_rules_jvm` 0.31.0 verified
2026-09-12:

```bash
grep -rn -e java_junit5_test -e java_test_suite -e 'org.junit.jupiter' \
  --include='BUILD.bazel' --include='BUILD' --include='*.bzl' .
```

These rows rest on reading the ruleset's own source and README, not on measured
practice: none of the six public Bazel-carrying exemplars references
`contrib_rules_jvm`, `apple_rules_lint` or JUnit 5 at all, and the ruleset is
pre-1.0.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| BZL-JAVA-24 | Run JUnit 5 under Bazel through `contrib_rules_jvm`'s `java_junit5_test` for one class, or `java_test_suite(runner = "junit5", ...)` for a directory, and add `JUNIT5_DEPS`, or `JUNIT5_VINTAGE_DEPS`, to `deps` explicitly. A hand-written `java_test` with your own launcher is acceptable only if it demonstrably honours `XML_OUTPUT_FILE`, `TESTBRIDGE_TEST_ONLY`, `TEST_PREMATURE_EXIT_FILE` and the `TEST_SHARD_` variables. | `java_junit5_test` is a thin `java_test` wrapper, a fixed `main_class`, fixed `runtime_deps` and an agent that traps `System.exit`, over roughly 150 lines of already-correct Bazel test-protocol code. A naive launcher that calls the JUnit platform directly *runs the tests* and silently breaks `--test_filter`, sharding, premature-exit detection and CI XML parsing, with no build error anywhere. The generated target ships zero JUnit 5 jars itself, and its runner fails fast naming the missing coordinate. | The block grep: every `java_junit5_test` or `java_test_suite` must resolve its `load()` to `@contrib_rules_jvm//java:defs.bzl` and nothing else, so **a hit whose `load()` names any other source is a hallucinated symbol.** Then **a `java_test(` whose `deps` or `runtime_deps` name `org.junit.jupiter` or `org.junit.platform` artifacts, while its `main_class` is not the ruleset's own JUnit 5 runner, is the finding**, unless that launcher visibly reads all four environment variables above. **No JUnit 5 anywhere is nothing to check.** | MUST where a JUnit 5 suite runs under Bazel, N/A otherwise |
| BZL-JAVA-25 | Never treat a transitively fetched `apple_rules_lint` as a linting adoption, and never add `lint_setup()`, `linter.register()` or a `checkstyle_test` target as part of a change whose purpose was JUnit 5 support. | `contrib_rules_jvm`'s own `MODULE.bazel` carries an unconditional, non-dev `bazel_dep` on `apple_rules_lint` so it can lint itself, so module resolution fetches that module for anyone depending on `contrib_rules_jvm` at all. **Fetching is not using**: no lint target, no build action and no extension call is generated until the consumer writes the call. An agent reading the transitive dependency as "linting is on now, let me configure it" turns a test-only change into a linting-policy change nobody decided on. | `grep -n -e 'lint_setup(' -e 'linter.register(' MODULE.bazel`. **Absence is the correct default and is the pass.** Presence alongside `java_junit5_test` usage is a second, deliberate decision to review against BZL-JAVA-23. Diff-review half: a change titled "add JUnit 5 tests" that also touches a `linter` block is doing two unrelated things. | SHOULD |
| BZL-JAVA-26 | Treat the single merged lcov report from `bazel coverage --combined_report=lcov //...` as the only Java coverage artifact Bazel produces, and enforce any numeric floor as an explicit CI step parsing that file. Never as a Bazel flag, never per target, and never by expecting a JaCoCo XML or HTML report. | The output is lcov only, and there is exactly one combined report no matter how many test targets ran, so there is no per-`java_test` signal a floor can attach to. Bazel's command-line reference has no `--coverage_threshold`, `--fail_under` or `--minimum_coverage`, and `bazel coverage` fails when a *test* fails, never because a percentage was low. The `java-quality` set's JaCoCo branch-counter floor is written against a shape Bazel never emits, so a CI step globbing for a JaCoCo XML report after `bazel coverage` finds nothing and **passes vacuously**. Only passing tests contribute to the report. | Read the lines following any `bazel coverage` invocation in the CI file. **A JaCoCo or `.xml` path with no intervening lcov conversion between them is the finding**, as is any `--coverage_threshold`, `--fail_under` or `--minimum_coverage` on the command line, none of which exists. Then confirm the lane pipes `_coverage_report.dat`, under `bazel info output_path` in `_coverage`, through `lcov --summary` or a parser reading its `LF`, `LH`, `BRF` and `BRH` counters, with an explicit percentage check. Its absence in a repository that *claims* a coverage gate is the finding. Finally `grep -n coverage_report_generator .bazelrc`. **Empty output is the pass here, not a gap.** | MUST where a coverage gate is claimed over Bazel-built Java, N/A otherwise |
| BZL-JAVA-27 | Pass `runner = "junit5"` explicitly on every `java_test_suite` covering JUnit 5 sources. Never rely on the macro's default. | The attribute defaults to `"junit4"`. A suite of test files written against Jupiter annotations, left on the default, generates JUnit-4-runner `java_test` targets, and the failure mode is either zero tests discovered with a green build or an opaque class-load error. Never a legible "wrong runner" message. This is the cheapest silent green in the family. | Two halves, and the cross-reference is the check. `grep -rln 'org.junit.jupiter' --include='*.java' --include='*.kt' .` for the files a `java_test_suite(` glob matches, then read that call for `runner = "junit5"`. **Jupiter imports without the attribute is the finding. No Jupiter sources is nothing to check.** Not reducible to one grep. | MUST |

## Gaps

- **No `bazel` binary was run for any row here.** The strict-deps flag's
  canonical name is the one open item that a single `bazel help build --long` on
  a pinned binary closes permanently, which is why BZL-JAVA-05 ships SHOULD.
- Bazel's own Java page states the default javac options are `-source 8
  -target 8 -encoding UTF-8` two sections after stating that
  `--java_language_version` defaults to `11`. Both cannot be live, and the page
  was unfixed as of its 2026-09-05 revision. Read the user manual, the only page
  with self-consistent numeric defaults for all four flags.
- `--incompatible_language_version_bootclasspath` appears in one public
  exemplar's rc file with no further citation. Whether it changes anything about
  BZL-JAVA-09's `--patch_module` split is unmeasured.
- **The coverage gap is permanent, not open.** The upstream request for a richer
  JVM coverage format,
  [bazelbuild/bazel#12159](https://github.com/bazelbuild/bazel/issues/12159), was
  closed `not_planned` on 2024-06-29, and no maintained lcov-to-JaCoCo converter
  was run down, so a shop running both Gradle and Bazel needs two coverage
  toolchains with no bridge between their numbers.
- Whether `rules_jvm_external`'s `java_export` and `maven_publish` produce a
  Maven Central Portal-acceptable bundle, a sources jar, a javadoc jar, a
  per-file signature and a complete POM, was not run down. Today Bazel hands off
  publishing to Gradle or Maven, which is what the canonical-Bazel public
  exemplar does.
- Worker-state reproducibility for `KotlinCompile` is unmeasured: no primary
  source states a correctness defect and no exemplar overrides the default, so no
  rule ships. Building twice with and without `--strategy=KotlinCompile=local`
  and diffing the jars would settle it.
- BZL-JAVA-24, BZL-JAVA-25 and BZL-JAVA-27 have zero exemplars on either side,
  and BZL-JAVA-26 has one-sided evidence only.

## What Agents Get Wrong Here

1. **Inventing `--java_version` or `--tool_java_version`** by analogy with a
   single combined flag. Neither exists, and the line no-ops rather than
   erroring, so the build reads as configured and is not (BZL-JAVA-01).
2. **Assuming the `maven_install.json` freshness gate is unconditional**, by
   muscle memory from crate_universe, whose gate hard-fails on every ordinary
   build. Here it is opt-in and defaults off (BZL-JAVA-11).
3. **Reporting "strict deps is off" from an absent rc line**, then repairing the
   missing-dependency error it hid by writing `=off`. The unset default already
   errors (BZL-JAVA-04).
4. **Conflating `ijar` and `turbine` in either direction**, calling header
   compilation ijar-based because one prose page names only `ijar`, or concluding
   from "the header compiler is turbine" that the Kotlin inline-function trap is
   obsolete. Disabling header compilation does not cure the Kotlin trap, and
   routing Kotlin through `maven.install` does not change the header compiler
   (BZL-JAVA-07, BZL-JAVA-18).
5. **Assuming `bazel build //...` built the fat jar.** `_deploy.jar` is an
   implicit output named in no BUILD file, so a grep for it returns nothing even
   where `java_binary` targets exist (BZL-JAVA-21).
6. **Reaching for `bazel sync` or a fetch subcommand to repin the Maven graph.**
   The repin verb is `REPIN=1 bazel run @maven//:pin`, a run of a generated
   target, and no `--lockfile_mode`-style flag exists for this file
   (BZL-JAVA-12).
7. **Adding Error Prone as a `maven.install` coordinate or a `bazel_dep`**,
   because that is how every other JVM tool is pulled in, then hunting for the
   upstream repository's Bazel config to copy, which does not exist
   (BZL-JAVA-06).
8. **Assuming Kotlin lambda bytecode is identical across Gradle and Bazel**
   "because it is the same compiler", then attributing the observed difference to
   a compiler or JDK-target mismatch. Same compiler, different default
   (BZL-JAVA-19).
9. **Lowering `--java_language_version` to ship older bytecode**, missing that it
   also re-selects the toolchain through `source_version` matching
   (BZL-JAVA-03).
10. **Saying "the lockfile" in a repository that has two.** `maven_install.json`
    and `MODULE.bazel.lock` have unrelated schemas, freshness mechanisms and
    merge drivers, and pointing Bazel's own lockfile merge driver at the Maven
    one parses the wrong schema (BZL-JAVA-12).
11. **Assuming a more specific JVM registration wins toolchain resolution**, by
    analogy with constraint-value specificity elsewhere in Bazel. For same
    operating-system and CPU definitions the documented rule is pure registration
    order (BZL-JAVA-08).
12. **Expecting JaCoCo XML or an HTML index after `bazel coverage`**, because
    "Java coverage" reads as "JaCoCo" in nearly all training data, or inventing a
    coverage-threshold flag by analogy with other test runners. A CI step
    globbing for a JaCoCo report finds nothing and passes (BZL-JAVA-26).
13. **Hallucinating a first-party `java_junit5_test`**, taking
    `java_test_suite`'s `junit4` default for JUnit 5, or reading a transitively
    fetched `apple_rules_lint` as "linting is on here". The first two ship a
    green suite that discovered zero tests, the third turns a test-only change
    into a linting-policy change (BZL-JAVA-24, BZL-JAVA-27, BZL-JAVA-25).
