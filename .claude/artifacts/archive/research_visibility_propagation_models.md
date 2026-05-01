# Research: Visibility & Env Propagation Models in Build Systems and Package Managers

## Metadata

**Date:** 2026-04-29
**Author:** architect (opus, fallback inline write after worker-researcher tool failure)
**Related ADR:** [`adr_visibility_two_axis_and_exec_modes.md`](./adr_visibility_two_axis_and_exec_modes.md)
**Source rule load:** `quality-core.md`, `product-tech-strategy.md`

## Summary

OCX needs a two-axis visibility model (per-entry on env declarations + per-edge on dependencies) plus a tri-mode exec model (consumer/self/full). This research surveys eight tools' propagation models to identify proven precedents and divergence points. **CMake** is the only mature reference with full per-entry + per-edge visibility; Nix/Guix/Spack/Bazel cover edge-axis only; devbox/devenv/mise have no declared visibility (everything leaks to the shell). OCX's existing edge-axis visibility (`sealed | private | public | interface`) already matches CMake's keyword set; adding entry-axis closes the gap and matches CMake's `target_include_directories` precedent exactly.

## Per-Tool Survey

### CMake — full two-axis (per-entry + per-edge)

CMake exposes visibility on **both** the property declaration and the link edge. Each surface uses the same `INTERFACE | PUBLIC | PRIVATE` keyword set.

