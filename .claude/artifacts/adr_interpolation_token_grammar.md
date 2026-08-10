# ADR: Unified Interpolation Token Grammar

## Metadata

**Status:** Accepted (post-hoc — implementation already merged on `hex/interpolation-token-grammar`)
**Date:** 2026-08-09
**Deciders:** Michael Herwig (architect: opus)
**GitHub Issue:** [ocx-sh/ocx#303](https://github.com/ocx-sh/ocx/issues/303) — unify the OCX interpolation token grammar
**Consumes:** [ocx-sh/ocx#73](https://github.com/ocx-sh/ocx/issues/73) (closed — `${self.installPath}`), [ocx-sh/ocx#175](https://github.com/ocx-sh/ocx/issues/175) (closed — scoped env)
**Consumed by:** [ocx-sh/ocx#221](https://github.com/ocx-sh/ocx/issues/221) (`customizations`) — **its stated contract changes; see D3 and Consequences**
**Research:** [`research_interpolation_token_grammar.md`](./research_interpolation_token_grammar.md) (Part 0 = verified discovery of the live code), [`research_interpolation_capability.md`](./research_interpolation_capability.md)
**Related ADRs:** [`adr_entrypoint_args_interpolation.md`](./adr_entrypoint_args_interpolation.md) (D1/D3/D6 — the capability gate this extends), [`adr_deps_name_interpolation.md`](./adr_deps_name_interpolation.md) (direct-deps-only scoping), [`adr_env_modifier_types.md`](./adr_env_modifier_types.md) (D3/D4 — forward-compat asymmetry precedent), [`adr_declared_binaries_metadata.md`](./adr_declared_binaries_metadata.md) (`bin_scan`, a downstream consumer of the `${installPath}` literal)
**Tech Strategy Alignment:**
- [x] Follows the Golden Path in `product-tech-strategy.md` — Rust 2024, no new language, no new runtime dependency
**Domain Tags:** metadata · package-manager · cli · wire-format
**Reversibility Classification:** **One-Way Door — Medium.** OCX claims the whole `${…}` space (D3), so the world stays closed and *no published package can ever contain a token OCX does not recognise*. That makes every future grammar **addition** — a new root, a field under `self.`, a new render modifier — purely additive and safe: it only makes previously-rejected documents publishable, and an older reader still fails closed. Nothing needs reserving, and there is no root freeze. D14 softens it further: an ocx that meets a token it does not know can still *read* the package and refuses only on use, so a grammar addition costs later readers nothing until they try to run it. The irreversible parts are narrow: the meaning of the four recognised token bodies, the `$${` escape semantics, and the claim rule itself in the *tightening* direction. Loosening later is cheap; tightening is not.

> **Sourcing caveat.** This session had no shell or GitHub access, so issues #303 and #221
> were not re-read directly, and the crate survey underlying Axis A/Deviation 1 is the
> **recorded** table in `research_interpolation_token_grammar.md` §2.1, re-checked against the
> reduced requirement rather than freshly searched. The claim-everything rule in D3 is taken
> verbatim from the owner directive relayed in this session. Any slice-numbering divergence
> from the issue body is flagged explicitly in **Slice Boundaries**.

---

## Context

OCX interpolates `${…}` tokens in exactly two places today: env-variable `value`
strings and entrypoint `args` elements. The vocabulary is two tokens —
`${installPath}` and `${deps.NAME.installPath}` — and the world is **closed**: any
other `${…}` sequence is rejected at publish with `TemplateError::UnknownPlaceholder`
(exit 65) by `validation::first_unknown_placeholder`.

**The closed world is kept.** This ADR grows the vocabulary and adds an escape; it does not
open the grammar to anybody else's tokens. Three things force the work:

1. **#73 / #175 (both closed) landed a namespace design that was never implemented.**
   `${self.installPath}` reads correctly next to `${deps.NAME.installPath}`;
   `${installPath}` reads like a global. `${self.env.VAR}` gives a package a way to
   reference its own declared environment instead of repeating a path three times.
2. **#221 (`customizations`) needs OCX to carry a payload it does not own.** A VS Code
   settings blob contains `${workspaceFolder}` and `${localEnv:HOME}` — byte sequences that
   share OCX's `${…}` delimiter. Today those strings are **unpublishable**, and so is the
   escaped form. **The escape (D2), not pass-through, is what unblocks #221**: the payload is
   authored `$${workspaceFolder}`, and OCX emits the literal. The authoring cost of that is
   real and is stated once, in D3 and Consequences.
3. **Windows JSON payloads need forward slashes.** `C:\Users\x` is not valid inside an
   unescaped JSON string; `C:/Users/x` is. A render modifier (`:posix`) is the seam.

There is no shared implementation to extend. `adr_entrypoint_args_interpolation.md` D6
states "one tokenizer scans `${…}` segments and classifies them" — **that type was never
built**. The live code is three independent mechanisms (research §0.1):

| # | Mechanism | Location |
|---|---|---|
| 1 | `str::contains` + `str::replace` on the literal `"${installPath}"` | `template.rs:174`, `:209` |
| 2 | `DEP_TOKEN_PATTERN` regex | `slug.rs:25-29`, driven from `template.rs:226` and `validation.rs:229` |
| 3 | `UNKNOWN_TOKEN_RE` catch-all (`\$\{[^}]*\}`), publish-time rejection only | `validation.rs:35-36`, `:42-48` |

Two further consumers read the literal `"${installPath}"` directly and are invisible to
the token machinery entirely: `template::classify_install_path_rooted_dir` (feeds
`bin_scan`'s executable auto-scan) and `libc_lint::resolve_scan_scope`
(`libc_lint.rs:219`, `:236`, `:239`). Research §0.5 named only the first.

This ADR replaces all five sites with one scanner and one grammar.

---

## Decision Drivers

- **OCX's own namespace must not be hostage to other tools' vocabularies.** A design that
  passes unrecognised `${…}` through requires OCX to reason about what VS Code, devcontainers,
  Kubernetes and every future consumer spell, and to freeze its own root set forever so it
  never collides with them. That imports knowledge of the user's toolchain into OCX and
  raises complexity for a benefit OCX cannot verify. Claiming the whole space removes the
  coupling and the freeze together. **This is the owner's directive and the primary driver**
  (Axis C).
- **A closed world makes every future grammar addition additive.** Because an unrecognised
  `${…}` can never be published, adding a root or a field later cannot change the meaning of
  anything already in a registry. The permissive design had the opposite property: a fifth
  root would have re-claimed bytes some published package was already passing through.
- **Published metadata is immutable.** A token accepted today resolves the same way in
  five years. Getting the *meaning* of an accepted token wrong is unfixable.
- **Offline-first breaks the "the author will notice" assumption.** Resolution happens
  on a different machine, months after publish, with nobody watching. Every tool that
  tolerates silent-empty or silent-passthrough does so because the edit-and-run loop is one
  person, one minute. OCX's is not (research §2.5). Hence: **no `${…}` ever resolves to
  silence** — not to an empty string (D11), not to unexamined literal text (D3).
- **The capability gate is the placement authority.** `Usage` → `AllowedTokens` already
  decides which tokens are legal where. New tokens slot into it; a second mechanism
  would be a drift generator.
- **Modifiers are a rendering seam, not a template language.** No conditionals, no
  loops, no string functions. The moment a modifier carries free text, OCX inherits
  devcontainer.json's unresolved colon-ambiguity bug (research §1.1).
- **KISS / YAGNI.** The grammar has four leaves. Importing a template engine to parse
  four leaves is an innovation-token spend with nothing bought.

---

## Decision Summary

| # | Decision |
|---|---|
| D1 | One hand-written single-pass scanner replaces all five recognition sites |
| D2 | Escape is `$$` **immediately followed by `{`**; a bare `$$` is ordinary text |
| D3 | **OCX claims every `${…}`.** There is no foreign-token concept and no pass-through path; an unrecognised token is a hard error (exit 65), and `$${` is the only way to emit a literal `${…}` |
| D4 | `${self.installPath}` is an exact alias of `${installPath}`; the bare form is never removed and never deprecated in code |
| D5 | Render modifiers `:native` (default) and `:posix`; user-facing vocabulary is **variable *type*** vs **token *render modifier*** |
| D6 | `${self.env.VAR}` is legal in **env values only**, scoped to **earlier-declared vars of the same package** — no second pass, no topological sort, no cycle detection |
| D7 | `${self.env.KEY}` with `KEY` declared more than once earlier is a hard error, not a pick |
| D8 | `${self.env.VAR}` resolution is **surface-independent**: it sees the package's own declared vars regardless of visibility or `carrier_crosses` |
| D9 | `AllowedTokens` gains `self_env: bool`; `self.installPath` inherits `installPath`'s always-allowed status |
| D10 | `classify_install_path_rooted_dir`, `libc_lint::resolve_scan_scope` and `first_unknown_placeholder` all route through the scanner — one recogniser, no literal `"${installPath}"` left in the tree |
| D11 | A recognised token with no value is a hard error (exit 65), never an empty string |
| D12 | `UnknownPlaceholder` is **renamed** `UnknownToken` and its job is kept; `UnknownDependencyField` generalises to `UnknownField`; no `MalformedToken` variant is minted; the install-path `${` injection defence is deleted as structurally unnecessary |
| D13 | The unknown-token error message carries either a **root suggestion** or the **escape hint** — the only diagnostic that survives; there are no publish-time warning classes |
| D14 | **Refusal is scoped to resolution, not to reading.** Token validation leaves `ValidMetadata::try_from`; it becomes an explicit publish gate plus the resolver's own failure, so every compose/execute path refuses and every read-only path shows the token verbatim |

---

## Trade-off Analysis

### Axis A — recognition strategy

Re-argued against the **reduced** grammar. Under D3 there is no claimed/foreign split, no
verbatim-emit path, and no root-run-before-body ordering subtlety: the scanner is one
left-to-right loop with three branches, and every `${…}` either parses into one of four
bodies or errors. That shrinks the artifact from the ~120–180 lines the permissive design
needed to roughly **80–130 lines**, and it moves the axis.

Criteria, deliberately **unweighted**: **correctness under escape-before-match** (the escape is
unimplementable without it), **auditability** (this parses published wire data),
**dependency cost**, **effort**.

| | A1 — hand-written single-pass scanner | A2 — regex + escape pre-pass | A3 — `winnow` combinator | A4 — template crate (`shellexpand` / `minijinja`) |
|---|---|---|---|---|
| Escape correctness | **Structural.** The `$` position is examined before the `${` branch; `$${` can never be seen as `${`. | Requires a pre-pass. No byte sentinel exists that a publisher cannot author, so it needs a *positional* mask (record the indices of escaped `${`, match, discard matches at masked indices). **Untouched by the reduction** — this is the reason A2 lost and it is unrelated to pass-through. | Structural (same left-to-right property). | None ship an escape at all (research §2.1). |
| Auditability | ~80–130 lines, no hidden backtracking, every branch reachable from a unit test. | One regex plus a masking scheme plus a body parser; the mask/regex interaction is the bug surface. | Combinator tree for a grammar with **three branches** — the crate's error and backtracking model must still be learned to review it. | Opaque; behaviour is the crate's, changes on upgrade. |
| Dependency cost | Zero. | Zero (`regex` already present). | Zero *link* cost (`winnow` is already in `Cargo.lock` via `toml_edit`) but a new **direct** dependency declaration. | New direct dependency; `minijinja` imports Jinja's entire syntax to serve four leaves. |
| Effort | Low–moderate; lower than under the permissive design. | Low to write, high to convince yourself it is right. | Moderate — unchanged, because the crate's learning cost does not shrink with the grammar. | Low, then permanent. |
| Risk | Hand-rolled parser on a published wire format (Deviation 1) — **materially smaller than before**: the one path with no battle-tested precedent (research §1.4's "leave the bytes alone" rule) is not built at all. | A positional mask threaded through a regex: correct-but-coupled, and every future grammar addition must not break the masking contract. *(Not a catastrophic-backtracking risk — Rust's `regex` is a finite-automaton engine with linear-time guarantees.)* | Over-machinery, and **more so than before**: generality is the thing a three-branch loop needs least. | Claims tokens it does not know how to reject; `shellexpand` cannot express `namespace.path:modifier` at all. |

**Chosen: A1.**

**How the reduction moved the axis.** The A1-vs-A3 tiebreak was already auditability over
generality. Removing the pass-through path removes the only part of the grammar where a
combinator's structure earned anything — the interaction between "recognise", "reject" and
"emit verbatim". What is left is a loop over three branches, which is the shape a combinator
library is *worst* value for. A3 is therefore **less** attractive than it was, not more, and
the cost of choosing A1 over it is correspondingly lower.

**What A1 still costs, stated rather than argued away.** `winnow` remains a library that
implements the parsing requirement, at zero link cost, scoring *Structural* on escape
correctness exactly as A1 does. Choosing A1 accepts a Block-tier `quality-core.md` deviation
(hand-rolled parsing of an external wire format) that A3 would have avoided. A4 fails on
capability rather than taste: re-checked against research §2.1's table under the *reduced*
requirement — closed vocabulary, hard error on unknown, `$$`-escape, `:modifier` suffix — the
pass-through requirement that eliminated most crates is gone, and `subst` now matches on the
error-on-unknown axis, but **no surveyed crate ships a `$$`-style escape or a `:modifier`
suffix**, so all seven still fail. (Recorded table, not a fresh survey — see the sourcing
caveat.)

**Reversibility:** high. The scanner is an internal module with no stable surface; swapping
A1 for A3 later is a contained refactor behind the same contracts.

### Axis B — `${self.env.VAR}` resolution mechanism

Unaffected by the reversal. Weighted criteria: **surface-independence** (×3 — the same
metadata must resolve to the same bytes under `ocx env`, `ocx env --self`, the launcher, and
`inspect`), **new machinery** (×3), **expressiveness** (×1), **failure modes** (×2).

> **Correction to an earlier draft, retained.** This axis once scored B2's machinery as "a
> per-package `Vec<Entry>` the composer's loop already builds". No such vec exists. The
> `entries: &mut Vec<Entry>` threaded into `emit_interface_vars` and `emit_root_vars`
> (`composer.rs:535-587`) is one **global, cross-package, surface-gated** accumulator.
> Reading `${self.env.*}` out of it would violate D6.1 (it holds other packages' vars) and
> D8 (it holds only *crossing* vars) simultaneously — which is B1's rejected failure mode
> wearing a different hat.

| | B1 — args-only, read the composed `entries` | B2 — declaration-order scoping over the package's own vars | B3 — two-pass within a package | B4 — topological sort + cycle detection |
|---|---|---|---|---|
| Surface-independence | **Fails.** The launcher composes `self_view=true`, where `carrier_crosses(vis, is_root=true, self_view=true)` is `has_private()` — an INTERFACE-only var of the root is **not** in `entries`. The same token resolves differently per surface. | **Holds.** Resolution reads only the metadata document. | Holds. | Holds. |
| New machinery | A new resolver input threaded from `exec.rs`. | **Three pieces, none of which exist today.** (1) A new *per-package* `Vec<Entry>` accumulator — the composer's `entries` vec is global and gated, so it cannot serve. (2) `EnvResolver` gains order-dependent state: today it takes one `&Var` and has no view of the array. (3) The composer must resolve **every** declared var of a package into that private accumulator *before* gating, because D8 requires a crossing var to see a non-crossing earlier one. Lookup itself is then a linear scan. | A second loop plus a "deferred" bucket plus a rule for what pass 2 may reference. | A dependency graph over vars, a topo sort, and a cycle detector — none of which exist anywhere in composition today (research §2.7). |
| Expressiveness | Args only; env values unserved. | Forward references are illegal. | One level of indirection; a chain of three needs pass 3. | Any acyclic reference graph. |
| Failure modes | Silent surface divergence — the worst kind. | Forward reference → `UndefinedSelfEnvRef` (65). Self-reference is the same error, for free. | Ambiguous: what does pass 2 see of pass 2? | Cycle → a new error class; cycle detection is a thing that can be wrong. |
| Acyclicity | n/a | **By construction.** A var may only reference strictly earlier vars, so a back-edge is unrepresentable. | Needs a rule. | Needs a detector. |

**Chosen: B2, restricted to env values (D6).** B2 is the only option where the cycle
question does not arise — not "we checked for cycles", but "a cycle cannot be written".
Its machinery cost is real and is paid in slice S2 alone; B4's is the same three pieces
*plus* a graph, a topological sort, and a cycle detector. The precedent is POSIX shell,
`.env` files, and systemd: earlier-wins, no forward references, no graph. Entrypoint args
are deliberately excluded in v1 (the same layer argument as
`adr_entrypoint_args_interpolation.md` D3: env values are the *declarative composition*
surface, args are *imperative invocation* parameters); widening later is additive.

**Reversibility:** B2 → B4 is additive (B4 accepts a superset of B2's documents). B2 →
B1 is not, and B1 is rejected on correctness anyway.

### Axis C — what OCX does with an unrecognised `${…}`: **settled by owner directive**

This axis had three live options in earlier drafts — keep the closed world, blanket
pass-through, or a reserved-root split where OCX claims a fixed set of roots and passes
everything else through byte-identical. The last was chosen, and the owner has **reversed it**:

> "you want to have like predefined known variables that are passed through? No, don't do
> that. This brings knowledge of users into OCX and raises the complexity drastically.
> Everything but that syntax is considered OCX environment variable expansion and only the
> dollar dollar escape sequence can escape"

The reasoning is the design driver: a pass-through rule makes OCX's own namespace choices
hostage to what other tools spell. To pass `${workspaceFolder}` through safely, OCX has to
know it exists; to keep passing it through safely forever, OCX has to freeze its own root set
so it never re-claims a byte sequence somebody is already relying on. Both are couplings to
somebody else's vocabulary, and neither buys OCX anything it can verify.

**Settled: OCX claims every `${…}`** (recorded in D3). The rejected alternatives are recorded
here so the decision is legible, not re-argued: blanket pass-through is `envsubst`'s
documented corruption case (research §1.4); the reserved-root split is the same failure in
smaller scope plus a permanent freeze; both make a typo outside the claimed set publish
silently into a digest-pinned artifact.

Three consequences follow directly and are recorded where they land: the Kubernetes
CRD-pruning risk class — error becoming silence — **does not arise at all** (Migration);
the root set needs no reservation and no freeze (Reversibility); and #221's payloads must be
authored with escapes (D3, Consequences).

### Axis D — an unterminated `${`

A `${` with no `}` before end of value. Today this is legal and inert: `UNKNOWN_TOKEN_RE`
requires a `}`, so it never matches. Under D3 the question is live again, because `${` is now
unambiguously an OCX sigil.

| | D-1 — literal text | D-2 — hard error | D-3 — literal text + publish warning |
|---|---|---|---|
| Consistency with D3 | Weaker: a `${` sigil is treated as text. Defensible — *no token exists*, so nothing resolves silently wrong; the bytes are exactly what the publisher wrote. | Strongest: an unterminated sigil is malformed input. | Same as D-1. |
| Effect on the accept set | **None.** Every document that publishes today still publishes. | **Shrinks it.** This would be the only shape in the whole ADR that is legal today and rejected after. | None. |
| Precedent | Every surveyed tool treats an unterminated delimiter as text. | None found. | — |
| Cost | Zero — it is the R3 fallback. | One arm, plus a lookahead-to-EOF special case. | A warning class, which D13 otherwise deletes entirely. |
| Publisher feedback on a truncated token | None. Accepted risk. | Immediate. | Immediate, ignorable. |

**Chosen: D-1 — literal text.** The deciding property is that it keeps the migration story
exactly one sentence long: *the publish accept-set only grows, with one resolution change*
(Migration). Buying publisher feedback on a rare truncation by making the only
currently-legal-now-rejected shape in the ADR is a bad trade, and D-3 would resurrect the
warning class D13 deletes. The residual risk — a publisher who writes `"${installPath"` gets
silence — is recorded in Risks and is bounded: the value contains the literal bytes the
author typed, so nothing resolves to a wrong value.

---

## Decisions

### D1 — one hand-written single-pass scanner; three mechanisms and two literal readers collapse into it

A new module `crates/ocx_lib/src/package/metadata/template/scanner.rs` owns recognition.
It exposes a pure, filesystem-free, allocation-light classification of a `&str` into a
sequence of `Segment`s:

```
Segment::Literal { text: &str, at: usize }   // emit verbatim — ordinary text, and the ${ a fired escape produced
Segment::Token(Token)                        // a recognised token, fully parsed
```

`Token` carries the parsed shape (`InstallPath`, `SelfEnv { key }`, `Dep { name }`)
plus an optional `RenderModifier` and the raw source text (for error messages). A `${…}`
that does not parse into one of the four recognised bodies is an **error returned from the
scan** — never a `Literal`.

`Dep` carries **no `field`**: `installPath` is the only leaf a dependency exposes, so a
field that can hold exactly one value is a field that encodes nothing. An unrecognised leaf
(`${deps.cmake.version}`) is rejected in `parse_shape` as `UnknownField` rather than parsed
into a shape and refused later.

Everything downstream drives this one function:

| Consumer | Today | After |
|---|---|---|
| `TemplateResolver::resolve` | `str::replace` + `captures_iter` | scan → gate → substitute |
| `validation::validate_env_tokens` | `DEP_TOKEN_PATTERN` loop + `UNKNOWN_TOKEN_RE` | scan → check refs |
| `validation::validate_entrypoint_args` | `disallowed_dep_token` + `first_unknown_placeholder` | scan → gate |
| `template::classify_install_path_rooted_dir` | `strip_prefix("${installPath}/")` | scan → shape match (D10) |
| `libc_lint::resolve_scan_scope` | `contains`/`==` on the `"${installPath}"` literal | scan → shape match (D10) |

`UNKNOWN_TOKEN_RE`, `first_unknown_placeholder` and `disallowed_dep_token` are **deleted**.
`DEP_TOKEN_PATTERN` is deleted from `slug.rs`. The scanner validates the dep-name segment
by `DependencyName::try_from`, which applies `SLUG_PATTERN` **and** `SLUG_MAX_LEN` — so the
scanner accepts exactly the names `DependencyName` accepts, and neither the character class
nor the length bound can drift. (Reaching for `SLUG_PATTERN_STR` directly would re-check the
pattern and silently drop the length bound.)

**Rationale.** Single-pass is not a performance argument, it is a correctness one: output
bytes are never re-examined, so bytes that came from the filesystem can never be
re-interpreted as a token (see D12). The escape is only expressible in a left-to-right
scan. And five recognisers is five chances for the sixth grammar addition to update four
of them.

### D2 — the escape is `$$` immediately followed by `{`; a bare `$$` is ordinary text

On encountering `$` at index *i* — these are rules R1–R3 of **The Scanner Specification**,
which is the normative statement; this is the `$`-local view of it:

1. If `input[i..]` starts with `$${` → emit the two bytes `${` to the output and advance
   *i* by **3**. The emitted `${` is output, never rescanned.
2. Else if `input[i..]` starts with `${` → attempt token recognition.
3. Else → emit `$` and advance by 1.

Consequences, all deterministic and left-to-right:

| Input | Output |
|---|---|
| `$${installPath}` | `${installPath}` (literal) |
| `$$` | `$$` (untouched) |
| `$$foo` | `$$foo` (untouched) |
| `$$${installPath}` | `$` + escape → `$${installPath}` |
| `price: $$5` | `price: $$5` |

**Rationale, and a deliberate divergence from precedent.** GNU Make, Bazel and Docker
Compose collapse `$$` → `$` *unconditionally* (research §2.3). OCX must not: its values
routinely carry `$` that means nothing to OCX — a shell fragment in a `constant`, a regex,
a literal price, a `$`-bearing password. Collapsing every `$$` would silently corrupt them,
and unlike Make there is no author present to notice. Kubernetes' scoped `$$(VAR)` escape
had exactly the mirror-image bug — a bare `$$` collapsed when it should not have
([kubernetes#101137](https://github.com/kubernetes/kubernetes/issues/101137)). Narrowing the
escape to `$${` shrinks that class to its **irreducible** minimum: an escape must consume
*some* byte sequence, and `$${` is the shortest one that can express the rule at all.

**The escape carries more weight under D3 than it did before.** It is not a corner-case
affordance: it is the *only* way to put a literal `${…}` into published metadata, so every
foreign payload routes through it. What it does not buy is unchanged and worth restating:

1. **`$${` in a payload OCX does not own is still rewritten.** In a shell fragment `$$` is
   the PID, so `mkdir /tmp/$${BUILD_ID}` publishes and resolves to `mkdir /tmp/${BUILD_ID}` —
   PID-then-variable silently becomes variable (S-021). No warning fires (D13).

   **This cost is live, not hypothetical, and it is not only a runtime one.** Terraform's
   string templates use exactly this spelling — `$${` for a literal `${`, `%%{` for `%{`
   ([HCL string templates](https://developer.hashicorp.com/terraform/language/expressions/strings)),
   the one shipping ecosystem that picked the same three-byte brace-conditional form rather
   than an unconditional `$$`→`$` (Make, Bazel, Compose) or a different sigil (systemd `%%`).
   [hashicorp/terraform#27895](https://github.com/hashicorp/terraform/issues/27895) is open
   and reports the consequence: doubling `$` inside an embedded shell script — cloud-init or
   `user_data` rendered through `templatefile()` — breaks `shellcheck` and other static
   analysis of the embedded fragment. So the escape degrades *tooling* on the payload, not
   just its shell semantics. Accepted with the rest of S-021: the alternative is a
   pass-through rule, which D3 rejects for a stronger reason.
2. **Escaping a foreign token yields bytes the downstream consumer still expands.**
   `$${workspaceFolder}` resolves to `${workspaceFolder}`, which VS Code then expands
   normally (S-022) — which is exactly what #221 wants. The escape defends against **OCX**,
   not against the consumer OCX is delivering to; there is no OCX-side spelling of "literal
   `${workspaceFolder}` at the consumer", because that is the consumer's own escaping problem.

**This is one of two behaviour changes to already-published packages** (the other is the
post-hoc resolved-value budget, unrelated to the escape — see **Migration**). Today
`$${installPath}` resolves to `$<content-path>` — a plain `str::replace` match at index 1
(research §0.3). After this change it resolves to the literal `${installPath}`. See
**Migration**.

### D3 — OCX claims every `${…}`; an unrecognised token is a hard error

There is no foreign-token concept, no reserved-root set, no allowlist of anyone else's
vocabulary and no pass-through path. Every `${…}` in an env value or an entrypoint arg is an
OCX interpolation token. It parses into one of exactly four bodies —

```
installPath      self.installPath      self.env.KEY      deps.NAME.installPath
```

— or it is a **hard error**, exit 65, message naming the offending token (D12, D13) — quoted
through `scanner::for_message`, which truncates the echo at 120 bytes with a `…` marker and
runs `str::escape_debug` over it first (CWE-117/150 forged-diagnostic defence), so "naming the
token" is exact only for the common case of a short, control-free token. It is not literal text
and it is not an empty expansion. *When* that error fires is D14's subject: at publish, and on
every path that resolves a value — never on a read-only path.

**`$${` is the only exit.** A publisher who needs the bytes `${foo}` to reach a consumer
writes `$${foo}` (D2). That is the whole mechanism; there is nothing to configure and nothing
for OCX to know about the consumer.

**Why this over claiming a namespace.** Recorded in full at Axis C. In one line: passing
tokens through requires OCX to model somebody else's vocabulary and to freeze its own root
set forever, and it converts a typo outside the claimed set into silent literal text inside a
digest-pinned artifact.

**What the closed world buys, beyond the coupling argument.** Because an unrecognised `${…}`
can never be published, adding a root or a field or a modifier in a later release cannot
change the meaning of anything already in a registry: it only makes previously-rejected
documents publishable, and an older reader still fails closed the moment it tries to *use* one
(D14). So the fail-closed property that the withdrawn reserved-root design bought with a
reservation now comes free, for *every* future token, and there is nothing to reserve and no
set to freeze. **The owner-facing decision on a reserved `ocx` root is withdrawn.**

**There is no layer caveat.** The withdrawn design had to distinguish scanner-layer
byte-identity from resolver-layer behaviour, because a passed-through token could still be
joined under the install path by a `path` var. Under D3 nothing passes through, so that
distinction is gone. Escaped bytes are still ordinary resolved bytes and are still acted on
by the layers above — `"$${workspaceFolder}/bin"` in a `path` var is relative and is joined
under the install path (S-023, C-006) — but that is the plain behaviour of a relative path
value, not a qualification on a pass-through promise.

**#221 note, stated once and not relitigated elsewhere.** #221 states as a hard constraint
that a VS Code `${workspaceFolder}` or a devcontainer `${localEnv:HOME}` inside a
`customizations` payload survives byte-identical. **Under D3 it does not.** Every such payload
must be authored `$${workspaceFolder}` / `$${localEnv:HOME}`, and an unescaped one is a
publish error. That is a real authoring burden proportional to the number of tokens in the
payload, and it falls hardest on generated or copy-pasted devcontainer blobs — a VS Code or
devcontainer settings block pasted unmodified will not work. **#221's stated byte-identical
guarantee is superseded by this ADR and its issue text needs amending.**

**One rule, no per-surface exception.** The capability gate (D9) carries *placement* policy —
which tokens are legal where — and it deliberately gains **no** unknown-token policy axis.
There is no `Usage::Customizations` and no surface on which an unrecognised `${…}` is emitted
verbatim. The single-rule option was chosen over a per-surface one because a second policy is
a second thing every future reader of this grammar must hold in their head, and the escape
already expresses the same intent locally and visibly in the source bytes.

#221 also no longer owns the `$${…}` escape — it ships here, in slice S1, because it is
inseparable from the scanner (see **Slice Boundaries**). Whether #221 wants an authoring
affordance that escapes a pasted blob is #221's call, not this ADR's.

### D4 — `${self.installPath}` is an exact alias; `${installPath}` is permanent

Both tokens resolve to the same bytes through the same code path. The bare form is:

- **never removed** — published packages depend on it;
- **never deprecation-warned in code** — a warning on the dominant existing form is noise
  on a common benign state;
- **demoted in documentation only** — the reference and user guide use
  `${self.installPath}` in every example and describe `${installPath}` once, as the
  original spelling that keeps working.

Precedent: Bazel's `$(location)` versus `$(execpath)`/`$(rootpath)` — both work forever,
docs steer to the successor, no deprecation machinery
([Make Variables](https://bazel.build/reference/be/make-variables)). This also matches
OCX's pre-1.0 no-shim doctrine: there is nothing to shim, because nothing is being removed.

Every future *package-referent* token is a field under `self.` (`${self.platform.os}`,
`${self.version}`) or under `deps.NAME.` (`${deps.x.version}`). A token whose referent is
*not* the package — `${project.root}` (see the proposed `adr_project_toolchain_links.md`), the
invoking workspace, the ambient host environment — takes a new root, which under D3 is a
free, additive change in the release that defines it.

### D5 — render modifiers `:native` and `:posix`; vocabulary separates *type* from *render modifier*

A recognised token may carry one optional modifier suffix from a **closed enum**:

| Modifier | Meaning |
|---|---|
| `native` | The resolved path in the host's native form. Identical to omitting the modifier. |
| `posix` | On Windows: `native`, then every `\` replaced by `/`. Drive letter preserved (`C:\Users\x` → `C:/Users/x`). On every other host: **the identity function.** |

- The modifier never carries free text. `:posix` and `:native` are the entire vocabulary.
  This is what makes OCX structurally immune to devcontainer.json's open colon-ambiguity
  bug ([devcontainers/spec#565](https://github.com/devcontainers/spec/issues/565)) — a
  modifier that can never contain a URL can never truncate one. **Record this as a designed
  invariant: a future modifier that takes an argument requires a new ADR, not a new arm.**
- `posix` is host-conditional, not an unconditional slash flip. A POSIX filename may
  legitimately contain a backslash; an unconditional flip would corrupt it. CMake resolves
  the same ambiguity the same way — "native refers to the host platform"
  ([cmake_path](https://cmake.org/cmake/help/latest/command/cmake_path.html)). Resolution
  runs on the host, for the host's own installed package, so host *is* consumer.
- Rendering composes **after** `dunce::simplified`, never instead of it. Neither modifier
  may ever emit a `\\?\` verbatim prefix, and the install path must not route through
  `std::fs::canonicalize` before rendering (research §3.6).
- The render function takes the host **explicitly** — `render(s, RenderModifier, Host)` — so
  both `:posix` legs are testable on any CI host (C-014, C-015). `TemplateResolver` carries a
  `Host` defaulting to the real one, overridable through the canonical test-only seam so the
  *resolver-level* modifier contracts (C-013, C-017) also run on both legs. The seam covers
  `render`'s host argument only; `dunce::simplified` stays a real `cfg(windows)` call, which
  is why C-016 is Windows-only.
- UNC paths and verbatim prefixes are **explicit non-goals**, not silently-mishandled
  cases. OCX's input space is paths it generated itself: `$OCX_HOME`-rooted,
  digest-sharded, ASCII-slugified.
- A modifier is legal on the three **install-path** bodies — `${installPath}`, its alias
  `${self.installPath}`, and `${deps.NAME.installPath}` — and refused on `${self.env.KEY}`
  with a dedicated `ModifierNotApplicable`. The gate is one predicate,
  `TokenShape::takes_modifier`, and it is judged before the suffix is resolved against the
  modifier set, so `${self.env.K:POSIX}` reports the modifier as inapplicable rather than as
  a typo the publisher could "fix" by writing `:posix` — which is refused too.

  Two reasons, in order:

  1. **Narrowing is the reversible direction.** This grammar is permanent on published
     metadata. Widening it later breaks nothing; narrowing it later would refuse documents
     already in registries. A rule with no known use case ships narrow.
  2. **The flip is meaningless-to-corrupting off a path.** `render` rewrites *every* `\` in
     the resolved value, and `self.env.KEY`'s referent has no static type — a var holding a
     regex, a compiler flag, or a `list` loses backslashes it meant to keep. OCX cannot
     type-check the referent, so it declines to offer the axis.

  Path *composition* through `self.env` keeps the render axis: the modifier goes on the
  token in the **declaring** var (`SDK_ROOT = ${installPath:posix}/sdk`) and every
  `${self.env.SDK_ROOT}` inherits the rendered form. Only *re-rendering* an already-rendered
  value into the other form is lost, which was never coherent.

  > Superseded: this bullet previously read *"A modifier is legal on **every** recognised
  > token […] One rule beats a per-token applicability table"*. The applicability table
  > turned out to be one `const fn` with three arms, so its cost never materialised, and the
  > argument for it never weighed the one-way-door asymmetry above.

**Vocabulary (hard problem 6).** `env::modifier::Modifier` is the wire-visible `"type"`
tag (`path`/`constant`/`list`) and answers *how does this value combine with an existing
one?* — a **combination** axis. `:posix` answers *how is this resolved value rendered?* —
a **rendering** axis, never serialized.

- User-facing docs call the wire field the variable's **type** (which is its literal wire
  spelling) and call `:posix` a **render modifier**. Docs never call `type` a "modifier".
- Rust-side, the new type is `RenderModifier`, distinguishable from `Modifier` /
  `ModifierKind` at every call site. Renaming `env::modifier::Modifier` → `VarKind` is
  the cleaner end state and is permitted (internal names carry no stability), but it is a
  mechanical rename across many files, orthogonal to this ADR, and is **deliberately not
  bundled** — recorded as a follow-up, not a gate.

### D6 — `${self.env.VAR}`: env values only, scoped to earlier-declared vars of the same package

```
env: [
  { "key": "TOOL_HOME", "type": "constant", "value": "${self.installPath}" },
  { "key": "TOOL_CFG",  "type": "constant", "value": "${self.env.TOOL_HOME}/etc/tool.conf" }
]
```

Rules:

1. **Scope** — `${self.env.KEY}` resolves against vars of the *same package* declared
   **strictly earlier** in the `env` array. Never a later var, never its own var, never a
   dependency's var, never the project tier's `[env]`, never `--env`, never the ambient
   process environment.
2. **Value** — the referenced var's **resolved `Entry.value`**, i.e. after its own template
   resolution and (for a `path` var) after path normalization. Never the *folded* result:
   `${self.env.PATH}` yields `<content>/bin`, not the whole composed `PATH`. Folding
   happens later, against the ambient environment, and would make a published artifact's
   resolution machine-dependent.
3. **Acyclicity** — by construction. A forward or self reference is not a cycle to detect,
   it is an undefined name: `UndefinedSelfEnvRef` (65). No topological sort, no cycle
   detector; `${deps.*}`'s digest-pinning acyclicity guarantee (research §2.7) is not
   needed here because there is no graph.
4. **Placement** — legal in env values, **rejected in entrypoint `args`** with
   `DisallowedToken` at publish and at runtime (D9). Same layer argument as
   `adr_entrypoint_args_interpolation.md` D3. Widening to args later is additive.
5. **Bytes depend on the referenced var's declared *type*, not just its value.** A `constant`
   `X = "bin"` yields `bin`; a `path` `X = "bin"` yields `<install>/bin`, because
   `env/resolver.rs:67-71` joins a relative `path` value under the install path before the
   value is taken. This is the intended reading of "the referenced var's resolved
   `Entry.value`" (rule 2) and is pinned by C-023 (token-bearing path var) and C-024
   (bare-relative path var).

**The order invariant this rests on, stated.** `Env` is `{ variables: Vec<Var> }` — a JSON
array, and nothing in the metadata tree sorts it, so "declared strictly earlier" is a
well-defined, stable property of the document. Pinned by C-029.

The usual objection — "you have made array order load-bearing" — is answered by the code
rather than by precedent: **order is already load-bearing here.** `emit_root_vars`
(`composer.rs:565-587`) pushes entries in declaration order, and the composer's documented
PATH invariant (`composer.rs:589-611` — "the last entry pushed ends up first") makes that
order observable in the resolved `PATH` today. D6 adds a *second* consumer of an existing
ordering rule; it does not introduce order-sensitivity. (The POSIX-shell / `.env` / systemd
precedent cited under Axis B is corroboration, not the argument.)

**Generator hazard, worth one doc line.** A generator that builds `env` from an unordered map
— Go `map`, Java `HashMap`, Python `set` — emits declaration order that varies run to run.
A package using `${self.env.X}` then publishes on some runs and fails `UndefinedSelfEnvRef` on
others, with no change to the generator's input (S-024). The reference documentation for
`${self.env.*}` says this explicitly: emit `env` from an ordered structure.

### D7 — a duplicated key is refused, not picked

If `KEY` is declared by two or more vars *earlier than* the referencing var,
`${self.env.KEY}` is `AmbiguousSelfEnvRef { key }` (65). Duplicate keys are permitted in
`Env` today (`env.rs:16-19` enforces no uniqueness) and are meaningful — two `path` vars
on `PATH` both contribute. With two earlier contributions and no fold to point at, there is
no non-arbitrary answer to "which one", and last-writer-wins would silently pick one.

**Scope of the rationale, narrowed deliberately.** "The value of `PATH` is ambiguous" is a
broader statement than this rule enforces: `KEY` declared once earlier and once *later* is
also ambiguous in that sense, and D7 permits it, resolving silently to the earlier
contribution. That is intended — rule D6.1 already defines the visible scope as "strictly
earlier", so a later declaration is simply not in scope, exactly as a var in another package
is not. D7's ground is therefore the narrower one: **ambiguity within the earlier scope**,
where two candidates are both legally visible and neither is privileged.

This is the `AmbiguousDependencyRef` pattern reused verbatim: fail closed, name both, let
the publisher disambiguate. It is not a new uniqueness rule on `Env` — duplicates stay
legal; only *referencing* an ambiguous key is refused, exactly as
`adr_deps_name_interpolation.md` keeps same-basename deps legal until a token names them.

### D8 — `${self.env.VAR}` is surface-independent

A `${self.env.KEY}` reference sees the package's own declared vars **regardless of their
`visibility` and regardless of `carrier_crosses`**. An `interface` var may reference a
`private` var; the resolved bytes are identical under `ocx env`, `ocx env --self`,
`ocx package env`, the launcher's `self_view=true` composition, and
`ocx inspect --closure`.

**Rationale.** The alternative — resolve against the active surface — makes the *same*
metadata produce *different* bytes depending on who asked. That is the exact bug class
`adr_two_env_composition` and `composer::{dep_admitted, carrier_crosses}` were unified to
eliminate. It is also not a visibility leak: the publisher authored both vars in one file
and deliberately embedded one in the other. Embedding a private value in an interface
value is authorship, identical in effect to typing the literal.

**Consequence, stated plainly.** The composer's per-package loop today is
*gate-then-resolve* — a var failing `carrier_crosses` is `continue`d **before**
`EnvResolver::resolve` runs (`composer.rs:551-553`, `:579-581`). D8 requires
*resolve-then-gate*.

**The shape, specified.** Per package, in both `emit_interface_vars` and `emit_root_vars`:

1. Resolve the package's **whole** `env` array, in declaration order, into a **private**
   `Vec<Entry>` local to that package. This vec is what `${self.env.KEY}` scans — never the
   `entries: &mut Vec<Entry>` parameter, which is global across packages and already
   surface-gated (that is B1's rejected failure mode).
2. Then apply `carrier_crosses` per var and push only the crossing entries into `entries`.

Push order into `entries` is unchanged, so the PATH ordering invariant documented at
`composer.rs:589-611` is untouched.

**What "resolve" must and must not do once it runs for every var.** `EnvResolver::resolve`
can raise three things today, and all three would newly fire on vars that never resolved
before:

| Assertion | Site | Disposition under D8 |
|---|---|---|
| `RequiredPathMissing` | `env/resolver.rs:94` | **Emit-only.** Asserts a property of the runtime environment, not of the value. (C-026) |
| `SeparatorEdgedListValue` on the **resolved** value | `env/resolver.rs:109` | **Emit-only** (OQ-3). It is a shape assertion on the composed contribution; a var that never joins a fold has nothing to be edged against. |
| `DependencyNotInstalled` → exit **79** | `metadata/template.rs:269`, inside template resolution | **Emit-only.** (C-027) |

The last one is the trap, and it is *not* reached by a split placed inside `EnvResolver`: it
fires within `TemplateResolver::resolve`. It is reachable in ordinary use —
`build_dep_context_map` maps every metadata-declared dep to `store.content(...)`, falling back
to the declaration identifier when the dep is absent from the resolved toolchain — so a
**non-crossing** var referencing a declared-but-uninstalled dep would turn a working install
into exit 79.

**Therefore the split is stated as a rule, not as a list:** *value resolution always; **every**
filesystem and shape assertion on emit only.* Mechanically, non-emitted vars route through the
existing `check_exists = false` seam on `TemplateResolver::resolve_inner`
(`template.rs:161`) — a parameter that exists today and has exactly one caller, which passes
`true` (`template.rs:158`). D8 is its first real use.

**The seam exists; the public surface to reach it does not.** `resolve_inner` is *private*, so
"route through the existing seam" is a statement about the parameter, not about reachability:
no caller outside `metadata::template` can ask for an existence-free resolve today. D8
therefore adds a **new public entry point** on `TemplateResolver` alongside `resolve` — one
method, no new parameter — and that addition is part of the work, not a flag flip (C-027).

The remaining follow-on effect: a template fault in a *non-crossing* var now surfaces where it
previously never ran. A package whose own metadata cannot resolve is broken regardless of who
is looking, so this is a fail-closed improvement — but it is a behaviour change on published
packages, called out in **Migration** and **Open Questions (OQ-3)**.

This change lands only in slice S2. S1 does not touch the composer.

### D9 — capability gate extension

```rust
pub struct AllowedTokens {
    pub deps: bool,
    pub self_env: bool,
}
```

| `Usage` | `deps` | `self_env` |
|---|---|---|
| `Environment` | true | true |
| `EntryPointArgs` | false | false |

- `installPath` and `self.installPath` are **always** allowed, on every surface, under
  every capability set. `${self.installPath}` inherits the bare form's always-allowed
  status because it is the same referent (D4) — a gate that treated them differently would
  make an alias observably not an alias.
- **The capability gate never looks at modifiers.** A modifier is a rendering property of a
  token already admitted, not a capability of its own, so no `AllowedTokens` field gates
  one. Which shapes *may carry* a modifier is a grammar question, settled once in the
  scanner (D5) — not a per-surface one.
- **Gate-before-substitution is now structural, not a hand-placed early return.** The
  scanner classifies the whole input before the resolver substitutes anything, so a
  disallowed token is rejected before any `dep_contexts` lookup can occur. D6 of
  `adr_entrypoint_args_interpolation.md` asked for this ordering and got it via a
  `disallowed_dep_token` pre-check; that helper is deleted, and its correctness claim is
  now a property of the pipeline shape rather than of one call's position.

Extending later: a new capability is one `bool` field and one `From<Usage>` arm. No trait
registry (research §2.4 / `research_interpolation_capability.md` YAGNI verdict, unchanged).

### D10 — one recogniser; no literal `"${installPath}"` survives anywhere

Both downstream literal readers are rewritten against scanner output:

- **`classify_install_path_rooted_dir(value) -> Option<RelativePath>`** — returns `Some`
  iff the scan of `value` is exactly `[Token(InstallPath, no modifier)] [Literal("/…")]`
  with no further token. Both `${installPath}/bin` and `${self.installPath}/bin` therefore
  classify to `bin`. A modifier-bearing token (`${self.installPath:posix}/bin`) returns
  `None` — best-effort scan-scope exclusion, matching today's treatment of shapes it
  cannot classify. A scan **error** also returns `None`, and under D14 that is a **reachable**
  case, not a defensive one: both readers run on read paths, which no longer refuse an
  unrecognised token. `None` is the correct answer there — the same best-effort exclusion the
  modifier case gets — and `libc_lint` records the segment in `unresolvable` rather than
  dropping it silently.

  **The consequence of that recording, stated because three decisions collide here and none
  of them says it.** D5 makes `:native`/`:posix` legal on *every* body; this bullet turns a
  modifier-bearing token into `None`; and `libc_lint` is deliberately fail-closed, so it
  converts that `None` into a hard refusal. Net effect:
  `PATH = "${self.installPath:posix}/bin"` is **unpublishable on any `linux/*` or `any`
  target** — `ocx package create` exits 65 with `UnresolvableScanScope`, a message about a
  scan scope rather than about the modifier that caused it. The *same document* publishes
  cleanly for `darwin/*` and `windows/*`, where `bin_scan` takes the identical `None` as
  "nothing to scan" and the `binaries` claim silently comes out short. `--no-libc-lint` is
  the only escape. Both halves are the intended behaviour — a lint that reported "libc
  verified" over a directory it never inspected would be the failure this lint exists to
  prevent — but the asymmetry between the two consumers of one `None` is a real design
  question this ADR does not pose, and the message quality is WP3's.

  **What shipped (recorded post-hoc).** `libc_lint`'s `ScanScope` does not fold the
  modifier-bearing case into `unresolvable` as drafted above — it carries a second list,
  `modifier_bearing: Vec<String>`, kept apart because the two need different remedies: an
  `unresolvable` value has a shape problem, a `modifier_bearing` one has none at all, and the
  respelling `unresolvable`'s message would suggest leaves it exactly as unscoped as before.
  `check_declared_libc` refuses on either list independently, non-empty `modifier_bearing`
  first-classing to its own `LibcLintError::ModifierBearingScanScope { values }` variant
  (`libc_lint.rs:149-152,211,374`) rather than reusing `UnresolvableScanScope`. This delivers
  the WP3 deferred item this paragraph named ("`UnresolvableScanScope` names the segment but
  not the reason"): which variant fired now states the reason on its own — an unclassifiable
  shape vs. a modifier on an otherwise-recognised one — with no message-text guessing needed.
  The decision itself (a modifier-bearing `PATH` value is out of scan scope, both consumers'
  asymmetry) is unchanged; only the error taxonomy recording it gained a second variant.
- **`libc_lint::resolve_scan_scope`** — replaces `INSTALL_PATH_TOKEN` string comparisons
  with scanner-shape checks over each `:`-separated segment.

**This second site is a fail-open hazard the research did not name.** `libc_lint.rs:236`
reads `if !segment.contains("${installPath}") { continue; }`. The byte sequence
`${installPath}` is **not** a substring of `${self.installPath}`, so a package authored
with the new alias would have every segment silently skipped, the scan scope would come
out empty, and the lint would report "the package puts nothing of its own on `PATH` —
nothing to check". A glibc/musl mismatch would ship unnoticed. The segment is not even
recorded in `unresolvable`. `bin_scan`'s degradation costs a missed *claim*; this one
costs a missed *loader check* on a lint that is otherwise deliberately fail-closed.

**Neither is a regression from today, and that is the sharper point.**
`${self.installPath}` is currently unpublishable, so no package can reach either site with the
alias spelling. **D4 creates both hazards and D10 closes them, in the same release.** That
makes the ordering a hard constraint rather than a preference: within S1, the
`classify_install_path_rooted_dir` / `libc_lint::resolve_scan_scope` rewrite must land
**before** the publish path accepts the alias, or there is a window in which
`ocx package create` on `PATH = "${self.installPath}/bin"` yields an empty libc scan scope and
reports "nothing to check" (see **Slice Boundaries**).

**Exactly one recogniser is rewritten per hazard, and `bin_scan.rs` holds neither.**
`bin_scan.rs` contains no `${installPath}` recogniser at all — only doc comments and test
fixtures; it delegates at `bin_scan.rs:94` to `classify_install_path_rooted_dir`, which lives
in `template.rs:86`. So D10 rewrites **two** functions, one of them in `template.rs` and one in
`libc_lint.rs` (`libc_lint.rs:219`, `:236`, `:239`), and `bin_scan.rs` is a *consumer* whose
behaviour is fixed for free by the first.

Both hazards are silent, so both need contracts that can go red — and both red states are
against `main`, not against a mutant:

- **C-010**: on `main`, `classify_install_path_rooted_dir("${self.installPath}/bin")` returns
  `None`, because `template.rs:87-88` is `value.strip_prefix("${installPath}/")` and the byte
  sequence `${installPath}/` is not a prefix of `${self.installPath}/bin`. A silent wrong
  answer — the var is dropped from the scan scope and the binaries claim comes out short.
- **C-011**: on `main`, `libc_lint.rs:236` is `if !segment.contains("${installPath}")
  { continue; }`, so the `self.` spelling skips every segment and the scan scope comes out
  empty — reported as "nothing to check" rather than as unresolvable.

### D11 — a recognised token with no value is a hard error

`${self.env.NOPE}`, `${deps.nope.installPath}`, `${deps.x.installPath}` where `x` is
declared but not installed — all hard errors (exit 65, except
`DependencyNotInstalled` → 79, unchanged). Never an empty string, never a warning.

**Rationale.** Every tool that tolerates silent-empty (Make, Compose, GitHub Actions) does
so because the loop catching the mistake is the same machine, same minute. OCX breaks that
assumption by design: offline-first index snapshots mean resolution runs on a different
machine, months later, decoupled from the publisher (research §2.5). An empty-string
`${self.env.VAR}` baked into a digest-pinned artifact is permanently and silently wrong.

**D3 and D11 are one rule seen from two sides**, which is a simplification the reversal
bought: *no `${…}` ever resolves to silence.* D3 rules out unexamined literal text for a token
OCX does not recognise; D11 rules out an empty string for a token it does. Neither has an
exception, and the publisher's single escape hatch — `$${` — is explicit, local and visible in
the source bytes. Both are statements about *resolution*; when the refusal fires is D14.

### D12 — error taxonomy consolidation; one rename, one generalisation, one deletion

- **`UnknownPlaceholder` is renamed `UnknownToken { token, hint: UnknownTokenHint }`, and its
  job is kept.** Under D3 it is the catch-all it has always been — "OCX does not recognise this
  `${…}`" — with two changes: the name matches the rest of the taxonomy's *token* vocabulary,
  and it now fires from the scanner, so it is raised at resolve time as well as at publish
  (belt-and-braces; unreachable for metadata that passed validation). No `MalformedToken`
  variant is minted: under a claim-everything rule there is no boundary at which "does not
  parse" and "parses but is unknown" call for different publisher action, and one variant with
  three message branches (D13) serves both.
- **`UnknownDependencyField` generalises to `UnknownField { namespace, field, supported }`**
  (`namespace` = `"self"` or `"deps.cmake"`). One variant serves both `${self.foo}` and
  `${deps.cmake.version}` instead of minting a second — type economy over a parallel
  hierarchy. It fires only where OCX can *locate* the mistake: a recognised namespace shape
  with exactly one unknown leaf. Everything else is `UnknownToken`.
- **The install-path `${` injection defence is deleted** (`template.rs:200-208`). It exists
  because substitution is two-phase today: `str::replace` writes the install path into the
  string, then `captures_iter` re-reads those bytes. Single-pass scanning makes re-reading
  structurally impossible — substituted bytes go to the output buffer and are never
  examined again. Deleting a defence needs proof, so C-009 pins the property directly: an
  install path literally containing `${deps.x.installPath}` must appear verbatim in the
  output, not resolve.

  **The premise has a second falsifier, and it needs its own contract.** "Substituted bytes
  are never rescanned" is a property of *one* template resolution. D6 adds a composition path:
  `B = "${self.env.A}/x"` where `A`'s resolved bytes contain `${deps.x.installPath}`. An
  implementation that inlines `A`'s **template** into `B` and re-scans would resolve those
  bytes — reintroducing precisely the injection the deleted guard existed for. C-029 pins the
  composed case; the rule is that `${self.env.*}` substitutes `A`'s *resolved value*, never its
  template (D6.2).

### D13 — one diagnostic: a root suggestion, or the escape hint; no warning classes

The withdrawn design carried three publish-time **warning** classes, all of which existed
because a typo could pass through silently. Under D3 a typo is a hard error naming the token,
so the warnings have nothing left to warn about and are **deleted**: no near-miss warning
class, no unterminated-prefix warning, no escape-fired warning, and no lib→CLI diagnostic
plumbing.

What survives is a message-design rule on `UnknownToken`, because the message is now the only
guidance a blocked publisher gets. Three branches, chosen from data the scanner already has:

1. **Unknown root within a length-scaled edit distance of a recognised root** → suggest it.
   ```
   unknown token '${slef.env.HOME}': did you mean root 'self'
   ```
   *(Corrected at execution time: the original example ended in `?`. Trailing punctuation is
   Block-tier in `quality-rust.md` and is forbidden by this ADR's own Error Taxonomy preamble —
   the example contradicted the rule two sections above it.)*
2. **Unknown root with no near-miss** → the escape hint.
   ```
   unknown token '${workspaceFolder}': ocx expands every ${…}; write '$${workspaceFolder}' to emit a literal
   ```
3. **Recognised root, body not in the closed set** (`${self!}`, `${deps.x}`,
   `${installPath.foo}`) → list the four supported bodies. The escape hint is *not* offered
   here: a recognised root means the publisher was writing an OCX token.

**Branch 1 earns its keep, and branch 2 is why.** The escape hint is the right advice for a
foreign token and exactly the wrong advice for a typo — it tells the publisher to escape their
own mistake, which "fixes" the error by shipping the typo as literal text into a digest-pinned
artifact. That is the silent-wrong-value failure returning through the error message. The
suggestion is what keeps it out.

**The threshold is rustc's, not a flat 1:**

```
distance(candidate, recognised_root) <= max(recognised_root.len(), 3) / 3
```

- Precedent, and the reason the flat form was rejected: rustc's `find_best_match_for_name`
  scales the cutoff by name length
  ([rustc_span/src/edit_distance.rs](https://github.com/rust-lang/rust/blob/master/compiler/rustc_span/src/edit_distance.rs)).
  For `self` / `deps` (4 chars) the scaled threshold is ~1, identical to the flat rule; for
  `installPath` (11 chars) it is 3. Under a flat 1, `${instalPatch}` — two edits — would fall
  to branch 2 and be told to escape itself (C-033).
- clap independently rejected a flat cutoff, using Jaro/Jaro-Winkler instead — and had to
  patch a real false positive where any two strings sharing a ≥10-char common prefix scored
  as a perfect match ([clap v4.5.23](https://github.com/clap-rs/clap/blob/v4.5.23/CHANGELOG.md)).
  Taken as evidence that the *similarity* rule is where this class of lint goes wrong, hence
  the plainer scaled-edit-distance choice over a prefix-weighted metric.
- **The metric is `strsim::osa_distance`, not `strsim::levenshtein`. Corrected at execution
  time — the earlier wording said "Levenshtein" and could not satisfy C-033.** Plain
  Levenshtein has no transposition operation, so `levenshtein("slef", "self")` is **2** (two
  substitutions), while `self` is 4 characters and its scaled threshold is `max(4,3)/3` = **1**.
  Leg 1 of C-033 would therefore emit no suggestion and fall through to branch 2 — telling a
  publisher to escape their own typo, the precise outcome D13 exists to prevent. Optimal string
  alignment counts one adjacent transposition as a single edit, giving `osa_distance("slef",
  "self")` = 1 ≤ 1. This also matches the cited precedent: rustc's `edit_distance` is
  transposition-aware. Leg 2 is unaffected either way (`instalPatch` → `installPath` is 2
  substitutions, under a threshold of 3). **An implementer who takes "Levenshtein" from prose
  ships a green build and a red C-033.**
- **Computed with [`strsim`](https://docs.rs/strsim/), not by hand.** Edit distance is a
  generic algorithm, not OCX domain code — hand-rolling it is the "generic capability dressed
  up as a specific feature" signal from `quality-core.md`. `strsim` 0.11.1 is already in
  `Cargo.lock`, so the cost is a declaration in the root `Cargo.toml` plus a
  `.workspace = true` line in `crates/ocx_lib/Cargo.toml`, at zero link cost. See the
  Constitution Gate.

**Emission site: the error, nothing else.** `hint: UnknownTokenHint` is a field on the variant,
computed where the error is constructed, rendered by `Display`. `ocx_cli` gains no code and
`ocx_lib` gains no `warn!` call on this path.

### D14 — refusal is scoped to resolution, not to reading

D3 says an unrecognised token is a hard error. D14 says **where**: at publish, and on every
path that resolves a value. A read-only surface shows the token and does not fail.

**The rule, in D8's shape, one layer up.** D8 splits *value resolution always; every filesystem
and shape assertion on emit only*. D14 is the same split applied to token validation:

> **Classification always; refusal on resolve only** — plus one explicit publish gate.

**The layering change this forces.** Today the whole token check lives in
`ValidMetadata::try_from` (`validation.rs:74-84`), which runs on **every** ingress path:
`find_in_store` (`tasks/common.rs:63`), `load_object_data` (`:124`), `load_config_metadata`
(`:210` — registry ingress), `pull_local.rs:165`, and
`resolve_env_from_package_root` (`package_manager.rs:540`). That is exactly the too-early layer.
`validate_env_tokens` and `validate_entrypoint_args` therefore **leave**
`ValidMetadata::try_from`; `validate_env_modifier_types` and `validate_env_list_entries` stay,
because an unreadable modifier type is a statement about the document's *grammar*, not about
whether a value can resolve. `ValidMetadata`'s doc comment ("metadata that has passed
publish-time validation") is corrected in the same commit — after D14 it means structurally
readable, not resolvable.

**What replaces it is one call and one thing that already happens.**

1. **Publish gate — explicit.** `ocx package create` / `push` call the strict pass directly.
   The publisher is present, and a typo must not reach a registry.
2. **Compose/execute — no new gate.** Resolution *cannot* produce bytes for a token it does not
   recognise (D3) or cannot resolve (D11), so `TemplateResolver::resolve` already errors. Every
   composing surface routes through it via `resolve_env` → `composer::compose` → `EnvResolver`.
   **Nothing is added on this side**, which is the point: the enforcement point is the operation
   that needs the value, not a checkpoint guessing on its behalf.

**The enumeration — the deliverable.**

| Surface | Behaviour | Why |
|---|---|---|
| `ocx package create`, `ocx package push` | **Hard error**, exit 65 | Explicit publish gate (1) |
| `ocx env`, `ocx --global env`, `ocx package env` | **Hard error**, exit 65 | `resolve_env` → composer → resolver |
| `ocx run`, `ocx package exec` | **Hard error**, exit 65 | Same, before spawn |
| `ocx launcher exec` | **Hard error**, exit 65 | Resolves baked entrypoint `args` |
| `ocx package test`, `ocx patch test`, `ocx patch sync` | **Hard error**, exit 65 | Each composes via `resolve_env` |
| `ocx direnv export`, `ocx patch why` | **Hard error**, exit 65 | Compose via `resolve_env_with_patch_boundary` (`direnv_export.rs:172`, `patch_why.rs:56`) |
| `ocx inspect`, `ocx package inspect` (incl. `--closure`) | **Succeeds**; token shown verbatim | Never resolves — `--closure` is already value-omitted |
| `ocx package info`, `ocx package describe` | **Succeeds**; token shown verbatim | Prints stored metadata |
| `ocx package which`, `ocx package deps` | **Succeeds**; token shown verbatim | Structural reads — see the correction below |
| `ocx index catalog`, `ocx index list`, `ocx status` | **Succeeds** | Never reads env values |
| `ocx pull`, `ocx package install`, `ocx package pull` | **Succeeds** | Neither resolves. Derived, not chosen — see below |

**Correction to this table, from a code census run at execution time (not a decision change).**
An earlier revision grouped `ocx package which` and `ocx package deps` with `direnv export` under "compose or emit
resolved entries". **The code does not support that**: neither command calls
`TemplateResolver::resolve`, `EnvResolver::resolve` or `composer::compose` anywhere in its path —
`which.rs` and `deps.rs` are pure structural reads (paths, dependency graph, visibility), and they
reject a bad token today only as an *incidental* side-effect of routing through
`ValidMetadata::try_from`. Once that check leaves, nothing reintroduces refusal. They therefore land
on the succeed side **by D14's own principle** — they look, they do not run — and adding a gate to
hold them on the refuse side would be exactly the second enforcement point this decision rejects.
`ocx direnv export` genuinely composes and stays. **`ocx patch why` was missing from the table
altogether** and composes by the same route, so it is added to the refuse side.

**Why `pull`/`install` land on the permissive side, derived rather than preferred.** The
ingress gate at `tasks/common.rs:210` refuses to *write* unvalidated metadata to disk. Keeping
it would make the read-only promise unreachable: `inspect`, `info` and `describe` all read an
object that had to be pulled first, so a pull that rejects the document means there is nothing
left to look at. The permissive read path only exists if the ingress path is permissive too.
The consequence is stated plainly: a package using a newer token installs on an older ocx and
fails at first use, not at install. That is the same fail-closed shape as S-017, moved one step
later.

**What a read-only surface shows: the token, verbatim, unannotated.** The stored value is the
template string; there is no resolved value to display and OCX invents none. `--format json`
consumers therefore see the raw value unchanged, which is what they already see today for
every other value. No "unresolvable" marker is added — it would mean running the scanner on
read paths and teaching three output types a new field, to tell a human something the verbatim
`${workspaceFolder}` already says.

**Both sides need pinning, or neither is a check.** A refusal that fires everywhere is
indistinguishable from one that fires nowhere, so C-036 asserts read-only success and
compose failure **on the same document** (C-036…C-038).

---

## The Scanner Specification

**Normative and ordered.** An earlier draft stated this as ABNF, which was wrong in a way
worth recording: ABNF alternatives are **unordered**, and a `literal-char = %x00-10FFFF`
fallback overlaps both `escape` and `token`. Under that grammar `$${installPath}` had three
valid derivations and `${installPath}` had two. Precedence is therefore stated explicitly
below, and the only ABNF that survives is **anchored**: it describes the body of a token whose
extent is already known.

`input` is a UTF-8 `&str`. The scanner walks it left to right; at each position *i* the
**first** rule that matches wins, and the bytes it consumes are appended to the output and
never re-examined.

**R1 — escape (highest precedence).** If `input[i..]` starts with `$${`, emit the two bytes
`${` and advance *i* by 3. Being checked before R2 is what makes `$${installPath}`
unambiguous: the `${` at *i+1* is never reachable as a token start.

**R2 — token.** Else if `input[i..]` starts with `${`:

- **R2.0 — terminator.** Let *t* be the index of the first `}` at or after *i+2*. If there is
  none, **no token exists**: fall through to R3 (emit `$`, advance by 1), so the `${` and
  everything after it become literal text one character at a time (Axis D).
- **R2.1 — parse.** Parse `input[i+2..t]` — the *whole raw body* — against the anchored body
  grammar below, then against the closed set of four bodies. It must satisfy **both**. A body
  that does not is an **error returned from the scan**: never a `Literal`, never emitted
  verbatim. Advance *i* to *t+1*.
- **R2.2 — diagnostics only.** When R2.1 fails, extract `root` = the maximal run of
  `[A-Za-z0-9_-]` starting at *i+2* (possibly empty) and use it to pick the message branch
  (D13): recognised root ⇒ branch 3, near-miss ⇒ branch 1, otherwise ⇒ branch 2.

**R3 — literal (fallback only).** Emit the character at *i* and advance by its UTF-8 length.
R3 is reachable only after R1 and R2 have both failed — it is the residue, not an alternative.

**What the reversal removed here, and why the remainder is safer.** The withdrawn design used
the R2.2 root run as a *correctness* step: it decided claimed-versus-foreign **before** the
body was parsed, so getting it wrong meant `${self!}` silently shipped as foreign text. It is
now a *diagnostic* step, consulted only on a path that is already an error. An implementation
that extracts the root wrongly now produces a worse message, not a wrong resolution — the
sharpest single risk reduction in the reversal.

### The body grammar — anchored, applied to every token

```abnf
body      = root *( "." segment ) [ ":" modifier ]

root      = 1*( ALPHA / DIGIT / "_" / "-" )
segment   = 1*( ALPHA / DIGIT / "_" / "-" )     ; no ".", ":", "}", "$", "{"
modifier  = 1*( %x61-7A / DIGIT )               ; lowercase + digits; closed enum
```

Unambiguous because it is **anchored at both ends** against a body of known extent
(`input[i+2..t]`), with no literal fallback in scope: exactly one derivation, or none.

**Two layers, both required, in this order.** The grammar is only the *syntactic* filter;
passing it is necessary and not sufficient. `${installPath.foo}` and `${localEnv:home}` both
derive cleanly and are still errors, because the second layer is the **closed set** enumerated
below — four bodies, no others.

### Recognised bodies — the complete closed set

| Body | Meaning | Modifier |
|---|---|---|
| `installPath` | the consuming package's `content/` directory | optional |
| `self.installPath` | exact alias of the above (D4) | optional |
| `self.env.KEY` | resolved value of this package's earlier-declared var `KEY` (D6) | optional |
| `deps.NAME.installPath` | direct dependency `NAME`'s `content/` directory | optional |

- `KEY` must satisfy the same env-key grammar as `Var.key` (`env::is_valid_env_key`). **This is a
  second filter on top of the anchored `segment` rule, not a restatement of it**: `segment` admits a
  leading digit and `-`, which `is_valid_env_key` (`env.rs:1053`) rejects, so `${self.env.1ABC}` and
  `${self.env.A-B}` are `UnknownToken` rather than recognised. Pinned by **C-039** — added at
  execution time because a post-stub review found this was the one constraint in the whole grammar
  that an implementation could silently drop with no contract able to notice.
- **The `:suffix` is captured verbatim and then checked against the closed modifier set** — it is not
  filtered by the `modifier` production during parsing. This resolves a contradiction between the
  body grammar and the error table below: the grammar's `modifier = 1*(%x61-7A / DIGIT)` would make
  `${self.installPath:POSIX}` an `UnknownToken`, while the table and C-008 say `UnknownModifier`.
  **The table wins**, and capture-then-check is what makes it win by construction rather than by
  accident.
- `NAME` must be accepted by `DependencyName::try_from` — `SLUG_PATTERN`
  (`^[a-z0-9][a-z0-9_-]*$`) **and** `SLUG_MAX_LEN`. Going through the constructor rather than
  the pattern alone is what keeps the accepted set identical to `DependencyName`'s.
- `modifier ∈ { "native", "posix" }`.
- **`installPath` is a complete token, not a namespace.** It is the one recognised root that
  is simultaneously a root and a whole body — unlike `self` and `deps`, which are namespaces
  and never a token by themselves. It therefore admits **no** dotted continuation.

There is no `RESERVED_ROOTS` constant and no root freeze. The closed set of four bodies is the
single source of truth; the root list `{ installPath, self, deps }` is derived from it for
D13's suggestion branch alone.

Everything else is a **hard error**:

| Input | Outcome |
|---|---|
| `${self.foo}` | `UnknownField { namespace: "self", field: "foo", … }` |
| `${deps.x.version}` | `UnknownField { namespace: "deps.x", … }` |
| `${installPath:frobnicate}` | `UnknownModifier` |
| `${self.installPath:POSIX}` | `UnknownModifier` — the modifier class is lowercase |
| `${self}` , `${deps}` , `${self.}` , `${deps.}` , `${deps.x}` | `UnknownToken`, branch 3 — recognised root, body outside the closed set |
| `${self!}` , `${self.env.A B}` , `${installPath }` , `${deps.x.installPath!}` | `UnknownToken`, branch 3 — body fails the anchored grammar on a character outside `segment` / `modifier` |
| `${installPath.foo}` , `${installPath.foo:posix}` | `UnknownToken`, branch 3 — `installPath` is a complete token, not a namespace |
| `${deps.Python.installPath}` | `UnknownToken`, branch 3 — uppercase fails the slug class |
| `${installpath}` , `${slef.env.HOME}` , `${instalPatch}` | `UnknownToken`, **branch 1** — unknown root, near-miss, message suggests the recognised root |
| `${workspaceFolder}` , `${localEnv:HOME}` , `${env:HOME}` , `${containerWorkspaceFolder}` , `${1}` , `${}` , `${a b}` | `UnknownToken`, **branch 2** — unknown root, no near-miss, message carries the escape hint |
| `${ocx.version}` | `UnknownToken`, branch 2 — `ocx` is not a recognised root and is not reserved either (D3) |

### Unterminated `${`

R2.0: a `${` with no `}` before end of input is **literal text** — there is no token, so the
error path is never entered. Chosen at Axis D; it is the one shape where OCX's claim over
`${…}` does not reach, and it is the reason the publish accept-set only ever grows.

### Nesting

There is none. R2.0 consumes from `${` to the **first** `}`. `${a${b}}` therefore has its
terminator at the inner `}` and a body of `a${b`, which fails the grammar on `$` — so it is
an `UnknownToken` and the scan errors; the trailing `}` is never reached. *(This changes from
the withdrawn design, where the same input passed through verbatim.)* Substituted values are
never rescanned either, so a resolved value containing `${…}` is inert (C-009, C-029).

---

## Component Contracts

Numbered, testable from this document alone. `→` means "scan-and-resolve produces".
**IDs are renumbered from scratch in this revision** — see the Changelog.

**Scanner — escape (D2)**

- **C-001** `$${installPath}` → `${installPath}` (literal). No leading `$` remains.
  *(Red state reachable on `main`: today's code produces `$<content-path>`.)*
- **C-002** `$$`, `$$foo`, `price: $$5`, and a trailing `$` at end of input each pass
  through byte-identical.
- **C-003** `$$${installPath}` → `$${installPath}`. `$$$${installPath}` → `$$${installPath}`.

**Scanner — claim-all rejection (D3)**

- **C-004** *(the inverted golden corpus — highest-value fixture in the set)* Every one of
  `${workspaceFolder}`, `${containerWorkspaceFolder}`, `${localEnv:HOME}`, `${env:HOME}`,
  `${localEnv:PATH:default}`, `${1}`, `${}`, `${a b}`, `${installpath}`, `${a${b}}`,
  `${ocx.version}` is a **hard error** (exit 65) whose message names the token verbatim.
  Mixed case: `${installPath}/x:${workspaceFolder}/y` errors naming `${workspaceFolder}` and
  resolves nothing. Table-driven, so adding a token to the corpus is a one-line change.
  *(Red state: an implementation that emits any of them verbatim. This is the same corpus the
  withdrawn design asserted pass-through on — the inputs are unchanged, the expectation is
  inverted.)*
- **C-005** An unterminated `${self.installPath` passes through byte-identical and publishes
  (Axis D). *(Red state: an implementation that errors on an unterminated `${`.)*
- **C-006** *(the #221 authoring path, two legs)* (a) A **`constant`** var
  `"$${workspaceFolder}/x"` publishes and resolves to the literal `${workspaceFolder}/x`,
  byte-identical end to end. (b) A **`path`** var with the same value resolves to
  `<install_path>/${workspaceFolder}/x`, because the resolved value is relative and
  `env/resolver.rs:67-71` joins it under the install path. *(Leg (b) pins that the layers
  above the scanner still act on escaped bytes — S-023.)*

**Scanner — recognised tokens**

- **C-007** `${installPath}` and `${self.installPath}` resolve to identical bytes for the
  same resolver, in every position (alone, prefixed, suffixed, repeated twice in one value,
  and two different spellings in one value).
- **C-008** Each row of the grammar error table produces the error variant named there, and
  each message contains the offending token text verbatim: `${self.foo}` and
  `${deps.x.version}` → `UnknownField`; `${installPath:frobnicate}` and
  `${self.installPath:POSIX}` → `UnknownModifier`; `${self}`, `${deps}`, `${self.}`,
  `${deps.}`, `${deps.x}`, `${self!}`, `${self.env.A B}`, `${installPath }`,
  `${deps.x.installPath!}`, `${installPath.foo}`, `${deps.Python.installPath}` →
  `UnknownToken`. *(Red state: any of them resolving, or being emitted as text.)*
- **C-009** With `install_path` = a directory literally named `/opt/${deps.foo.installPath}/x`
  and `foo` present in `dep_contexts`, `${installPath}/tool` resolves to
  `/opt/${deps.foo.installPath}/x/tool` — the injected bytes appear **verbatim** and are not
  substituted. *(This replaces the defence D12 deletes; it must be provable red by
  re-introducing a two-phase substitution.)*

**Downstream recognisers (D10)**

- **C-010** `classify_install_path_rooted_dir` returns `Some("bin")` for **both**
  `${installPath}/bin` and `${self.installPath}/bin`; `None` for `${installPath}`,
  `foo/${installPath}/bin`, `${installPath}/../etc`, and `${self.installPath:posix}/bin`;
  `Some("")` for `${installPath}/` and `${self.installPath}/`. Homed at `template.rs:86`,
  where the function lives — not in `bin_scan.rs`, which only calls it (`bin_scan.rs:94`).
  *(Red state, against `main` and not against a mutant: `template.rs:87-88` is
  `value.strip_prefix("${installPath}/")`, and `${installPath}/` is not a prefix of
  `${self.installPath}/bin`, so the alias leg returns `None` today — a silent wrong answer.)*
- **C-011** `libc_lint::resolve_scan_scope` produces an identical `ScanScope` for a metadata
  document using `${self.installPath}/bin` and one using `${installPath}/bin`, including the
  `:`-joined mixed case `${self.installPath}/bin:${deps.other.installPath}/bin` (scope = `bin`,
  dep segment ignored, `unresolvable` empty). *(Red state: the current `contains` guard yields
  an empty scope for the `self.` spelling.)*

**Render modifiers (D5)**

- **C-012** *(pure-function identity)* `render(s, RenderModifier::Native, host) == s` for
  every host and every input — `:native` is the identity function, not a second code path.
- **C-013** *(resolver level — the **defaulting** seam)* For an `install_path` containing a
  backslash, `TemplateResolver` output for `${installPath}` is byte-identical to
  `${installPath:native}` on **both** injected hosts. With `Host::Windows` it additionally
  **differs from** `${installPath:posix}`; with `Host::Unix` all three agree, because `:posix`
  is the identity there.
  *(The third clause is what makes this a check. An omitted modifier has no representation in
  a `render(s, RenderModifier, host)` call, so C-012 cannot see the defaulting seam at all: a
  resolver that wrongly maps an unmodified token to `Posix` leaves `render` identity-correct
  and passes C-012 green. Red state = that mis-defaulting resolver.)*
- **C-014** `render("C:\\Users\\x", Posix, Host::Windows)` == `"C:/Users/x"` — drive letter
  preserved, no `\\?\`, no `/c/`, no `/mnt/c/`.
- **C-015** `render("/home/a\\b", Posix, Host::Unix)` == `"/home/a\\b"` — identity off
  Windows, so a POSIX filename containing a backslash is not corrupted. *(The render
  function takes the host as a parameter so both legs run on any CI host.)*
- **C-016** *(resolver level; **`#[cfg(windows)]`**)* Given an `install_path` carrying a
  `\\?\` verbatim prefix, `TemplateResolver` output for `${installPath}`,
  `${installPath:native}` and `${installPath:posix}` contains no `\\?\` prefix.

  **Windows-only, deliberately, and the cost is stated rather than engineered away.**
  `dunce::simplified` is a no-op off Windows, so on Linux the `\\?\` prefix survives into the
  output and an ungated C-016 **reds on every Linux run**. Gating it `#[cfg(windows)]` means it
  executes only on the `Build & Unit Test (Windows)` leg of `.github/workflows/verify-deep.yml`
  and **never under a local `task verify`**. The rejected alternative was a host-injected seam
  like C-013's: injecting a `Host` does not help, because the host-conditional thing here is
  `dunce::simplified` itself, so making C-016 run on Linux would mean owning a
  host-parameterised reimplementation of the verbatim-prefix strip — hand-owning non-domain
  code to make one contract portable, which is the trade this ADR already pays once
  (Deviation 1) and will not pay twice. C-014/C-015 avoid the problem by living below `dunce`
  on the pure `render` function; C-016 sits above it and cannot.
- **C-017** *(resolver level)* `${installPath:posix}` (bare form + modifier) is accepted and
  renders identically to `${self.installPath:posix}`, with `Host::Windows` injected so both CI
  legs exercise a non-identity `:posix`.

  **The `Host` seam C-013 and C-017 rely on.** `TemplateResolver`'s render step consults a
  `Host` that defaults to the real host and is overridable in tests through the canonical
  test-only seam (`#[cfg(any(test, feature = "__testing"))]`, per `arch-principles.md`
  "Test-only seams"). This is a seam over **`render`'s host argument only** — it does not, and
  must not, divert `dunce::simplified`. That is exactly why C-016 cannot use it.

**`${self.env.VAR}` (D6–D8)**

- **C-018** Vars `A` then `B="${self.env.A}/x"` → `B` resolves to `<A's resolved value>/x`.
- **C-019** Forward reference (`B` references `C` declared after it) → `UndefinedSelfEnvRef`
  naming `C` and listing the keys declared before `B`.
- **C-020** Self reference (`A="${self.env.A}"`) → `UndefinedSelfEnvRef` — same error, no
  cycle detector involved.
- **C-021** `A` declared twice, then `B="${self.env.A}"` → `AmbiguousSelfEnvRef { key: "A" }`.
  `A` declared twice with **no** reference to it → still valid metadata (duplicates stay
  legal).
- **C-022** A `path` var `P="${installPath}/bin"` referenced as `${self.env.P}` yields the
  *resolved single contribution* (`<content>/bin`), not a folded `PATH`.
- **C-023** *(type dependence — C-022 sibling)* A `path` var `P = "bin"` (bare relative, no
  token) referenced as `${self.env.P}` yields `<install_path>/bin`, whereas a `constant`
  `C = "bin"` referenced as `${self.env.C}` yields `bin`. *(C-022 covers only the
  token-bearing path var; the bare-relative case is where the type distinction is invisible in
  the value.)*
- **C-024** Surface independence: a document with a `private` var `S` and an `interface` var
  `I="${self.env.S}"` yields byte-identical `I` under `self_view=false` and `self_view=true`,
  **and that agreed value equals `S`'s own resolved value**, asserted literally on both
  surfaces. *(The value assertion is not redundant with C-018: an implementation that resolves
  `${self.env.S}` to the empty string on **both** surfaces satisfies equality perfectly, so
  equality alone cannot tell surface-independence from uniformly-degenerate.)*
  (`ocx inspect --closure` is value-omitted and therefore never resolves; out of scope.)

  **Corrected at execution time — the fixture must sit on a dependency edge, not at the root.**
  At the root, `carrier_crosses(vis, is_root = true, self_view)` reduces to `has_private()`,
  which is `false` for `INTERFACE`, so `I` is simply **absent** from the private surface and
  there is nothing for the two surfaces to agree on. On a dependency edge
  `carrier_crosses(vis, is_root = false, _)` reduces to `has_interface()`: `I` crosses both
  surfaces, `S` crosses neither, and the two runs differ only in the surface asked for — which
  is the shape the contract is actually about.
- **C-025** *(order invariant — end-to-end, not serde)* Two metadata documents that differ
  **only** in the order of the `env` array: `A` then `B="${self.env.A}"` resolves successfully
  with `B` carrying `A`'s value; the swapped document fails `UndefinedSelfEnvRef` naming `A`.
  *(What makes this a check is that the two inputs are byte-identical modulo array order, so
  any implementation that ignores order gives both documents the same verdict and one leg
  reds. C-018 and C-019 cover the two outcomes against **different** documents and therefore
  do not pin order as the cause.)*
- **C-026** *(D8 split — `RequiredPathMissing`, two legs, both required)* (a) A `required`
  path var whose target is absent and that does **not** cross the active surface does not
  raise `RequiredPathMissing`, even though its value is resolved. (b) An otherwise identical
  `required` path var that **does** cross still raises it. *(Leg (b) is what makes (a) a
  check: (a) alone passes if the existence assertion is deleted outright.)*
- **C-027** *(D8 split — `DependencyNotInstalled`, C-026's sibling)* A var referencing
  `${deps.x.installPath}` where `x` is **declared but not installed on disk**: the install
  succeeds when that var does **not** cross the active surface, and fails with
  `DependencyNotInstalled` (exit 79) when an otherwise identical var **does** cross. *(Red
  state for the first leg = leaving `check_exists = true` on the non-emitted path, which turns
  a working install into exit 79. The assertion lives inside `TemplateResolver`, not
  `EnvResolver`, so a split placed only in the latter does not satisfy it.)*

  **Satisfying this needs a new public entry point, not just the existing seam.**
  `TemplateResolver::resolve_inner` (`template.rs:161`) is **private**, has exactly one caller
  (`template.rs:158`), and that caller hardcodes `check_exists = true`. The composer's
  non-emitted path needs a second public method alongside `resolve`. Stated here so the work is
  not mistaken for a one-line argument flip.
- **C-028** `${self.env.X}` in an entrypoint `args` element → `DisallowedToken` at publish
  **and** at runtime, asserted on the **error variant**, which is what makes each leg
  falsifiable: delete the D9 gate and publish *accepts* the document (args carry no self-env
  scope for a publish-time reference check to fail against), while runtime fails as
  `UndefinedSelfEnvRef`. Both differ from `DisallowedToken`.
- **C-029** *(D6 order invariant is C-025; this is the composed-injection sibling of C-009)*
  Var `A` resolves to bytes that literally contain `${deps.x.installPath}` (its install path
  contains that text), and `B = "${self.env.A}/x"`. `B` resolves with those bytes
  **verbatim**, with `x` present in `dep_contexts`. *(D12 deletes the install-path `${`
  injection defence on the grounds that substituted bytes are never rescanned;
  `${self.env.*}` is a second composition path where that premise can be falsified.)*

  **Do not attempt an against-`main` red run for this one.** On `main`, `${self.env.A}` is
  rejected before any of this is reached, so the run yields the old unknown-placeholder error —
  evidence of nothing, since it fails for a reason the contract does not test. C-029's red
  state is a **mutant of the new code**: an implementation that substitutes `A`'s template
  rather than `A`'s resolved value.

**Capability gate (D9)**

- **C-030** `AllowedTokens::from(Usage::Environment)` == `{ deps: true, self_env: true }`;
  `from(Usage::EntryPointArgs)` == `{ deps: false, self_env: false }`.
- **C-031** Under `EntryPointArgs` with a **populated, on-disk** `dep_contexts`,
  `${deps.uv.installPath}/x` → `DisallowedToken`, never a resolved path. (Existing contract,
  re-pinned against the new pipeline.)
- **C-032** Under `EntryPointArgs`, both `${installPath}` and `${self.installPath}` resolve
  normally.

**Diagnostics (D13)**

- **C-033** *(three message branches, all four states required)* `${slef.env.HOME}` errors
  with a message suggesting root `self` (**OSA** distance 1 — one adjacent transposition —
  against a scaled threshold of 1; plain Levenshtein scores this 2 and fails the leg);
  `${instalPatch}` — edit distance 2 from
  `installPath`, length 11, scaled threshold 3 — errors with a message suggesting
  `installPath`; `${workspaceFolder}` errors with a message carrying the **escape hint** and
  **no** suggestion — and the hint **names the token itself** (`$${workspaceFolder}`), never a
  generic `$${…}`, which D13's prose example shows but this contract text did not say; `${self.env.A B}` errors with a message listing the four supported bodies
  and **no** escape hint. *(Red state for leg 2 = the flat distance-1 threshold, under which
  `${instalPatch}` falls to the escape-hint branch and the publisher is advised to ship their
  own typo as literal text — the exact failure D13 exists to prevent. Red state for leg 4 = a
  message that offers the escape hint on a recognised root.)*

**Deviation-1 mitigations**

- **C-034** *(property test)* One `proptest`: for arbitrary UTF-8 `s`, `scan(escape(s)) == s`,
  where `escape` is the inverse the documentation promises publishers. Plus byte conservation
  on the scan of an arbitrary input containing no `${`: no literal byte is dropped, duplicated
  or reordered. *(An enumerated corpus cannot catch the failure mode `quality-core.md`'s worked
  example describes — a wrong rule affirmed identically by the test and the doc comment, with
  no fixture containing the offending byte.)*

  **What shipped (recorded post-hoc).** No `proptest` dependency was added (0 matches in every
  workspace `Cargo.toml` and in `Cargo.lock`). Both legs run instead over `generated_corpus()`
  (`template/scanner.rs:1211-1224`): the exhaustive enumeration of every string up to length 4
  over `ROUND_TRIP_ALPHABET` (`scanner.rs:1174`) — `['$','{','}','\\','/',':','a']`, seven
  symbols: every byte the grammar branches on (including `:`, the body/modifier separator —
  without it the round trip never exercises the one byte that decides where a body ends),
  both path separators, and one ordinary letter — union six non-ASCII clusters (C-035's) each
  placed bare, inside a token body, inside an escaped token, between two recognised tokens, and
  against a trailing `$${`. This answers C-034's stated worry at least as well as a `proptest`
  would: `quality-core.md`'s failure mode is a wrong rule that a *hand-picked* fixture never
  happens to contain the offending byte for; an exhaustive enumeration over the branching
  alphabet has no such gap by construction — every arrangement of every byte the scanner's
  `match` distinguishes, up to a length that spans `$${` with a byte on either side, is tested,
  not sampled. A `proptest` trades that certainty for unbounded-length random sampling with no
  shrink-to-exhaustive guarantee over a seven-symbol alphabet this small. The swap keeps both
  required legs (the
  round-trip inverse and byte conservation) and the C-035 non-ASCII coverage; nothing in C-034
  was narrowed, only the input-generation mechanism changed.
- **C-035** *(non-ASCII)* CJK text, emoji (including a ZWJ sequence), and a combining-mark
  cluster each pass through byte-identical when placed before, after and between recognised
  tokens, and when adjacent to `$`, `$$` and `$${`. *(The reasoning the scanner relies on,
  recorded because it is the whole safety argument for byte indexing: in UTF-8 every byte of a
  multi-byte sequence has the high bit set, so an ASCII byte — `$`, `{`, `}`, `.`, `:` — can
  never occur inside one. Index arithmetic on those five bytes cannot split a character.)*

**Env-key grammar (added at execution time)**

- **C-039** `${self.env.1ABC}`, `${self.env.A-B}` and `${self.env.}` are each `UnknownToken`, because
  `KEY` must satisfy `env::is_valid_env_key` and not merely the anchored `segment` rule, which admits
  a leading digit and `-`. The positive leg is required: `${self.env.A_B1}` is recognised. *(Red
  state: an implementation that validates `KEY` with the `segment` production alone — which passes
  every other scanner contract, which is why this needs its own ID. The sibling `NAME` /
  `DependencyName::try_from` rule is already pinned by `uppercase_dep_name_is_rejected`; this one
  had nothing.)*

**Failure timing (D14)**

All three run against **one** fixture package whose env value contains `${workspaceFolder}`,
installed on disk. Using one document is what makes the set a check rather than three
independent assertions.

- **C-036** *(both sides, the load-bearing one)* `ocx package inspect` and `ocx package info`
  on the fixture **succeed**, exit 0, and print the value containing `${workspaceFolder}`
  **verbatim**; `ocx env` and `ocx package exec` on the **same** fixture **fail** with `UnknownToken`,
  exit 65. *(Red state for the success legs: today's `find_in_store` → `ValidMetadata::try_from`
  rejects the document, so inspect fails — the existing unit test
  `inspect::tests::inspect_default_malformed_metadata_is_internal` asserts exactly that
  behaviour and **inverts** under D14. Red state for the failure legs: deleting the resolver's
  unknown-token arm, under which `ocx env` emits an empty or literal value.)*
- **C-037** `ocx package create` / `push` on the same document **fail**, exit 65, before
  anything is written to a registry. *(Red state: removing the explicit publish gate, under
  which publish succeeds because `ValidMetadata::try_from` no longer refuses.)*
- **C-038** `ocx pull` / `ocx package install` of the same document **succeed**. *(Red state: leaving
  the token check at `tasks/common.rs:210`, under which the object never lands on disk and
  C-036's read-only legs have nothing to read. This contract is what makes the permissive read
  path reachable rather than notional.)*

---

## UX Scenarios

| # | Action | Expected outcome |
|---|---|---|
| **S-001** | Publisher writes `"value": "${self.installPath}/bin"` and runs `ocx package create` | Accepted. Resolves identically to `${installPath}/bin` at every consumer. **Test:** C-007 plus an acceptance step in `test/tests/test_env.py` publishing the alias spelling and asserting the composed value. |
| **S-002** | Existing package with `${installPath}/bin` is installed by the new ocx | Byte-identical to today. No warning, no deprecation notice. **Test:** every existing `template.rs` and `env/resolver.rs` value assertion passes **unmodified** — the promise in **Migration**, promoted here to the named regression step. A diff that edits one of those assertions falsifies S-002. |
| **S-003** | Publisher writes a `customizations` payload containing an unescaped `${workspaceFolder}` | **Rejected at publish**, exit 65, message names the token and carries the escape hint. The payload must be authored `$${workspaceFolder}`. *This is the #221 authoring burden; stated here and in D3, and nowhere else.* |
| **S-004** | Publisher writes `${localEnv:HOME}` in a `constant` or `list` env value | **Rejected at publish**, exit 65, escape hint. *Unchanged from today, which also rejects it.* |
| **S-005** | Publisher needs a literal `${installPath}` in a payload and writes `$${installPath}` | Resolves to the literal `${installPath}`. |
| **S-006** | Publisher writes `$$` in a password or shell fragment | Untouched. `$$` is not an escape unless `{` follows. |
| **S-007** | Publisher writes `${self.instalPath}` (typo inside a recognised namespace) | **Rejected at publish**, exit 65, `UnknownField`, message names the token and lists supported fields under `self`. |
| **S-008** | Publisher writes `${slef.env.HOME}` (typo in the root) | **Rejected at publish**, exit 65, message suggests root `self`. *Changed from the withdrawn design, where it published silently as literal text.* |
| **S-009** | Publisher writes `${installpath}/bin` (wrong case) | **Rejected at publish**, exit 65, message suggests `installPath`. *Unchanged from today; the withdrawn design would have published it.* |
| **S-010** | Publisher writes `"value": "${self.installPath:posix}"` and a consumer runs on Windows | `C:/Users/…/content`. Valid inside an unescaped JSON string. |
| **S-011** | Same package on Linux | `/home/…/content` — `:posix` is the identity; no double-rendering, no `/c/` form. |
| **S-012** | Publisher writes `${self.installPath:frobnicate}` | Rejected at publish, exit 65, message lists `native, posix`. |
| **S-013** | Publisher declares `TOOL_HOME` then `TOOL_CFG="${self.env.TOOL_HOME}/etc"` | Both resolve; `TOOL_CFG` embeds `TOOL_HOME`'s resolved value. |
| **S-014** | Publisher declares `TOOL_CFG="${self.env.TOOL_HOME}/etc"` **before** `TOOL_HOME` | Rejected at publish, exit 65: undefined `TOOL_HOME`, listing keys declared before `TOOL_CFG`. |
| **S-015** | Publisher puts `${self.env.X}` in an entrypoint `args` element | Rejected at publish, exit 65, `DisallowedToken`: only valid in env values. |
| **S-016** | Publisher declares `PATH` twice, then references `${self.env.PATH}` | Rejected at publish, exit 65, `AmbiguousSelfEnvRef`. Declaring `PATH` twice **without** referencing it stays valid. |
| **S-017** | Consumer on a **pre-D14** ocx installs a package using `${self.installPath}` | Fails closed at `ValidMetadata::try_from` with the old unknown-placeholder error — on *every* path, inspection included, because that is where the released binary put the check and D14 cannot reach back and move it. Adopting the new grammar raises the package's effective minimum ocx — same social contract as `list` (`adr_env_modifier_types.md` D4). **This is the last release boundary that behaves this way**; from D14 on, an ocx meeting a token it does not know still reads the package and refuses only on compose/execute (S-026). The fail-closed property survives exactly where it matters and is given up only where it never did. **Untestable in-tree** — asserting it needs a *previous* ocx binary, which no gate in this repository has; it is a property of code being deleted. Verified by reading `first_unknown_placeholder`'s allowlist on the released version, not by a test. |
| **S-018** | Publisher declares `binaries` implicitly via `PATH = "${self.installPath}/bin"` and runs `ocx package create --bin-scan` | The scan finds the directory and fills the claim, identically to the `${installPath}` spelling (C-010). |
| **S-019** | Publisher targets Linux with `PATH = "${self.installPath}/bin"` and mismatched libc | The libc lint scans `bin` and refuses, identically to the `${installPath}` spelling (C-011). |
| **S-020** | A package published before this change containing `$${installPath}` or `$${deps.NAME.installPath}` is installed | Resolution changes from `$<content-path>` (or `$<dep-content-path>`) to the literal token text — `${installPath}` or `${deps.NAME.installPath}`. Both bodies were legal grammar before this ADR; `${self.installPath}` and `${self.env.KEY}` were not, so no already-published package can carry either of those in any spelling. See **Migration**. |
| **S-021** | Publisher puts a shell fragment `mkdir /tmp/$${BUILD_ID}` in a `constant` | **Published, and rewritten**: resolves to `mkdir /tmp/${BUILD_ID}`. In a shell `$$` is the PID, so PID-then-variable silently becomes variable. **No warning fires** — D13 deletes the warning classes. Irreducible: `$${` must mean *something* (D2). |
| **S-022** | Publisher writes `$${workspaceFolder}` intending the consumer to see a literal | **Published**; OCX emits `${workspaceFolder}`, which VS Code then expands. This is the **primary** #221 authoring path, not an edge case. The escape defends against OCX only — a literal at the *consumer* is the consumer's own escaping problem and OCX has no spelling for it. |
| **S-023** | Publisher writes a **`path`** var `"value": "$${workspaceFolder}/bin"` | Escape fires, then the resolved value is relative, so the resolver joins it under the install path: `<install>/${workspaceFolder}/bin`. With `"required": true` and no such directory, publish-time resolution raises `RequiredPathMissing` (C-006b). |
| **S-024** | A generator (Go `map`, Java `HashMap`) emits `env` from an unordered structure, and the package uses `${self.env.X}` | **Non-deterministic**: publishes on runs where `X` happens to land earlier, fails `UndefinedSelfEnvRef` (exit 65) on runs where it does not, with no change to the generator's input. Documented in the reference: emit `env` from an ordered structure (D6). |
| **S-025** | Publisher writes a value containing `${` with no closing `}` | **Published**, byte-identical, as literal text (Axis D). No token exists, so nothing resolves; the publisher gets no feedback on a truncated token. Accepted risk. |
| **S-026** | Consumer on an **older** ocx pulls, installs and runs `ocx package inspect` / `info` on a package that uses a newer token | Pull, install and both read-only commands **succeed**; the token is shown verbatim. `ocx env` / `ocx package exec` on the same package **fail**, exit 65, naming the token. Looking at a package never becomes impossible because it is too new; running it does (D14, C-036, C-038). |
| **S-027** | Publisher runs `ocx package create` on a document containing `${workspaceFolder}` | **Rejected**, exit 65, before anything reaches a registry — the explicit publish gate, not `ValidMetadata::try_from` (D14, C-037). |

---

## Error Taxonomy

All variants live on `TemplateError` (`metadata/template.rs`), classified via
`ClassifyExitCode`. Messages follow `C-GOOD-ERR`: lowercase, no trailing punctuation,
acronyms preserved.

| Variant | Status | Fires when | Exit |
|---|---|---|---|
| `UnknownToken { token, hint }` | **renamed** from `UnknownPlaceholder`; job kept, `hint: UnknownTokenHint` added | any `${…}` that is not one of the four recognised bodies and is not more specifically diagnosed below — covers a body that fails the grammar, a recognised root with a body outside the closed set, and every unrecognised root (D3, D13) | 65 `DataError` |
| `UnknownField { namespace, field, supported }` | **new** (generalises `UnknownDependencyField`) | recognised namespace shape, unknown leaf — `${self.foo}`, `${deps.cmake.version}` | 65 |
| `UnknownModifier { modifier, supported }` | **new** | `:frobnicate`, `:POSIX` | 65 |
| `UndefinedSelfEnvRef { key, declared_before }` | **new** | `${self.env.K}` where `K` is not declared earlier in the same package (covers forward and self references) | 65 |
| `AmbiguousSelfEnvRef { key }` | **new** | `${self.env.K}` where `K` is declared ≥2× earlier (D7) | 65 |
| `DisallowedToken { token }` | unchanged | recognised token not permitted by the active `AllowedTokens` — `${deps.*}` or `${self.env.*}` in args | 65 |
| `UnknownDependencyRef { ref_name, declared }` | unchanged | `${deps.NAME.*}` where `NAME` is not a declared direct dep | 65 |
| `AmbiguousDependencyRef { ref_name, first, second }` | unchanged | two direct deps share the interpolation name | 65 |
| `DependencyNotInstalled { ref_name, dep_identifier }` | unchanged | declared dep's content path absent on disk | 79 `NotFound` |
| `ResolvedValueTooLarge { limit }` | **new, post-hoc** — not drafted above, added during implementation | resolved output for one template exceeds `MAX_RESOLVED_VALUE_BYTES`, checked after every substituted segment | 65 |
| `UnknownDependencyField { … }` | **deleted** (absorbed by `UnknownField`) | — | — |
| `MalformedToken { … }` | **not minted** | — | — |

Net delta: four new variants, one rename with one added field, one generalisation, one
deletion. The withdrawn design's `MalformedToken` is not built: under a claim-everything rule
there is no boundary at which "does not parse" and "parses but is unknown" call for different
publisher action, and `UnknownToken`'s three message branches (D13) serve both.

**What shipped (recorded post-hoc): a resolved-value size budget.** `${self.env.KEY}`
substitutes the referenced var's *resolved* value (D6.2), and that value can itself contain
another `${self.env.*}` reference — so a chain of vars each doubling the previous one's length
turns a metadata document small enough to publish into a resolved value with no upper bound,
reconstructed by every consumer that resolves it (amplification; CWE-400, CWE-409, CWE-776).
Neither this concern nor the variant was part of the drafted taxonomy above. What shipped:
`MAX_RESOLVED_VALUE_BYTES = 64 * 1024` (`template.rs:52`), checked in `resolve_inner` after
every segment is appended to the output buffer — not once at the end, so a value already over
budget cannot be handed another token's worth to double (`template.rs:301-303`) — and
`TemplateError::ResolvedValueTooLarge { limit: usize }` (`template.rs:753`), classified by
`ClassifyExitCode` to `ExitCode::DataError` = 65 (`template.rs:767`).

Wrapping context is unchanged: `Error::EnvVarInterpolation { var_key, source }` for env
values, `Error::EntrypointArgInterpolation { entrypoint, arg, source }` for args. `#[source]`
on both; `#[non_exhaustive]` on `TemplateError` (it is an error enum, so the internal-enum
exhaustiveness convention does not apply).

There are **no warning variants and no `tracing::warn!` calls** on this path (D13).

**Where these fire is D14, not `ValidMetadata::try_from`.** Every variant above is raised by the
explicit publish gate or by `TemplateResolver::resolve`. None is raised by simply reading a
metadata document, so an unrecognised token is inert on `inspect` / `info` / `describe`.

---

## Migration / Rollout

**The publish accept-set only grows.** That is the whole migration story, and it is a direct
consequence of D3: OCX rejects unrecognised `${…}` today and rejects it after, so nothing that
publishes today becomes unpublishable, and nothing that is rejected today starts resolving to
something OCX did not choose. Concretely, these move from rejected to accepted —
`${self.installPath}`, `${self.env.*}`, `:native`/`:posix`, and `$${…}` in any form — and
nothing moves the other way.

**The error→silence risk class does not arise.** The withdrawn design inverted a hard error
into silent pass-through, whose closest precedent is Kubernetes CRD structural-schema pruning
(unknown fields moving from erroring to silently dropped — research §2.6). That analysis is
**deleted rather than adapted**: there is no such flip in this design, so there is no migration
risk to mitigate, no `x-kubernetes-preserve-unknown-fields`-shaped escape hatch to design, and
no residual "a typo publishes silently" risk to accept.

**Already-published packages — two changes, honestly stated.**

- `${installPath}` and `${deps.NAME.installPath}`: byte-identical resolution. Every existing
  unit test in `template.rs` and `env/resolver.rs` asserting these values must pass unchanged.
- **`$${` immediately preceding a recognised body is the first exception — and the affected
  class is two bodies, not one.** Old code had no escape and recognised each token form
  independently over raw bytes, so a `$$` immediately before a body old code already resolved
  left that body substituted, with a stray leading `$` kept in the output. Exactly two bodies
  were legal, publishable grammar before this ADR: bare `${installPath}` (`$${installPath}` →
  `$<content-path>`) and `${deps.NAME.installPath}` (`$${deps.cmake.installPath}/bin` →
  `$<cmake-content-path>/bin`, via `DEP_TOKEN_PATTERN.captures_iter` — unanchored, so it
  matches the inner token starting at byte 1 regardless of the extra leading `$`; `slug.rs:25`,
  `template.rs:226` at `e454ce83`). After D2 both resolve to the literal token text instead —
  `${installPath}` and `${deps.cmake.installPath}/bin`. `${self.installPath}` and
  `${self.env.KEY}` are **not** in this class: neither existed as legal grammar before this
  ADR, so no already-published package can contain either spelling, escaped or not —
  `UNKNOWN_TOKEN_RE`'s publish-time catch-all rejected both regardless of a leading `$$`
  (`validation.rs:34-46` at `e454ce83`). The probability of a published package containing
  either affected spelling is very low — a `$`-prefixed path is nonsense — but the change is
  real, it is on the read path, and it is one of two places this ADR does not preserve
  published behaviour. It ships as a **BREAKING** commit subject; see OQ-1.
- **The resolved-value budget is the second, and it is post-hoc — not drafted anywhere above.**
  A document that composes today with no error can, after this change, exit 65 with
  `ResolvedValueTooLarge` if resolving one of its values crosses `MAX_RESOLVED_VALUE_BYTES`
  (64 KiB). Unreachable before this ADR: nothing composed a value out of another value's
  *resolved* bytes, so nothing could grow past a small template's own length. `${self.env.KEY}`
  chains make it reachable — see the post-hoc note under Error Taxonomy. Also a **BREAKING**
  commit subject.
- Two runtime behaviours tighten from "pass through as literal" to "hard error", both
  unreachable for published packages because publish already rejects them:
  `${deps.Python.installPath}` (uppercase) and `${deps.x.version}` at resolve time. The
  existing tests `template::uppercase_dep_name_not_matched` and
  `resolver::uppercase_dep_name_not_matched` invert and are rewritten in place.
  A third existing test is rewritten for a different reason — not a behaviour inversion but a
  deleted variant: `env::resolver::unsupported_field_returns_error` (`env/resolver.rs:261-278`)
  matches on `TemplateError::UnknownDependencyField`, which D12 deletes, so the crate does not
  compile with tests enabled until it moves onto `UnknownField`. This is the complete exception
  list to the "**Not modified**, as the S-002 regression proof" statement in Documentation &
  Schema Surfaces.

**One layering change, and the test that inverts with it (D14).** `validate_env_tokens` and
`validate_entrypoint_args` leave `ValidMetadata::try_from`, so every ingress site listed in
D14 stops refusing on token grounds. The existing unit test
`inspect::tests::inspect_default_malformed_metadata_is_internal`
(`tasks/inspect.rs:1533`) asserts today's behaviour — that `inspect` on a document whose env
references an undeclared dependency surfaces `PackageErrorKind::Internal` — and **inverts**:
after D14 that inspect succeeds and shows the token verbatim. It is rewritten in place, and it
is the sharpest available red state for C-036. `ValidMetadata`'s own doc comment is corrected
in the same commit: after D14 it means *structurally readable*, not *resolvable*.

**Publishers.**

- Nothing a publisher writes today needs editing.
- A payload containing `${…}` that OCX must carry verbatim must be authored `$${…}`. That is
  new capability, not a migration: no such payload can be published today at all.
- Adopting `${self.*}` or a render modifier **raises the package's effective minimum ocx**,
  because an older reader fails closed at `ValidMetadata::try_from` (S-017) — that is the
  *already-released* binary's behaviour and D14 cannot change it retroactively. D14 makes the
  **next** such boundary softer: from this release on, an ocx meeting a token it does not know
  can still read the package and refuses only on use (S-026). Same publisher
  guidance as `list` (`adr_env_modifier_types.md` D4): wait for your fleet floor. No dual-form
  parsing, no warning schedule, no migration prose in user docs — the changelog line is the
  commit subject.

**Schema.** No shape change. Research §0.8 confirms no token syntax is `pattern`-constrained
anywhere in the generated schema. Five `description` sites go stale and must be rewritten in
the same commit: `Path.value`, `Constant.value`, `List.value`, the `Entrypoints` manual
`JsonSchema` prose (`entrypoint.rs:311-328`), and the `template.rs` module doc.

**Never touched.** `ProjectEnv` (`project/env.rs:28`) states "values are literal in v1, no
interpolation of any kind". This ADR does not extend the grammar to `ocx.toml`'s `[env]`. Doc
wording must not imply otherwise.

---

## Slice Boundaries

**Two implementation slices, ordered, not independently shippable.** The slices are a **design
and review ordering** — what must be decided and correct before what — not release boundaries:
the second parses through the first's scanner, and the execution decomposition cuts work
packages across both.

| Slice | Contents | Value when complete | Contracts |
|---|---|---|---|
| **S1 — grammar foundation, including render modifiers** | Scanner (D1) · escape (D2) · claim-all rule (D3) · `${self.installPath}` alias (D4) · render modifiers `:native`/`:posix` (D5) · one-recogniser rewrite of `classify_install_path_rooted_dir` and `libc_lint::resolve_scan_scope` (D10) · error taxonomy (D12) · diagnostics (D13) · failure-timing split (D14) | `${self.installPath}` reads correctly; #221's payloads become publishable via the escape *and* Windows paths embeddable in unescaped JSON; five recognisers become one; `$${` stops silently double-resolving; an older ocx can still read a newer package | C-001…C-017, C-030…C-038 |
| **S2 — scoped self env** | `${self.env.VAR}` (D6–D8), composer resolve-then-gate split | A package stops repeating a path three times; the last token in the target grammar lands | C-018…C-029 |
| **S3 — #221 consumption** | Out of scope here. `customizations` consumes S1 whole, and owns its own authoring-affordance question (D3). | — | — |

**Two deviations from the issue's slicing, both stated.**

1. **The escape is folded into S1** rather than shipping as a separate unit. The escape is not
   a feature layered on the scanner, it is the scanner's first branch (R1), and under D3 it is
   the only exit from the claimed space — shipping the scanner without it would mean shipping a
   release in which no payload containing `${…}` can be published at all, *and* in which
   `$${installPath}` still silently double-resolves through new code. Splitting them would also
   make C-001's red state unreachable in S1.
2. **Render modifiers are folded into S1 too.** #303 orders the slices `${self.*}` →
   `${self.env.VAR}` → render modifiers, written **before** D1 settled on one scanner. With one
   scanner, the modifier grammar *is* part of the grammar foundation:
   `render(s, RenderModifier, Host)` is a prerequisite of the scanner, and the scanner owns
   `RenderModifier` parsing and `UnknownModifier`. A nominal "modifiers ship later" boundary
   would therefore **accept and publish modifier-bearing metadata** before the modifier work
   was considered shipped or reviewed as such — on a permanent wire format. That is the one
   thing this ADR cannot risk, so the boundary is moved rather than defended.
   `${self.env.VAR}` stays second, which is where #303 put it, so this is the only ordering
   divergence that remains.

   The alternative was to keep them separate and have the scanner **reject every modifier**
   until a later slice added parse + render + validate + publish-acceptance atomically. That
   works, but it buys a boundary nobody ships across at the cost of an error arm that exists
   for one release and is then deleted — and it would make `${installPath:posix}` an
   `UnknownToken` in a released version, which is a wire-format statement OCX would then have
   to take back.

**Ordering is a hard dependency chain**, not a preference: S2 parses through S1's scanner.

**A second ordering constraint inside S1.** D14's publish gate must land in the same commit as
the removal of the check from `ValidMetadata::try_from`. Between the two there is a window in
which `ocx package create` accepts an unrecognised token and pushes it to a registry — the one
outcome the whole grammar exists to prevent.

**One ordering constraint *inside* S1.** The `classify_install_path_rooted_dir` /
`libc_lint::resolve_scan_scope` rewrite (D10) must land before the publish path accepts
`${self.installPath}` (D4). D4 creates the fail-open hazard and D10 closes it (see D10);
between the two there is a window in which `ocx package create` on
`PATH = "${self.installPath}/bin"` produces an empty libc scan scope and reports "nothing to
check". Within one commit this is free; across two commits it is an ordering requirement.

---

## Constitution Gate

Re-checked against the **reduced** design, per
[`arch-principles.md`](../rules/arch-principles.md),
[`quality-core.md`](../rules/quality-core.md), [`quality-rust.md`](../rules/quality-rust.md),
[`quality-rust-errors.md`](../rules/quality-rust-errors.md),
[`quality-rust-exit_codes.md`](../rules/quality-rust-exit_codes.md), and
[`CLAUDE.md`](../../CLAUDE.md).

**Clean:**

- Crate layout — the substance is all in `ocx_lib`; the CLI stays thin. With D13's warning classes
  deleted there is no diagnostic plumbing question left to answer, and D14's publish gate is a
  lib-side function, not a check bolted onto `command/package_create.rs`.

  **Corrected at execution time — an earlier revision claimed `ocx_cli` gains no code *at all*, and
  that is false.** The two publish-path call sites are *in* `ocx_cli`:
  `command/package_create.rs:167` and `command/package_push.rs:191` both call
  `ValidMetadata::try_from` directly and rely on it to do the token check. Once that check leaves
  `try_from`, those two lines **silently stop enforcing the publish gate** — a typo reaches the
  registry, the one outcome the grammar exists to prevent. Both must swap to the stricter lib-side
  constructor. That is two one-line constructor swaps, so the *intent* of the rule holds — no logic
  moves into the CLI — but "zero `ocx_cli` edits" was an unchecked claim and is withdrawn.
  `command/package_test.rs:138` and `command/patch_test.rs:347` do **not** need the strict gate:
  their commands refuse later via `resolve_env` in the same invocation.

  **Second correction, from WP2's implementation — `package_create.rs` is a swap *and a move*,
  and the move is deliberate.** On the base the call was the arm's last step, *after*
  `libc_lint::check_declared_libc`; it now runs before it. It has to. The libc lint resolves a
  scan scope out of interface `PATH` values, so a document carrying an unrecognised token in one
  of those values reached the lint first and exited 65 as `UnresolvableScanScope` — a message
  about a libc scope, for a typo'd token. With the order swapped the publisher is told which
  token is wrong. The visible consequence, stated rather than left to be rediscovered: for a
  document that fails *both* checks, the reported error changes. `package_push.rs` needed no
  move — it has no libc lint — so it stayed the one-line swap the bullet above describes.
- Module structure — new files `metadata/template/scanner.rs` and `metadata/template/render.rs`
  under the existing `template.rs` parent. One concept per file, no `mod.rs`. **There is no
  `metadata/template/` directory today** — `template.rs` is a plain file — so neither new file
  is compiled until `template.rs` declares `mod scanner;` / `mod render;`. A stub landing
  without those two lines type-checks green while being dead text.
- Internal enum exhaustiveness — `Segment`, `Token`, `RenderModifier` carry no
  `#[non_exhaustive]`; `TemplateError` keeps it (error-enum exemption).
- Error style — all messages lowercase, no trailing punctuation, `#[source]` preserved on
  wrapping variants, three-layer chain intact.
- Exit codes — every new variant maps to an existing code (65 / 79). No new code minted.
- No stability shims — `UnknownDependencyField`, `first_unknown_placeholder`,
  `disallowed_dep_token`, `UNKNOWN_TOKEN_RE` and `DEP_TOKEN_PATTERN` are **deleted**, not
  deprecated; `UnknownPlaceholder` is renamed in place, as if the old name never existed.
- Type economy — `UnknownPlaceholder`'s job is kept rather than deleted-and-re-minted, and
  `UnknownField` absorbs `UnknownDependencyField` instead of a parallel hierarchy. No
  `MalformedToken`. No `RESERVED_ROOTS` constant: the closed set of four bodies is the single
  source of truth and the root list is derived from it.
- Utility catalog — the `:posix` transform is `str::replace('\\', '/')`; nothing new is
  upstreamed to `utility/`.
- Non-domain code — D13's edit distance is **`strsim`**, not hand-rolled. `strsim` 0.11.1 is
  already resolved in `Cargo.lock`, so the change is a declaration at zero link cost — in
  **two** manifests, per this repo's convention: `strsim = "0.11.1"` under
  `[workspace.dependencies]` in the root `Cargo.toml`, and `strsim.workspace = true` in
  `crates/ocx_lib/Cargo.toml`. `deny.toml` needs no edit (MIT is already allowed). It goes
  through `subsystem-deps.md`'s protocol like any other dependency addition. This leaves the
  scanner (Deviation 1) and the `:posix` transform (Deviation 2) as the only two things this
  ADR newly owns.
- YAGNI — no trait registry for token kinds (two capability flags), no cycle detector (D6
  makes cycles unrepresentable), no per-token modifier applicability table (D5 allows the
  modifier everywhere), **no reserved-but-undefined root** (D3 makes future roots additive, so
  reserving one would be building an option nothing needs), **no warning classes** (D13).
- `deny_unknown_fields` — untouched; no config-tree struct is in scope.
- `CHANGELOG.md` — not edited. The changelog line is the commit subject.

**Deviations** (re-stated against the reduced design; anything that no longer applies is
deleted rather than carried):

| # | Rule | Deviation | Justification |
|---|---|---|---|
| 1 | `quality-core.md` — *Don't Own Non-Domain Code* (**Block-tier** for anything parsing an external wire format) | A hand-written parser for token syntax embedded in published `metadata.json` | **Re-argued, and the exposure is smaller than it was.** Criterion 1 re-checked against research §2.1's recorded table under the *reduced* requirement — closed vocabulary, hard error on unknown, `$$`-escape, `:modifier` suffix. The pass-through requirement that eliminated most crates is gone, and `subst` now matches on the error-on-unknown axis, but **no surveyed crate ships a `$$`-style escape or a `:modifier` suffix**, so all seven still fail. A **parser-combinator** library is a different search: `winnow` (A3) does implement the requirement at zero link cost and scores *Structural* on escape correctness exactly as A1 does. A1 is chosen over A3 on auditability of a three-branch loop (Axis A), **accepting a Block-tier deviation A3 would have avoided.** What changed: the one part of the old design with no battle-tested precedent — research §1.4's "claim my namespace, leave everyone else's bytes alone on the same delimiter" — **is not built at all**, so the scanner is now doing what `subst`/`envsubst-rs`/today's OCX already do rather than standing at the frontier. **Mitigations required, not optional:** (a) the golden corpus (C-004), now asserting *rejection* — cheaper and stronger than asserting a leave-alone path; (b) contracts C-001…C-011 each demonstrably red before green; (c) the scanner is a pure function with no I/O, so exhaustive unit coverage is cheap; (d) **a property/roundtrip invariant** (C-034) — `scan(escape(s)) == s` for arbitrary `s`, plus byte conservation; (e) **a non-ASCII contract** (C-035). (d) and (e) are not optional either: `quality-core.md`'s worked example for this exact rule is a hand-written emitter whose unit test *and* doc comment both affirmed the wrong escape boundary while no golden fixture contained the offending byte. |
| 2 | `quality-core.md` — same rule | A hand-rolled path-separator transform instead of a crate | Exemption criterion 3 (a few lines, no edge cases) *plus* criterion 1: `typed-path`'s `with_unix_encoding()` drops the drive letter — documented, intentional behaviour for a Windows→Unix *encoding* conversion, and correct for what that crate is for; it is simply not the operation `:posix` needs (a slash flip that keeps the drive). `path-slash` does exactly the right thing but has been unreleased since 2022-08. Input space is constrained to paths OCX generated itself. UNC and verbatim prefixes are declared explicit non-goals (D5), not silently mishandled. Re-open the buy question if arbitrary/untrusted Windows paths ever reach this code. |
| 3 | `arch-principles.md` — *Type names: full descriptive names* | Three types named `Modifier`-ish coexist (`Modifier`, `ModifierKind`, new `RenderModifier`) | Renaming `env::modifier::Modifier` → `VarKind` is the correct end state and is permitted (internal names carry no stability), but it is a large mechanical rename across many files, orthogonal to this grammar, and bundling it would make the #303 diff unreviewable. `RenderModifier` is unambiguous at every call site; user-facing docs already separate *type* from *render modifier* (D5). Recorded as a follow-up refactor, not a gate. |
| 4 | `adr_entrypoint_args_interpolation.md` D6 — "one tokenizer scans `${…}` segments and classifies them" | Not a deviation but a **correction of the record**: that type was never built | D1 builds it. This ADR supersedes D6's factual claim about the implementation while preserving D6's *decision* (capability gate, gate-before-substitution) intact and strengthened — the ordering becomes structural rather than a hand-placed early return (D9). **Execution-time action:** `adr_entrypoint_args_interpolation.md` D6 gains a superseded note pointing here, delivered by the documentation work package on the same branch. |
| 5 | Behaviour preservation on the read path (`CLAUDE.md` — *metadata and OCI manifest changes stay backward compatible on the read path*) | `$${installPath}` and `$${deps.NAME.installPath}` resolve differently after D2 | Unavoidable: the escape is unimplementable while `$${` immediately preceding a body old code already recognised also resolves that body's inner token, and the two readings are mutually exclusive. The affected shapes are nonsensical (a `$`-prefixed path) and no such package is known. Surfaced as a BREAKING commit subject and as OQ-1 for the owner, rather than smoothed over. |

---

## Open Questions

**OQ-1 — Is the `$${installPath}` / `$${deps.NAME.installPath}` behaviour change acceptable,
and does it carry a BREAKING marker?**
D2 changes how two byte-sequence classes resolve in already-published metadata: bare
`${installPath}` and `${deps.NAME.installPath}` are the only two bodies that were legal,
publishable grammar before this ADR, so they are the only bodies a `$$`-prefixed spelling
could have reached (S-020, Deviation 5). It is the only escape-driven change in the ADR; a
second, unrelated behaviour change — the resolved-value budget — is recorded separately in
Migration.
*Recommended: accept, and ship slice S1 under a `feat(metadata)!:` subject so `git-cliff`
renders it as **BREAKING**.* The affected shapes are `$`-prefixed paths — semantically
meaningless — and the alternative is having no escape at all, which under D3 means no payload
containing `${…}` can ever be published. The commit subject is the only place this is
announced; no migration prose in user docs.

**OQ-2 — Reader-first release staging, or reader and writer together?**
`adr_env_modifier_types.md` D4 established that a reader shipping one release ahead of the
writer softens the fleet-floor jump; in practice `list` shipped both together.
*Recommended: ship together.* The read-side already fails closed (S-017), the write side is
publisher opt-in, and a staged release buys a smaller floor only for publishers who adopt
`${self.*}` in the very first week. Owner's call at release time; either way the publisher
guidance is identical — adopting the new grammar raises your effective minimum ocx.

**OQ-3 — Accept that slice S2 surfaces template faults in non-crossing env vars?**
D8's resolve-then-gate ordering means a broken template in a `sealed`/non-crossing var now
errors where it previously never ran. *Recommended: accept.* A package whose own metadata
cannot resolve is broken regardless of which surface is being asked, and the alternative —
resolving `${self.env.*}` against the active surface — makes the same interface var produce
different bytes under `ocx env` and `ocx env --self`, which is the exact bug class the
two-env composition unification exists to prevent.

The three assertions that would otherwise become new failures are all moved to the emit path
rather than weakening D8 — see D8's table. Two are closed mechanically:
`RequiredPathMissing` (C-026) and `DependencyNotInstalled`, exit **79**, which is the
sharp one: `build_dep_context_map` maps every declared dep to a content path, falling back to
the declaration identifier when absent, so a non-crossing var naming an uninstalled dep would
turn a working install into a hard failure (C-027).

**The third is decided here rather than left open:** `SeparatorEdgedListValue`
(`env/resolver.rs:109`) is also **emit-only**. It asserts a property of a *composed
contribution* — that the resolved value will not make the list fold's flank match ambiguous —
and a var that never joins a fold has nothing to be edged against. Deferring it costs nothing:
the moment the same var does cross on some surface, the assertion fires there. What the owner
is being asked to accept is only the first paragraph; the mechanics are settled.

*(The withdrawn design's fourth question — whether to reserve an `ocx` root — is **withdrawn,
not replaced**. Under D3 a future root is additive, so there is nothing to reserve.)*

---

## Documentation & Schema Surfaces

Every surface below lands on the **same branch** as the behaviour it describes. The schema
`///` edits are the one group where the stronger "same commit as the behaviour" claim is free
to keep: those five files are untouched by every other work package. The website pages and the
prior-ADR note are genuinely cross-cutting and stay in the documentation work package.

**Schema descriptions (no shape change):**
- `metadata/env/path.rs` — `Path.value` `///`
- `metadata/env/constant.rs` — `Constant.value` `///`
- `metadata/env/list.rs` — `List.value` `///`
- `metadata/entrypoint.rs:311-328` — `Entrypoints` manual `JsonSchema` description prose
- `metadata/template.rs:4-7` — module `//!` doc

**Website:**
- `website/src/docs/reference/metadata.md` — the token grammar section: **OCX expands every
  `${…}`**, the four recognised bodies, the hard error on anything else, the escape as the
  only way to emit a literal `${…}` *including the two cases it does not cover* (D2 — `$${`
  inside a shell fragment, and an escaped foreign token the consumer still expands), render
  modifiers, `${self.env.*}` scoping rules, and the unterminated-`${` rule (Axis D). Uses
  `${self.installPath}` in every example; mentions `${installPath}` once as the original
  spelling (D4). States D14's line plainly: a token this ocx does not recognise blocks
  publishing and blocks running, and is shown verbatim by read-only commands.
- `website/src/docs/reference/env-composition.md` — `${self.env.*}` declaration-order rule, the
  "resolved contribution, not folded value" rule (D6.2), the *type*-dependence of the
  referenced bytes (D6.5), and the **generator hazard**: emit `env` from an ordered structure
  (S-024).
- `website/src/docs/user-guide.md` — only if the token vocabulary appears in a narrative.
- `website/src/docs/reference/environment.md` — unchanged (no new env var).

**Dependencies — two manifests, not one:**
- `Cargo.toml` (workspace root) — add `strsim = "0.11.1"` to `[workspace.dependencies]`.
- `crates/ocx_lib/Cargo.toml` — add `strsim.workspace = true`.
- `deny.toml` — **no change.** `strsim` is MIT, which `deny.toml:17` already allows.
- Follows `.claude/rules/subsystem-deps.md`.

**Prior ADRs (record correction, Deviation 4):**
- `.claude/artifacts/adr_entrypoint_args_interpolation.md` — D6 gains a superseded note: the
  tokenizer it described was never built; this ADR's D1 builds it, and D6's *decision*
  survives, strengthened by D9. Lands on the same branch, in the documentation work package.

**Rules (same commit, per the catalog protocol):**
- `.claude/rules/subsystem-package.md` — `metadata/template.rs`, `metadata/validation.rs`,
  `metadata/slug.rs`, `bin_scan.rs`, `libc_lint.rs` rows.
- `.claude/rules/subsystem-metadata-schema.md` — only if a custom `JsonSchema` impl changes
  (expected: none).
- `.claude/rules/arch-principles.md` — ADR index row.
- `.claude/rules.md` — no new rule file; no catalog change expected.

**Tests:**
- Unit, `scanner.rs` — C-001…C-005, C-008, C-009, C-034, C-035.
- Unit, `render.rs` — C-012, C-014, C-015. Only the pure `render(s, RenderModifier, Host)`
  legs live here.
- Unit, `template.rs` — C-006, C-007, C-010, C-013, C-016, C-017, C-027, C-030…C-032. All
  resolver-level. **C-010 belongs here, not under `bin_scan`:** it asserts on
  `classify_install_path_rooted_dir`, which lives at `template.rs:86`. **C-016 is
  `#[cfg(windows)]`** and therefore runs only on `verify-deep`'s Windows leg.
- Unit, `validation.rs` — C-033 (all message branches, publish-side).
- Unit, `libc_lint.rs` — C-011.
- Unit, `composer` / `resolver` — C-018…C-026, C-028, C-029.
- Property: C-034 as a `proptest` alongside the scanner's unit tests.
- Golden fixture: the rejection corpus (C-004) as a single table-driven test, so adding a token
  to the corpus is a one-line change.
- Acceptance: `test/tests/test_entrypoints.py` (C-028 runtime leg), `test/tests/test_env.py` or
  equivalent (C-024 surface-independence across `ocx env` / `ocx env --self`; S-001 alias
  publish-and-compose; S-003/S-022 — an unescaped foreign token is rejected and the escaped
  form publishes). **C-036…C-038 are acceptance-level by nature** — they assert on whole
  commands and exit codes across one installed fixture — and belong in one test module so the
  fixture is built once: pull/install succeed, `inspect` / `info` succeed and echo the token,
  `env` / `exec` exit 65, `package create` exits 65.
- Rewritten in place: `template::uppercase_dep_name_not_matched`,
  `env::resolver::uppercase_dep_name_not_matched`,
  `env::resolver::unsupported_field_returns_error`, and
  `inspect::tests::inspect_default_malformed_metadata_is_internal` — which **inverts** under
  D14 and is C-036's unit-level red state (see Migration).
- **Not modified, as the S-002 regression proof:** every other existing value assertion in
  `template.rs` and `env/resolver.rs`.

---

## Consequences

**Positive**
- One recogniser replaces five. The sixth grammar addition updates one place.
- **The closed world is kept, and that makes every future grammar addition additive.** No root
  reservation, no freeze, no one-way door on the vocabulary: a new root, field or modifier in a
  later release only makes previously-rejected documents publishable, and an older reader still
  fails closed. The withdrawn design had to spend a one-time reservation budget to buy a weaker
  version of the same property.
- **Every typo in every token is caught at publish** — `${slef.env.HOME}`, `${installpath}`,
  `${instalPatch}` included, with a suggestion. The withdrawn design accepted these as residual
  risk guarded only by a warning.
- **OCX is not at the frontier.** Research §1.4 found no tool that documents "claim my
  namespace, pass everyone else's tokens through byte-identical on the same delimiter" as a
  tested pattern; that is the thing no longer being built. Claim-everything-closed-world is
  what OCX already does and what several surveyed crates do.
- **No layer caveat to explain.** There is no scanner-layer-versus-resolver-layer byte-identity
  distinction, because nothing passes through.
- #221 becomes buildable via the escape, and Windows paths render into valid JSON.
- Two silent-degradation hazards — `bin_scan`'s missed claim (via
  `classify_install_path_rooted_dir`) and the previously-unnamed `libc_lint` fail-open — are
  created by D4 and closed by D10 within the same release, with contracts whose red state is
  reachable on `main` (C-010, C-011).
- The gate-before-substitution correctness claim becomes structural rather than positional.
- Cycle detection is not deferred — it is designed out.
- **An older ocx meeting a newer package stays usable as a reader** (D14). Refusal moves from
  "every path that reads metadata" to "every path that resolves a value", so `inspect`, `info`
  and `describe` keep working and only execution refuses. The enforcement point becomes the
  operation that actually needs the value, and nothing new is added on the execute side —
  resolution already cannot produce bytes for a token it does not recognise.
- The design is materially smaller: no reserved-root set, no foreign-token path, no publish
  warning classes, no near-miss lint infrastructure, and a scanner of roughly 80–130 lines
  instead of 120–180.

**Negative**
- **#221's stated contract changes, and this is the headline cost.** A VS Code
  `${workspaceFolder}` or a devcontainer `${localEnv:HOME}` inside a `customizations` payload
  does **not** survive byte-identical. Every such payload must be authored `$${workspaceFolder}`
  / `$${localEnv:HOME}`, and an unescaped one is a publish error (S-003). The burden is
  proportional to the number of tokens in the payload and falls hardest on generated or
  copy-pasted devcontainer blobs, where a producer that knows nothing about OCX emits
  unpublishable output. **#221's stated byte-identical guarantee is superseded by this ADR and
  its issue text needs amending** — a copy-pasted VS Code or devcontainer settings block will
  not work unmodified. The per-surface alternative (a `Usage::Customizations` variant whose
  unknown-token policy is "emit verbatim") was considered and rejected in favour of one rule.
- **A package with an unrecognised token now installs and fails later** (D14). The refusal moved
  off the ingress path so the read-only surface is reachable at all, which means an older ocx
  discovers the incompatibility at first `ocx env` / `ocx package exec` rather than at `ocx package install`.
- A hand-written parser on a published wire format, chosen over `winnow` — which would have
  avoided the Block-tier deviation — on auditability and simplicity. Mitigated, but owned
  (Deviation 1).
- The escape rewrites publisher bytes in payloads OCX does not own; `$${` in a shell fragment
  turns PID-then-variable into variable (S-021), **with no publish warning**, because D13
  deletes the warning classes. Irreducible — an escape must consume some byte sequence.
- One published-behaviour change (`$${installPath}`), and one composer ordering change whose
  blast radius is non-crossing env vars (OQ-3).
- Adopting the new grammar raises a package's effective minimum ocx — publishers must wait for
  their fleet floor.
- The `Modifier` / `ModifierKind` / `RenderModifier` naming cluster stays awkward until a
  separate rename lands.

**Risks**
- **Escaping is per-token and easy to get wrong at scale.** A payload with twelve VS Code
  tokens needs twelve escapes; missing one is a hard publish error, which is the fail-closed
  direction, but the friction is real and repeats on every payload edit. The mitigation, if it
  is ever needed, is an authoring affordance in #221 — deliberately not designed here.
- Publishers will reflexively expect `${self.env.*}` in entrypoint args (it works in env
  values). The dedicated `DisallowedToken` message — naming the entrypoint, the arg, and the
  policy — is the mitigation, exactly as it was for `${deps.*}`.
- Declaration order becomes a second-order correctness property for `${self.env.*}`: a
  generator emitting `env` from an unordered map publishes non-deterministically (S-024).
  Documentation is the only guard; OCX cannot see that the producer was a map.
- A truncated token (`"${installPath"`) publishes silently as literal text (S-025, Axis D). No
  value resolves wrongly — the bytes are what the author typed — but the publisher gets no
  feedback. Accepted in exchange for keeping the accept-set monotone.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-08-09 | Michael Herwig (architect, opus) | Initial draft. Settles D1–D13; three-option trade-offs on scanner strategy, `${self.env.*}` resolution, and foreign-token policy; formal grammar; C-001…C-028; S-001…S-020; four slices. |
| 2026-08-09 | Michael Herwig (architect, opus) | Revision after a three-reviewer panel (spec / architecture / SOTA). D3 reserved a fourth root `ocx`; D13 adopted rustc's length-scaled edit-distance threshold via `strsim`, added an escape-fired warning, settled its emission site; D8's split widened to *every* filesystem and shape assertion. Axis A dropped weights that did not select the winner and recorded that `winnow` would have avoided the Block-tier deviation; Axis B re-scored B2's machinery. New: C-029…C-037, S-021…S-025. |
| 2026-08-09 | Michael Herwig (architect, opus) | Second revision, after a cross-model (Codex) adversarial gate and a spec re-validation. ABNF replaced by an ordered scanner specification (R1–R3); root extraction made body-independent; S1/S2 merged; C-012 moved to the resolver level with C-038 split out; C-035 replaced a vacuous serde round-trip; C-015 stated as `#[cfg(windows)]`. |
| 2026-08-09 | Michael Herwig (architect, opus) | **Third revision — the central decision is reversed by owner directive.** OCX now claims **every** `${…}`; an unrecognised token is a hard error and `$${` is the only escape (new D3). Driver, quoted in Axis C: pass-through makes OCX's namespace hostage to what other tools spell, importing knowledge of users' toolchains and raising complexity. **Deleted:** the reserved-root set, the freeze, the fourth root `ocx` (owner-facing decision **withdrawn**), Axis C's C1–C4 comparison, the entire foreign-token path (scanner rule R2.4), the scanner-layer-versus-resolver-layer byte-identity caveat, the Kubernetes-CRD-pruning migration-risk analysis (the error→silence flip never happens), all three D13 publish-warning classes, the `RESERVED_ROOTS` constant and its contract, and the `MalformedToken` variant. **Reversed:** the foreign-token golden corpus keeps its inputs and inverts its expectation to hard rejection (C-004); `UnknownPlaceholder` is renamed `UnknownToken` and kept rather than deleted. **New:** Axis D settles an unterminated `${` as literal text, on the ground that it is otherwise the only currently-legal shape the ADR would reject; D13 becomes a message-design rule (root suggestion / escape hint / supported-body list) with `strsim` retained because the escape hint is the wrong advice for a typo. **Records corrected:** Reversibility drops from One-Way Door High to Medium — a closed world makes every future grammar addition additive, so the fail-closed property the reservation bought now comes free; Deviation 1's exposure shrinks because research §1.4's unprecedented "leave-alone" path is no longer built; A3 (`winnow`) is *less* attractive against a three-branch loop; the `Segment::Literal` ambiguity finding is resolved by the reversal, since there are no foreign tokens. **Cost recorded once, in D3 and Consequences:** #221's payloads must be authored `$${…}` and its stated byte-identity contract changes. **All contract and scenario IDs were renumbered** — C-001…C-035 and S-001…S-025, contiguous, no holes — so any plan keyed to the previous numbering must be re-keyed. |

| 2026-08-09 | Michael Herwig (architect, opus) | **Fourth revision — owner settles the two questions the third revision's brief had left as assumptions.** **New D14:** refusal is scoped to resolution, not to reading. `validate_env_tokens` / `validate_entrypoint_args` leave `ValidMetadata::try_from` (which runs on every ingress path — `tasks/common.rs:63`, `:124`, `:210`; `pull_local.rs:165`; `package_manager.rs:540`) and become one explicit publish gate plus the resolver's own failure; nothing is added on the execute side, because resolution already cannot produce bytes for a token it does not recognise. Same shape as D8 one layer up, and stated as a rule the same way. Full command enumeration is the deliverable: publish + every compose/execute surface refuse; `inspect` / `info` / `describe` / catalog succeed and show the token **verbatim, unannotated** (chosen over an "unresolvable" marker). `pull` / `install` land on the permissive side **by derivation, not preference** — the ingress gate at `:210` would make the read-only surface unreachable, since there would be nothing on disk to look at. Consequence stated: a newer package installs on an older ocx and fails at first use. New contracts **C-036…C-038** pin both sides on one fixture; `inspect::tests::inspect_default_malformed_metadata_is_internal` inverts and is the sharpest red state. New scenarios S-026, S-027. Second intra-S1 ordering constraint added: the publish gate lands in the same commit as the removal. **Second question settled the other way:** claim-all everywhere, no `Usage::Customizations`, the capability gate gains **no** unknown-token policy axis — recorded in D3, and the earlier hedge about whether #221's payload is scanned at all is removed. #221's byte-identical guarantee is recorded as **superseded, its issue text needing amendment**, in D3 and Consequences. Open questions stay at three; none was added. |

| 2026-08-09 | `/hex-execute` (orchestrator) | **Execution-time record corrections — three factual errors, no decision changed.** A code census of D14's blast radius (10 `ValidMetadata::try_from` call sites, every inverting test, every stale comment) falsified three claims this ADR made without checking. (1) **`ocx package which` / `ocx package deps` were on the wrong side of D14's table** — neither calls `TemplateResolver::resolve`, `EnvResolver::resolve` or `composer::compose`, so they are structural reads that refuse today only as a side-effect of the check being moved; they land on the succeed side by D14's own principle, and gating them would add the second enforcement point D14 rejects. (2) **`ocx patch why` was absent from the table**; it composes via `resolve_env_with_patch_boundary` and joins the refuse side. (3) **"`ocx_cli` gains no code at all" is withdrawn** — the two publish-path call sites are in `ocx_cli`, and left unchanged they would silently stop enforcing the publish gate; each needs a one-line constructor swap. The census also found **15 further inverting tests** the ADR's rewrite list omitted (12 in `validation.rs`, plus `common.rs:763` and the two already named), four more stale doc comments, and that the D14 test's module path is `package_manager::tasks::inspect::spec_tests::`, not `inspect::tests::`. Pre-stub evidence capture recorded one substantive nuance: for the C-009 shape the existing injection defence **fires today** (`UnknownPlaceholder`, exit 65), so D12's deletion is a **65 → success** change rather than a removed guard. |

*Contract and scenario IDs cited in the first three rows are the **superseded** numbering and
do not resolve against the current Component Contracts section. Only the third-revision row
uses live IDs.*
