# Research: typify spike for the Rust SDK type layer (Option E vs Option D)

**Date:** 2026-10-04
**Axis:** tool measurement (spike)
**Consumer:** `adr_ocx_interface_contract.md` open question 1 and phase-2 go/no-go (b); `plan_ocx_interface_contract.md` WP-30 (contract C-026 b), decided at WP-35
**Method:** throwaway spike under `.tmp/typify-spike/` (gitignored, not committed). Every number below names the command that produced it. Nothing here is an estimate.

## Question

Can `typify` generate the Rust SDK's type layer from the published `reports/v2` and `errors/v1` schemas (Option E), or must OCX own a Rust backend (Option D)? Four measurements: pinned version; tagged-union dispatch with the unknown arms stripped and present; every `::crate` path against the SDK dependency budget; post-pass LOC to reach open enums.

## Setup

| Item | Value | Command |
|---|---|---|
| typify | **`=0.8.0`** (latest stable; `0.10.0-alpha.1` is the newest release and is a pre-release, `0.9.x` does not exist, alpha not measured) | `cargo add typify` resolved `0.8.0`; `cargo info typify` shows `0.10.0-alpha.1`; `cargo info typify@0.9` → "could not find" |
| schema crate for typify input | `schemars =0.8.22` (typify 0.8 uses it) | `cargo tree -i schemars` in `.tmp/typify-spike` |
| formatter | `prettyplease 0.2.37`, `syn 2` | `.tmp/typify-spike/Cargo.lock` |
| Input schemas | `crates/ocx_schema/tests/golden/reports.json` (`https://ocx.sh/schemas/reports/v2.json`, 276 `$defs`) and `errors.json` (`errors/v1.json`, 6 `$defs`). These are the golden files `golden_schemas.rs` asserts equal to the generated schemas (the test was not re-run here) | `sha256sum crates/ocx_schema/tests/golden/{reports,errors}.json` → `8b75fa44…81cb2b` / `1a7622d5…4962c2` at HEAD `72405b0fb` |
| Jobs | `CARGO_BUILD_JOBS=4` | |

Schema shapes in `reports.json`, counted with a Python walk (`json.load` + recursive search for the key): 13 `oneOf` unions, each with one `x-ocx-unknown-variant` arm (`type` is `not: {enum: [known…]}`); 45 `x-ocx-enum` nodes (44 `string`, 1 `integer`) and **no** `enum` keyword. `errors.json`: 3 `x-ocx-enum` (`ErrorDetail`, `ErrorCategory` strings; `ExitCode` integer).

Spike binary: reads a schema, applies a mode transform, runs `TypeSpace::add_root_schema`, formats with prettyplease.

```sh
cd .tmp/typify-spike && CARGO_BUILD_JOBS=4 cargo build
G=../../crates/ocx_schema/tests/golden
./target/debug/typify-spike $G/reports.json <mode> out/reports.<mode>.rs     # same for errors.json
```

Modes (the "stripped" and "present" runs the brief asked for, plus mitigation runs):

| Mode | Schema transform before typify |
|---|---|
| `present` | none: unknown arms and `x-ocx-enum` exactly as published |
| `stripped-unions` | delete every `oneOf` arm carrying `x-ocx-unknown-variant` |
| `stripped-all` | `stripped-unions` + rewrite string `x-ocx-enum` to `enum` (values only) so typify sees closed enums |
| `stripped-all-lean` | `stripped-all` + drop every `pattern` + `with_conversion(date-time → ::std::string::String)` (budget mitigation) |
| `open-lean` | `stripped-all-lean` + the owned post-pass (appendix) |
| `open-lean-inline` | `open-lean` + inline `{"$ref": X, ..siblings}` union arms |

All modes generated without error for both schemas (exit 0). Generated-output compile checks: `present`, `stripped-unions`, `stripped-all` compile with `serde`, `serde_json`, `chrono`, `regress` (`.tmp/dispatch`, `cargo run`); `stripped-all-lean`, `open-lean`, `open-lean-inline` compile with `serde` + `serde_json` only (`.tmp/lean`, `cargo build`).

## 1. Tagged dispatch

Output size and enum shape, per file (`wc -l out/*.rs`; `grep -c '#\[serde(tag = '`, `grep -c '#\[serde(untagged)\]'`, `grep -c '^pub enum '`):

| `reports.json`, mode | lines | `tag =` | `untagged` | `pub enum` |
|---|---|---|---|---|
| `present` | 7964 | 0 | **13** | 13 |
| `stripped-unions` | 7452 | 10 | 2 | 12 |
| `stripped-all` | 8495 | 10 | 2 | 58 |

Per union (13 `oneOf` defs), attribute on the generated enum (`grep -B3 "^pub enum <Name>\b"`):

