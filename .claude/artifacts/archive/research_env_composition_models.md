# Research: Env Composition Models — Validation of OCX Two-Env Composition

## Direct Answer

OCX's two-env composition model is well-grounded and matches CMake's INTERFACE/PRIVATE/PUBLIC semantics. **Proceed.** The model as planned (default-private entry visibility, `sealed` edge default, `--self` boolean flag, consumer view as default, OR-per-axis diamond merge) is validated by CMake as the primary reference and by consensus across Nix, Guix, Spack, and Bazel.

The pre-refactor "intersects(view) mask" implementation produced semantically wrong answers under the matrix walk-through (see `.claude/plans/delightful-toasting-dongarra.md`). Two-env composition (`interface_env` + `private_env` per package, only `interface_env` crosses edges) is a strict simplification of the same algebra, free of the masking-conflation problems.

## Industry Context & Trends

**Trending:** OCI Spec v1.1 (Feb 2024) added `artifactType` + `subject` for non-image artifact association — infrastructure OCX builds on, no env-composition semantics. CMake 3.28-3.30 tightened PRIVATE defaults (CMP0154: generated headers private; CMP0166: INTERFACE_LINK_ correctness). Direction of travel in the most mature reference: more conservative defaults.

**Established:** CMake two-axis (entry + edge) PRIVATE/PUBLIC/INTERFACE is the only proven full two-axis model in wide use. Nix/Guix/Spack/Bazel cover edge-axis only.

**Emerging:** Nothing in the 2024-2026 scan (Nix RFCs, Spack RFCs, OCI Package WG, Bazel BEP) is converging on a new multi-surface env composition model. OCX's model remains ahead of the ecosystem.

**Declining:** Opt-out propagation defaults (devbox/devenv/mise pattern of "everything leaks") are the documented anti-pattern across both Nix and Guix community discussions.

## Research Axis Findings

### 1. CMake INTERFACE / PUBLIC / PRIVATE (the documented inspiration)

Sources: [`target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html), [`target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html)

Two orthogonal surfaces using the same three keywords:

**Entry axis** (`target_include_directories`, `target_compile_definitions`, etc.):
- `PRIVATE items` → populate `INCLUDE_DIRECTORIES` of T only. Consumers never see them.
- `INTERFACE items` → populate `INTERFACE_INCLUDE_DIRECTORIES` for consumers only. T's own compile doesn't see them.
- `PUBLIC items` → both: T uses them, and they are forwarded to consumers.

**Edge axis** (`target_link_libraries`):
- `PRIVATE dep` → dep's INTERFACE+PUBLIC properties used by T's compile. Not forwarded.
- `INTERFACE dep` → dep's INTERFACE+PUBLIC properties forwarded to T's consumers. T does not link against dep directly.
- `PUBLIC dep` → dep's properties apply to T's compile AND are forwarded to consumers.

**Diamond merge:** Implicit at use-time. CMake walks the transitive graph; no per-target merge state. Deduplication is by value; no defined ordering within the same level. OCX's "later wins" diverges (discussed in §9).

**Default:** PRIVATE when omitted (modern CMake requires explicit keywords).

**OCX mapping (planned two-env composition):**

| CMake | OCX entry axis | OCX edge axis |
|---|---|---|
| PRIVATE | `visibility: "private"` (own private surface only) | `visibility: "private"` (dep.interface → consumer.private only) |
| PUBLIC | `visibility: "public"` (own both surfaces) | `visibility: "public"` (dep.interface → consumer both) |
| INTERFACE | `visibility: "interface"` (own interface surface only) | `visibility: "interface"` (dep.interface → consumer.interface only) |
| "compile T" context | `--self` reads private surface | — |
| "consume T from outside" context | default reads interface surface | — |

The mapping is exact. OCX's planned implementation is structurally isomorphic to CMake's two-axis model, translated from compile-time includes/flags to runtime env vars.

### 2. Nix `propagatedBuildInputs` vs `buildInputs`

