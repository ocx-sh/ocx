# Research: Dependency Path Interpolation in Package Managers

**Date:** 2026-04-19
**Issue:** ocx-sh/ocx#32 — `${deps.NAME.installPath}` interpolation in env metadata
**Expires:** 2027-01-01 (re-verify mise template vars; rest is stable prior art)

## Summary of Findings

### Question 1: How should NAME be derived?

Cross-tool consensus is strong: **NAME = repository basename, lowercased**. Every surveyed tool uses the short identifier.

| Tool | NAME is... | Example |
|------|-----------|---------|
| Mise | Tool short name | `{{tools.python.path}}` → `python` |
| Nix | `pkgs` attribute path | `${pkgs.cmake}` → `cmake` |
| Homebrew | Formula name | `Formula["python"]` → `python` |
| Spack | Package short name | `spec["python"]` → `python` |
| Cargo | Explicit `links` value | `DEP_OPENSSL_INCLUDE` from `links = "openssl"` |
| pkg-config | N/A (single-package only) | `${prefix}/lib` — no cross-package keys |
| pnpm | Explicit alias (`npm:pkg@ver`) | Declarative alias in `dependencies` |

For OCX: extract via `identifier.repository().split('/').last()`. Example: `ocx.sh/tools/cmake:3.28@sha256:...` → `cmake`.

**Collision problem requires explicit aliases.** The OCX ADR already enforces no duplicate `(registry, repo)` pairs in one `dependencies` array, but `ocx.sh/python:3.11` and `ghcr.io/myorg/python:3.13` both produce basename `python`. The fix: add `alias: Option<String>` on `Dependency`. If present, the alias is the NAME for interpolation purposes.

### Question 2: Extensibility — other fields beyond `installPath`?

Start with `installPath` only. Reserve the namespace for `version` and `digest` when a concrete use case arises. Do **not** add `binDir`, `libDir`, or `includeDir` shortcuts — callers compose these from `installPath` themselves (e.g., `${deps.python.installPath}/bin/python`). This matches Nix, Homebrew, and Spack.

Future field candidates (do not implement now, just reserve):
- `${deps.NAME.version}` — mise already has `{{tools.NAME.version}}`
- `${deps.NAME.digest}` — useful for reproducible env audit

The existing `${installPath}` (self-reference) should become an alias for `${self.installPath}` in a future migration — do not do this now; keep backward compat.

### Question 3: Publish-time validation?

Yes — validate at `package create`. The full dependency array is known at create time. Scan all `env[].value` strings for `${deps.NAME.*}` patterns and check:
1. NAME is a known basename or alias in the `dependencies` array
2. The field (initially only `installPath`) is supported

Error message quality (Rust `C-GOOD-ERR` style):
```
env variable 'PYTHON' references unknown dependency 'pythn' in '${deps.pythn.installPath}/bin/python'
declared dependencies are: python, cmake
did you mean 'python'?
```

## Industry Trend Framing

- **Established** (30+ years): pkg-config `${prefix}` self-referential interpolation; Nix derivation interpolation for cross-package paths.
- **Trending** (10+ years and widely adopted): Cargo `DEP_NAME_KEY` with explicit `links` alias; mise Tera templates for declarative TOML-first tool resolution.
- **Emerging**: pixi per-package activation scripts (push model — packages declare env that the consumer activates), short alias syntax in cross-registry pnpm-style dependency graphs.
- **Declining**: bash `$(brew --prefix foo)` + shell wrappers — the community is moving to declarative, machine-readable formats that CI/devcontainer/GitHub Actions can consume without running a shell.

OCX's `${deps.NAME.installPath}` sits in the "trending + adopted in new systems" cluster: declarative, author-time validated, cross-package addressable by explicit short name.

## Tool Survey

### Mise — `{{tools.NAME.path}}`
Closest analog. `mise.toml` env vars support Tera template syntax. NAME is the tool short name.
```toml
[env]
PYTHON = { value = "{{tools.python.path}}/bin/python", tools = true }
```
Fields exposed: `.path` (install root), `.version` (resolved string).
Source: https://mise.jdx.dev/templates.html

### Nix — `${pkgs.NAME}` derivation interpolation
Gold standard. Coercing a derivation in a string context yields its primary store path. Every interpolated reference is validated at eval time (Nix compile-time). `${pkg.dev}` vs `${pkg.out}` for multi-output derivations.
```nix
buildInputs = [ python3 cmake ];
shellHook = ''export PYTHON="${pkgs.python3}/bin/python"'';
```
Source: https://nix.dev/manual/nix/2.28/language/string-interpolation.html