| Mode | Result |
|---|---|
| `present` | all 13 `#[serde(untagged)]` |
| `stripped-unions` / `stripped-all` | 10 tagged: `EntrySource`, `IndexFinding`, `Unrepairable`, `WriteOutcome`, `PruneSelection`, `SkippedReason`, `HandoffFailure`, `Note` as `tag = "type"`; `AliasState` (`content = "digest"`) and `Prior` (`content = "value"`) as adjacent tagging. 2 `untagged`: `Var`, `Reason`. `Metadata` (two arms before stripping, one after) is not an enum at all: it collapses to a struct |
| `open-lean-inline` (inline pre-pass, 18 LOC) | 12 tagged, `Metadata` now a tagged enum, `Reason` fixed; `Var` still `untagged` |

Causes (from the schema, `python3` dump of the arms): `Var`, `Reason`'s `yielded_to` arm and `Metadata`'s `bundle` arm are `{"$ref": "#/$defs/X", "type": "object", "properties": {"type": {"const": …}}}` (schemars flattening a newtype struct). typify does not read the `const` beside the `$ref`, so the discriminator degrades to `String`. Inlining the `$ref` fixes `Reason` and `Metadata`; `Var` additionally carries `properties`/`required` (`key`, `visibility`) beside its `oneOf`, which typify merges into every arm.

Behavioural probes (`.tmp/dispatch`, `CARGO_BUILD_JOBS=4 cargo run`; `from_str` on each generated type; output columns trimmed):

| Type, input | `present` | `stripped-unions` / `stripped-all` |
|---|---|---|
| `EntrySource` `{"type":"package"}` | `Variant0 { type_: "package" }` | `Package` |
| `EntrySource` `{"type":"patch","rule":"r","companion":"c"}` | **`Variant0 { type_: "patch" }`**: the first arm swallows it, `rule`/`companion` lost | `Patch { companion, rule }` |
| `EntrySource` `{"type":"future_kind"}` | `Variant0`, no signal that the tag is unknown | `Err: unknown variant 'future_kind', expected 'package' or 'patch'` |
| `WriteOutcome` `{"type":"future_kind","x":1}` | `Variant2 { type_: "future_kind" }`, matches the wrong arm | `Err: unknown variant …` |
| `WriteOutcome` valid `refused` with nested `would_empty_index` | `Variant1` (correct, by luck of field shape) | `Refused { reason: WouldEmptyIndex { … } }` |
| `HandoffFailure` `{"type":"future_kind"}` | `Variant3 { type_: HandoffFailureVariant3Type("future_kind") }` | `Err: unknown variant …` |
| `Reason` `{"type":"future_kind"}` | `Variant7 { type_: "future_kind" }` | `Variant7 { type_: "future_kind" }` (untagged, no dispatch) |
| `Var` `{"type":"future_kind","key":"K","value":"v","visibility":"public"}` | `Variant1` (the constant arm) | `Variant1`: **silent mis-dispatch to a known arm** |
| `Var` valid `path` then re-serialised | n/a | `{"key":"K","required":true,"value":"v","visibility":"public"}`: **`type` is dropped** on the way out |

Reading: with the unknown arms **present**, typify emits untagged enums whose arms carry `type` as a plain `String`; decoding picks the first arm whose fields fit, so a known tag can land on the wrong arm and an unknown tag is never recognised. With them **stripped**, 10 of 13 unions come out as correct `serde` tagged dispatch, unknown tags are a decode error (closed), and 2 of 13 (`Var`, `Reason`) stay untagged and mis-dispatch (`Var` also loses `type` on serialisation).

Scalar enums, `present`: typify ignores `x-ocx-enum`, so no enum is emitted for any scalar. `WriteStatus` is `pub struct WriteStatus(pub ::std::string::String)` (`grep -n "WriteStatus" reports.present.rs`): already open, no known values. `stripped-all` closes them: 43 named string enums plus 3 anonymous `Var*Visibility` enums plus 12 union enums = 58 `pub enum` (set arithmetic over the golden `$defs` and the generated file). The integer `x-ocx-enum` (`ExitCode`) becomes `pub struct ExitCode(i64)` in both modes; in `stripped-all` of `errors.json` it carries a `TryFrom<i64>` allow-list (`[0, 1, 64, 65, …]`) that rejects a new code; the fix is not closing integer enums in the pre-pass (16-line `close_scalars` string-only guard), after which `ExitCode(99)` decodes.

## 2. Foreign `::crate` paths against the dependency budget

Budget (`adr_ocx_interface_contract.md` § Zero-dependency exceptions): Rust SDK runtime = `serde`, `serde_json`, optional `tokio`; std only otherwise. Census of root crates of every `::`-prefixed path (Python: `re.finditer(r'(?<![\w:>])::(\w+)', text)`, counted per file):