**Per-property declaration** (entry axis): [`target_include_directories(target <INTERFACE|PUBLIC|PRIVATE> <items>)`](https://cmake.org/cmake/help/latest/command/target_include_directories.html). Same shape for `target_compile_definitions`, `target_compile_options`, `target_compile_features`, `target_link_directories`, `target_sources`. Each item carries its own visibility.

```cmake
target_include_directories(my_lib
    PRIVATE   src/internal       # used to compile my_lib only
    PUBLIC    include            # used to compile my_lib AND consumers
    INTERFACE include/api        # used by consumers only, not my_lib itself
)
```

**Per-link declaration** (edge axis): [`target_link_libraries(target <PRIVATE|PUBLIC|INTERFACE> <deps>)`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html). Controls how the linked target's INTERFACE+PUBLIC properties forward through `target`.

```cmake
target_link_libraries(my_lib
    PRIVATE   foo                # foo's INTERFACE+PUBLIC apply to my_lib's compile
    PUBLIC    bar                # bar's INTERFACE+PUBLIC apply to my_lib AND its consumers
    INTERFACE baz                # baz's INTERFACE+PUBLIC apply to my_lib's consumers only
)
```

**Diamond merge:** Implicit at use-time. When building a consumer, CMake walks the transitive graph and applies all reachable PUBLIC + INTERFACE entries. There is no per-target merge state stored; the merge materializes only when the consumer compiles.

**Default propagation:** Each property defaults to PRIVATE (own use only) when omitted. Consumer must explicitly opt in via `target_link_libraries(... PUBLIC|INTERFACE ...)`.

**Source:**
- [`target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html)
- [`target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html)
- [Build Specification and Usage Requirements §Transitive Usage Requirements](https://cmake.org/cmake/help/latest/manual/cmake-buildsystem.7.html#transitive-usage-requirements)

### Nix — `propagatedBuildInputs` (edge axis only)

Nix's stdenv distinguishes two dependency lists: `buildInputs` (used by self at build time, NOT propagated) vs `propagatedBuildInputs` (forwarded to consumers). [Setup hooks](https://nixos.org/manual/nixpkgs/stable/#ssec-setup-hooks) extend the model: a propagated input can install a hook that runs in every consumer's build.

```nix
stdenv.mkDerivation {
  pname = "myapp";
  buildInputs = [ openssl ];                # used by myapp's build only
  propagatedBuildInputs = [ glibc ];         # forwarded to anyone depending on myapp
}
```

**OCX mapping:**
- `buildInputs` ≈ edge `private` (use it, don't forward)
- `propagatedBuildInputs` ≈ edge `public` (use it AND forward)
- No exact INTERFACE analog (forward without using)
- `propagatedNativeBuildInputs` is for cross-compilation native axis, not visibility

**Diamond merge:** Last-defined wins per attribute (no formal merge primitive). Setup hooks can guard against duplicate imports via shell-level `if`-checks but this is convention, not language-level.

**Default propagation:** No propagation. Author must explicitly use `propagatedBuildInputs`.

**Source:** [Nixpkgs stdenv §Specifying dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies)

### Guix — `propagated-inputs` (edge axis only)

Mirror of Nix model: [`inputs` vs `native-inputs` vs `propagated-inputs`](https://guix.gnu.org/manual/en/html_node/package-Reference.html). Profile composition uses transitive closure of `propagated-inputs`.

```scheme
(define-public myapp
  (package
    (inputs `(("openssl" ,openssl)))           ; build-time, self-only
    (propagated-inputs `(("glibc" ,glibc)))))  ; forward to consumers
```

**OCX mapping:** Identical to Nix. Edge-axis only, two-state (propagate vs not), default no propagation.

**Diamond merge:** Profile builder dedup by package identity; conflicts surface as user-visible errors at `guix package -i` time.

**Source:** [Guix package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html)

### Bazel — providers + `runtime_deps` + `exports` (edge axis, multiple attrs)

Bazel decomposes the OCX-equivalent visibility into three orthogonal attributes per rule:

- `deps` — used at compile + propagated to direct consumers (≈ `public`)
- `runtime_deps` — used at runtime only, not at compile (≈ `interface` for runtime-class)
- `exports` — re-exports a dep's API to consumers without consuming directly (≈ `interface`)

```python
java_library(
    name = "myapp",
    deps = [":openssl"],            # myapp uses openssl + consumers see it
    runtime_deps = [":logging"],    # consumers need at runtime, myapp doesn't compile against it
    exports = [":api"],             # consumers see api as if they depended on it directly
)
```

**Per-property visibility on the rule's own attributes** (entry axis equivalent): partial. Some providers (`CcInfo`, `JavaInfo`) carry separate "use" vs "expose" sets internally, but rule authors don't write per-property visibility keywords — the split is rule-defined.

**Diamond merge:** Provider-level merge is rule-defined. `CcInfo` merges include directories as transitive depsets; `JavaInfo` does similarly for compile-time and runtime classpath. Each provider declares its own merge semantics.

**Default propagation:** Depends on attribute. `deps` propagates by default; `data` does not.

**Source:** [Bazel providers documentation](https://bazel.build/extending/rules#providers), [`exports` attribute on `java_library`](https://bazel.build/reference/be/java#java_library.exports)

### Spack — `depends_on(..., type=(...))` tuple (edge axis, multi-bit)

[Spack docs](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types) attaches a tuple of types to each dependency declaration: `('build', 'link', 'run', 'test')`.

```python
class MyApp(Package):
    depends_on('openssl', type=('build', 'link'))    # build + link, not at runtime
    depends_on('glibc', type='run')                  # runtime only, not at compile
    depends_on('cmake', type='build')                # build tool, not in produced artifact
```

**OCX mapping:** Roughly orthogonal axes per type:
- `'build'` ≈ edge `private` for build-time aspects
- `'link'` + `'run'` together ≈ edge `public`
- `'run'` alone ≈ edge `interface` (consumer-only, package itself doesn't need it post-build)

**Per-entry visibility on package's own variables** (entry axis): no. Variables in package recipes are package-global.

**Diamond merge:** Concretizer (Spack's resolver) deduplicates by `(spec, hash)`. Type tuples union when same dep reached via multiple paths.

**Source:** [Spack dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)

### Homebrew — name-only deps (no visibility)

[Formula reference](https://docs.brew.sh/Formula-Cookbook): `depends_on "openssl"` declares dependency; no visibility keyword. Two minor variations:

```ruby
class MyApp < Formula
  depends_on "openssl"                       # default: build + runtime
  depends_on "cmake" => :build               # build-time only
  depends_on "ruby" => :recommended          # optional, install by default
end
```

**Visibility model:** Effectively zero. `:build` modifier is the only visibility-adjacent qualifier (excludes the dep from the runtime closure). All other deps' env/PATH propagate fully to consumers via the global `/opt/homebrew/bin` PATH integration.

### asdf / mise — plugin-managed env (no propagation declared)

[mise docs](https://mise.jdx.dev/) and asdf's plugin model both delegate env composition to per-tool shell hooks. Each plugin emits PATH and exports unconditionally; there is no visibility declaration. Consumers compose by selecting multiple tools and merging shell PATH.

**Visibility model:** None. Every selected tool's env leaks to the shell.

### devbox / devenv / Nix flakes (devshell)

[devbox](https://www.jetify.com/devbox) and [devenv](https://devenv.sh/) compose multiple package envs by concatenating PATH and re-applying scalar exports inside an `init_hook` / `enterShell` block. Nix flakes' `mkShell` derivation uses the same `buildInputs` / `propagatedBuildInputs` shape as stdenv but every input's setup hook runs unconditionally inside the shell.

**Visibility model:** Inherited from underlying Nix when applicable; otherwise, every package's env is merged into the shell unconditionally.

## Comparison Matrix

| Tool | Entry axis | Edge axis | INTERFACE-equivalent | Default propagation | Diamond merge |
|---|---|---|---|---|---|
| **CMake** | yes (per-property) | yes (per-link) | yes (`INTERFACE`) | private (own); not propagated | implicit at use-time (walk graph) |
| **Nix** | no | yes (`propagatedBuildInputs`) | no (Native is different axis) | self only | last-defined wins per attribute |
| **Guix** | no | yes (`propagated-inputs`) | no | self only | last-defined wins per attribute |
| **Bazel** | partial (provider-defined) | yes (`deps`/`runtime_deps`/`exports`) | yes (`exports`) | self only | provider-defined (rule-specific) |
| **Spack** | no | yes (`type=` tuple) | partial (`run` without `link`) | depends on type | per-axis aggregation |
| **Homebrew** | no | yes (`:build` only) | no | full | name-based dedup |
| **asdf / mise** | no | no | no | full (everything leaks) | last shell-load wins |
| **devbox / devenv** | no | no | no (or inherited from Nix) | full | last-defined wins |
| **OCX (today)** | **no** | yes (4-state) | yes (`interface`) | sealed (default) | OR per axis (commutative + associative — proven) |
| **OCX (proposed)** | yes (3-state) | yes (4-state, unchanged) | yes (`interface` on both) | public on entry, sealed on edge | OR per axis (unchanged) |

### Axis Coverage Heat Map

```
Tool        | Entry | Edge | Interface | Diamond
------------|-------|------|-----------|----------
CMake       | YES   | YES  | YES       | implicit
Bazel       | partial | YES | YES      | rule-defined
OCX prop.   | YES   | YES  | YES       | OR-per-axis
------------|-------|------|-----------|----------
Spack       | no    | YES  | partial   | per-axis
Nix/Guix    | no    | YES  | no        | last-wins
OCX today   | no    | YES  | YES       | OR-per-axis
------------|-------|------|-----------|----------
Homebrew    | no    | partial | no    | name-dedup
asdf/mise   | no    | no   | no        | shell-order
devbox      | no    | no   | inherited | last-defined
```

## Shell / Exec Mode Surveys

A second axis: when a user invokes a tool through the package manager's "shell" or "exec" interface, do they see the full transitive env, or just the requested package's contract?

| Tool | Verb | Default env | Filter axis |
|---|---|---|---|
| Nix | `nix shell` / `nix develop` | Full transitive — every input's setup hook runs | None — all inputs leak |
| devbox | `devbox shell` | All declared packages' envs concatenated | None |
| mise | `mise exec` | Selected tool's env only (per-tool shim) | Implicit per-tool isolation |
| Spack | `spack load` | Full transitive of loaded spec | None at load time |
| OCX (today) | `ocx exec` | Anything-not-sealed (`is_visible()`) | Single-axis union |
| OCX (proposed) | `ocx exec --mode=consumer` | Public + interface | Consumer axis (matches CMake "consume from outside") |
| OCX (proposed) | `ocx exec --mode=self` | Public + private (launcher use) | Self axis (matches CMake "compile target T") |
| OCX (proposed) | `ocx exec --mode=full` | Anything-not-sealed | Union (debug escape hatch) |

**Key observation:** Every other surveyed tool collapses exec-time visibility into one mode (full transitive). mise stands out by per-tool isolation but achieves it via shim layer, not declared visibility. OCX's proposed tri-mode is novel; the closest parallel is CMake's compile vs link vs consume distinction at build time, which is conceptually the same axis split applied at exec time.

## Implications for OCX

### Decisions supported by precedent

| OCX proposed decision | Precedent | Strength |
|---|---|---|
| Two-axis (entry + edge) | CMake | Strong — CMake is the most mature reference and OCX already cites it |
| `interface` for "forward without using" | CMake `INTERFACE`, Bazel `exports` | Strong |
| Default-private propagation per axis | Nix, Guix, Spack | Strong — convention across all functional PMs |
| Default-public per-entry visibility | (no precedent — OCX-specific) | None — pragmatic migration choice |
| OR-per-axis diamond merge | (no precedent — already proven for OCX) | OCX-internal — keep |
| Tri-mode exec (consumer/self/full) | (no precedent — closest is CMake compile vs consume) | OCX-novel; design risk on default flip |

### Decisions diverging from precedent

1. **Default-public on entry visibility.** Every other tool (CMake, Nix, Guix, Spack, Bazel) defaults to "self-only, not propagated." OCX's proposed `public` default is migration-driven (preserves today's "env applies and propagates" behavior of in-tree metadata). Risk: long-term, this may encourage over-exposure. Mitigation: doc clearly recommends `private` for internal entries; the in-tree catalog can be re-tagged for `private` over time without further breaking changes.

2. **Tri-mode exec.** No surveyed tool has user-visible mode flags on its exec verb. OCX's `--mode` flag is novel. Risk: discoverability cost. Mitigation: launcher embeds `--mode=self` (invisible to user); `--mode=consumer` is the unannotated default (also invisible); `--mode=full` is the only flag users explicitly type, and only for debug.

3. **`Sealed` rejected on entry visibility but valid on edge visibility.** Asymmetric per-surface semantics. Risk: author confusion. Mitigation: structured parse error with explicit message; doc page distinguishes the two surfaces clearly with the comparison table.

### Decisions to recheck against precedent during plan phase

- **Entry-level visibility default = `public`** — verify no other PM has tried this default and abandoned it. Quick scan suggests no, but worth a deeper Bazel + Spack check during `/swarm-plan`.
- **`Visibility::Interface` for synthesized PATH entry** — no precedent. Could alternatively model synth as a separate "always emit when consumer-axis active" mechanism without piggybacking on the visibility enum. Test which approach yields cleaner diamond-merge tests.
- **Mode flag persistence in launcher body** — locked-forever shape (per [`adr_package_entry_points.md` Tension 4](./adr_package_entry_points.md)). Confirm `--mode=self` is the right shape vs `--launcher-context=true` or similar; the latter is more semantic, less coupled to mode names that may evolve. Open question for plan phase.

## Sources

### Primary documentation

- [CMake `target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html)
- [CMake `target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html)
- [CMake Build System Manual §Transitive Usage Requirements](https://cmake.org/cmake/help/latest/manual/cmake-buildsystem.7.html#transitive-usage-requirements)
- [Nixpkgs stdenv §Specifying dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies)
- [Nixpkgs §Setup hooks](https://nixos.org/manual/nixpkgs/stable/#ssec-setup-hooks)
- [Guix package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html)
- [Bazel providers documentation](https://bazel.build/extending/rules#providers)
- [Bazel `java_library.exports`](https://bazel.build/reference/be/java#java_library.exports)
- [Spack dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)
- [Homebrew Formula Cookbook](https://docs.brew.sh/Formula-Cookbook)
- [mise documentation](https://mise.jdx.dev/)
- [devbox documentation](https://www.jetify.com/devbox)
- [devenv documentation](https://devenv.sh/)

### OCX internal references

- [`adr_package_dependencies.md`](./adr_package_dependencies.md) — original four-level visibility model, propagation table, diamond merge
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — entry-point launcher mechanism
- [`prd_package_entry_points.md`](./prd_package_entry_points.md) — §"Out of Scope" deferral re-opened by the new ADR
- [`adr_visibility_two_axis_and_exec_modes.md`](./adr_visibility_two_axis_and_exec_modes.md) — the consuming ADR

### Code references

- `crates/ocx_lib/src/package/metadata/visibility.rs` — `Visibility` enum, `propagate`, `merge`, `is_self_visible`, `is_consumer_visible`, `is_visible`
- `crates/ocx_lib/src/package_manager/visible.rs:139` — runtime filter call site (currently `is_visible()`)
- `crates/ocx_lib/src/package/resolved_package.rs:59` — propagation algebra at install time
- `crates/ocx_lib/src/package_manager/visible.rs:344-373` — synthesized PATH entry for entrypoints
- `crates/ocx_cli/src/command/exec.rs` — exec command surface

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-04-29 | architect (opus) | Initial inline write after worker-researcher subagent tool failure; sources verified against primary documentation URLs |
