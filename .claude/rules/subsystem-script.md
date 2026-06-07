---
paths:
  - crates/ocx_lib/src/script/**
---

# Script Subsystem — Starlark Host Style Guide

Style rule for OCX's host-side `#[starlark_module]` surface at `crates/ocx_lib/src/script/`. Derivative of the [Bazel `.bzl` style guide](https://bazel.build/rules/bzl-style) — scoped to OCX's authoring of typed Starlark values, host functions, and exposed namespaces. Applies to every host-allocated typed value authored under `crates/ocx_lib/src/script/**`.

The Starlark surface is a small but persistent contract: a script written today must keep running against future OCX versions. Settle conventions once; do not re-litigate per addition.

## Naming Conventions

| Element | Casing | Example | Mirrors |
|---|---|---|---|
| Type namespaces (root-level on `ocx`) | lowercase, matches the attribute name on values that hold an instance of the type | `ocx.os`, `ocx.arch` | Bazel `@platforms//os:linux`, Buck2 `host_info().os` |
| Enum variants inside a type namespace | PascalCase, mirrors the internal Rust enum variant exactly | `ocx.os.Linux`, `ocx.arch.Amd64` | Rust `OperatingSystem::Linux` / `Architecture::Amd64` |
| Type tag (what `type(value)` returns) — primitive/enum-like values | lowercase common noun, matches namespace name | `type(ocx.os.Linux) == "os"`, `type(ocx.arch.Amd64) == "arch"` | starlark-rust `"int"`, `"bool"`, `"string"`, `"namespace"`, `"set"`, `"dict"`, `"function"` |
| Type tag — record-like values (multi-attribute) | PascalCase | `type(p) == "Platform"`, `type(r) == "RunResult"` | starlark-rust `"AnyArray"`; Bazel `"Provider"`, `"Label"`, `"Field"` |
| Host functions on `ocx` (call-with-args / side effects) | snake_case method (parens always) | `ocx.run(...)`, `ocx.read_file(path)` | Buck2 `host_info()` |
| Host attributes on `ocx` (per-run constant data, no args) | snake_case attribute (no parens) | `ocx.target_platform` | Buck2 `ctx.attr.<name>` |
| Instance attributes on typed values | snake_case, canonical domain short form | `p.os`, `p.arch`, `p.is_any`, `r.exit_code` | Bazel `ctx.attr`, Buck2 `host_info().os` |

**Rationale — lowercase namespace.** A user reading `p.os == ocx.os.Linux` expects the attribute name (`os`) and the namespace name (`os`) to be the same word — that is what makes the comparison read as a single thought. The variant name (`Linux`) stays in the case of the internal Rust enum so there is zero translation at the boundary: when a Rust author adds `OperatingSystem::FreeBSD`, the corresponding Starlark constant is `ocx.os.FreeBSD` with no renaming step.

**Rationale — type-tag split.** starlark-rust's own built-in types follow the same split: primitive/enum-like values report lowercase short tags (`"int"`, `"bool"`, `"string"`, `"namespace"`, `"function"`), while user-defined record-like values report PascalCase tags (`"Provider"`, `"Label"`, `"AnyArray"`). OCX's enum-like values (`os`, `arch`) take the primitive path; multi-attribute record values (`Platform`, `RunResult`) take the record path.

**Rationale — attribute vs method on `ocx.*`.** Default is method (parens always) — that is consistent with the rest of the surface and signals "this is a host-side call". An attribute (no parens) is the right shape when the value is a **per-run constant** with no arguments and no side effects, where forcing the user to type parens is ceremony for a constant. `ocx.target_platform` (the platform reflected from the `-p` flag) is the canonical example: it never changes during a script run and takes no arguments. Materialize such attributes at globals-build time inside the `ocx_module` namespace closure (`b.set("name", value)`), with `host::scoped` installed beforehand. A bare noun like `ocx.platform` is still rejected — it does not communicate intent — but `target_platform`, `package_root_path`, `scratch_root_path` etc. are legal as attributes when the value qualifies.

## Typed Value Authoring

Every host-exposed typed value lives in its own file under `crates/ocx_lib/src/script/`:

- File name: `{type}_value.rs` (`os_value.rs`, `arch_value.rs`, `platform_value.rs`, `run_result.rs`).
- Wrapper type name: the Starlark-side short form (`OsValue`, `ArchValue`, `PlatformValue`, `RunResultValue`). The wrapper exists to project a Rust type into Starlark; naming it after the Starlark surface keeps the boundary single-named.
- Internal Rust enums (`OperatingSystem`, `Architecture`) stay fully-spelled — the boundary projects, it does not rename.
- The wrapper implements `StarlarkValue<'static>` via `#[starlark_value]` and `ProvidesStaticType + Allocative + Debug + Display + Clone + Copy`-where-cheap.
- `Display` returns the canonical OCI / domain string (`"linux"`, `"darwin"`, `"amd64"`), so `str(value)` is meaningful in scripts.
- `equals(&self, other) -> crate::Result<bool>` compares the inner discriminant. Cross-type comparisons (`ocx.os.Linux == ocx.arch.Amd64`, `ocx.os.Linux == "linux"`) return `false` — never panic, never raise.
- Variant-namespace registration walks the matching Rust `VARIANTS` const:

  ```rust
  for variant in OperatingSystem::VARIANTS {
      builder.set(variant.starlark_name(), OsValue(*variant));
  }
  ```

  No hand-written list of variants — drift between Rust and Starlark is locked out by construction.

### Optional / variant surfaces

For values with two shapes (a sentinel and a populated form, e.g. `Platform::Any` vs `Platform::Specific`), expose the difference as **a `T | None` attribute plus a companion `is_*` boolean on the same value** — never a tagged-union type.

```python
p = ocx.target_platform()
if p.is_any:
    ...
else:
    expect.eq(p.os, ocx.os.Linux)
```

Concretely: `p.os` is `OsValue | None`, `p.arch` is `ArchValue | None`, `p.is_any` is `bool`. Do not introduce a separate `PlatformAny` / `PlatformSpecific` type; that doubles the surface and offers no information the boolean does not.

Mirrors Buck2's `host_info` shape and the starlark-rust `T | None` convention.

### Closed enum sets only

Every variant exposed in a Starlark namespace must be present in the corresponding Rust `VARIANTS` const. No `Other(String)` escape hatch — a script that reaches an unsupported variant must fail at the Rust boundary (rejected at parse / `TryFrom` time), not in a script.

## Host Function Authoring

- Every `#[starlark_module]` function carries a `///` rustdoc — these docs are surfaced by the Starlark `Globals::documentation()` machinery and feed the user-facing host-API reference.
- The rustdoc summary line is the surfaced one-liner; write it as a precise return-type signature plus a one-sentence purpose statement, not a long prose paragraph:

  ```rust
  /// ocx.target_platform() -> Platform — reflects the `-p` flag, not the host.
  ```

- Reuse `script::sl_error::{fail, script_type}` for error paths. Do not invent ad-hoc error sites that bypass the firewall.

## Firewall Discipline (restated for visibility)

The structural test `script.rs::firewall_tests::no_starlark_import_outside_firewall` enforces it; this rule restates it so the constraint is visible while editing.

- No `use starlark*`, `starlark::`, `starlark_syntax`, `starlark_map`, or `starlark_derive` import path may appear outside `crates/ocx_lib/src/script/**`.
- A typed Starlark value type is part of the firewall surface even when it wraps a public `ocx_lib` type — the wrapper itself never escapes.

## Test Surface (locks the contract)

Every new typed value gets, in the same change:

1. **Rust unit tests** in the new module:
   - `equals` returns `true` for same variant, `false` for different.
   - Cross-type `equals` returns `false` (e.g. `OsValue == ArchValue`).
   - `Display` roundtrips with the Rust enum's `Display`.
   - For values with attributes: `get_attr` returns the expected `Value` for every documented attribute and `None` (or `NoneType`) for the sentinel form.
2. **VARIANTS-parity structural test** (one global test, extended per type): evaluate a `.star` snippet asserting every `Rust::VARIANTS` entry is present as a constant in the corresponding `ocx.*` namespace, and that `str(constant) == Rust::Display`. This is the gate that breaks the build when a new variant lands Rust-side but not Starlark-side.
3. **Acceptance test** in `test/tests/test_package_test_script.py` exercising attribute access from a `.star` snippet against a real materialized package, including a cross-type wall check (`ocx.os.Linux != "linux"`).

## Documentation

User-facing reference for the host API lives at `website/src/docs/reference/script-host-api.md`. When a host function or typed value is added, updated, or renamed, the reference page is updated in the **same commit** — it is the canonical user-facing surface for the host API, and drift is a Block-tier finding in code review.

## Bazel Style Clauses Adopted Verbatim

The following clauses from the Bazel `.bzl style guide` apply unchanged:

- **Line length**: 100 columns (matches `rustfmt.toml` max_width).
- **Naming hygiene**: no `myFunction`, no abbreviated identifiers (`ann` → `annotation`, `t` → `text`); domain initialisms (`OCI`, `OS`, `URL`) keep canonical casing.
- **Function docs are required** on every exported function; `doc=` parameter strings (where applicable) are full sentences.
- **No magic strings** at script-facing boundaries — every domain value goes through a typed namespace.

## Cross-References

- [`workflow-feature.md`](./workflow-feature.md) — when adding a new typed value, follow the contract-first TDD sequence.
- [`subsystem-tests.md`](./subsystem-tests.md) — acceptance tests for the script surface live in `test/tests/test_package_test_script.py`.
- [`subsystem-mirror.md`](./subsystem-mirror.md) — mirror tests consume the host API; mirror corpus migration tracks this rule.
- Bazel `.bzl style guide`: <https://bazel.build/rules/bzl-style> (external).
- Buck2 `host_info` reference: <https://buck2.build/docs/api/build/native/#host_info> (external).