| File | `::std` | `::serde` | `::serde_json` | `::chrono` | `::regress` |
|---|---|---|---|---|---|
| `reports.present` | 2340 | 614 | 19 | **18** | **8** |
| `reports.stripped-unions` | 2148 | 562 | 19 | **18** | **8** |
| `reports.stripped-all` | 1996 | 570 | 19 | **18** | **8** |
| `reports.stripped-all-lean` | 1981 | 568 | 19 | 0 | 0 |
| `errors.present` / `errors.stripped-all` | 80 / 62 | 14 / 16 | 0 | 0 | 0 |

Items behind the foreign roots (same regex, whole path): `::serde_json::{Value, Map}` (in budget); `::serde::{Serialize, Deserialize, Deserializer, de::Error}` (in budget); **`::chrono::DateTime<::chrono::offset::Utc>`, `::chrono::offset::Utc`** (from `format: date-time`: the `Timestamp` newtype, 9 occurrences each); **`::regress::Regex`, `::regress::Regex::new`** (from `pattern`: 4 string newtypes, each a `LazyLock<Regex>`). So default typify output **exceeds the budget by `chrono` and `regress`**, both for `reports.json` only; `errors.json` is in budget.

Mitigation measured (`stripped-all-lean`): one `TypeSpaceSettings::with_conversion` for `date-time` → `::std::string::String` (9 lines as written) plus deleting `pattern` from the schema (12-line `strip_pattern` walk). Result: foreign roots drop to zero, the files compile against `serde` + `serde_json` only. Costs: `Timestamp` becomes a `String` newtype and the `pattern` validation is no longer enforced on decode (`$defs` carrying a `pattern`: `Digest`, `Timestamp`, `DependencyName`, `EntrypointName`).

## 3. Post-pass to open enums