Sources: [Nixpkgs stdenv §dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies), [Nix Pills §Basic Dependencies](https://nixos.org/guides/nix-pills/20-basic-dependencies-and-hooks.html)

Edge-axis only. No per-entry visibility.

- `buildInputs` — added to build env of self only. Not propagated.
- `propagatedBuildInputs` — every downstream derivation receives these as additional `buildInputs`, recursively.
- `nativeBuildInputs` / `propagatedNativeBuildInputs` — different axis (host vs target arch), not visibility.

No INTERFACE equivalent: cannot forward a dep to consumers without using it yourself. `interface` edge in OCX has no Nix analog (passthrough meta-package required).

**Default:** No propagation (`buildInputs` ≈ OCX `private` edge).

### 3. Bazel `deps` / `runtime_deps` / `exports`

Sources: [Bazel providers](https://bazel.build/extending/rules#providers), [`java_library.exports`](https://bazel.build/reference/be/java#java_library.exports)

- `deps` — compile-time + propagated. ≈ OCX `public`.
- `runtime_deps` — consumer runtime only. ≈ OCX `interface` for runtime deps.
- `exports` — re-exports dep's API to consumers as if consumer depended directly. **Exact INTERFACE equivalent.**
- `data` — runtime data files; not propagated.

Providers (`CcInfo`, `JavaInfo`) carry separate "use" vs "expose" sets internally. Per-entry granularity exists but only via provider design, not user-facing syntax.

### 4. Spack `type=` tuples

Source: [Spack packaging guide §Dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)

Four types per `depends_on`: `build` (PATH at build), `link` (compiler injection), `run` (PATH via `spack load`), `test`. Default: `('build', 'link')`. All-or-nothing per dep — no per-entry granularity.

- `run` alone ≈ OCX edge `interface`
- `build` alone ≈ OCX edge `private`
- `('build', 'link', 'run')` ≈ OCX edge `public`

### 5. NPM/PNPM `peerDependencies`

Source: [npm §Peer Dependencies](https://nodejs.org/en/blog/npm/peer-dependencies)

`peerDependencies` declares dep is resolved from consumer's `node_modules`. Structurally analogous to OCX's `interface` edge. No per-entry control.

### 6. Guix `propagated-inputs` / `inputs`

Sources: [Guix package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html), [futurile.net 2024](https://www.futurile.net/2024/03/29/guix-package-structure-inputs/)

Mirrors Nix exactly. Guix docs explicitly state propagation "should be avoided unless necessary, or else it pollutes the user profile." Diamond conflicts surface as errors at install time.

### 7. Comparison Table (Updated to Final OCX State)

This supersedes the table at `adr_visibility_two_axis_and_exec_modes.md` lines 115-123, which showed an intermediate state.

| Tool | Entry axis | Edge axis | INTERFACE-equiv | Default edge propagation | Default entry propagation | Diamond merge | Exec-time filter |
|---|---|---|---|---|---|---|---|
| **CMake** | yes (per-property) | yes (per-link) | yes (`INTERFACE`) | PRIVATE | PRIVATE | implicit walk-graph | N/A |
| **Nix** | no | yes (2-state) | no | self only | N/A | last-defined wins | none (full transitive) |
| **Guix** | no | yes (2-state) | no | self only | N/A | version-conflict error | none |
| **Bazel** | partial (provider-internal) | yes (3 attrs) | yes (`exports`) | `deps` propagates | N/A | provider-defined | hermetic |
| **Spack** | no | yes (`type=` tuple) | partial (`run` alone) | `('build','link')` | N/A | per-axis aggregation | `spack load` |
| **NPM peerDeps** | no | yes (3-state) | yes (`peerDependencies`) | `dependencies` | N/A | hoisting dedup | module resolution |
| **OCX (pre-refactor, intersects mask)** | yes (3-state) | yes (4-state) | yes (`interface`) | `sealed` | `private` | OR-per-axis | view mask `intersects` |
| **OCX (planned two-env composition)** | yes (3-state) | yes (4-state) | yes (`interface`) | `sealed` | `private` | per-surface vec merge | binary `--self` selects which surface |

**Ordering semantics (planned):** dep contributions first, then own env vars, then entrypoints (interface only). `type: path` prepends; `type: constant` and `type: env` last-wins. Recursion safety on launchers comes from `--self` selecting the private surface (which has no entrypoints), not from PATH ordering.

### 8. Trend Scouting 2024-2026

**CMake:** CMP0154 (3.28) made generated headers private by default. CMP0166 (3.30) fixed `INTERFACE_LINK_*` correctness. Vocabulary stable.

**OCI v1.1 (Feb 2024):** `artifactType` + `subject` — infrastructure for OCX storage; carries no env-composition semantics.

**Nix RFCs 2024-2026:** No accepted RFC proposing entry-axis visibility. Python community proposed two-derivation split (build vs test) — analogous in spirit to surface separation, still edge-axis only.

**Guix community:** "linked-inputs" proposal (Prikler) would create a finer-grained `buildInputs` variant; still no per-entry visibility.

**No convergence:** Spack, PNPM, Cargo, Bazel BEP, OCI Package WG show no movement toward per-entry visibility as of April 2026. The space above the edge axis is uncrowded.

### 9. Pitfalls and Known Footguns

**CMake: PUBLIC-everywhere anti-pattern.** Most documented CMake mistake — tagging all `target_link_libraries` PUBLIC "because it works." Forces consumer recompiles, leaks implementation deps as contract. Sources: [Effective Modern CMake (mbinna)](https://gist.github.com/mbinna/c61dbb39bca0e4fb7d1f73b0d66a4fd1), [Modern CMake Do's and Don'ts](https://cliutils.gitlab.io/modern-cmake/chapters/intro/dodonot.html). **OCX implication:** default-private entry policy is the correct prevention. Inverse footgun: publishers forgetting to mark `JAVA_HOME` / `PATH` as `public` break consumers silently — needs migration audit + clear docs.

**Nix/Guix: over-propagation bloating closures.** Over 1400 packages in Nixpkgs transitively depend on glib due to `propagatedBuildInputs` overuse ([Discourse](https://discourse.nixos.org/t/python-propagatedbuildinputs-could-we-do-better/29035)). Guix's [linked-inputs proposal](https://www.mail-archive.com/guix-devel@gnu.org/msg58924.html) shows independent upgrades become impossible at scale. **OCX implication:** `sealed` as edge default is correct mitigation, validated by two ecosystems independently.

**Bazel: host env leaking into hermetic builds.** `use_default_shell_env` lets host PATH bleed into actions ([Bazel #2574](https://github.com/bazelbuild/bazel/issues/2574)). Long-standing footgun. **OCX implication:** "clean-env execution" principle is correct. Pre-refactor "intersects(PUBLIC) union" mask was exactly this anti-pattern — ambient env from private deps leaking into every `ocx exec` call.

**Composition-order divergence from CMake:** CMake's implicit graph walk has no defined ordering within the same transitive level. OCX's "later wins" (own overrides dep at same key; `type: path` prepends; `type: constant` last-wins) is more deterministic but subtly different. Right choice for runtime env (matches shell semantics) but documentation must clearly state last-wins behavior with a worked example.

## Sources

- [CMake `target_link_libraries`](https://cmake.org/cmake/help/latest/command/target_link_libraries.html)
- [CMake `target_include_directories`](https://cmake.org/cmake/help/latest/command/target_include_directories.html)
- [CMake 3.28 release notes](https://cmake.org/cmake/help/latest/release/3.28.html) — CMP0154
- [CMake 3.30 release notes](https://cmake.org/cmake/help/latest/release/3.30.html) — CMP0166
- [Effective Modern CMake (mbinna)](https://gist.github.com/mbinna/c61dbb39bca0e4fb7d1f73b0d66a4fd1)
- [Modern CMake Do's and Don'ts](https://cliutils.gitlab.io/modern-cmake/chapters/intro/dodonot.html)
- [Nixpkgs stdenv §dependencies](https://nixos.org/manual/nixpkgs/stable/#ssec-stdenv-dependencies)
- [Nix Pills §Basic Dependencies](https://nixos.org/guides/nix-pills/20-basic-dependencies-and-hooks.html)
- [NixOS Discourse §Python propagatedBuildInputs](https://discourse.nixos.org/t/python-propagatedbuildinputs-could-we-do-better/29035)
- [Guix package reference](https://guix.gnu.org/manual/en/html_node/package-Reference.html)
- [Guix-devel: Rethinking propagated inputs (Prikler)](https://www.mail-archive.com/guix-devel@gnu.org/msg58924.html)
- [futurile.net §Guix package structure: inputs (2024)](https://www.futurile.net/2024/03/29/guix-package-structure-inputs/)
- [Bazel providers documentation](https://bazel.build/extending/rules#providers)
- [Bazel issue #2574 — host env leaking](https://github.com/bazelbuild/bazel/issues/2574)
- [Spack packaging guide §Dependency types](https://spack.readthedocs.io/en/latest/packaging_guide.html#dependency-types)
- [npm §Peer Dependencies](https://nodejs.org/en/blog/npm/peer-dependencies)
- [OCI Image + Distribution Spec v1.1 release](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/)

## Recommendation

**Proceed with two-env composition refactor.** Five-tool consensus (CMake, Nix, Guix, Spack, Bazel) validates default-private entry visibility and `sealed` edge default. The `--self` boolean flag is OCX-novel but correctly grounded in CMake's "compile T" vs "consume T" mental model. Per-surface vec merge replaces OR-per-axis algebra cleanly: `interface_env` and `private_env` partitions only ever grow via known set-union semantics, no axis intersection logic needed in the hot path.

**Documentation action items:**
1. `website/src/docs/reference/metadata.md` must explicitly document last-wins ordering for scalar env entries with worked example showing publisher's `TOOL_HOME=${installPath}` shadowing dep's `TOOL_HOME`.
2. New `website/src/docs/reference/env-composition.md` documenting build order (deps → own → entrypoints) and prepend/replace semantics.
3. ADR `adr_visibility_two_axis_and_exec_modes.md` superseded by `adr_two_env_composition.md` — old ADR's intersects-mask algorithm is wrong under the matrix walk-through.

**Banned-term taxonomy (post-refactor):** `intersects`, `view filter`, `view mask`, `mode filter`, `is_visible`, `is_self_visible`, `is_consumer_visible`, `ExecMode`, `mode.includes`. Replace with `surface`, `interface_env`, `private_env`, "two-env composition".