### Spack — `spec["dep"].prefix`
Structured Python API. Name = lowercase Spack package name. `setup_dependent_build_environment` injects dep env into dependent builds.
```python
py_prefix = self.spec["python"].prefix
return [f"-DPYTHON_EXECUTABLE={py_prefix.bin}/python3"]
```
Source: https://spack.readthedocs.io/en/latest/packaging_guide_build.html

### Homebrew — `Formula["name"].opt_prefix`
Cross-formula path access. Name = lowercase formula name. `opt_` gives stable symlink paths across upgrades.
```ruby
py = Formula["python"].opt_prefix
bin.install_symlink py/"bin/python3" => "mytool-python"
```
Source: https://docs.brew.sh/Formula-Cookbook

### Cargo — `DEP_LINKS_KEY`
Flat env convention. Name = `links` value uppercased. Only immediate deps receive metadata vars. No publish-time validation.
```rust
let inc = env::var("DEP_FOO_INCLUDE").unwrap();
```
**Key lesson applicable to OCX**: The interpolation key (`openssl`) is an explicit `links` declaration, NOT derived from the crate name. The author commits to a stable alias up-front, so same-crate-name collisions across registries are impossible by construction. OCX's `alias: Option<String>` with basename fallback is a weaker form of the same pattern — fallback covers the common case, explicit alias resolves any collision.
Source: https://doc.rust-lang.org/cargo/reference/build-scripts.html

### pkg-config — `${prefix}` self-reference (30+ years)
The oldest convention in this space. `.pc` files use single-level variable interpolation — `${prefix}` is the root path, `${libdir}` and `${includedir}` derive from it. Crucially, there is **no cross-package interpolation** — each `.pc` file is self-contained. OCX already uses the equivalent syntax (`${installPath}`) for self-reference; the new work is adding the cross-package layer that pkg-config never developed.
Source: https://people.freedesktop.org/~dbn/pkg-config-guide.html

### pnpm — Explicit Aliases
npm's `npm:pkg@ver` alias form and pnpm's explicit `alias: <key>` in the dependency map let a consumer rename a dep at declaration time. The alias becomes the addressable name for subsequent references. Weaker than Cargo's `links` (no cross-package env emission), but confirms the convergence: declarative alias fields have replaced name derivation in multi-source ecosystems.
Source: https://pnpm.io/aliases

### Hermit — Gap
No cross-package path interpolation exists. A known gap in the tool, noted as motivation for OCX's approach.
Source: https://cashapp.github.io/hermit/packaging/reference/

### OmegaConf — `${resolver:argument}` pattern
Best-practice extensibility design for interpolation: `${namespace:argument}` separates the resolver from its argument. Enables adding `${env:VAR}`, `${oci:digest}` etc. without ambiguity.
Source: https://omegaconf.readthedocs.io/en/latest/custom_resolvers.html

## Gotchas and Failure Modes

1. **Same-basename collision** — two deps with identical basename from different registries. Mitigation: require `alias` field to disambiguate.

2. **`sealed` + interpolation is the canonical pairing** — `sealed` deps are pulled but their env never propagates; interpolation provides the path-access escape hatch without env propagation. Document this as the primary use case.

3. **Missing dep at resolve time** — dep failed to install or was never pulled. The resolver must check that `PackageStore::content()` path exists before expanding; fail with a clear error (not a broken path string).

4. **Circular interpolation is structurally impossible** — `${deps.NAME.installPath}` resolves to a filesystem path (a plain string). Install paths are determined by the object store layout, not further metadata. No re-expansion needed.

5. **Transitive dep paths** — limit interpolation to **direct (declared) dependencies only**. Matches Cargo `DEP_*` convention. Transitive dep paths are accessible via chain owner if truly needed. Forces explicit dependency declarations.

6. **NAME case sensitivity** — OCI repo names are lowercase by convention. Enforce: `${deps.Python.installPath}` is an error, not a case-insensitive match.

## Recommendation

Implement `${deps.NAME.installPath}` with:
- NAME = `identifier.repository().split('/').last()`, lowercase
- `alias: Option<String>` on `Dependency` for collision resolution; validate uniqueness + identifier format at deserialization
- Start with `installPath` field only; reject unknown fields at `package create` with an edit-distance suggestion
- Validate at `package create`: scan env values for `${deps.NAME.*}`, check NAME against dep basenames + aliases
- Restrict to direct (declared) dependencies only
- `sealed` + `${deps.NAME.installPath}` is the canonical use case — document it prominently