Target form (SD § 3.14 Option D shape): scalar enum with `Unknown(String)`, union with `Unknown(serde_json::Value)`, `#[non_exhaustive]`. The spike post-pass (appendix, a `syn` rewrite of typify's token stream, run before prettyplease): rename each typify enum `X` to `XKnown` together with the `impl`s that belong to it (`impl X`, `impl Trait for X`, `impl From<X> for T`), then emit a wrapper

```rust
#[derive(Deserialize, Serialize, Clone, Debug)] #[serde(untagged)] #[non_exhaustive]
pub enum X { Known(XKnown), Unknown(/* String | serde_json::Value */) }
```

Counts (non-blank, non-comment lines after `cargo fmt`; `python3` over `src/*.rs`):

| Piece | LOC |
|---|---|
| Post-pass `postpass.rs` (whole file incl. `use`s) | **70** |
| Schema pre-pass: `strip_unknown` 14, `close_scalars` 16 | 30 |
| Budget mitigation: `strip_pattern` 12, `date-time` conversion 9 | 21 |
| `$ref`-sibling inlining `inline_arm_refs` | 18 |
| Total owned code E needs for the Rust types, excluding the driver `main` | 139 |

Output: 58 wrappers in `reports.open-lean` (59 in `open-lean-inline`), 2 in `errors.open-lean`; `reports.open-lean-inline.rs` is 8894 lines, `errors.open-lean.rs` 2558. It compiles against `serde` + `serde_json` only (`.tmp/lean`, `cargo build`). `Default` derives on enclosing structs still compile.

Decode probes on the wrapped output (`.tmp/lean`, `cargo run`):

| Input | Result |
|---|---|
| `EntrySource` `{"type":"patch","rule":"r","companion":"c"}` | `Known(Patch { … })`, re-serialises identically |
| `EntrySource` `{"type":"future_kind","x":[1]}` | `Unknown(Object{…})`, re-serialises identically |
| `WriteStatus` `"updated"` / `"rewritten_next_year"` | `Known(Updated)` / `Unknown("rewritten_next_year")` |
| `ErrorCategory` `"brand_new"` | `Unknown("brand_new")` |
| `ExitCode` `99` | `ExitCode(99)` (after the integer guard) |
| `WriteOutcome` `{"type":"refused"}` (known tag, required `reason` missing) | **`Unknown(Object{type: refused})`**, not an error |

Properties and gaps of this post-pass:

- Because the wrapper is `untagged`, a known tag with a malformed payload decodes as `Unknown` instead of failing. A tag-peeking custom `Deserialize` would fix that and is **not** counted in the 70 LOC.
- The domain value `CapabilityStatus::Unknown` (`"unknown"`) exists in the schema; the wrapper nests it as `Known(CapabilityStatusKnown::Unknown)`, so there is no name collision with the `Unknown(String)` arm.
- Known values sit one level down (`X::Known(XKnown::V)`), not as `X::V` as in SD § 3.14. `is_success()` per status enum (SD § 3.14 command layer) is owned in both options and not measured here.
- `Var` stays an untagged enum with a lost `type` (section 1); fixing it needs a schema change (move `key`/`visibility` into the arms) or a hand-written `Var` override via typify's `with_replacement`, neither measured.
- `x-ocx-*` keywords are ignored by typify; the spike's pre-pass is what maps them to typify's vocabulary.

## D-vs-E implication (no decision; WP-35 decides)

The ADR's gate was "typify produces internally tagged dispatch and `Option` + default, and its enums can be post-processed into the open form in under a day of owned code". Measured: with the schemas' unknown arms stripped, typify produced correct `serde` tagged dispatch for 10 of 13 unions (12 of 13 after an 18-line `$ref` inlining pre-pass), `Option<T>` with `#[serde(default, skip_serializing_if)]` throughout (298 `Option<`, 325 `skip_serializing_if` in `reports.present.rs`), and a 70-line post-pass turned every enum into an open one that decodes and re-serialises unknown values. Against that: with the arms present typify's output is wrong (untagged, first-match, no unknown signal), so E means the owned generator strips the arms and re-adds them, i.e. typify is fed a closed copy of the published schema; default output also breaches the SDK dependency budget (`chrono`, `regress`), which costs two more owned transforms (21 LOC) and drops date-time and pattern validation; `Var` remains mis-dispatched; the wrapper decodes a known tag with a bad payload as `Unknown`; and the SDK's types nest known values under `Known(..)`. E therefore means roughly 140 LOC of schema-and-token rewriting around a pinned pre-1.0 tool (`0.8.0`, schemars 0.8) plus a closed-copy convention, in exchange for not owning struct, field, `Option` and `$ref` emission; D owns all of that emission, and the open-enum shape of SD § 3.14 directly, but also its test burden. Not measured: typify `0.10.0-alpha.1`, the owned LOC of D's struct/union backend, and the command layer (owned under both options).

## Appendix: post-pass source (`.tmp/typify-spike/src/postpass.rs`)

```rust
use quote::{format_ident, quote};
use syn::{Item, visit_mut::VisitMut};

fn is_ty(ty: &syn::Type, name: &str) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.is_ident(name))
}

// `impl N`, `impl X for N`, or `impl From<N> for X`: the impls that belong to the closed enum.
fn belongs_to(i: &syn::ItemImpl, name: &str) -> bool {
    let arg_is = |args: &syn::PathArguments| {
        matches!(args, syn::PathArguments::AngleBracketed(a)
        if a.args.iter().any(|g| matches!(g, syn::GenericArgument::Type(t) if is_ty(t, name))))
    };
    is_ty(&i.self_ty, name)
        || i.trait_
            .as_ref()
            .is_some_and(|t| t.1.segments.last().is_some_and(|s| arg_is(&s.arguments)))
}

struct Rename<'a>(&'a str, syn::Ident);
impl VisitMut for Rename<'_> {
    fn visit_ident_mut(&mut self, i: &mut syn::Ident) {
        if i == self.0 {
            *i = self.1.clone();
        }
    }
}

pub fn open_enums(file: &mut syn::File) {
    let names: Vec<String> = file
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Enum(e) => Some(e.ident.to_string()),
            _ => None,
        })
        .collect();
    let mut wrappers = Vec::new();
    for name in names {
        let known = format_ident!("{name}Known");
        let ident = format_ident!("{name}");
        let mut is_union = false;
        for item in &mut file.items {
            let hit = match item {
                Item::Enum(e) => {
                    let own = e.ident == name.as_str();
                    if own {
                        is_union = e.attrs.iter().any(|a| quote!(#a).to_string().contains("tag ="));
                    }
                    own
                }
                Item::Impl(i) => belongs_to(i, &name),
                _ => false,
            };
            if hit {
                Rename(&name, known.clone()).visit_item_mut(item);
            }
        }
        let payload = if is_union {
            quote!(::serde_json::Value)
        } else {
            quote!(::std::string::String)
        };
        wrappers.push(
            syn::parse2(quote! {
                #[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
                #[serde(untagged)]
                #[non_exhaustive]
                pub enum #ident { Known(#known), Unknown(#payload) }
            })
            .unwrap(),
        );
    }
    file.items.extend(wrappers);
}
```

Pre-pass transforms (`serde_json::Value` walks, run before `RootSchema` deserialisation): `strip_unknown` drops `oneOf` arms having `x-ocx-unknown-variant`; `close_scalars` renames `x-ocx-enum: [{value, …}]` to `enum: [value, …]` where `type == "string"`; `strip_pattern` removes every `pattern` key; `inline_arm_refs` replaces an arm's `$ref` by the target's `properties`/`required` merged with the arm's own. `TypeSpaceSettings::with_struct_builder(false)` in all modes.
