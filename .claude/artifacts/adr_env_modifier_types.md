# ADR: Env Modifier Vocabulary — `list` Operator + Forward-Compat Reader

- **Status:** Proposed (panel-reviewed 2026-08-05: spec-compliance + adversarial architect, both PASS-WITH-ACTIONABLES; actionables applied)
- **Date:** 2026-08-05
- **Deciders:** owner + Fable session (design settled in conversation, 2026-08-05; D2 separator
  requiredness amended by review panel — owner may veto, see D2)
- **Resolves:** [ocx-sh/ocx#277](https://github.com/ocx-sh/ocx/issues/277)
- **Research:** [`research_env_list_consumers.md`](./research_env_list_consumers.md)
- **Prior ADRs:** `adr_project_env_declaration.md` (ModifierKind grammar, S9a schema),
  `adr_idempotent_path_move_to_front.md` (idempotency invariant, strategy-enum escape hatch)

## Context

Packages can declare env vars as `path` (unique prepend, OS pathsep) or `constant`
(replace). Option-list variables — `JDK_JAVA_OPTIONS`, `JAVA_TOOL_OPTIONS`, `NODE_OPTIONS`,
`GODEBUG`, `RUST_LOG`, `NO_PROXY`, `CFLAGS` — fit neither: `constant` clobbers the user's
and sibling packages' values, `path` joins with the wrong separator and the wrong
direction. The motivating case is already in our own docs: `user-guide/patches.md` tells
corp-CA users to inject a truststore via `JAVA_TOOL_OPTIONS`, which a patch companion
cannot do declaratively today.

Separately, the current forward-compat posture is asymmetric
(`explore-core`, 2026-08-05): an unknown **sibling field** on a known modifier is silently
ignored (no `deny_unknown_fields` anywhere under `package/metadata/`), while an unknown
**`"type"` tag** is a hard serde error surfacing as
`JSON serialization error: unknown variant …` (exit 65) — correct behavior, unactionable
message, and it arrives at whatever fleet member first touches a newer package.

## Decision Drivers

- Published packages must keep resolving forever (read-path compat is the one hard rule).
- A fleet member meeting a too-new package must fail **closed, attributably, actionably** —
  never run with a silently wrong environment (npm `engines` warn-and-ignore is the
  documented failure mode of the alternative).
- Consumer reality ([research][research]): option-list consumers resolve duplicates
  themselves (last-wins for 5/6 surveyed) — the downstream parser is the merge engine;
  ocx only orders contributions.
- One vocabulary across all three declaration surfaces (metadata JSON, `ocx.toml`, `--env`)
  — `ModifierKind::FromStr`/`Display` round-trip is shared by design
  (`adr_project_env_declaration.md`). The vocabulary has exactly one `FromStr` home;
  hand-rolled copies (the `OCX_ENV` decoder is one today, `env.rs:1125`) are consolidated,
  not multiplied.

## Decision

### D1 — Operator vocabulary: add `list`, a unique-append modifier

| Type | Operator | Formal | Dedup unit | Consumer contract |
|---|---|---|---|---|
| `constant` | replace | `_ ⊕ v = v` | — | takes value whole |
| `path` | unique prepend (move-to-front) | `xs ⊕ v = v : (xs \ v)` | element (pathsep split) | first match wins |
| `list` | unique append (move-to-back) | `xs ⊕ v = (xs \ v) ++ v` | whole contribution (never tokenized) | last match wins |

Shared laws: **later applier wins** (vector-position precedence unchanged, stages 1–6
untouched); **idempotent per contribution** (`f∘f = f` — inherits the "idempotency is a
correctness invariant, not a preference" rule from `adr_idempotent_path_move_to_front.md`);
not commutative (order *is* precedence). The folded string is a **render, not state**:
`[prepend-zone: vector reversed] [ambient] [append-zone: vector order]`.

**The fold algorithm is pinned, position-free, and identical in-process and in every shell
snippet** (panel A3/A4 — the two folds must not disagree on separator-bearing values):

> Wrap the existing value in the separator; replace **every** occurrence of
> `sep + value + sep` with `sep`, repeating until none remains (adjacent duplicates);
> strip the wrapper; append `sep + value` (bare `value` when the result is empty).
> Empty `value` is a no-op — including on an absent key (deliberately unlike `add_path`'s
> empty-insert asymmetry, `env.rs:328`).

Remove-*every* (not first) is what preserves `f∘f = f` against pre-existing duplicates —
the sibling primitive has a dedicated `removes_every_repeated_occurrence` test
(`utility/path.rs:132`) for exactly this. Values that start or end with their own separator
are rejected at all three parse boundaries (they would make flank-matching ambiguous) —
**and re-validated after template resolution** (Codex gate, 2026-08-06: parse-time checks
see authored bytes, but the fold operates on resolved bytes — `${installPath}` with
separator `/` resolves to a `/`-edged value no parse gate can see). The post-resolution
check lives in `EnvResolver` before `Entry` construction and applies equally to decoded
`OCX_ENV` payloads.
Known named boundary (accepted): a contribution equal to the concatenation of two adjacent
prior contributions matches the flank rule and is removed as a span — deterministic,
idempotent, vanishingly rare, and any fix would require tokenizing elements.

Move-to-back (not skip-if-present): nix `makeWrapper --suffix` appends only-if-absent, so
a later layer's identical value keeps its old, losing position — the same shape as
rustup's documented precedence bug. Re-application must *move* the contribution to the
winning end.

Element content is opaque **by contract**: ocx never parses list elements, so "unparsable
element" is a non-category. All future structure arrives as wire-shape changes old serde
rejects (D3), never as syntax inside the string.

### D2 — Syntax: one vocabulary, three surfaces; separator required on the wire

```json
{ "key": "JDK_JAVA_OPTIONS", "type": "list", "separator": " ",
  "value": "-Djavax.net.ssl.trustStore=${installPath}/cacerts", "visibility": "interface" }
```

```toml
[env]
JDK_JAVA_OPTIONS = { type = "list", value = "-ea" }             # separator omitted → " "
GODEBUG = { type = "list", separator = ",", value = "gctrace=1" }
```

```sh
--env "JDK_JAVA_OPTIONS:list=-Xmx2g"        # KEY[:TYPE[:SEP]]=VALUE, SEP omitted → " "
--env "GODEBUG:list:,=gctrace=1"            # split first '='; lhs first two ':'
```

**`separator` is REQUIRED in package metadata (the wire), defaulted only on the
human-facing surfaces** (`ocx.toml`, `--env`). Panel amendment to the conversation default
(owner may veto): the research splits real consumers 3/3 space-vs-comma, a wrong silent
default fails silently (GODEBUG ignores unrecognized settings), and the requiredness
asymmetry is decisive — required-now → optional-later is a free relaxation; optional-now →
required-later breaks published packages. On the wire, where no human is present to be
told, explicit wins; on interactive surfaces, ergonomics win. Constraints everywhere:
non-empty, must not contain `=`. `value` templates (`${installPath}`, `${deps.*}`) work as
on path/constant.

**One separator per key per composition** (Codex gate, 2026-08-06): a human-surface
default is not a blind `" "` — the first `list` entry for a key carrying an explicit
separator **establishes** that key's separator, and a later entry with `None` **inherits**
it (falling back to `" "` only when nothing established one). Two entries for the same key
with *conflicting explicit* separators fail closed at compose time (65, naming both
sources). Without this, a package appending `GODEBUG` with `","` plus a project entry
omitting the separator would produce `gctrace=1 foo=2` — the exact silent-wrong-separator
failure this section exists to prevent, re-entering through layer composition.

Runtime carriage: `Entry` and project `EnvValue` gain `separator: Option<String>` as a
**plain optional field** — `None` *surviving the compose-time separator agreement above*
means "default `" "`" at fold time;
no constructor invariant (panel A11: `Entry`'s fields are `pub` with five struct-literal
production sites — an invariant there is unenforceable theater). "`separator` only with
`type = list`" is validated at the three parse boundaries instead (65 / 78 / 64).
`ModifierKind::List` stays a unit discriminant; `ModifierKind` gains a `JsonSchema` derive
so schemas `$ref` it instead of hand-spelling the vocabulary (discharges the
`project/env.rs:318` `ponytail:` note — "a third variant is an ADR-level event"; this is
that event). The `OCX_ENV` forwarding envelope gains the `separator` field and its decoder
routes through `ModifierKind::FromStr` (today it hand-matches two strings and would
hard-error — or worse, silently default-space a comma list — panel A1/A2).

### D3 — Encoding rule: merge semantics ride the `type` tag, never a field

The `type` tag is the **capability firewall**. Sibling fields may parameterize an operator
(separator = which char); they may never change who wins. Grounding: absent
`deny_unknown_fields`, an old ocx silently drops an unknown field (wrong-direction merge,
no error) but hard-rejects an unknown tag. Therefore future direction variants —
`path_append` (fallback dirs), `list_prepend` (overridable defaults) — are **new tags**,
pre-cleared by `adr_idempotent_path_move_to_front.md`'s strategy-enum escape hatch.

### D4 — Forward-compat reader ships one release before the writer

Release N: `Modifier` gains an `Unknown { type_name }` fallback — parse survives;
`ValidMetadata::try_from` rejects it with package identity, var key, unknown type, and
remedy ("upgrade ocx"), exit 65. `Var::value()` returns `None` for `Unknown` (nothing to
resolve); `ModifierKind` gains **no** `Unknown` variant — the `From<&Modifier>` conversion
becomes `TryFrom`, with the one post-gate call site (`resolver.rs:100`) using
`.expect` on the invariant that `ValidMetadata` ran first (per quality-rust: expect is
legitimate for invariants proven by preceding logic). `Unknown` is never serialized: a
custom `Serialize` errors, and the **published JSON schema is kept byte-unchanged via a
manual `JsonSchema` impl on `Modifier`** (the derive would leak `Unknown` into the
`oneOf` — panel A5; `subsystem-metadata-schema.md`'s custom-impl list gains the entry).

**Implementation trade recorded (panel A6), spiked in R1 stubs before commitment:** the
cheap alternative is `#[serde(other)] Unknown` — a *unit* variant, so the error cannot
name the offending type string. The custom-`Deserialize` route keeps the name but must
survive `#[serde(flatten)]`'s `FlatMapDeserializer` (no public API to buffer unknown
fields without a `Value` round-trip). If the spike shows the custom route regresses
known-type error messages or needs contortions, the recorded fallback is `#[serde(other)]`
with the package + var key still named by `ValidMetadata` context and only the type string
sacrificed. Characterization tests pin existing known-type error text either way.

Reader-first also covers the **project surface**: `ModifierKind::FromStr`'s error message
(shared by `ocx.toml` parsing and `--env`) gains the same remedy text in release N —
a checked-in `type = "list"` on old ocx currently says only "expected `path` or
`constant`" (78), and a mixed-version team hits that *before* any package does (panel A8).
`ocx package deps`' lenient site downgrades its outer "skipping corrupted install" line to
name the real cause when the chain is an unknown modifier type — a newer package is not a
corrupted one (panel D3).

Blast-radius honesty (panel A7): project-tier locks pin digests, so locked projects meet
new metadata only at explicit `ocx update` — but the OCI tier has no lock: a floating-tag
`ocx package install java:21` on a fresh machine meets new metadata with no update action.
The real bound is the reader-first release gap plus the ecosystem-standard social
contract: **adopting a new env type raises the package's effective minimum ocx** —
publishers wait for their fleet floor, exactly as with every ecosystem's engine floor.
Release N+1 then ships `list`.

The `${…}` template region needs no new work: `ValidMetadata::try_from` already runs on
every load site (store reads, post-pull, install-info), so unknown placeholders already
fail closed at read time — the enabling prerequisite for
[#175](https://github.com/ocx-sh/ocx/issues/175) exists today.

## Alternatives Rejected

- **`map` type** (`pair_sep`/`kv_sep`, GODEBUG/RUST_LOG-style): observably identical to
  comma-`list` because those consumers resolve per-key themselves (GODEBUG backward-scan
  last-wins; RUST_LOG most-specific-wins ignores order past specificity). Pure cosmetics;
  [#277](https://github.com/ocx-sh/ocx/issues/277) itself calls it "not a hard requirement".
- **Nested/per-element structure**: inner grammar (JVM `-Xlog:a,b`) belongs to the
  consumer; parsing it means owning per-ecosystem option grammars — the
  "don't own non-domain wire formats" Block-tier. Last-wins consumers make it unnecessary.
- **`position: prepend|append` field on `list`**: violates D3 (semantics in a field — old
  ocx silently inverts precedence); no mainstream first-wins non-pathsep consumer found.
- **`default` (set-if-absent) type**: real prior art (nix `--set-default`; direnv
  `env_default` does **not** exist — stale citation corrected), no concrete ocx request.
  Parked; would be a new tag per D3.
- **Warn-and-skip on unknown types**: an env entry is part of the package's execution
  contract; skipping runs the package in a state the publisher never published, failing
  un-attributably downstream (npm `engines` precedent). Managed-config fail-closed
  doctrine applies.
- **`minOcxVersion` metadata field**: the type enum is already the precise capability
  signal; a parallel version field drifts and lies.
- **Token-level dedup for lists**: requires tokenizing elements → quoting trap
  (JDK quote grammar, NODE_OPTIONS's total absence of one). Contribution-level substring
  is quote-safe by construction.
- **Optional-with-default separator on the wire**: superseded by panel amendment in D2 —
  silent wrong-separator failure mode plus the one-way requiredness asymmetry.

## Consequences

- Cost center is `shell.rs`: one idempotent `export_list` snippet × 10 shells,
  implementing the pinned wrap-replace-strip algorithm. The **separator is untrusted
  text** and routes through each shell's value escaper exactly like the value (panel
  spec-4); list matching is **case-sensitive on every shell** — PowerShell needs `-cne`
  (default `-ne` is case-insensitive), cmd needs a new pattern (its move-to-front matches
  `value<sep>`, structurally blind to last position; two mirrored substitutions inside the
  single-statement constraint is the spike's starting point — panel D5). The Batch
  amendment precedent forbids an "impractical" waiver without proof. `move_to_front` is
  not reusable (hardcoded `std::env::split_paths`); the new primitive is UTF-8 `&str`.
- `--format json` env output gains `"type": "list"` + `"separator"` (skip-if-`None`) —
  additive output-contract change across `env`, `status`, `patch test`, and the inspect
  closure surfaces (changelog subject).
- CI exporters need an append-direction sibling of `ci::prepend_existing`, and the
  `Flavor::write_entry` trait signature grows the separator; the documented A3
  bucket-order gap extends to list entries (accepted, documented, unfixed — precedent).
- Docs must scope honestly per [research][research]: RUST_LOG is "layer,
  most-specific-wins"; NODE_OPTIONS values must not contain whitespace (no escape exists);
  `-I`/`-L` are first-wins. JDK quoting is handled by the JVM, not by ocx.
  `in-depth/project.md:160,172` currently states "same `constant`/`path` typing" and
  "later stage overrides earlier for the same key" — both false once `list` exists.
- Companion-patch overlay: a `List` entry is treated like `Path` at the
  project-shadowing debug filter (`resolve.rs:244` `log_project_env_shadowing` — a log
  filter, not a dedup gate); a list contribution shadows nothing, so no constant-collision
  logging applies.
- [#265](https://github.com/ocx-sh/ocx/issues/265) `unset` is a **key-set directive**
  (`unset = ["PYTHONPATH"]`), not a value modifier — outside this vocabulary, unaffected.
- A literal `${` in a list value (Spring/logback passthrough) still trips the publish
  gate — pre-existing tension; the answer, if ever needed, is an authoring-surface escape
  (`$$`), not reopening the region.

<!-- refs -->
[research]: ./research_env_list_consumers.md
