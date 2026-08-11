# ADR: Package `integrations` — Vendor-Namespaced Uninterpreted Metadata

- **Status:** Accepted (2026-08-09) · **amended 2026-08-10** — see *Amendment: the pass-through decision was reversed*
- **Deciders:** owner (design ratified in [`ocx-sh/ocx#221`](https://github.com/ocx-sh/ocx/issues/221)); architect (this record)
- **Domain Tags:** api, data, integration
- **Tech Strategy Alignment:** follows the Golden Path — Rust 2024 / `serde` / `schemars`, no new dependency (`serde_json::Value` is already a workspace dep). No deviation.
- **Related:** [`adr_interpolation_token_grammar.md`](./adr_interpolation_token_grammar.md) (**supersedes this record's D7 pass-through rule** — [`ocx-sh/ocx#303`](https://github.com/ocx-sh/ocx/issues/303)), [`research_package_integrations.md`](./research_package_integrations.md) (prior-art survey backing every ratified decision), [`research_interpolation_capability.md`](./research_interpolation_capability.md) (validates the `Usage`→`AllowedTokens` gate this extends), [`adr_declared_binaries_metadata.md`](./adr_declared_binaries_metadata.md) (the claim-array + attribution pattern this reuses), [`adr_entrypoint_args_interpolation.md`](./adr_entrypoint_args_interpolation.md) (D6 gate-before-regex), [`adr_two_env_composition.md`](./adr_two_env_composition.md) (surface algebra), [`adr_inspect_metadata_closure.md`](./adr_inspect_metadata_closure.md) (closure surface + blob cap precedent), `subsystem-package.md`, `subsystem-metadata-schema.md`, `subsystem-package-manager.md`, `subsystem-cli-api.md`
- **Provenance:** written from [`#221`](https://github.com/ocx-sh/ocx/issues/221) directly (issue body read in full; it has no comment thread — the body is the whole record) plus first-hand code reading. The body's two "Open questions" were answered by the owner after that text was written and are recorded here as decisions: cap in `ValidMetadata::try_from` at exit 65, 8 KiB per namespace / 32 KiB per package (D8); composed payload key `value` (D5 — **since reversed to `payload`**, see the 2026-08-11 changelog row).

---

## Amendment: the pass-through decision was reversed (2026-08-10)

**What changed.** This record was written against a rule that no longer holds:
*a `${…}` OCX does not recognise inside a payload is emitted byte-identical*
(D7, and #221's own stated guarantee). The owner **reversed** that, and
[`adr_interpolation_token_grammar.md`](./adr_interpolation_token_grammar.md) /
[`#303`](https://github.com/ocx-sh/ocx/issues/303) landed the replacement as
`339383af feat(package)!: every ${…} in package metadata follows one grammar`.

**What replaced it.** OCX **claims every `${…}`** in package metadata. A payload
is not exempt: it goes through the same scanner, so the same refusal. There is
no foreign-token concept and no pass-through path anywhere in the tree.

| | Before (as this ADR was written) | After (#303, shipped) |
|---|---|---|
| `${workspaceFolder}` in a payload | emitted byte-identical | **refused at publish, exit 65**, message names the token and offers the escape |
| `$${workspaceFolder}` in a payload | escape existed but was optional | **the only** way to emit a literal `${workspaceFolder}` |
| Recognised bodies | `${installPath}`, `${deps.NAME.installPath}` | + `${self.installPath}` (exact alias), each install-path body taking an optional `:native` / `:posix` render modifier |
| Where the refusal fires | n/a | resolution (compose / `env` / `exec` / `run`) **plus one publish gate** (`create` / `push`). Read-only paths — `pull`, `install`, `inspect`, `deps`, `which` — still echo an unrecognised token verbatim |

**Why the reversal.** Pass-through makes OCX's namespace hostage to other
tools' vocabularies: to pass `${workspaceFolder}` through safely OCX has to know
it exists, and to keep doing so it has to freeze its own root set forever. The
full argument is `adr_interpolation_token_grammar.md` Axis C and D3; it is not
re-argued here.

**What it costs this feature, stated plainly.** A devcontainer or VS Code
settings blob pasted unmodified no longer publishes. Every foreign token in it
must be doubled — `$${workspaceFolder}` — and the authoring burden is
proportional to how many there are. This ADR's original §8(d) called that
outcome "killing the feature outright"; the owner weighed it against the
namespace-coupling cost and chose it anyway. What survives unchanged is the
payload's *contents* rule: OCX still never interprets, merges, or validates
what a payload means — only its own `${…}` vocabulary inside it.

**Scope of this amendment.** D7 and every worked example carrying a foreign
token are rewritten in place below; §5.3's discriminating test is void and is
replaced; D22/OQ-3's follow-up is closed by #303 landing. Everything else —
the no-merge rule, the container validation, the surface algebra, the caps,
the wire shape — is untouched and still describes the shipped behaviour.

---

## Problem

A package can declare what it contributes to a *process environment* (`env`,
`entrypoints`, `binaries`, `dependencies`) but has nowhere to carry structured
configuration for tools OCX does not model — an editor extension list, a
devcontainer block, a JetBrains plugin set, a language-server setting. Today a
publisher's only options are to fork the metadata format or to ship a
side-channel file consumers must know to look for.

The surveyed answer ([`research_package_integrations.md`](./research_package_integrations.md))
is unanimous: reserve a **name pattern**, refuse to validate inside it. Ten
ecosystems do exactly this (`devcontainer.json` `integrations`, Cargo
`[package.metadata.<tool>]`, PEP 621 `[tool.<name>]`, OCI `annotations`, k8s
annotations, Helm/Artifact Hub, CycloneDX `properties`, Nix `passthru`). The
one thing nobody got right — and the one thing OCX cannot punt on, because
package metadata is an interface-tier contract — is **merge semantics**.

---

## Decision Summary

Every row below is **ratified in #221** and is not reopened here. Rows marked
*(this ADR)* are the design work #221 delegated.

| # | Decision | Source |
|---|---|---|
| D1 | The field is named `integrations`, homed in the package metadata config blob (`Bundle`), not a referrer artifact or an OCI annotation. `extensions` was rejected — it collides head-on with `integrations.vscode.extensions` in the very first use case; `metadata` was rejected because `metadata.json` is the file | #221 |
| D2 | **No merge.** Contributions are never combined, ranked, or conflict-checked. Two packages declaring one namespace produce two rows; the consuming application adjudicates | #221 |
| D3 | **No validation of payload contents.** The payload is `serde_json::Value` — any JSON, of any shape | #221 |
| D4 | **Interface surface only.** `--self` composes zero integrations | #221 (mechanism: *this ADR*, §4) |
| D5 | **Map when authored, flat attributed rows when composed.** Authored: `{"<ns>": <payload>}`. Composed: `[{namespace, package, payload}]`, one row per (package, namespace) pair; the payload key is `payload` — **reversed 2026-08-11**, #221 had specified `value` | #221, reversed by the owner |
| D6 | Namespace keys follow **reverse-DNS by convention, documented not validated** | #221 (what *is* rejected: *this ADR*, §3) |
| D7 | **Same interpolation engine, same vocabulary, same closed world** — whatever token set the scanner recognises is what a payload gets: `${installPath}`, its exact alias `${self.installPath}`, `${deps.NAME.installPath}`, each taking an optional `:native` / `:posix` render modifier. Every other `${…}` is **refused**, never emitted verbatim. `$${…}` is the one way to put a literal `${…}` in a payload | #221 (vocabulary); **amended by #303** — the pass-through half is reversed, see the Amendment |
| D8 | Size cap enforced in `ValidMetadata::try_from`, exit 65 — **8 KiB per namespace, 32 KiB per package**, raise-only | #221 (accounting basis + boundary: *this ADR*, §3.3) |
| D9 | `carrier_crosses` needs **no change**; propagation is governed entirely by `Dependency.visibility` on the edge. D4 is a statement about which surface the field belongs to, not a visibility rule — which is why the type carries no `visibility` field and needs no default | #221 (arithmetic proof that no `Visibility` value could express it either: *this ADR*, §4.1) |
| D10 | **`$${…}` escapes to a literal `${…}`.** Doubling is the familiar spelling (shell, compose, make); backslashes are miserable inside JSON. Added to the shared engine, so it applies to env values and entrypoint `args` too — "by construction". Under #303 this is the **only** escape and the only way any surface emits a literal `${…}`; it shipped with the scanner, not with this feature | #221; escape semantics + landing site amended by #303 |
| D11 | Interpolation applies to **string values only**; object keys, numbers, booleans and nulls pass through verbatim. Structure is never re-parsed | *this ADR* |
| D12 | Shell completions are **a namespace** (`sh.ocx.completions`), not a first-class field. Rendering any consumer's format (`.vscode/settings.json`, completion files, devcontainer fragments) belongs in a plugin, not core | #221 |
| D13 | **Absent ≡ empty** (`#[serde(default, skip_serializing_if)]`), unlike `binaries`' deliberate `Option` tri-state | *this ADR* |
| D14 | **Patch companions contribute integrations exactly the way packages do** — patches are packages loaded into the environment, so no carrier gets a companion-specific rule. Reversed 2026-08-10; the original "contribute nothing" reading forbade the inert carrier while permitting `env`, the far stronger one. See C-017 | *this ADR* |
| D15 | Plain output: namespaces named in the existing hint line (flat) / a tree branch (closure); the **payload is never rendered in plain** | *this ADR* |
| D16 | `inspect --closure` keeps its own nested envelope and carries `{namespace, package}` **without** the payload — the split that already exists for `binaries`/`entrypoints`, not a third arrangement | #221 (shape: *this ADR*, C-016) |
| D17 | `--shell` / `--ci` carry no integrations — structurally, no new code | *this ADR* |
| D18 | **Never collapse the array when there is one root.** A single-package invocation emits the same array shape, attribution included even where redundant | #221 |
| D19 | Explicit non-goals: no project-tier `[integrations]` in `ocx.toml`; no `[package."<repo>"]` opt-in or opt-out knob; no namespace registry | #221 |
| D20 | **The shared-engine `$${…}` escape is accepted**, retroactive change to already-published `env` values and entrypoint `args` included — `$${installPath}` has no meaningful reading today, so nothing published can depend on the old behaviour. The registry sweep is optional confirmation, not a gate | owner, 2026-08-09 (§12) |
| D21 | **`${deps.*}` inside payloads IS validated** — a token naming an undeclared dependency is invalid metadata, not a payload opinion. Must be a **direct** dependency, never transitive. Enforced by the publish gate (`create` / `push`) — under #303's D14 token validation left `ValidMetadata::try_from`, so a read-only path no longer re-runs it | owner, 2026-08-09 (§12); gate site amended by #303 |
| D22 | **Native path separators by default on Windows.** No integrations-only normalization — the forward-slash need is served by the general render-modifier scheme, `${self.installPath:posix}`, which **shipped in #303**. Closed, not deferred | owner, 2026-08-09 (§12); **resolved by #303 landing** |

---

## Decision Drivers

- **The published read path is a one-way door.** Once a package ships a
  `integrations` block, every future ocx must keep resolving it
  (`CLAUDE.md` stability tiers: metadata read-path changes stay backward
  compatible). Anything rejected at read time can only ever be loosened.
- **The composer's surface algebra is a shared contract**, not a private
  helper. `composer::{dep_admitted, carrier_crosses}` are the one
  implementation `ocx env` and `ocx package inspect --closure` both route
  through, and an oracle test asserts they agree
  (`test/tests/test_package_inspect_closure.py:372-428`).
- **Backend-first** (`product-context.md` Principle 1): the machine-readable
  path is the product; plain is a glance.
- **A cap belongs in v1, not a follow-up** — Kubernetes hit the unbounded-blob
  wall and retrofitted a hard 256 KiB server-side cap
  ([`research_package_integrations.md`](./research_package_integrations.md) §2).
- **No new dependency, no new subsystem.** Every edit site already exists.

## Industry Context

**Research artifacts:** [`research_package_integrations.md`](./research_package_integrations.md),
[`research_interpolation_capability.md`](./research_interpolation_capability.md).

**Key insight:** the escape hatch is *syntactic*, never *schematic* — the format
owner recognizes the shape and refuses to look inside. *(Amendment qualifier: OCX
still refuses to look at what a payload **means**, but it does read one thing
inside — its own `${…}` sigil, which the Amendment reclassified from payload
content to container syntax. None of the surveyed systems interpolates into a
namespaced payload at all, so none of them faced this question.)* **Second key
insight:**
no surveyed system propagates a private dependency's declared attribute to a
top-level consumer by default (CMake `PUBLIC`/`PRIVATE`, Gradle
`api`/`implementation`, Bazel `exports`/`deps`, Nix `propagatedBuildInputs`).
OCX's `Visibility` algebra already implements the majority rule, so the edge
term of this feature needs no new machinery — only the carrier term does (§4).

---

## 1. Wire Format

### Authored (`metadata.json`, and the `-metadata.json` authoring sidecar)

```json
{
  "type": "bundle",
  "version": 1,
  "integrations": {
    "com.microsoft.vscode": {
      "extensions": ["rust-lang.rust-analyzer"],
      "settings": { "rust-analyzer.server.path": "${installPath}/bin/rust-analyzer" }
    },
    "com.jetbrains": { "plugins": ["com.jetbrains.rust"] }
  }
}
```

### Composed (`ocx --format json env`, `ocx --format json package env`)

```json
{
  "entries": [ … ],
  "binaries": [ … ],
  "entrypoints": [ … ],
  "integrations": [
    {
      "namespace": "com.microsoft.vscode",
      "package": "ocx.sh/rust:1.83@sha256:aaaa…",
      "payload": {
        "extensions": ["rust-lang.rust-analyzer"],
        "settings": { "rust-analyzer.server.path": "/home/u/.ocx/packages/…/content/bin/rust-analyzer" }
      }
    }
  ]
}
```

The map→rows transform (D5) is the whole point: **authored** form is keyed
because a publisher writes one block per vendor; **composed** form is a flat
ordered list because N packages may declare the same namespace and OCX refuses
to pick a winner (D2). A keyed composed object would force exactly the merge
decision #221 rejected. **One row per (package, namespace) pair** — array
length exceeding distinct-namespace count is the structural guarantee that
nothing merged.

The name imports one expectation OCX refuses, and the docs must defuse it in
the first sentence (#221): devcontainer's `integrations` **merges**; ours
concatenates.

---

## 2. Component Contracts

Numbered, implementation-free. A tester can write failing tests from these
without reading any implementation.

### C-001 — `Bundle.integrations`

`crates/ocx_lib/src/package/metadata/bundle.rs`, added to `Bundle` (currently
`bundle.rs:42-79`):

```rust
/// Vendor-namespaced configuration blocks for tools OCX does not model.
/// Keys are namespaces (reverse-DNS by convention, not enforced); values are
/// opaque JSON OCX never interprets, merges, or validates the contents of.
#[serde(default, skip_serializing_if = "Integrations::is_empty")]
pub integrations: Integrations,
```

Contract:
- Absent on the wire ⇒ deserializes to an empty `Integrations`.
- Empty ⇒ omitted on serialization. Absent and empty are the **same** state
  (D13 — deliberately *not* `binaries`' `Option` tri-state; see §7).
- The field is additive: `bundle::Version` stays `V1`.

### C-002 — `Integrations`

New file `crates/ocx_lib/src/package/metadata/integrations.rs` (one concept
per file, no `mod.rs` — `arch-principles.md` Code Style).

```rust
pub struct Integrations(std::collections::BTreeMap<String, serde_json::Value>);
```

- Derives `Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema`.
  **No custom `Deserialize`, no manual `JsonSchema`** (see §3.4 and §7.2).
- `fn is_empty(&self) -> bool`
- `fn iter(&self) -> impl Iterator<Item = (&str, &serde_json::Value)>` —
  yields in **lexicographic namespace order** (`BTreeMap`), deterministic
  across runs and platforms.
- `fn get(&self, namespace: &str) -> Option<&serde_json::Value>`
- **Schema contract:** the generated schema for this field MUST be
  `{"type":"object","additionalProperties":true}`. If the schemars derive
  drifts from that, the field carries
  `#[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]`
  rather than a hand-written `impl JsonSchema`. Pinned by an `ocx_schema` test
  in the shape of `metadata_schema_omits_the_unknown_modifier_fallback`.

### C-003 — `Metadata::integrations()`

`crates/ocx_lib/src/package/metadata.rs` (peer of `binaries()` at `:76-80`):

```rust
pub fn integrations(&self) -> &Integrations
```

Returns a reference, never `Option` — the empty collection is the "declares
none" answer (C-001). Mirrors `dependencies()` (`metadata.rs:41-45`), not
`binaries()`.

### C-004 — `AuthoringBundle.integrations`

`crates/ocx_lib/src/package/metadata/authoring.rs`: same field, same serde
attributes, on `AuthoringBundle` (`authoring.rs:62-102`); `to_published()`
(`authoring.rs:145-163`) copies it through with a plain `.clone()`. No
authoring-form delta — the sole authoring/published delta remains the optional
dependency digest.

### C-005 — Namespace key grammar

Rejected at `ValidMetadata::try_from` (C-007), never by serde. A namespace key
is refused iff **any** of:

| Rule | Rejects | Error |
|---|---|---|
| non-empty | `""` | `IntegrationNamespaceInvalid { namespace, reason: "empty" }` |
| ≤ 128 bytes | a 129-byte key | `… reason: "longer than 128 bytes"` |
| no control characters — **Unicode `char::is_control()`**, i.e. C0 `0x00`–`0x1F`, DEL `0x7F`, **and C1 `U+0080`–`U+009F`** | `"a\nb"`, `"a\tb"`, `"a\u{0085}b"` | `… reason: "contains a control character"` |
| **no invisible characters** — the union of general category `Cf` and the `Default_Ignorable_Code_Point` property. Neither contains the other: U+3164 HANGUL FILLER is `Lo` + default-ignorable and not `Cf`, while U+0600 ARABIC NUMBER SIGN is `Cf` and deliberately excluded from the property. U+2800 BRAILLE PATTERN BLANK is in neither and is knowingly accepted | `"com.evil\u{202E}txt.moc"`, `"com.microsoft\u{200B}.vscode"`, `"com.evil\u{3164}.vscode"` | `… reason: "contains an invisible character"` |
| no whitespace — **Unicode `char::is_whitespace()`**, not just ASCII | `"com.foo bar"`, `"com.foo\u{00A0}bar"` | `… reason: "contains whitespace"` |

Everything else is accepted. In particular `vscode`, `VSCode`, `com.微软`,
`a`, `x/y`, `123` are all **legal** — reverse-DNS is documented, not validated
(D6). Case is preserved and case-distinct keys are two distinct namespaces
(no case-fold-collision check, unlike `BinaryName`).

The error message names the namespace with `{:?}` (Rust debug quoting), for
the same reason `InvalidListSeparator` does: a key is refused precisely for
carrying something unprintable, and a raw newline would forge log lines
(CWE-117) and hide the offending byte.

**Why the character rules are Unicode-scoped, not ASCII-scoped** (security
review, 2026-08-09). A namespace key is printed verbatim into the plain-text
hint line (`api/data/env.rs:169-195`), so it is untrusted publisher data
reaching a terminal. An ASCII-only control rule leaves C1 controls and the
Unicode bidi-override set legal, letting a key *display* as a different
namespace than it is — Trojan Source, **CWE-451** (user-interface
misrepresentation). The JSON path fails closed on its own (a consumer matching
`com.microsoft.vscode` byte-for-byte never matches a spoofed key), so the
exposure is display-only and this is Warn-tier, not Block-tier.

The reason it is fixed **here rather than later** is R-2: this grammar sits on
the read path, so it can only ever be *loosened*. Adding a rejection after
publication is a tightening, which un-resolves already-published packages.
Costless now, impossible later.

Homoglyph confusability (`com.microsoft` with a Cyrillic `о`) is deliberately
**not** addressed — it is unbounded, and unlike bidi it degrades safely: an
exact-match consumer simply sees no matching namespace. Consistent with D6,
reverse-DNS documented not validated.

### C-006 — Size caps

`integrations.rs`:

```rust
/// Maximum compact-serialized size of one namespace's payload.
pub const MAX_INTEGRATION_NAMESPACE_BYTES: usize = 8 * 1024;
/// Maximum compact-serialized size of the whole `integrations` map.
pub const MAX_INTEGRATIONS_BYTES: usize = 32 * 1024;
```

**Accounting basis:** `serialized_len(value)` (`validation.rs:182-186`) — a
`ByteCounter` `std::io::Write` sink driven by `serde_json::to_writer`, so the
per-namespace payload size is measured without heap-allocating the document
just to throw it away. Compact re-serialization either way, so the
measurement is independent of the source document's whitespace and of key
ordering. The per-package total is **not** a second serialization pass over
the whole map — `validate_integrations` (`validation.rs:460-506`)
accumulates it incrementally, one namespace at a time: the outer `{}` braces,
then each namespace's `serialized_len(key) + ":".len() + serialized_len(payload)`
plus one byte for the comma before every entry but the first. The framing
arithmetic is hand-written but `serde_json`-derived at every leaf, and it is
pinned byte-for-byte against `serde_json`'s own measurement by the
boundary-pair differential tests
(`a_integrations_map_at_the_per_package_cap_boundary_is_accepted` /
`a_integrations_map_one_byte_over_the_per_package_cap_is_rejected`).

**Boundary:** inclusive. `len == CAP` passes; `len > CAP` fails. Both bounds
are checked; the per-namespace error is reported first (declaration order —
`BTreeMap` iteration, so lexicographic and reproducible).

**Raise-only.** These constants sit on the *read* path (C-007), so lowering
either would un-resolve an already-published package — forbidden by the
metadata read-path compatibility rule. Starting low is the safe direction: a
package that could not publish was never published, so raising costs nothing.

### C-007 — `validate_integrations` (container) and `validate_integration_tokens` (publish)

`crates/ocx_lib/src/package/metadata/validation.rs`. #303's D14 split token
validation off the ingress chain, so the two halves land in two different
functions on two different paths:

```rust
// ValidMetadata::try_from — every ingress path, structural readability only
validate_env_modifier_types(&metadata)?;
validate_env_list_entries(&metadata)?;
validate_integrations(&metadata)?;          // container: keys + caps

// validate_for_publish — `ocx package create` / `push` only
validate_env_tokens(&valid)?;
validate_entrypoint_args(&valid)?;
validate_integration_tokens(&valid)?;       // tokens: grammar + refs
```

Ordering rationale, unchanged and applied twice: an integrations fault is
publisher hygiene and must not shadow a fault the *reader* cannot work around
(wrong ocx version, unfoldable list) or a fault on the env/entrypoint surfaces
the package actually runs on. Both integrations steps therefore run last in
their own chain.

`fn validate_integrations(metadata: &Metadata) -> Result<(), crate::Error>`
performs, in order, per namespace in `BTreeMap` order: key grammar (C-005),
then the per-namespace cap (C-006); then the per-package cap (C-006) once.

`fn validate_integration_tokens(metadata: &Metadata) -> Result<(), crate::Error>`
scans every **string leaf** of every payload through the shared
`scanner::scan` and refuses, per leaf:

1. any `${…}` the scanner does not parse into one of the four recognised bodies
   — the closed-world rule (#303 D3), the same refusal an env value gets;
2. any token class this surface does not permit — `AllowedTokens { deps: true,
   self_env: false }`;
3. a `${deps.NAME.installPath}` naming an undeclared or ambiguous dependency,
   via the same `build_name_and_collision_maps` helper `validate_env_tokens`
   uses — direct dependencies only, never transitive.

Step 3 is an **interpretation** of #221's "no validation", flagged as such: it
validates OCX's *own* tokens, which #221 folds into "resolves its own
interpolation tokens inside it", not the payload. Without it a `${deps.typo}`
publishes clean and then hard-fails `ocx env` at compose time with
`UnknownDependencyRef` — a footgun the identical check already prevents for
env values. See D21 (resolved: yes, checked).

Step 1 is what the Amendment reversed. This ADR originally specified the
opposite — *"it does NOT call `first_unknown_placeholder`; that omission **is**
the no-validation rule"* — on the reading that a foreign token is payload
content OCX must not touch. Under #303 a `${…}` is OCX's sigil wherever it
appears, so the payload gets no exemption; `first_unknown_placeholder` and
`UNKNOWN_TOKEN_RE` were deleted outright and there is no per-surface
unknown-token policy left to opt out of.

Pure syntax: no filesystem access, no network, no dependency resolution.

### C-008 — Interpolation capability: the default resolver, minus `self.env`

**No new `Usage` variant and no `template.rs` edit for this feature.** D7 is
explicit: integrations are one more consumer of the shared engine, not a
dialect. The resolver is built with plain `TemplateResolver::new(content_path,
&dep_contexts)`, whose default is `Usage::Environment`, so the compose-side call
sites need no `.usage(…)` call at all.

The publish gate is where the one narrowing lives: `AllowedTokens { deps: true,
self_env: false }`, spelled as a struct literal rather than minted as a
`Usage::Integrations` variant, because that single `bool` is the whole of the
difference from `Usage::Environment`.

- `${deps.*}` **resolves** to the named dependency's install path, subject to
  the same `EnvResolver`-time guarantees env values get. This is the point of
  `"C_Cpp.default.compilerPath": "${installPath}/bin/clang"` — a digest-derived
  path no human can hand-write.
- `${self.env.KEY}` is **refused at publish** as `DisallowedToken`. A payload is
  resolved by a `TemplateResolver` built without `with_self_env`, so the token
  names an empty scope and would exit 65 on *every* consumer — publishable and
  unusable. Refusing is also the reversible direction: the accept set may only
  grow (R-2's asymmetry, applied to the token grammar).

### C-009b — `$${…}` escape (D10) — **shipped in #303, not here**

`$${…}` yields a literal `${…}`; the inner token is not substituted, and the
emitted `${` is output, never rescanned. It lives in the scanner
(`metadata/template/scanner.rs`, rule R1), inseparable from token recognition:
the escape is only expressible in a left-to-right scan that examines the `$`
position *before* the `${` branch, which is why a pre/post `replace` cannot
implement it and why `$$${installPath}` (→ `$` + literal `${installPath}`) is
unambiguous.

Two facts this ADR originally recorded as work items are now history rather
than instruction:

- The engine had **no escape** before #303 — `$${installPath}` resolved to
  `$<path>`. That resolution changed for already-published `env` values and
  entrypoint `args` when the scanner landed. See R-3 and D20 (resolved:
  accepted).
- `first_unknown_placeholder` and `UNKNOWN_TOKEN_RE` did not "learn the rule" —
  they were **deleted**, along with `disallowed_dep_token` and
  `DEP_TOKEN_PATTERN`. One recogniser now serves the resolver, both publish
  gates, `classify_install_path_rooted_dir` and `libc_lint`, so the escape
  cannot be understood differently by two of them.

Under the closed grammar this escape carries more weight than it did when D10
was ratified: it is not a corner-case affordance, it is the **only** way a
payload puts a literal `${…}` in front of its consumer.

### C-009 — The interpolation walker

`integrations.rs`:

```rust
/// Resolves the engine's tokens in every string LEAF of `payload`, recursively.
///
/// Object keys, numbers, booleans and nulls pass through untouched. The
/// result is built by in-place substitution into `Value::String` leaves —
/// the output is never re-parsed as JSON, so the payload's structure is
/// invariant under interpolation.
fn interpolate(value: &serde_json::Value, resolver: &TemplateResolver<'_>)
    -> Result<serde_json::Value, TemplateError>;

/// Resolves every namespace's payload for one package.
pub fn resolve(&self, resolver: &TemplateResolver<'_>)
    -> Result<Vec<IntegrationEntry>, crate::Error>;
```

Positions, exhaustively:

| JSON position | Interpolated? |
|---|---|
| string value at any depth (object value, array element, top level) | **yes** |
| object **key** | **no** — verbatim (D11) |
| number, boolean, null | **no** |
| the namespace key itself | **no** |

`resolve` wraps a `TemplateError` in
`Error::IntegrationInterpolation { namespace, source }` (C-019) so the
message names the offending namespace, exactly as `EnvVarInterpolation` names
the var key.

**Reuse check** (`quality-core.md` "Don't Own Non-Domain Code",
`arch-principles.md` Utility Catalog): no existing helper walks
`serde_json::Value` string leaves; `serde_json` ships none; the walk is ~15
lines with no edge cases (rung 6 of the ladder). No dependency is added.

### C-010 — `IntegrationEntry`

`integrations.rs`:

```rust
/// One resolved integration contribution: the namespace and its
/// interpolated payload. Attribution to the declaring package is carried by
/// the pair this appears in, not by the struct — the same shape
/// `inspect::Surface::env` uses for `ClosureEnvVar`.
#[derive(Debug, Clone)]
pub struct IntegrationEntry {
    pub namespace: String,
    pub value: serde_json::Value,
}
```

**Type-economy check** (`feedback_type_economy_reuse_structs`): no existing
type fits. `BinaryAttribution::from_pairs` (`api/data/env.rs:108-117`) projects
`(PinnedIdentifier, T: Display)` and has no payload slot; extending it with an
`Option<Value>` would leave a permanently-`None` field on every `binaries` and
`entrypoints` row. `ClosureEnvVar` is the precedent for a payload-carrying
attributed pair and is the shape copied here.

### C-011 — `composer::integrations_cross`

`crates/ocx_lib/src/package_manager/composer.rs`, beside `carrier_crosses`
(`composer.rs:161-171`):

```rust
/// Whether the integrations carrier crosses onto the requested surface.
///
/// Interface surface only, at EVERY depth: `--self` composes zero
/// integrations. This is a SURFACE-LEVEL rule, not a visibility one — no
/// `Visibility` value produces it under `carrier_crosses` (proof: ADR
/// `adr_package_integrations.md` §4.1). Homed here, beside the algebra it
/// deviates from, so `compose` and `inspect::project_surface` share the one
/// implementation the surface contract requires.
pub(crate) fn integrations_cross(self_view: bool) -> bool {
    !self_view
}
```

The **edge** term is unchanged and stays algebraic: a dependency contributes
integrations iff `dep_admitted(effective, /* self_view = */ false)` — i.e.
`effective.has_interface()`. Only the **carrier** term is structural.

Takes no `is_root`: the answer is the same at every depth, and a parameter the
body ignores is a lie about the rule.

### C-012 — `ComposeOutput.admitted_integrations`

`composer.rs`, added to `ComposeOutput` (`composer.rs:65-94`):

```rust
/// Declared `integrations` contributed by each admitted identifier.
///
/// Interface surface only — always empty when `self_view == true`
/// (`integrations_cross`). Payloads are interpolated with the DECLARING
/// package's own `${installPath}`. Ordered: each root's admitted deps in
/// topological order, then the root; cross-root dedup applies, so a shared
/// dep contributes once. Within one package, lexicographic by namespace.
pub admitted_integrations: Vec<(oci::PinnedIdentifier, IntegrationEntry)>,
```

Collection sites, mirroring `admitted_binaries` exactly:
- dep side, `composer.rs:329-338`, using the already-loaded `meta` and
  `dep_content` — **zero extra I/O**;
- root side, `composer.rs:375-388`, using `root.metadata()` and
  `root.dir().content()`.

Both reuse the `dep_contexts` map `build_dep_context_map` already builds
(`composer.rs:344`, `:392`), so the `${deps.*}` capability gate fires against a
real map — the gate, not an empty map, is the safety mechanism (the
gate-before-regex proof, `template.rs:647-665`).

### C-013 — `AdmittedBinaries` → `AdmittedClaims`

`crates/ocx_lib/src/package_manager/tasks/resolve.rs:413-424`. **Rename in
place**, no alias, no re-export (`CLAUDE.md`: internal structure has no
stability):

```rust
pub struct AdmittedClaims {
    pub binaries: Vec<(oci::PinnedIdentifier, BinaryName)>,
    pub entrypoints: Vec<(oci::PinnedIdentifier, EntrypointName)>,
    pub integrations: Vec<(oci::PinnedIdentifier, IntegrationEntry)>,
}
```

`resolve_env_with_attribution` (`resolve.rs:768-782`) keeps its signature
shape, returning `AdmittedClaims` in the 4-tuple's last slot.

### C-014 — CLI envelope

`crates/ocx_cli/src/api/data/env.rs`:

```rust
/// One admitted integration contribution, attributed to the declaring
/// package. `payload` is the interpolated payload — arbitrary JSON OCX does not
/// interpret. `package` is `Option` for the same reason
/// `BinaryAttribution::package` is: `None` means "attribution unknown", never
/// "no payload".
#[derive(Serialize)]
pub struct IntegrationAttribution {
    pub namespace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub value: serde_json::Value,
}

impl IntegrationAttribution {
    pub fn from_pairs(pairs: &[(ocx_lib::oci::PinnedIdentifier, IntegrationEntry)]) -> Vec<Self>;
}
```

`EnvVars` (`api/data/env.rs:139-154`) gains
`pub integrations: Vec<IntegrationAttribution>` as a **fourth top-level
sibling array** — never nested inside `entries` — and `EnvVars::new` takes a
fourth argument. Present as `[]` when empty, never omitted (the
`binaries`/`entrypoints` rule, pinned by
`envelope_binaries_and_entrypoints_present_as_empty_arrays_never_omitted`).

**Tier parity is free**: `EnvVars` is literally one shared type, constructed
by both `command/toolchain_env.rs:416-418` and `command/env.rs:206-208`.
`ocx env` and `ocx package env` therefore gain the array in one edit —
required by `feedback_no_subcommand_format_divergence`.

Field name is `namespace`, not `name` (#221): two of the three sibling arrays
are name claims that resolve on `PATH`; an integration row is a keyed payload.
Key order matches #221's worked example — `namespace`, `package`, `payload`.

**One row per (package, namespace) pair, never one row per namespace.** Array
length exceeding distinct-namespace count is the structural guarantee that
nothing merged (D2). And the array is **never collapsed for a single root**
(D18): `ocx package env clang` emits the same shape with the same attribution,
so `[0]` is never a working shortcut and the one idiom that works —
`[.integrations[] | select(.namespace=="…")]` — is the only one anyone
learns. Tier parity is a requirement: that exact `jq` line must work
unchanged against both `ocx env` and `ocx package env`.

### C-015 — Plain hint line

`binaries_hint` (`api/data/env.rs:169-195`) is renamed `availability_hint` and
gains a third `summarize_claims`-style clause. Output shape:

```
5 binaries available (cmake, ctest, cpack, ...); 2 integration namespaces \
(com.jetbrains, com.microsoft.vscode); use --format json for the full list
```

- Clause order: binaries, entrypoints, integrations, then the trailing
  `use --format json for the full list`.
- Same `HINT_NAME_PREVIEW = 3` truncation and singular/plural handling;
  singular label `integration namespace`, plural `integration namespaces`.
- **Payloads never appear.** Namespace keys only.
- Hint fires when any of the three claim kinds is non-empty
  (`api/data/env.rs:236-238`).
- The **static** wording — clause labels, punctuation, the trailing
  `use --format json for the full list` suffix — is ASCII (help-text ASCII
  gate, `hint_static_portions_are_ascii_even_when_a_namespace_is_not`, checked
against the fixed template). A publisher's
  own namespace key is not subject to that gate: it renders verbatim inside
  the parenthesized list, and `com.微软` is legal (C-005) and prints exactly
  as declared.

The `entries` table (Key | Type | Value [| Source]) is **byte-stable**: no new
column. A column would misrepresent a dataset with no natural per-entry-row
mapping — the identical reasoning `adr_declared_binaries_metadata.md` Decision
C already applied.

### C-016 — `inspect --closure`

- `ClosureNode` (`tasks/inspect.rs:177-197`) gains
  `pub integrations: Vec<String>` — the node's declared **namespace keys**,
  in `BTreeMap` order. No payloads: a closure node is not installed, so
  `${installPath}` has no value and the payload would be a half-truth (the
  same reason `Surface::env` omits env values, `inspect.rs:139-142`).
- `Surface` (`tasks/inspect.rs:132-146`) gains
  `pub integrations: Vec<(oci::PinnedIdentifier, String)>`.
- `project_surface` (`inspect.rs:601-637`) collects them under
  `composer::integrations_cross(self_view)` — the same shared predicate
  `compose` uses, satisfying the "inspect never re-derives" contract
  (`composer.rs:122-127`).
- `SurfaceOut` (`api/data/package_inspect.rs:182-198`) gains
  `integrations: Vec<NamespaceAttribution>` — a dedicated type, not a reuse
  of `BinaryAttribution`. `NamespaceAttribution { namespace, package }`
  (`api/data/package_inspect.rs:241-246`) is the payload-free sibling of the
  flat envelope's `IntegrationAttribution` (C-014): same `namespace` key
  spelling, same `Option<String>` `package`, no `payload`. `binaries` and
  `entrypoints` keep `BinaryAttribution` and its `name` key on this same
  struct — the split is deliberate: `name`/`BinaryAttribution` is a
  PATH-resolving claim, `namespace`/`NamespaceAttribution` is a keyed payload
  declaration, and keeping the two shapes apart is what stops a consumer's
  `select(.namespace=="…")` filter from ever matching a binary row by
  accident.
- `NamespaceAttribution::from_pairs` (`api/data/package_inspect.rs:248-260`)
  projects `(PinnedIdentifier, String)` pairs the same way
  `BinaryAttribution::from_pairs` does — the payload-free sibling of
  `IntegrationAttribution::from_pairs`.
- `surface_node` (`api/data/package_inspect.rs:1015-1042`) gains a
  `integrations` branch of `namespace_attribution_leaf`s
  (`api/data/package_inspect.rs:1055-1057`), emitted only when non-empty,
  after `env`.
- `ClosureDepOut` (`api/data/package_inspect.rs:132-153`) — the wire
  projection of one non-root `ClosureNode` under `closure.deps[]` — gains
  `integrations: Vec<String>`, the same namespace-key list verbatim
  (`closure_dep_out` copies `node.integrations` through with no
  transform). Always present, `[]` when the dep declares none — matching
  `ClosureNode`'s own absent/empty equivalence, unlike `binaries`'s
  `Option` tri-state on this same struct.

**Oracle contract.** `test/tests/test_package_inspect_closure.py:372-428`
asserts the closure surface equals the flat `ocx env` / `ocx env --self`
output — **absent an active patch tier**. `inspect --closure` has no companion
path, so once a site's `[patches]` tier admits a companion, `env.integrations`
gains rows the closure has no way to produce; the two surfaces legitimately
diverge from that point on (C-017). The projection compared for integrations
is the **(namespace, package) set** — payloads are out of scope on both sides,
because the closure never carries one. With no patch tier active, both sides
must agree:
- interface: `closure.surface.interface.integrations` ≡
  `{(row.namespace, row.package) for row in env.integrations}`
- private: `closure.surface.private.integrations` ≡ `∅` ≡ the `--self`
  array (D4).

### C-017 — Patch companions

**A companion contributes its `integrations` exactly the way a package does.**
`resolve.rs`'s companion compose reads both `out.entries` and
`out.admitted_integrations`; the pairs join the base packages' on the same
`AdmittedClaims.integrations` array, attributed to the **companion's** own
`PinnedIdentifier`.

Rationale: patches are just packages loaded into the environment. Giving one
carrier a companion-specific rule is the exception, not the safeguard — the
simpler and more predictable end state is that a companion has no exceptional
rules at all.

`integrations_cross(self_view)` is called at exactly two sites:
`composer::compose` (`composer.rs:234`), which evaluates it fresh from the
surface's own `self_view` for the base roots, and
`resolve_env_with_attribution` (`resolve.rs:845`), which evaluates it once for
the whole companion overlay. Neither `compose_gated`'s dep/root collection
loops nor `compose_companion` call it again — both receive the already-computed
boolean as a plain parameter and gate their own collection with it. It is a
property of the carrier (interface-surface-only, at every depth — §4.1), not
of who declared the payload, so `ocx env --self` carries no integrations
from base or companion alike.

That parity does **not** come from the same mechanism `env` uses to reach it.
`env`'s two-axis `Visibility` carrier explains why a *package's own*
dependency crosses or does not cross under `self_view` — but a patch companion
is not a dependency edge. `resolve.rs`'s companion overlay is an unconditional
post-composition append of the companion's already-composed interface
projection onto the base's env; the append loop carries no `self_view` term of
its own, so a companion's `env` sits outside the surface algebra before
`integrations_cross` ever runs. `integrations_cross` is the only rule the
two carriers share, and it is **contributor-blind**: the same predicate fires
whether the contributor is the base package or the companion, which is what
makes a companion-specific branch unnecessary — not, as this ADR originally
argued, an appeal to `env`'s own visibility algebra.

**Implementation: the gate is threaded into the caller, not applied after the
fact.** `integrations_cross(self_view)` is not a filter run over an
already-resolved set — it is threaded INTO `build_site_patch_set` and
`composer::compose_companion`, so a companion's `integrations` payload is
never resolved at all when the surface in play would discard it. Both
`compose` and `compose_companion` route through the private
`composer::compose_gated(roots, store, self_view, collect_integrations)`
(`composer.rs:282-287`) — the one function that actually collects
`admitted_integrations`, at the dep loop (`composer.rs:445-460`) and the
root loop (`composer.rs:517-529`), each gated by a plain
`if collect_integrations { … }` rather than a second call to
`integrations_cross`. The suppression input changes only what is COMPUTED,
never what the surface CONTAINS when the gate is on: `compose_companion` pins
`self_view = false` and passes `collect_integrations` straight through
unevaluated, so with the gate on, its output is byte-identical to what
`compose` alone would produce for the same single root. This matters beyond
avoiding wasted work: resolution asserts that every `${deps.*}` token names a
dependency whose content directory actually exists, and without the gate
threaded in, a `required` companion whose integrations reference a
dependency not materialized on `--self` — a surface those integrations were
never going to reach — would hard-error the whole composition over a value
nobody asked for. Gating before resolving is what keeps a required companion
usable on a surface its integrations never cross.

**Dedup: `(PinnedIdentifier, namespace)`, across the whole composition.** Base
packages and companions are folded in by separate `compose` calls, and each
call dedups only within itself, so a second pass is needed: rows are deduped
by `(PinnedIdentifier, namespace)` **across the whole composition**, seeded
from the base's own rows before the companions' are folded in. A companion
matched against several bases, or one that happens to declare a namespace a
base dependency already contributed under the same identifier, collapses onto
the seeded row instead of appearing twice.

`admitted_binaries` / `admitted_entrypoints` stay discarded, and that is not an
inconsistency: both are PATH-shaped claims about executables the companion
overlay never puts on PATH, so admitting them would advertise binaries no
consumer can reach.

**Reversal (2026-08-10).** The original decision discarded a companion's
payloads, reasoning that a `system_required` companion is installed by site
policy with no project consent surface (`--no-patches` does not reach a
required companion), so injecting IDE configuration would be policy injection
with no opt-out.

That argument does not survive contact with the carrier it permits. A
companion's `env` already lands in `ocx env` and changes how every process in
the shell behaves; an integrations payload is inert JSON that does nothing
until a consumer goes looking for its namespace. The rule forbade the weaker
carrier and allowed the stronger one. And the actor it guarded against — the
site admin — already controls `PATH`, the patch set, the companion packages
and the `[managed]` config tier, so there is no coherent line to draw inside
that trust boundary.

**No opt-out ships, and none is needed.** An *optional* companion is already
droppable: `no-patches` removes it wholesale, `env` and `integrations`
together. A *required* companion is required, on the same terms its `env`
already is. And a site admin who does not want to inject configuration simply
does not declare `integrations` in the companion package — the companion and
the descriptor are authored by the same policy, so there is no third party to
protect.

One case is deliberately left uncovered: a package published for ordinary
consumption, carrying its own legitimate `integrations`, that a site admin
also wires up as a companion for its `env` alone. Its payload rides along. The
convention already points away from that shape — companions are env-only,
binary-free, `any`-platform packages — and the fix is to publish a
companion-specific package, not a config flag. Revisit if it appears in
practice.

### C-018 — `--shell` / `--ci` exclusion

**No code.** Both branches `return` before `EnvVars` is constructed
(`toolchain_env.rs:360-373`, `env.rs:154-164`), so integrations are
structurally absent from every eval-safe and CI-sink emission. A shell export
line and a `$GITHUB_ENV` row have no representation for a JSON document, and
inventing one (a serialized blob in an env var) would be a second wire format
for the same data. Contract to test: `--shell=bash` and `--ci=gitlab` output
contains no namespace key from any composed package.

### C-019 — Error variants

`crates/ocx_lib/src/package/error.rs`, appended to `Error` (`:14-105`), with
`ClassifyExitCode` arms (`:88-105`). Messages follow `C-GOOD-ERR`: lowercase,
no trailing punctuation.

```rust
/// An integrations namespace key violates the key grammar.
#[error("integrations namespace {namespace:?} is invalid: {reason}")]
IntegrationNamespaceInvalid { namespace: String, reason: &'static str },

/// An integrations payload exceeds its size cap.
#[error("integrations payload for {namespace:?} is {size} bytes, over the {max}-byte limit")]
IntegrationTooLarge { namespace: String, size: usize, max: usize },

/// The whole integrations map exceeds the per-package size cap.
#[error("integrations total {size} bytes, over the {max}-byte per-package limit")]
IntegrationsTooLarge { size: usize, max: usize },

/// Integrations payload template interpolation failed.
#[error("integrations namespace {namespace:?} {source}")]
IntegrationInterpolation {
    namespace: String,
    #[source]
    source: super::metadata::template::TemplateError,
},
```

Classification and remediation:

| Variant | Exit code | Where it fires | Remediation the user acts on |
|---|---|---|---|
| `IntegrationNamespaceInvalid` | `DataError` (65) | every ingress path — publish (`create`/`push`) *and* pull/inspect (it is a container check, C-007) | Rename the namespace: non-empty, ≤128 bytes, no whitespace, no control characters. Reverse-DNS (`com.vendor.tool`) is the convention. |
| `IntegrationTooLarge` | `DataError` (65) | same | Shrink that namespace's payload below 8192 bytes, or move bulk data into the package content tree and reference it via `${installPath}`. |
| `IntegrationsTooLarge` | `DataError` (65) | same | Drop or shrink namespaces until the whole map compacts below 32768 bytes. The per-namespace error fires first, so this one means the *total* is the problem. |
| `IntegrationInterpolation` → `UnknownToken` | `DataError` (65) | publish (C-007 token step), and again at compose | The token is not one OCX recognises. Fix the spelling, or — if the bytes are meant for a downstream consumer — double the dollar: `$${workspaceFolder}`. The message carries one of #303's three hints: a suggested root for a near miss (`${installpath}` → `installPath`), the escape hint for a plainly foreign token, or the supported-body list when the root was recognised but the body was not. |
| `IntegrationInterpolation` → `DisallowedToken` | `DataError` (65) | publish only | `${self.env.KEY}` has no scope in a payload (C-008). Move the value into the token itself, or into an env var the consumer reads. |
| `IntegrationInterpolation` → `UnknownDependencyRef` | `DataError` (65) | publish (C-007 token step), and again at compose | Declare the dependency, or fix the name. The error lists the declared names. |
| `IntegrationInterpolation` → `AmbiguousDependencyRef` | `DataError` (65) | same | Set `name` on one of the two colliding dependencies to disambiguate. |
| `IntegrationInterpolation` → `UnknownField` | `DataError` (65) | same | Only `installPath` is supported today. |
| `IntegrationInterpolation` → `UnknownModifier` | `DataError` (65) | same | Only `:native` and `:posix` exist, and only on the three install-path bodies. |
| `IntegrationInterpolation` → `DependencyNotInstalled` | `NotFound` (79) | compose only | The named dependency is not materialized. `ocx pull` / re-install. Same behavior an `env` value with the same token already has. |

Every classification traces to `TemplateError`'s own `ClassifyExitCode` impl —
no new exit code, no new classification rule.

**Amended by #303.** `UnknownPlaceholder` was renamed `UnknownToken` and its job
changed: it is no longer a defence-in-depth artefact of an install path that
contains `${` (that branch was deleted as structurally unnecessary once the
scanner stopped re-examining output bytes) — it is now the ordinary refusal
every unrecognised token gets, and it fires at publish, not only at compose.
`UnknownDependencyField` generalised to `UnknownField`. `DisallowedToken` and
`UnknownModifier` are reachable from this surface where they previously were
not.

### C-020 — Derived surfaces

Per `subsystem-metadata-schema.md` and `feedback_plans_must_list_docs`, one
commit must also carry:

| Surface | Action |
|---|---|
| `website/src/public/schemas/metadata/v1.json` | regenerate — `task schema:generate` |
| `website/src/docs/reference/metadata.md` | new `integrations` section. **First sentence must defuse the imported expectation** (#221): devcontainer's `integrations` merges, ours concatenates. Then: field table, namespace convention (reverse-DNS, not enforced), caps (raise-only), "OCX never interprets this". **Second expectation to defuse, added by the Amendment**: a payload's `${…}` is OCX's, not the consumer's — a pasted devcontainer or VS Code block needs every token doubled (`$${workspaceFolder}`), and the reference must show the doubled form in every worked example, including the Windows `${self.installPath:posix}` one. Token grammar itself is documented once, in the interpolation reference — not restated per carrier |
| `website/src/docs/reference/env-composition.md` | the composed `integrations` array, its ordering, the no-merge rule, interface-surface-only |
| `website/src/docs/reference/command-line.md` | `ocx env` / `ocx package env` JSON envelope gains a fourth array; `package inspect --closure` surface gains `integrations` |
| `.claude/rules/subsystem-package.md` | Module Map row for `metadata/integrations.rs`; Metadata Schema tree |
| `.claude/rules/subsystem-package-manager.md` | `composer.rs` row: the `integrations_cross` deviation; `ComposeOutput` / `AdmittedClaims` shape |
| `.claude/rules/subsystem-metadata-schema.md` | note that `Integrations` needs **no** manual `JsonSchema` and why |
| `.claude/rules/subsystem-cli-commands.md` | `env` / `package env` / `package inspect` rows |
| `.claude/rules/product-context.md` | Differentiator #8 extended — integrations composes vendor-namespaced, non-env configuration across the closure, not only `env` |
| `CHANGELOG.md` | ⛔ **never** — the entry is the commit subject |

---

## 3. Open Points Resolved Here

### 3.1 What "no validation" does and does not cover

OCX validates the **container**, never the **contents**. The container is the
namespace key (C-005), the map's total size (C-006), and — since #303 — **every
`${…}` sequence inside a string leaf**, not merely the ones OCX recognises
(C-007; see D21 and the Amendment). Everything else inside a payload — shape,
types, semantics, whether a devcontainer block is well-formed — is the consuming
application's business, exactly as `devcontainer.json` (schema literally
`{"type":"object"}`), Cargo `[package.metadata.*]` (exempted from the
unused-key warning by name), and OCI annotations ("consumers MUST NOT error on
unknown key") all do.

**Where the line moved.** `${…}` was reclassified from *payload content* to
*container syntax*. The `${` sequence is OCX's own sigil wherever it appears in
package metadata, so reading it is not reading the payload's meaning — it is
reading OCX's own vocabulary embedded in it. A publisher who needs those bytes
to reach the consuming application writes `$${…}` and gets them byte-identical.
The payload's *meaning* is still never inspected: OCX does not know or care that
`extensions` is a VS Code array or that `C_Cpp.default.includePath` is a path
list.

This is not a hedge on D3: a key OCX itself prints in a hint line, uses as a
map key, and echoes into JSON *is* external input at a trust boundary, and
"validate external input at system boundaries" is a Block-tier universal rule
(`quality-core.md`).

### 3.2 A non-object payload is legal

`"com.example": "hello"`, `"com.example": [1, 2]`, `"com.example": null` and
`"com.example": 42` all parse and compose. Refusing them would be validating
the contents. A bare string payload is a single string leaf and is
interpolated like any other (C-009).

### 3.3 Cap accounting and the boundary

See C-006. Compact re-serialization, inclusive boundary, per-namespace
reported before per-package, `BTreeMap` order so the named offender is
reproducible.

Sanity against the existing budget: the whole metadata config blob is already
capped at 4 MiB (`MAX_METADATA_BLOB_BYTES`,
`package_manager/tasks/common.rs:134`). 32 KiB is 0.8% of that — deliberately
snug, because raising is free and lowering is impossible (C-006).

### 3.4 Duplicate namespace keys on the wire — last-wins, deliberately

#221 states duplicates are "structurally impossible". That holds for the Rust
type and for any JSON *emitter*, but not for hand-written JSON on the wire:
`{"com.foo": {"a":1}, "com.foo": {"b":2}}` parses, and `serde_json`'s
`BTreeMap` path is last-wins — it deserializes to `{"b":2}`. This ADR closes
that gap by **accepting last-wins** and does **not** add a custom `MapAccess`
`Deserialize`.

**This diverges from the `Entrypoints` precedent**, which does add one
precisely because "serde_json last-wins default is unsafe for registry data"
(`metadata/entrypoint.rs`). The divergence is named, not smoothed over:

- For `Entrypoints` the harm is concrete — a launcher name silently binds to a
  different target, and a collision is something OCX itself acts on.
- Here OCX never interprets the payload, so it can neither detect a meaningful
  conflict nor act on one. Every JSON reader on the consuming side (VS Code,
  a JetBrains backend) applies the same last-wins rule to the same bytes, so
  rejecting would make OCX stricter than the format's own consumers.
- The cost is not free: a custom `Deserialize` forfeits the plain schemars
  derive (C-002) and forces a hand-written `JsonSchema` — a manual write
  contract for a case no JSON emitter produces.

Escape hatch if this proves wrong: a **publisher-side** lint in
`ocx package create`, never a read-path rejection. The read path must stay
lenient for already-published artifacts.

---

## 4. Interface-Surface-Only: The Mechanism

#221 (D9) already settles the shape: `carrier_crosses` needs **no change**,
propagation is governed entirely by `Dependency.visibility` on the edge, and
"interface surface only" is a statement about which surface the field belongs
to rather than a visibility rule — which is why the type carries no
`visibility` field and needs no default. This section supplies the arithmetic
behind that and names where the rule lives.

### 4.1 Proof that no `Visibility` value could have expressed D4 either

The desired truth table for the integrations carrier:

| node | surface | want |
|---|---|---|
| root | interface | **yes** |
| root | private (`--self`) | **no** |
| dep (interface edge) | interface | **yes** |
| dep (interface edge) | private (`--self`) | **no** |

`carrier_crosses(vis, is_root, self_view)` (`composer.rs:161-171`) is:

```
is_root  → self_view ? vis.has_private() : vis.has_interface()
!is_root → vis.has_interface()                    // regardless of self_view
```

Evaluating all four `Visibility` constants against the four cells:

| constant | root/iface | root/self | dep/iface | dep/self | matches? |
|---|---|---|---|---|---|
| `PUBLIC` {p:t, i:t} | yes | **yes** ✗ | yes | **yes** ✗ | no |
| `INTERFACE` {p:f, i:t} | yes | no | yes | **yes** ✗ | no |
| `PRIVATE` {p:t, i:f} | **no** ✗ | yes | **no** ✗ | no | no |
| `SEALED` {p:f, i:f} | **no** ✗ | no | **no** ✗ | no | no |

**No constant matches.** The dep branch is `has_interface()` *regardless of*
`self_view`, so any value that reaches the interface surface at all also
reaches the private surface through a dep edge. That asymmetry is intentional
and correct for entrypoints — a parent invokes a dependency through its
launchers even in the parent's own runtime view — and it is exactly wrong for
a payload whose whole purpose is consumer-facing.

D4 is therefore **not expressible in the visibility algebra** — which is
exactly why #221 declines to give the type a `visibility` field. It is a
surface-level rule (D9, C-011).

`ocx launcher exec` forces `self_view = true`, so launcher composition carries
no integrations either — that falls out of `!self_view`, no special case.

### 4.2 Why not change `carrier_crosses`

Changing the dep branch to consult `self_view` would change the behavior of
every existing carrier — a dependency's entrypoints and interface env vars
would disappear from `ocx env --self`. That is a behavior break on a shipped
CLI contract, pinned by
`compose_root_entrypoints_are_interface_only` (`composer.rs:1090-1128`) and by
the closure oracle. Rejected.

### 4.3 Why the rule is homed in `composer.rs`

`subsystem-package-manager.md` states that inspect "MUST route every admission
/ crossing decision through" the shared predicates and "never re-derive". A
bare `if self_view { … }` in both `compose` and `project_surface` would be
re-derivation — two copies of one rule, free to drift. `integrations_cross`
is trivial (`!self_view`) and is a named, shared, documented function anyway,
for exactly that reason.

### 4.4 What still routes through the algebra

Only the *carrier* term is structural. The *edge* term is untouched: a
dependency contributes integrations iff `dep_admitted(effective, false)`,
i.e. `effective.has_interface()`. A `private` or `sealed` edge therefore
contributes nothing on any surface — the unanimous cross-ecosystem rule
(`research_package_integrations.md` §3), obtained here for free.

---

## 5. The Interpolation Contract

### 5.1 Engine and capability

One engine, one closed vocabulary, no dialect (D7):
`TemplateResolver::new(content_path, &dep_contexts)` per admitted package —
the constructor's default is already `Usage::Environment`, so no `.usage(…)`
call and no new `Usage` variant (C-008). `${installPath}` resolves to the
**declaring** package's content directory, never the root's; `${deps.NAME.*}`
resolves against that package's own declared dependencies, via the
`build_dep_context_map` result the composer already builds
(`composer.rs:344`, `:392`).

Whatever token set the engine grows later applies here and to env values
together, by construction — that is the property D7 buys, and under #303's
closed world it is a *safe* property: because an unrecognised `${…}` can never
be published, a later grammar addition cannot change the meaning of anything
already in a registry.

### 5.2 What each token class does

| Token in a payload string leaf | Behavior |
|---|---|
| `${installPath}`, `${self.installPath}` | substituted with the declaring package's content path — one referent, two spellings |
| `${installPath:posix}`, `${self.installPath:posix}` | same, then rendered with forward slashes on a Windows host; the identity function elsewhere. `:native` is the default and is spellable explicitly |
| `${deps.NAME.installPath}` (`:native` / `:posix` too) | **substituted** with that dependency's install path — same as in an env value |
| `$${installPath}`, `$${deps.x.installPath}` | **escaped** → literal `${installPath}` / `${deps.x.installPath}` (D10, C-009b) |
| `$${workspaceFolder}`, `$${localEnv:HOME}`, `$${env:FOO}` | **escaped** → the literal `${workspaceFolder}` / `${localEnv:HOME}` / `${env:FOO}` reaches the consuming application, which expands it itself |
| `${workspaceFolder}`, `${localEnv:HOME}`, `${containerEnv:HOME}`, `${env:FOO}` — bare | **refused**, exit 65 (#303 D3). The message names the token and offers the escape |
| `${installpath}` (wrong case) | **refused**, exit 65, with a suggested root — `installPath`. Not silence, not literal text |
| `${self.env.KEY}` | **refused** at publish as `DisallowedToken` — no `self.env` scope exists on this surface (C-008) |
| `$(anything)`, `%FOO%`, a bare `$$` | untouched — not OCX's sigil, nothing to claim |
| `${` with no closing `}` | literal text (#303 Axis D) — no token exists, so nothing resolves wrongly |

**Forward-looking, not decided here** (#221): if the engine ever grows a token
for the *package's own* environment beyond `${self.env.KEY}`'s
earlier-declared-vars scope, payload resolution would have to run **after** env
composition rather than per-package — a structural change to where C-012's
collection sites sit. Worth weighing at that point that VS Code spells its own
as `${env:HOME}` and devcontainer as `${localEnv:VAR}` / `${containerEnv:VAR}`
— near enough to invite confusion in exactly these payloads, and now doubly so,
because those spellings are refused rather than ignored.

### 5.3 No exemption exists — the payload is the same closed world

> **Superseded, deliberately kept.** This section previously specified an
> unknown-placeholder *exemption* for payloads: `first_unknown_placeholder` was
> invoked per call site, and the exemption was to be implemented by "not adding
> a third call site". #303 deleted that helper and made the payload subject to
> the one grammar; there is no exemption to implement. The reasoning is retained
> so a reader can see what changed rather than finding a section that never
> mentions the question.

There is one recogniser — `scanner::scan` — and every surface routes through
it: env values, entrypoint `args`, integrations payloads, and both readers
that used to match the `"${installPath}"` literal by hand. A payload gets no
per-surface unknown-token policy, by design (#303 D3, "one rule, no per-surface
exception"): a second policy is a second thing every future reader of the
grammar must hold in their head, and the escape already expresses the same
intent locally and visibly in the source bytes.

The one thing a payload *does* narrow is capability, not recognition:
`AllowedTokens { deps: true, self_env: false }` (C-008). That is a placement
rule of the kind `Usage::EntryPointArgs` already carries, not an
unknown-token rule.

**Test that discriminates.** The old discriminator is void — it asserted that a
payload accepts `${workspaceFolder}` while an env value rejects it, and both now
reject. The real discriminator is *bare refused / doubled accepted*, asserted on
**both** surfaces:

| Leg | Surface | Expected |
|---|---|---|
| 1 | `integrations` payload with bare `${workspaceFolder}` | `ocx package push` exits 65, message names the token |
| 2 | `integrations` payload with `$${workspaceFolder}` | publishes; composed `payload` carries the literal `${workspaceFolder}` |
| 3 | `env` value with bare `${workspaceFolder}` | refused at publish, exit 65 |
| 4 | `env` value with `$${workspaceFolder}` | publishes; composed entry carries the literal `${workspaceFolder}` |

Legs 1 and 2 must live together, and neither alone proves anything: leg 2 alone
cannot tell "the escape collapsed to a literal" from "the resolver stopped
claiming `${…}` at all", and leg 1 alone cannot tell "the token was refused"
from "the payload is unpublishable for some unrelated reason". Legs 3 and 4 pin
that the payload surface did not quietly acquire a dialect of its own. Shipped
as `test_integrations_bare_token_is_refused_at_publish` /
`test_integrations_interpolation_end_to_end` (payload) and
`test_escaped_token_publishes_and_composes_as_a_literal` (env value), with the
unit-level pair
`resolve_refuses_an_unrecognized_token_rather_than_passing_it_through` /
`resolve_escapes_doubled_dollar_to_a_literal_token`.

### 5.4 The `$${…}` escape

Ratified in #221 (D10): `$${…}` yields a literal `${…}`, doubling being the
familiar spelling from shell, compose and make, and backslashes being miserable
inside JSON. Under #303 it is no longer an affordance for the payload that
*wants* to emit an OCX-spelled token downstream — it is the **only** way any
`${…}` survives to a consumer, and every foreign token in every payload routes
through it.

Three facts, all now history rather than instruction (the escape shipped with
#303's scanner, not with this feature — see C-009b):

1. **The engine had no escape before #303.** `$${installPath}` resolved to
   `$<path>` — the `$$` was not collapsed and the token *was* substituted.
2. **It landed in the shared engine, so it changed env values and entrypoint
   `args` too.** That is D7's "changes it here and in env values together, by
   construction" working as designed; it retroactively changed how an
   already-published `env` value containing `$${` resolves. Narrow — the
   sequence has no reason to appear in anything published — but real. See R-3
   and D20 (resolved: accepted).
3. **The escape is `$$` immediately followed by `{`, never an unconditional
   `$$` → `$` collapse.** A bare `$$` in a payload — a price, a shell fragment,
   a regex — is untouched. The narrow form is what keeps OCX out of the
   Kubernetes `$$(VAR)` bug class.

What the escape does **not** buy, restated because it lands squarely on this
feature: `$${workspaceFolder}` yields the bytes `${workspaceFolder}`, which the
consuming application then expands normally. The escape defends against **OCX**,
not against the consumer OCX is delivering to. There is no OCX-side spelling of
"a literal `${workspaceFolder}` that VS Code must not expand" — that is VS
Code's own escaping problem.

### 5.5 Structure invariance

`interpolate` (C-009) substitutes into `Value::String` leaves in place and
**never re-parses** the result. A payload whose interpolated output happens to
look like JSON (`"{\"a\":1}"`) stays a string. The output document's shape —
key set, array lengths, value types — is identical to the input's, always.
This makes "a payload whose interpolation output changes the JSON structure"
structurally impossible rather than merely discouraged.

---

## 6. User-Experience Scenarios

### S-001 — Authoring

**Action:** publisher adds a `integrations` block to `-metadata.json`, runs
`ocx package create` then `ocx package push`.
**Expected:** the block is copied verbatim into the published `metadata.json`
(C-004), unresolved (`${installPath}` still literal on the wire, `$${…}` still
doubled on the wire — the escape collapses at resolve time, not at publish).
**Errors:** invalid key → 65 (C-005); over cap → 65 (C-006); `${deps.typo}`
naming an undeclared dependency → 65; a bare `${workspaceFolder}` or any other
unrecognised token → 65, naming the token and offering the escape (C-007).

### S-002 — `ocx --format json package env <id>`

**Action:** compose one installed package that declares two namespaces.
**Expected:** a top-level `integrations` array with two rows, each
`{namespace, package, payload}`; `payload` fully interpolated; rows in
lexicographic namespace order; `package` = the canonical resolved identifier.
The array is **not collapsed** despite the single root (D18) — the same shape
a multi-root invocation produces.
**Errors:** none specific to this feature.

### S-002b — Tier parity

**Action:** run the identical filter on both tiers:
`ocx --format json package env clang | jq '[.integrations[] | select(.namespace=="com.microsoft.vscode") | .value]'`
and the same with `ocx --format json env`.
**Expected:** both work, no case distinction, no shape difference (#221).

### S-003 — `ocx --format json env` (toolchain tier)

**Action:** compose a project whose lock pins three tools, two of which declare
integrations.
**Expected:** the same array shape (one shared `EnvVars` type, C-014), rows in
admitted-set visit order: for each root, its admitted deps first (topological),
then the root; roots in the order the tool set produced them.
**Errors:** none specific.

### S-004 — Nothing declares integrations

**Expected:** `"integrations": []` — present, empty, never omitted. No hint
line clause. Byte-identical `entries` table.

### S-005 — `--shell` / `--ci`

**Action:** `ocx env --shell=bash`; `ocx package env <id> --ci=gitlab`.
**Expected:** no integration data in either stream — no export line, no
JSON-lines row, no namespace key anywhere in the output (C-018).

### S-006 — `--self`

**Action:** `ocx --format json package env <id> --self` on a package that
declares integrations and depends on a package that declares integrations.
**Expected:** `"integrations": []`. The `binaries` array is still populated
(binaries are `PUBLIC`); `entrypoints` still shows the *dep's* launchers but
not the root's — none of that changes. Only integrations go to empty (D4).

### S-007 — Private-edge dependency contributes nothing

**Action:** root declares a dependency with `visibility: "private"`; that
dependency declares integrations. `ocx package env <root>`.
**Expected:** zero rows from the dependency, on **both** surfaces —
`dep_admitted` rejects it on the interface surface, and
`integrations_cross(true)` rejects the whole carrier on the private surface
(C-011, §4.4).

### S-008 — Two packages declare the same namespace

**Action:** two tools in one project both declare `com.microsoft.vscode`.
**Expected:** **two rows**, same `namespace`, different `package`, in admitted
order. No merge, no error, no `conflicts` entry — deliberately *not* routed
through the `closure.conflicts` machinery that handles entrypoint-name
collisions (D2; `research_package_integrations.md` §4). Exit 0.

### S-009 — Over-cap payload

**Action:** publish a package whose `com.example` payload compacts to 9 KiB.
**Expected:** `ocx package push` fails, exit 65, message names
`"com.example"`, the actual size, and the 8192-byte limit. The same rejection
fires on the *read* path (`load_config_metadata` → `ValidMetadata`) if such a
package somehow exists in a registry.

### S-010 — `ocx package inspect --closure --format json`

**Action:** inspect an uninstalled package whose closure declares
integrations at two depths.
**Expected:** `closure.surface.interface.integrations` lists
`{name: <namespace>, package: <id>}` per admitted contribution;
`closure.surface.private.integrations` is `[]`; **no payload anywhere**
(C-016). Plain mode renders a `integrations` branch under
`surface > interface`.

### S-011 — Plain output

**Action:** `ocx package env <id>` (no `--format`).
**Expected:** the `entries` table unchanged byte-for-byte, followed by one
hint line whose integrations clause names up to three namespaces then `...`
(C-015). No payload, no fourth column, no second table.

### S-012 — Diamond dependency

**Action:** two roots both depend on one package that declares
integrations.
**Expected:** **one** row for that dependency, at its first-seen position —
cross-root dedup applies to integrations exactly as it does to
`admitted_binaries` (pinned today by
`compose_multi_root_diamond_dep_claims_emitted_once`).

### S-013 — Patch companion

**Action:** a site patch admits a required companion that declares
integrations, then `ocx --format json env`.
**Expected:** the companion's rows appear, attributed to the **companion's**
identifier rather than the base's, alongside the base package's own — which is
the positive control proving the array is populated at all. The companion's
`env` overlay entries still appear (with `--show-patches` provenance), proving
the companion mechanism engaged. A companion matched by several bases
contributes once. Under `--self`, neither base nor companion appears (C-017).

### S-014 — Interpolation, end to end

**Action:** a clang package declares
`"C_Cpp.default.compilerPath": "${installPath}/bin/clang"`,
`"C_Cpp.default.includePath": ["$${workspaceFolder}/**"]`,
`"depCompilerPath": "${deps.zlib.installPath}/bin/clang"`, and
`"sdk": "$${installPath}"`. Compose it.
**Expected:** `compilerPath` carries the **declaring** package's absolute
digest-derived content path; `depCompilerPath` carries the **dependency's** —
and the two must differ, or the assertion cannot discriminate a resolver that
substitutes the root's path everywhere; `includePath[0]` is the literal
`"${workspaceFolder}/**"`, produced by the escape rather than by pass-through;
`sdk` is the literal `"${installPath}"` (escape, D10). One payload, four
behaviors.

### S-014b — The bare foreign token is refused

**Action:** the same payload with the escape removed —
`"C_Cpp.default.includePath": ["${workspaceFolder}/**"]`. Run
`ocx package create` / `push`.
**Expected:** exit 65, message naming `${workspaceFolder}` and offering the
escape. **Required sibling of S-014**: one green alone cannot tell a working
escape from a resolver that has simply stopped claiming `${…}` (§5.3).

### S-014c — Windows forward slashes

**Action:** a payload declares
`"rust-analyzer.server.path": "${self.installPath:posix}/bin/rust-analyzer"`;
compose on a Windows host.
**Expected:** the value carries forward slashes (`C:/Users/…/content/bin/…`),
the drive letter preserved and no `\\?\` prefix. On every other host `:posix`
is the identity function, so the same document composes correctly there too.
This is the mechanism D22 deferred and #303 delivered — an integrations-only
normalization was never needed.

---

## 7. Edge Cases

| # | Case | Resolution |
|---|---|---|
| E-01 | `integrations` absent vs `{}` | Same state. Absent → empty; empty → omitted on write (D13, C-001). |
| E-02 | Why not `binaries`' `Option` tri-state | "Publisher asserts zero integrations" is information nobody consumes — no scanner distinguishes it from "didn't declare". `binaries`' tri-state exists because an SBOM scanner genuinely needs "declared empty on purpose" vs "field predates the ADR". Named divergence, deliberate. |
| E-03 | Payload is a bare string / array / number / `null` | Legal (§3.2). Validating the shape would be validating contents. |
| E-04 | `${installPath}` inside an array element | Interpolated — arrays recurse (C-009). |
| E-05 | `${installPath}` inside a nested object value | Interpolated at any depth. |
| E-06 | `${installPath}` in an object **key** | **Verbatim.** Keys are identifiers in the consuming schema; substitution could collide two keys with no defined winner, which is a merge decision D2 forbids. Documented, not an error. |
| E-06b | An **unrecognised** `${…}` in an object key | Also verbatim, and **not refused** — the one place in package metadata where OCX does not claim `${…}`. `string_leaves` (the read-only sibling of `interpolate`) yields exactly the positions interpolation would touch, so the publish gate can never fire where resolution would not have substituted. Deliberate: refusing a key OCX has already promised never to rewrite would be a rule with no resolution behind it. |
| E-07 | `${installPath}` or any other `${…}` in the **namespace** key | Verbatim, same reasoning as E-06b. The key grammar (C-005) is what governs a namespace, and it has no opinion about `$`. |
| E-08 | Interpolation changes JSON structure | Impossible by construction (§5.5). |
| E-09 | Duplicate namespace keys on the wire | Last-wins, deliberately (§3.4). Named divergence from `Entrypoints`. |
| E-10 | Payload at exactly 8192 bytes | Passes — inclusive boundary (C-006). 8193 fails. |
| E-11 | Two namespaces, each 8 KiB, total 16 KiB | Passes both caps. Five such namespaces (40 KiB) fails the per-package cap only. |
| E-12 | Multi-root ordering | Roots in input order; within a root, admitted deps topologically then the root; within a package, lexicographic namespace. Fully deterministic. |
| E-13 | Diamond dependency | One row (S-012). |
| E-14 | Same package is both an explicit root and a transitive dep | Deferred to the root-emission pass (`composer.rs:262`) — one row, attributed to the root's tag-bearing identifier. Inherited from the existing dedup, no new rule. |
| E-15 | Case-distinct namespaces (`vscode` vs `VSCode`) | Two distinct namespaces, both emitted. No case-fold-collision check (unlike `BinaryName`) — that would be validating the convention D6 declines to enforce. |
| E-16 | Payload contains a literal `${installPath}` — or any other `${…}` — that must survive | Write `$${…}` (D10, §5.4). Under #303 this is the only way, for a recognised and an unrecognised token alike. |
| E-16b | `$$${installPath}` (three `$`) | `$` + escaped literal `${installPath}`. The escape is applied in the same left-to-right pass as recognition (C-009b), so this is unambiguous; a pre/post `replace` would not be. |
| E-16c | An already-published `env` value containing `$${` | Its resolution **changed** when the scanner landed (#303). Narrow but real; see R-3 and D20 (resolved: accepted). |
| E-16d | A pasted devcontainer / VS Code settings blob with several `${…}` | **Does not publish** until every one is doubled. The authoring cost is proportional to the token count and falls hardest on generated blobs. Accepted with the reversal (Amendment); no OCX-side auto-escaping affordance exists, and whether one should is #221's call, not this record's. |
| E-16e | A payload token OCX will recognise in a *later* release | Refused today, publishable then, with the same meaning it will always have — the closed world makes grammar additions purely additive (#303 Reversibility). An older ocx reading a package published by a newer one refuses only when it tries to *resolve* it (D14), so `pull` / `inspect` still work. |
| E-17 | Windows: `${installPath}` substitutes a backslash path into JSON | JSON escapes backslashes correctly, so the wire stays valid. The *value* is a native Windows path by default — same as every other `${installPath}` consumer. A payload that needs forward slashes writes `${self.installPath:posix}` (S-014c, D22). |
| E-18 | `resolve.json` / the transitive closure | **Unchanged.** `ResolvedPackage` (`resolved_package.rs:29-36`) carries only `{identifier, visibility}`; `composer::compose` reads the FULL live `metadata.json` per node (`tasks/common.rs:114-126`). Integrations are read live and MUST NOT be persisted into the TC — a stale copy would survive a re-pull that changed the payload. |
| E-19 | A package published by a newer ocx carrying an unknown key *inside* a payload | Ignored by construction — `serde_json::Value` accepts anything. This is the whole point. |

---

## 8. Trade-off Analysis

Only points #221 left open. Nothing it ratified is re-litigated — (b) and (d)
below therefore analyse *how* to realize a ratified decision, not whether.

Weights are the same across all four analyses: **Correctness 5, Contract
safety (one-way-door / oracle) 4, Consistency with existing precedent 3,
Simplicity 2.** Scores 1–5, weighted sum.

### (a) Plain-text rendering

| Option | Correctness (5) | Contract safety (4) | Consistency (3) | Simplicity (2) | Σ |
|---|---|---|---|---|---|
| **A1. Hint-line clause (flat) + tree branch (closure)** | 4 | 5 | **5** | 4 | **63** |
| A2. Fourth table column carrying the payload | 1 | 2 | 1 | 2 | 20 |
| A3. Omit from plain entirely | 3 | 5 | 2 | **5** | 51 |

**Chosen: A1.** Both precedents already exist in this codebase and each fits
its own surface: the flat envelope compacts claim arrays into a hint line
(`env.rs:169-195`) precisely because they have no per-entry-row mapping; the
closure output renders them as tree branches (`package_inspect.rs:969-1004`)
because it is already a tree. Following each surface's own precedent scores
maximum consistency at zero conceptual cost. A2 is disqualified on the
plain-mode column budget alone — an unbounded JSON blob in a padded,
never-truncated table cell. A3 is defensible and cheap but silently drops
information from the default format for a feature whose most common question
("which vendors configured anything?") is answerable in eight words.

### (b) Expressing "interface surface only"

| Option | Correctness (5) | Contract safety (4) | Consistency (3) | Simplicity (2) | Σ |
|---|---|---|---|---|---|
| **B1. Shared surface-level predicate `integrations_cross`** | **5** | **5** | 3 | **5** | **64** |
| B2. New carrier-visibility constant | — | — | — | — | **impossible** (§4.1) |
| B3. Change `carrier_crosses` to consult `self_view` on deps | 2 | 1 | 2 | 3 | 26 |
| B4. Bare `if self_view` inline in both call sites | 5 | 2 | 1 | 5 | 46 |

**Chosen: B1.** #221 (D9) already rules out B3 by name — "`carrier_crosses`
needs no change" — and independently rules out B2 by declining to give the type
a `visibility` field; §4.1 supplies the arithmetic showing B2 was never
available anyway. B4 is correct but violates the "inspect never re-derives"
contract that exists because the env-asymmetry class of bug came from exactly
that duplication. B1 costs one three-line function and buys the single-source
property back.

Consistency scores 3, not 5, because this **is** a departure from "no per-kind
structural rules — membership comes from visibility alone". #221 pre-ratifies
the departure; §10 records it rather than hiding it.

### (c) Namespace key validation strictness

| Option | Correctness (5) | Contract safety (4) | Consistency (3) | Simplicity (2) | Σ |
|---|---|---|---|---|---|
| C1. Raw `String`, nothing rejected | 2 | 3 | 2 | **5** | 38 |
| **C2. `String` + minimal grammar checked in `ValidMetadata`** | **5** | 4 | 4 | 4 | **62** |
| C3. Validated newtype `IntegrationNamespace` | 5 | 4 | **5** | 2 | 60 |
| C4. Strict reverse-DNS regex | 3 | **1** | 2 | 3 | 31 |

**Chosen: C2.** C1 lets an empty key, a newline-bearing key (log forging,
CWE-117 — the concern `InvalidListSeparator` already encodes), and an unbounded
key reach a printer and a hint line; "validate external input at system
boundaries" is Block-tier. C4 contradicts D6 *and* is the worst one-way door in
the set: a regex on the read path can only ever be loosened, and reverse-DNS is
a convention nobody polices anywhere in the surveyed prior art.

C3 is a genuinely close second — a newtype is the house pattern for a validated
name, and it would carry the invariant in the type rather than in a pass. It
loses on two concrete points: (i) it risks the schemars map-key derive drifting
from the verified `{"type":"object","additionalProperties":true}` shape, which
C1/C2 preserve exactly; (ii) `feedback_type_economy_reuse_structs` — a newtype
whose only construction site is one validation pass, in a feature that already
mints two new types, is the third. The check landing in the same function as
the cap check (which #221 already placed in `ValidMetadata`) keeps both
container rules in one readable place.

### (d) Where interpolation runs, and how much it validates

D7 fixes the capability: the payload gets the engine's vocabulary, no dialect —
`Usage::Environment` is already the `TemplateResolver::new` default, so a new
`Usage` variant would be a dialect D7 forbids. The residual choice was what the
publish gate checks about those tokens (C-007).

| Option | Correctness (5) | Contract safety (4) | Consistency (3) | Simplicity (2) | Σ |
|---|---|---|---|---|---|
| D-a. Full dep-reference check, no unknown-token scan | 5 | 5 | 5 | 3 | 66 |
| D-b. Neither check — cap + JSON parse only | 2 | 3 | 2 | **5** | 42 |
| **D-c. Both checks, including unknown tokens** | 1 | **1** | 3 | 4 | 26 |

**Originally chosen: D-a. Shipped: D-c.** This is the one scoring in this ADR
the reversal inverted, and the honest reading is that the table asked the wrong
question, not that the arithmetic was wrong.

D-c scored 1 on correctness and 1 on contract safety **because it was scored
against #221's stated guarantee** — that a pasted VS Code block survives
byte-identical — under which refusing `${workspaceFolder}` "kills the feature
outright". #303 withdrew that guarantee. Under the closed world the same option
reads the other way round: refusing an unrecognised token is the *fail-closed*
answer (nothing resolves to silence or to unexamined literal text in a
digest-pinned artifact resolved months later on another machine), and it is the
*reversible* direction (the accept set may only grow, so a token refused today
can be recognised tomorrow without un-resolving anything already published).
The two criteria D-c scored worst on are the two it now scores best on.

What the table never weighed, because it was scoped to this one carrier, is the
cost D3 actually prices: pass-through couples OCX's own root set to every other
tool's vocabulary, permanently. That is not an integrations trade-off, which
is why it could not surface here and why the decision moved to #303.

D-b keeps its score and stays rejected on its own terms: it is the most literal
reading of "no validation" and the laziest, but it trades a publish-time error
for a **compose-time** one — `${deps.typo}` sails through `push` and then
hard-fails `ocx env` for everyone who installs the package, with an error naming
a dependency list the consumer cannot act on.

The line the "no validation" rule draws is unchanged: the gate interrogates
**OCX's own syntax**, never payload structure or semantics. What moved is the
boundary of "own syntax" — from *the tokens OCX recognises* to *every `${…}`
sequence*. See §3.1 and D21.

Superseded reasoning: an earlier draft of this ADR chose a new
`Usage::Integrations → { deps: false }`, on the grounds that resolving
`${deps.*}` could leak a *private* dependency's install path onto the consumer
surface. That objection over-reaches — an interface-visible `env` value can
already interpolate a private dep's path today, so the leak is pre-existing and
accepted, and #221 explicitly ratifies the full vocabulary. Recorded here
because the argument reads plausible and should not be re-derived.

---

## 9. One-Way-Door Risks

Flagged explicitly. The published-metadata read path is the sharpest surface in
this design.

| Risk | Why it is one-way | Mitigation |
|---|---|---|
| **R-1 — Size caps sit on the read path.** `ValidMetadata::try_from` runs from `load_config_metadata` on every pull and inspect, not only at publish. | Lowering either cap un-resolves an already-published package — forbidden. | Start low (8 KiB / 32 KiB). Raising is free and breaks nothing; a package that could not publish was never published. Documented raise-only in C-006 and in `metadata.md`. |
| **R-2 — Namespace key grammar sits on the read path.** | Tightening C-005 later un-resolves published packages. | The grammar rejects only genuinely unusable shapes (empty, control chars, whitespace, >128 bytes). Everything a publisher might plausibly want is already legal. Only ever loosen. |
| **R-3 — The `$${…}` escape lands in the shared engine (D10, C-009b).** *(realized: shipped in #303)* | It **retroactively changed** how an already-published `env` value or entrypoint `arg` containing `$${` resolves: before, `$${installPath}` → `$<path>`; after, → literal `${installPath}`. The read path is a one-way door and this walked through it. | Ratified in #221 as the deliberate "changes it here and in env values together, by construction" property. The exposure is vanishingly small — `$${` has no reason to appear in any published string — but it is not zero, and it cannot be undone once published packages start relying on the new meaning. Resolved as D20 — accepted. |
| **R-8 — The closed grammar applies to payloads (Amendment, #303 D3).** | Two directions, opposite in kind. **Tightening**: making an already-legal payload illegal is what this *was* — a document with a bare `${workspaceFolder}` published before #303 no longer resolves. **Loosening**: recognising a new token later is free and cannot reinterpret anything, because an unrecognised token could never have been published. | The tightening is bounded by D14: refusal is scoped to *resolution* plus the publish gate, so an affected package still pulls, installs and inspects — only composing it fails, and the message names the token and the escape. The loosening direction is what makes the whole grammar additive from here on, and it is the property the pass-through design could not have. Any *further* tightening of the payload grammar is forbidden by the same read-path rule that governs R-1 and R-2. |
| **R-4 — The composed row shape (`{namespace, package, payload}`).** | Consumers (Bazel rules, Action wrappers) will parse it. Renaming a key, collapsing the array for a single root, or nesting it is a CLI wire break. | All three keys ratified in #221. Never-collapse is D18. The array is a top-level sibling matching `binaries`/`entrypoints` — one envelope shape to learn. |
| **R-5 — `${deps.*}` resolves in payloads (D7).** | A payload can now depend on a dependency's install path. Removing that capability later breaks published packages. | Ratified. It is the same capability env values have, and it is why the feature works at all — a digest-derived path no human can hand-write. |
| **R-6 — Integrations must NOT be persisted into `resolve.json`.** | A TC-cached payload would go stale relative to a re-pulled `metadata.json`, and the TC is written once at install. | E-18. `compose` already reads the full live `metadata.json` per node; the collection sites (C-012) use that same already-loaded value. No `ResolvedPackage` change. |
| **R-7 — Interface-surface-only (D4).** | Not a one-way door, and #221 says why: adding an optional `visibility` field defaulting to `interface` later is additive on the wire *and* on the output. Already-published packages keep meaning what they meant (absent = interface = shipped behaviour); the private surface gains an array where it had none. | Safe direction by construction. Nothing reinterprets. |

---

## 10. Constitution Check

Checked against [`arch-principles.md`](../rules/arch-principles.md).

| Principle | Status |
|---|---|
| Crate layout — `ocx_lib` core, `ocx_cli` thin | ✅ All composition, validation and interpolation in `ocx_lib`; the CLI adds one `Serialize` struct and one hint clause. |
| Where Features Land — new metadata field → `package/metadata/` + schema + docs | ✅ C-002 (new file), C-020 (schema + docs). |
| One concept per file, no `mod.rs` | ✅ `metadata/integrations.rs`. |
| `JsonSchema` on every `Serialize`/`Deserialize` struct | ✅ C-002. (`subsystem-package.md`'s rule, scoped to package metadata — `IntegrationAttribution` (C-014) is an `ocx_cli` `api/data` type and derives `Serialize` only, like every sibling in that module.) |
| Three-layer errors | ✅ New variants on `package::error::Error`, `#[source]` on the wrapping variant, `ClassifyExitCode` delegation (C-019). |
| Error messages: lowercase, no trailing punctuation (`C-GOOD-ERR`) | ✅ C-019. |
| Exit codes: typed enum, sysexits-aligned | ✅ 65 (`DataError`) throughout, matching every other metadata refusal. |
| Internal enums omit `#[non_exhaustive]` | ✅ N/A — no enum gains a variant (C-008 reuses `Usage::Environment`). |
| Utility Catalog — check before writing a helper | ✅ Checked; no `serde_json` string-leaf walker exists, ~15 lines, no dependency added (C-009). |
| Don't Own Non-Domain Code | ✅ No hand-rolled serializer, codec or escaping. `serde_json` owns parsing and emission end to end — the per-package size total (C-006) hand-computes JSON object framing bytes rather than reserializing the whole map, but every counted byte is `serde_json`-derived (`serialized_len`'s `ByteCounter` sink over `serde_json::to_writer`) and the framing arithmetic itself is pinned byte-for-byte against `serde_json`'s own output by a differential boundary test, not an independently-maintained wire format. |
| No `deny_unknown_fields` in the `Config` tree | ✅ N/A — this is package metadata, not config. |
| Test-only seams convention | ✅ None introduced. |
| Composer surface algebra is the single source of truth | ⚠️ **Deviation — see below.** |

### Constitution Deviations

| Deviation | Rule departed from | Justification | Containment |
|---|---|---|---|
| The integrations carrier's surface membership is decided by a structural rule, not by a `Visibility` value | `subsystem-package-manager.md` / `composer.rs:96-127`: "No per-kind structural rules — membership comes from visibility alone; `self_view` only selects which surface is emitted" | Pre-ratified in #221 (D9), which states the field belongs to a surface rather than carrying a visibility. §4.1 adds the arithmetic: D4 is **not expressible** in the two-axis algebra — all four constants fail. The alternative (changing `carrier_crosses`) breaks a shipped contract with an oracle test, and #221 rules it out by name. | Confined to the *carrier* term. The *edge* term still routes through `dep_admitted` unchanged (§4.4). The rule is one shared, named, documented function homed beside the algebra it departs from (C-011), so `compose` and `project_surface` cannot drift. The composer module comment and `subsystem-package-manager.md` both record it. |
| The `$${…}` escape changes resolution of already-published `env` values and entrypoint `args` | `CLAUDE.md` stability tiers: "already-published packages must keep resolving, so metadata and OCI manifest changes stay backward compatible on the read path" | Ratified in #221 (D10) as the deliberate one-engine property. A published string containing `$${` has no reason to exist, so the practical exposure is ~zero. | Named in R-3 and RESOLVED as D20 — accepted by the owner: `$${installPath}` has no meaningful reading, so no published package can depend on the old behaviour. Shipped in #303, not here. |
| A bare unrecognised `${…}` in an already-published payload no longer resolves | Same read-path rule | The Amendment. Pass-through was reversed by the owner in #303 D3: OCX's namespace must not be hostage to other tools' vocabularies. A tightening on the read path, accepted deliberately rather than overlooked. | Bounded by #303 D14 — refusal is scoped to *resolution* plus the publish gate, so an affected package still pulls, installs and inspects; only composing it fails, with a message naming the token and the escape. Recorded as R-8, and as the one direction that may never be repeated: the payload grammar is loosen-only from here. |
| Duplicate namespace keys resolve last-wins rather than being rejected | `metadata/entrypoint.rs` precedent: "serde_json last-wins default is unsafe for registry data" | OCX never interprets the payload, so it cannot detect a meaningful conflict; every consuming JSON reader applies the same rule to the same bytes. Rejecting would forfeit the plain schemars derive for a case no JSON emitter produces. | §3.4, with a named publisher-side escape hatch (`ocx package create` lint) that keeps the read path lenient. |

---

## 11. Implementation Order

Contract-first TDD (`feedback_contract_first_tdd`). File-disjoint where marked.

0. ~~**`$${…}` escape first, on its own commit** (C-009b)~~ — **done elsewhere.**
   The escape shipped with #303's scanner, which is the only place it is
   expressible (C-009b). The isolation argument held and was honoured: it landed
   on its own change, with its own regression surface (`env` values, entrypoint
   `args`), before this feature's token gate existed.
1. **Stub** — `metadata/integrations.rs` (C-002, C-006, C-009, C-010),
   `Bundle` + `AuthoringBundle` + `Metadata` fields (C-001, C-003, C-004),
   error variants (C-019). Gate: `cargo check`.
2. **Specify** — unit tests for C-005/C-006/C-007 (grammar, cap boundary,
   dep-reference check, unrecognised-token refusal), C-009 (positions,
   structure invariance), C-011 (four-cell truth table), and §5.3's
   bare-refused / doubled-accepted pair on both surfaces. Gate: tests compile
   and fail.
3. **Implement lib** — validation, walker, `integrations_cross`,
   `ComposeOutput` + collection sites, `AdmittedClaims` rename.
4. **Implement CLI** *(disjoint from 5)* — `IntegrationAttribution`,
   `EnvVars` fourth array, `availability_hint` clause.
5. **Implement inspect** *(disjoint from 4)* — `ClosureNode`, `Surface`,
   `project_surface`, `SurfaceOut`, `surface_node`.
6. **Acceptance** — scenarios S-001…S-014c; extend
   `test/tests/test_package_inspect_closure.py` for the C-016 oracle on both
   surfaces.
7. **Derived surfaces** — C-020 (`task schema:generate`, docs, rules). Gate:
   `task verify`.

---

## 12. Open Questions

**None. All three are resolved** (owner, 2026-08-09) and recorded below as
decisions D20–D22; D22's deferred follow-up **closed** on 2026-08-10 when #303
landed. #221's own two open questions were already resolved and recorded as
decisions (cap in `ValidMetadata`, exit 65, 8 KiB / 32 KiB — D8, C-006; composed
payload key, since reversed to `payload` — D5); its explicit non-goals (project-tier
`[integrations]`, `[package."<repo>"]` opt-in/opt-out, namespace registry) are
D19 and are not reopened.

**D20 (was OQ-1) — The shared-engine `$${…}` escape is ACCEPTED**, retroactive
change to already-published `env` values and entrypoint `args` included.

The justification is semantic, not statistical, and is stronger than the
"~zero exposure" argument this ADR originally offered: **`$${installPath}` has
no meaningful reading today.** A literal `$` immediately followed by an
interpolated absolute path is not something any publisher would intend to emit,
so no published package can be relying on the behaviour that changes. The break
is formal rather than practical.

Contrast with the closest real precedent: Docker Compose altered `$$` handling
inside `command:` strings across versions and generated a run of user bug
reports ([docker/compose#12005](https://github.com/docker/compose/issues/12005),
[#12468](https://github.com/docker/compose/issues/12468)) — but *their* `$$`
carried a meaning that changed under users. Ours does not.

The pre-landing sweep of published `metadata.json` blobs for the literal `$${`
was therefore **optional confirmation, not a gate**. The escape shipped in #303.

**D21 (was OQ-2) — the publish gate DOES check `${deps.*}` references inside
payloads** (C-007's token step stays).

A `${deps.NAME}` naming a dependency the package does not declare is **invalid
metadata**, not a payload opinion — OCX is validating its own token, the one it
promises to resolve, never the payload's semantics. `NAME` must be a **direct**
dependency of that package; the token never resolves transitively.

This is exactly the existing `validate_env_tokens` rule (its name map comes from
`metadata.dependencies()` — direct-only — and already reports duplicate-name
collisions) extended to payload string leaves.

Without it, `${deps.zlibb.installPath}` publishes clean and then hard-fails
`ocx --format json env` for **every consumer** — a failure moved from the one
publisher who can fix it to N consumers who cannot.

*Amended by #303, on both halves of the original wording:*

- **Where it runs.** D14 moved token validation off `ValidMetadata::try_from`
  and onto `validate_for_publish`, so it fires at `create` / `push` — not again
  on every pull and inspect. That is the point of D14: a package OCX would
  refuse to compose can still be inspected, so a consumer can *see* what is
  wrong with it.
- **Foreign tokens.** The original text closed with "a VS Code
  `${workspaceFolder}` or a devcontainer `${localEnv:HOME}` still passes through
  byte-identical." **That is no longer true.** Both are refused at publish
  unless doubled. The dep-reference check is still narrow — it examines only
  tokens that parse as `deps.NAME.installPath` — but it now sits behind an
  unrecognised-token refusal that fires first.

**D22 (was OQ-3) — Native separators by default on Windows. RESOLVED by #303
landing; the follow-up is closed, not pending.**

An integrations-only normalization would make one carrier behave differently
from every other for the same token, and JSON escapes backslashes correctly, so
the wire is valid either way. The underlying need — a publisher targeting VS
Code wanting forward slashes — was deferred to a general mechanism rather than
special-cased here, and that mechanism **shipped**:
[**#303**](https://github.com/ocx-sh/ocx/issues/303) added the `:native` /
`:posix` render modifiers alongside the `${self.*}` namespace and
`${self.env.VAR}`, superseding #73 and #175.

So a Windows-hosted VS Code setting is authored
`"${self.installPath:posix}/bin/rust-analyzer"` and resolves with forward
slashes, drive letter preserved, no `\\?\` prefix; on every other host `:posix`
is the identity function, so one document is correct everywhere (S-014c). The
modifier is legal on the three install-path bodies and refused on
`${self.env.KEY}` — a flip that rewrites every `\` is meaningless-to-corrupting
off a path.

The prerequisite this ADR predicted held, and is worth recording because the
*reason* changed: `${self.*}` had to land before the modifier slice, but not —
as the original text said — because `${installPath:posix}` would be
"indistinguishable from the foreign `${localEnv:HOME}` shapes D3 requires OCX
to pass through". There is no pass-through and no foreign shape; under the
claim-all rule `${localEnv:HOME}` is simply an unrecognised token, refused. The
ordering was a grammar-design preference, not a disambiguation requirement.

---

## Changelog

| Date | Author | Change |
|---|---|---|
| 2026-08-09 | architect | Initial draft from the #221 decision relay + first-hand code reading. |
| 2026-08-09 | architect | Reconciled against the #221 body read in full. Three reversals: `${deps.*}` **resolves** in payloads (was: rejected) and needs no new `Usage` variant; the `$${…}` escape **is** ratified and lands in the shared engine (was: declined); the project-tier `[integrations]` open question is an explicit #221 non-goal (deleted). Added D18 never-collapse, D19 non-goals, the `${env.*}` forward-look, tier-parity scenario, and the escape's retroactive-change risk. |
| 2026-08-09 | orchestrator | Owner resolved all three open questions; §12 rewritten as decisions D20–D22 and added to the Decision Summary. Every inline `OQ-N` reference repointed. #303 filed for the render-modifier follow-up (supersedes #73, #175). CLI examples corrected: `--format` is a root flag and must precede the subcommand — trailing form exits 64, verified by execution. |
| 2026-08-09 | security review | C-005 tightened from ASCII-only to Unicode character rules: C1 controls, the bidi/format set (U+202E etc.) and non-ASCII whitespace are now rejected. Untrusted publisher keys reach a terminal hint line, so an ASCII-only rule left Trojan-Source display spoofing open (CWE-451). Fixed now because R-2 makes the grammar loosen-only after publication. Homoglyphs deliberately not addressed — unbounded, and degrades safely under exact-match consumers. |
| 2026-08-10 | orchestrator | **C-017 reversed — patch companions contribute `integrations`.** The owner rejected the discard: patches are just packages loaded into the environment, so no carrier gets a companion-specific rule. The original policy-injection rationale forbade the inert carrier (JSON nobody must read) while permitting `env` (which changes every process in the shell), and the actor it guarded against already owns `PATH`, the patch set and the `[managed]` tier. Amended: D14, C-017, S-013. Attribution is the companion's own `PinnedIdentifier`; emit-once rides the existing `emitted_companions` set; the `integrations_cross(self_view)` gate is applied **once** at the composition, so `--self` stays empty for every contributor. `admitted_binaries`/`admitted_entrypoints` stay discarded — PATH-shaped claims the overlay never puts on PATH. No opt-out ships: `no-patches` already drops an optional companion whole, a required one is required on the same terms its `env` is, and a site admin declines by not declaring. |
| 2026-08-10 | architect | **Pass-through reversed.** The owner replaced D7's "every other `${…}` is emitted byte-identical" with the claim-all rule; [`adr_interpolation_token_grammar.md`](./adr_interpolation_token_grammar.md) / [#303](https://github.com/ocx-sh/ocx/issues/303) landed it as `339383af`. A payload is not exempt from the closed grammar — same scanner, same refusal, exit 65, `$${` the only escape. Amended: the new **Amendment** section; D7, D10, D21, D22; C-007 (split into container + publish-token gates, `first_unknown_placeholder` deleted rather than not-called), C-008 (`AllowedTokens { deps: true, self_env: false }`; `${self.env.KEY}` refused), C-009b (the escape shipped in #303, not here), C-019 (`UnknownPlaceholder`→`UnknownToken`, `UnknownDependencyField`→`UnknownField`, `DisallowedToken` / `UnknownModifier` now reachable), C-020; §3.1 (`${…}` reclassified from payload *content* to container *syntax*), §5.1/§5.2/§5.3/§5.4, S-001, S-014 + new S-014b/S-014c, E-06b/E-07/E-16/E-16d/E-16e/E-17, §8(d) (D-c shipped, and *why* the scoring inverted), R-3 + new R-8, one Constitution deviation added, §11 step 0. Every worked example carrying a foreign token converted to the doubled form. §5.3's discriminating test was **void** (it asserted payload-accepts / env-rejects; both now reject) and is replaced by bare-refused / doubled-accepted on both surfaces. D22's OQ-3 follow-up is **closed** — `:posix` is how a Windows-hosted VS Code setting gets forward slashes. |
| 2026-08-11 | orchestrator | **Both names reversed before the feature ever published — the field is `integrations`, its composed payload key is `payload`.** The owner's objection to `customizations`: the name is borrowed from `devcontainer.json`, where the same key *merges* across features, so it imports an expectation this field refuses — a name that needs a refutation paragraph is doing negative work. `extensions` was rejected as the replacement (it implies OCX dispatches on the field, which it never does; it collides with the VS Code `extensions` list *inside* the payloads; and it is the natural word for a future thing that genuinely extends OCX). D5's `value` → `payload` for the same class of reason: `value` pairs with a `key` that this row does not have, and carries no information. `config` was rejected — `config` is a live CLI noun (`ocx config push`, `config.toml`, the `[managed]` tier) and the OCI image config blob `inspect` already prints, so it would mean three things in one tool's output; it also overclaims a shape OCX does not enforce. Timing was the deciding factor: nothing had published, so the wire break cost nothing. Renamed across 56 files, the module (`metadata/integrations.rs`), `MAX_INTEGRATION_NAMESPACE_BYTES` / `MAX_INTEGRATIONS_BYTES` / `INTEGRATION_TOKENS`, `Integrations` / `IntegrationEntry` / `IntegrationAttribution` / `NamespaceAttribution`, `integrations_cross`, `admitted_integrations`, the `Bundle.integrations` wire key, the availability hint ("N integration namespaces"), every error variant, the acceptance suite, the manual rig, and the docs. |
