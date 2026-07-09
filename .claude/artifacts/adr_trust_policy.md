# ADR: `[trust.policy]` Identity-Pinned Verify

## Metadata

**Status:** Proposed (MAX-tier, one-way door — published TOML surface + JSON envelope)
**Date:** 2026-07-09
**Deciders:** Michael Herwig (owner), Architect
**Issue:** [#98 — `[trust.policy]` identity-pinned verify](https://github.com/ocx-sh/ocx/issues/98)
**Branch:** `wip/98-trust-policy`
**Tech Strategy Alignment:**
- [x] Follows Golden Path in `product-tech-strategy.md` (Rust 2024 core, no new deps — `regex` already a workspace dep)
**Domain Tags:** security, supply-chain, config
**Supersedes:** N/A
**Superseded By:** N/A
**Related:** `adr_oci_referrers_signing_v1.md` (the sign/verify pipeline this extends), `adr_global_toolchain_tier.md` (project/config tier machinery), `adr_index_routing_semantics.md`

---

## As-Shipped Notes (naming/shape deviations from the sketches below)

The illustrative code blocks in this ADR predate the implementation. The shipped
API differs in three cosmetic ways — the semantics are unchanged:

- **Error type** `TrustError` → **`crate::trust::TrustPolicyError`** (variants `IdentityConflict` / `IdentityUnset` / `InvalidRegex`, all `{ scope }`).
- **No `TrustPolicySet` newtype.** The ANY-of set is a plain `&[crate::trust::CompiledPolicy]` slice (YAGNI — the set needs no behaviour of its own). Resolution is the free functions `trust::resolve` / `trust::resolve_compiled`; evaluation is `oci::verify::identity::verify_policies(cert_der, &[CompiledPolicy])`. `CompiledPolicy::exact(identity, issuer)` builds the flag-override single-element set.
- **`VerifyErrorKind` variant** `NoTrustPolicyMatch` → **`NoIdentityProvided`** (`kind_detail = "no_identity_provided"`, exit **64**). Same condition and rationale; the name reads better for the common "flags omitted" case.

`VerifyContext` therefore carries `policies: &'a [crate::trust::CompiledPolicy]` (not `identity_policy: &TrustPolicySet`), and `Context` gains `config_trust: TrustConfig` + `config_trust_policies()` (the narrow-projection seam this ADR recommends).

---

## Context

`ocx package verify` (`adr_oci_referrers_signing_v1.md`, Slice 1) performs keyless
Sigstore verification but requires **both** `--certificate-identity` and
`--certificate-oidc-issuer` as mandatory flags on **every** invocation
(`crates/ocx_cli/src/command/verify.rs:49-56`, both `required = true` String).
The two flags feed `VerifyContext` as `&str`
(`crates/ocx_lib/src/oci/verify/pipeline.rs:48-50`), and steps 9-10 of the pipeline
match them **byte-equal, exact** via `IdentityMatcher` / `IssuerMatcher`
(`crates/ocx_lib/src/oci/verify/identity.rs:71-101`, `pipeline.rs:159-160`).

This is correct but operationally painful:

1. **No reusable trust config.** Every CI job, script, and human must re-supply the
   full identity + issuer on each verify. There is no way to declare "packages under
   `ghcr.io/acme/*` are signed by our CI identity" once.
2. **No regex identities.** Fulcio workflow SANs embed a git ref
   (`…/build.yml@refs/heads/main`). Exact-match forces pinning one ref; a pattern
   (`…/build.yml@refs/.*`) is impossible.
3. **No key/workflow rotation.** During an identity rotation the old and new signer
   coexist; exact single-value matching cannot accept "either identity".

Issue #98 introduces a declarative, tiered `[[trust.policy]]` config so a scope
(package prefix) can pin the accepted signer(s) once, with regex support and
rotation-overlap semantics, while preserving today's flag-driven verify verbatim.

### Existing machinery this builds on

- **Tiered `config.toml`** — `Config` (`crates/ocx_lib/src/config.rs`) merged across
  system → user → `$OCX_HOME` tiers by `Config::merge` (scalar-wins + table key-merge,
  `config.rs:88-113`); `Config` root has **no** `deny_unknown_fields` (forward-compat,
  `config.rs:23`), section structs (`RegistryDefaults`, `RegistryConfig`) do.
- **`ocx.toml`** — `ProjectConfig` (`crates/ocx_lib/src/project/config.rs`), a two-pass
  parse through `RawProjectConfig` (both `deny_unknown_fields`), with manual
  `Clone`/`PartialEq`/`Eq` (`config.rs:110-137`) and a resolve-time `packages` field
  deliberately **excluded** from `declaration_hash` (a `no-patches` edit must not
  invalidate `ocx.lock`, `config.rs:81-85`).
- **Verify error taxonomy** — `VerifyErrorKind` (`crates/ocx_lib/src/oci/verify/error.rs`),
  `#[non_exhaustive]`, with an **exhaustive** `exit_code()` + `kind_detail()` match (no
  wildcard) and a frozen `kind_detail` contract test.

---

## Decision Drivers

- **Preserve today's flag verify byte-for-byte.** Both flags present → identical
  behavior to Slice 1 (exact byte-equal identity + issuer).
- **Security fail-closed.** Absent trust source → refuse. Malformed policy → refuse.
  Never silently ignore a policy an operator authored.
- **Rotation must not depend on tier order.** Overlapping old/new identities during a
  rotation must pass regardless of which tier holds which policy.
- **Leaf placement / DIP.** The shared trust type must not pull `oci` into `config` /
  `project`; `oci::verify` may depend on it (one direction only).
- **KISS / YAGNI.** Pure prefix scope model; no glob engine, no per-segment matcher
  until a real need appears.
- **Backward-compatible wire format.** New optional TOML section; absent = today's
  behavior. New JSON envelope fields are additive.

---

## Decision

### D1 — TOML schema: array-of-tables `[[trust.policy]]`

```toml
# config.toml (system/user/$OCX_HOME) OR ocx.toml (project)
[[trust.policy]]
scope        = "ghcr.io/acme/*"                       # required
identity     = "ci@acme.example"                      # identity XOR identity_regexp
oidc_issuer  = "https://token.actions.githubusercontent.com"  # required, exact

[[trust.policy]]
scope         = "ghcr.io/acme/legacy-*"
identity_regexp = "^https://github\\.com/acme/.*/\\.github/workflows/build\\.yml@refs/.*$"
oidc_issuer   = "https://token.actions.githubusercontent.com"
```

Per-entry rules:

- `scope` (String, **required**) — package prefix; `*` marks the wildcard tail.
- `identity` (String) **XOR** `identity_regexp` (String) — **exactly one**; both-set
  or neither-set is a config error (cosign `--certificate-identity` /
  `--certificate-identity-regexp` precedent).
- `oidc_issuer` (String, **required**) — exact-match issuer URL.

Serde field names verbatim: `identity_regexp`, `oidc_issuer`. The **entry** carries
`#[serde(deny_unknown_fields)]` (matches `PackageSettings` / `RegistryDefaults`
precedent — typos fail fast). The **container** (`[trust]` table) does **not**
`deny_unknown_fields` (forward-compat, mirrors `Config` root).

`[[trust.policy]]` is TOML for "a `trust` table holding a `policy` array-of-tables", so
the Rust container is a `trust` table with a `policy: Vec<TrustPolicy>` field.

### D2 — Shared type placement: new leaf module `crate::trust`

New `crates/ocx_lib/src/trust.rs` (`pub mod trust;` in `lib.rs`, peer of `project` /
`patch`). It must **not** depend on `oci`; `oci::verify` depends on it. `regex` is
already a workspace dep of `ocx_lib` (`crates/ocx_lib/Cargo.toml:51`,
`regex = "1.12.3"` at `Cargo.toml:143`) — **no Cargo change.**

Two distinct type families — the **schema** type (stored in config, string-only) and the
**compiled** type (built at resolution, holds a `regex::Regex`). This split is
**mandatory, not stylistic**: `regex::Regex` implements neither `PartialEq` nor `Eq`, and
`ProjectConfig`'s manual `PartialEq`/`Eq` requires every stored field to be `Eq`. Storing
a compiled regex in the config struct would break that impl.

```rust
// ── Schema types (stored in Config + ProjectConfig; on-disk identity) ──
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicy {
    pub scope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_regexp: Option<String>,
    pub oidc_issuer: String,
}

/// The `[trust]` container. No `deny_unknown_fields` (forward-compat).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TrustConfig {
    #[serde(default)]
    pub policy: Vec<TrustPolicy>,
}

// ── Compiled types (built at resolution; NOT stored in any config struct) ──
pub enum IdentityRule {
    /// Byte-equal exact match (today's `IdentityMatcher` semantics).
    Exact(String),
    /// Anchored full-string regex (see D6).
    Regex(regex::Regex),
}

pub struct CompiledPolicy {
    pub identity: IdentityRule,
    pub issuer: String, // always exact
}

/// The ANY-of set the verify pipeline evaluates: either the winning-specificity
/// policy group (policy mode) or a single exact pair (flag-override mode).
pub struct TrustPolicySet {
    rules: Vec<CompiledPolicy>,
}

/// Three-way result so the pipeline can choose IdentityMismatch vs IssuerMismatch.
pub enum PolicyOutcome {
    Match,                  // some rule matched issuer + identity
    IssuerMatchedIdentityDidNot,  // some rule's issuer matched, none's identity did
    NoIssuerMatch,          // no rule's issuer matched
}

impl TrustPolicy {
    /// XOR validation + regex-compile. Both-set/neither-set → error.
    pub fn compile(&self) -> Result<CompiledPolicy, TrustError>;
}

impl TrustPolicySet {
    /// Single exact pair — flag-override mode. One-element set.
    pub fn from_exact(identity: String, issuer: String) -> Self;

    /// Resolve a scope against a tier-merged pool (D3/D4). Validates + compiles
    /// every matched policy (fail-closed). `Ok(None)` = no scope matched.
    pub fn resolve(pool: &[TrustPolicy], target: &str) -> Result<Option<Self>, TrustError>;

    /// ANY-of evaluation over already-extracted cert fields.
    pub fn evaluate(&self, san: &str, issuer: &str) -> PolicyOutcome;
}

// crate::trust owns its own thiserror taxonomy; it does NOT depend on oci.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrustError { /* IdentityXorViolation { scope }, InvalidRegex { scope, source }, ... */ }
```

`Config` gains `pub trust: Option<TrustConfig>`; `ProjectConfig` (and its first-pass
`RawProjectConfig`) gain `trust: Option<TrustConfig>`. Like `packages`, the
`ProjectConfig` trust field is **excluded from `declaration_hash`** (a trust-policy edit
must not invalidate `ocx.lock`) and included in the manual `Clone`/`PartialEq`/`Eq` +
`from_parts`.

The cert-extraction helpers (`parse_certificate`, `subject_identity`, `oidc_issuer` in
`oci/verify/identity.rs`) stay in `oci::verify` — they are x509/OID-specific. Only the
**matching** logic (exact / regex / ANY-of) moves to `crate::trust`, operating on
extracted `&str` values. That keeps `crate::trust` a leaf.

### D3 — Tier merge: array-APPEND (union pool), never replace

Policies **accumulate** across all tiers:

```
system config.toml → user config.toml → $OCX_HOME config.toml → project ocx.toml
```

`Config::merge` gains a new arm that **appends** `other.trust.policy` onto
`self.trust.policy` (there is **no** array-append precedent in `Config::merge` today —
every current arm is scalar-wins or table key-merge; this is a deliberate new merge rule,
documented + unit-tested). Array-append operates **within the operator tier** — the
system / user / `$OCX_HOME` `config.toml` files pool into one operator trust set.

**Cross-tier precedence (security ruling — one-way door).** The operator trust set is
**authoritative over the project `ocx.toml`**. Resolution (`crate::trust::resolve_tiered`):

```
operator_match = resolve(operator_config_toml_policies, target)   # most-specific + ANY-of
effective      = if operator_match is non-empty { operator_match }
                 else                            { resolve(project_ocx_toml_policies, target) }
```

If **any** operator policy matches the target, only operator policies are evaluated and the
project `ocx.toml` is **ignored for that package**. A project config can therefore **add**
trust for scopes the operator has not governed, but can **never override or weaken** an
operator pin — even with a more-specific scope. Within the chosen tier, most-specific-wins
(D4) + ANY-of-among-equal still hold (rotation overlap works within a tier). This reverses
the earlier "specificity wins across tiers" sketch: cross-tier, **tier authority beats
specificity**.

### D4 — Scope resolution (pure prefix, longest-wins, ANY-of-among-equal)

- `literal_prefix(scope)` = the substring **before the first `*`** (the whole string if
  no `*`).
- A scope **matches** target `registry/repository` (canonical, no tag/digest) on
  **path-segment boundaries** (`matches_scope`): a no-wildcard scope `S` matches iff
  `target == S` OR `target.starts_with("{S}/")`; a `*` globs on the literal prefix before
  it (a trailing `/*` is the subtree glob); an empty scope is a catch-all.
- **Specificity** = `literal_prefix.len()` (substring before the first `*`, whole string if
  none).
- Among matching scopes, the **longest** literal prefix wins ("most-specific").
- Among policies whose scope has the **same winning specificity**, evaluation is
  **ANY-of**: the signature passes if it satisfies **any one** of them (rotation overlap).

**Segment-boundary matching IS enforced in v1** (overriding the earlier YAGNI sketch): raw
`starts_with` was rejected as a footgun — it let `scope = "ghcr.io/acme"` match
`ghcr.io/acmecorp/tool` and `scope = "ghcr.io/acme/tool"` match `ghcr.io/acme/tool-cli`,
silently widening or misapplying a security pin. The `/`-boundary rule closes that.

`target` is built in the verify pipeline from the resolved identifier
(`resolved.registry()` + `resolved.repository()`, `pipeline.rs:106-107`), *after*
default-registry expansion (`verify.rs:81` `with_domain`), so policies are authored
against fully-qualified `registry/repository` — consistent with how `ocx.toml`
`[package."…"]` keys are fully qualified.

#### Worked example A — most-specific-wins

Effective pool:

```toml
[[trust.policy]]                       # P1  literal_prefix "ghcr.io/acme/"  len 13
scope = "ghcr.io/acme/*"
identity = "ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]                       # P2  literal_prefix "ghcr.io/acme/secret-tool" len 24
scope = "ghcr.io/acme/secret-tool"
identity = "release-bot@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

Verify `ghcr.io/acme/secret-tool:1.0`:
- Both match by prefix. P2 (len 24) > P1 (len 13) → **P2 wins outright**. The signature
  must present `release-bot@acme.example`; `ci@acme.example` (P1) is **not** accepted for
  this tool. P1 still governs every other `ghcr.io/acme/*` package.

#### Worked example B — rotation overlap (ANY-of, order-independent)

```toml
[[trust.policy]]                       # both literal_prefix "ghcr.io/acme/" len 13
scope = "ghcr.io/acme/*"
identity = "old-ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"

[[trust.policy]]                       # SAME specificity as above
scope = "ghcr.io/acme/*"
identity = "new-ci@acme.example"
oidc_issuer = "https://token.actions.githubusercontent.com"
```

Verify `ghcr.io/acme/tool:2.0` signed by **either** `old-ci@…` or `new-ci@…`:
- Both policies tie at specificity 13 → winning group `{old, new}` → **ANY-of** → pass
  for either signer. If the two policies came from different tiers (e.g. `old` from the
  system tier, `new` from the project `ocx.toml`), the outcome is identical — tier order
  is irrelevant within the specificity class.

### D5 — CLI contract change

`--certificate-identity` + `--certificate-oidc-issuer` become `Option<String>`, declared
**both-or-neither** via **mutual clap `requires`** — each arg `requires` the other, so
supplying one without the other is a clap usage error (exit 64) automatically:

```rust
#[clap(long, value_name = "IDENTITY", requires = "certificate_oidc_issuer")]
certificate_identity: Option<String>,
#[clap(long, value_name = "URL", requires = "certificate_identity")]
certificate_oidc_issuer: Option<String>,
```

(`required_together` is **not** a clap derive attribute — mutual `requires` is the
idiom.)

| identity flag | issuer flag | policy matches scope | Outcome | Exit |
|---|---|---|---|---|
| set | set | (ignored) | **flag-override mode** — single exact `(identity, issuer)` pair; trust policy ignored entirely | 0 (or 77 on mismatch) |
| set | unset | — | clap mutual-`requires` error | **64** |
| unset | set | — | clap mutual-`requires` error | **64** |
| unset | unset | ≥1 match | **policy mode** — ANY-of over the winning-specificity set | 0 (or 77 on mismatch) |
| unset | unset | 0 match | new variant `NoTrustPolicyMatch` | **64** |
| any | any | matched policy is malformed (XOR / bad regex) | new variant `TrustPolicyInvalid` | **78** |

Flag-override mode builds a **one-element** `TrustPolicySet::from_exact(identity, issuer)`.
The pipeline's ANY-of evaluation over a single `Exact` rule is byte-for-byte identical to
today's `IdentityMatcher`/`IssuerMatcher` (both byte-equal, `identity.rs:74,98`) — the
"preserve today's verify verbatim" requirement is met by construction.

### D6 — Matcher extension + anchoring

- **Exact** identity stays byte-equal (`IdentityRule::Exact`).
- **Regex** identity (`identity_regexp`) is compiled **anchored, full-string**. Rust's
  `regex` matches substrings by default, so the user pattern is wrapped:

  ```rust
  // \A … \z = absolute string anchors (not ^/$ line anchors); (?:…) prevents a
  // top-level alternation in the user pattern from escaping the anchors.
  let anchored = format!(r"\A(?:{})\z", user_pattern);
  IdentityRule::Regex(regex::Regex::new(&anchored)?)
  ```

  The non-capturing `(?:…)` group is load-bearing: without it, a user pattern `a|b`
  would compile as `\Aa|b\z` (match `a`-at-start **or** `b`-at-end), not the intended
  whole-string alternation. This matches cosign's full-string identity-regexp semantics.
- **Issuer** is always exact — no `issuer_regexp` in v1 (YAGNI; issuers are stable URLs).

Pipeline steps 9-10 collapse to a single ANY-of evaluation. Parse the leaf cert once
(step 10 already does `parse_certificate`, `pipeline.rs:163`), extract `san` + `issuer`,
then `ctx.identity_policy.evaluate(&san, &issuer)`. Map the `PolicyOutcome`:
- `Match` → continue to build `VerifyResult`.
- `IssuerMatchedIdentityDidNot` → `VerifyErrorKind::IdentityMismatch` (77).
- `NoIssuerMatch` → `VerifyErrorKind::IssuerMismatch` (77).

This preserves the single-exact-pair behavior (a 1-element set yields exactly one of the
three outcomes, mirroring today's ordered identity-then-issuer checks). The
`IdentityMatcher`/`IssuerMatcher` structs are removed; their extraction helpers stay.

### D7 — OCI-tier purity carve-out

`ocx package verify` is an **OCI-tier** command, and `subsystem-cli.md` states the firm
rule "OCI-tier commands never consult `ocx.toml`." Reading `[[trust.policy]]` from
`ocx.toml` is a **deliberate, documented exception**:

- Trust policy is **security posture** ("whose signature do I trust"), *not*
  toolchain-binding resolution ("which version of a tool"). The purity rule exists to
  keep version resolution deterministic and CWD-independent; trust policy does not affect
  which artifact is fetched — only whether its signer is accepted.
- The carve-out is **narrow**: verify reads only the `[[trust.policy]]` section of
  `ocx.toml`; it never touches `[tools]` / `[group.*]` / `[package.*]`, never resolves a
  binding, never requires an `ocx.toml` to exist (absent → policy pool = config tiers
  only).

**Considered alternative (purity-preserving):** source trust policy **only** from
`config.toml` tiers (operator-controlled) and **not** from `ocx.toml`. This keeps OCI-tier
purity intact and closes the "developer-editable file widens/overrides trust" vector (see
Consequences). Rejected for v1 because #98 explicitly wants project-scoped trust
(a repo declaring its own signer), and the union-pool semantics mean `ocx.toml` can only
be consulted by the user who owns that checkout and runs verify in it.

This carve-out **requires a one-line note in `subsystem-cli.md`** (and
`subsystem-cli-commands.md`) at build time recording that `package verify` reads
`ocx.toml`'s trust section as a security-config exception.

---

## Consequences

### Positive

- One-time declarative trust config, tiered like every other OCX setting.
- Regex + ANY-of unlock Fulcio workflow SANs and zero-downtime signer rotation.
- Flag verify unchanged; existing scripts and the acceptance suite keep passing.
- No new dependency; `crate::trust` is a testable leaf with no `oci` coupling.

### Negative / risks

- **Cross-tier precedence — RESOLVED by owner ruling (security, one-way door).** The
  operator `config.toml` trust set is **authoritative**: if any operator policy matches the
  target, the project `ocx.toml` is ignored for that package (`resolve_tiered`). A
  compromised `ocx.toml` therefore **cannot** replace or weaken an operator pin — the
  earlier "a more-specific `ocx.toml` scope overrides the operator" hazard is closed by
  construction. A project `ocx.toml` can only **add** trust for scopes no operator policy
  governs. This is the sharpest one-way-door edge; it is unit-tested
  (`operator_tier_is_authoritative_over_project`, `project_tier_adds_trust_for_ungoverned_scopes`)
  and acceptance-tested. Reversible in the PR only by an explicit owner decision to allow
  project overrides.
- **New merge semantic.** Array-append in `Config::merge` is a new rule class; every
  existing arm is scalar/table. Needs its own tests so a future refactor cannot silently
  turn it back into replace.
- **Deferred, fail-safe (accepted as-is).** Two edges are intentionally left simple because
  neither can weaken a pin: (a) `ocx --global package verify` also sources `$OCX_HOME/ocx.toml`
  trust (the `ocx_home` accessor exists, so the earlier "skip global" workaround was
  unnecessary) — operator authority still holds; (b) ANY-of evaluation short-circuits on the
  first fully-matching policy in a rotation group — order-independent for pass/fail, so which
  sibling matches first is irrelevant. Both are documented, not gated.
- **Deferred validation.** XOR/regex validation runs at **resolution** (verify time) in
  `crate::trust`, not at config-parse time. A malformed policy in a never-matched scope is
  not reported until a verify hits it (fail-closed only for matched scopes). Considered
  alternative: fail-fast at parse per tier (natural in `ProjectConfig::from_str_with_path`,
  awkward for `config.toml` whose load path has no validation pass and would then pay the
  cost on *every* command). Chosen: single validation site (DRY), only paid when trust is
  consumed. To keep it fail-closed against operator surprise, `TrustPolicySet::resolve`
  validates the **entire matched-specificity group**, not just the first hit.

### Neutral

- `VerifyResult` still reads back the *actual* matched identity + issuer from the cert
  (`pipeline.rs:166-168`) for the report — unchanged.

---

## Error Variants + Exit Codes

Two new `VerifyErrorKind` variants (the enum is `#[non_exhaustive]`; `exit_code()` and
`kind_detail()` are exhaustive matches, so each new variant forces a new arm in both, and
the frozen `kind_detail_values_are_stable` + `verify_error_kind_display_rules` tests must
gain rows):

| Variant | Condition | Exit | `kind_detail` |
|---|---|---|---|
| `NoTrustPolicyMatch` | neither flags supplied **and** no policy scope matches the target | **64** (`UsageError`) | `no_trust_policy_match` |
| `TrustPolicyInvalid` | a matched policy is malformed (identity XOR violation, or `identity_regexp` fails to compile); carries `crate::trust::TrustError` via `#[source]` | **78** (`ConfigError`) | `trust_policy_invalid` |

**Why 64 for `NoTrustPolicyMatch` (not 78).** Continuity: today omitting a required flag is
a clap usage error → **64**. Users' `case $? in 64)` handlers already treat "you didn't
tell me whose signature to trust" as a usage problem. "No identity source at all" (no
flag, no policy) is the same class — **required caller input is absent**. This is *not* a
duplicate of `IdentityMismatch` (77): mismatch means "a signer was named and the cert
disagrees"; `NoTrustPolicyMatch` means "no signer was named." Reserving **78** for
`TrustPolicyInvalid` yields a clean split — **78 = trust config present but broken**
(joins `TrustRootLoad`/`TrustRootUnavailable` → 78), **64 = required trust input absent**.

---

## `VerifyContext` Field Change

`crates/ocx_lib/src/oci/verify/pipeline.rs` `VerifyContext<'a>`:

```rust
// REMOVE:
pub certificate_identity: &'a str,
pub certificate_oidc_issuer: &'a str,

// ADD (single resolved value; the command owns it, passes a borrow):
pub identity_policy: &'a crate::trust::TrustPolicySet,
```

Both flag-override mode (single `from_exact`) and policy mode (resolved ANY-of set) reduce
to one `TrustPolicySet`, evaluated once at steps 9-10.

---

## Context Seam for Verify (both policy sources)

`Context` (`crates/ocx_cli/src/app/context.rs:20-36`) **does not retain the merged
`Config`** — `config` is a local in `try_init` (line 79), consumed for mirrors / default
registry / patches and then dropped. Established pattern: narrow resolved values are
extracted into dedicated fields (`default_registry`, `config_view`, resolved patches),
never the whole `Config` (ISP). Recommendation follows that pattern:

1. **Config-tier pool — one cheap field.** In `try_init`, before `config` is dropped,
   extract `config.trust` into a new `Context` field `config_trust: TrustConfig` (default
   empty). Zero new I/O — `config` is already loaded. Expose
   `Context::config_trust_policies(&self) -> &[TrustPolicy]`.

2. **Project-tier pool — on-demand, verify-only.** `try_init` does **not** currently parse
   `ocx.toml` (`ConfigLoader::load` resolves the project *path* then discards it,
   `loader.rs:75`). Loading `ocx.toml` eagerly for every command would be wasted I/O
   (verify is the sole consumer). So verify assembles the effective pool lazily:

   ```
   pool = context.config_trust_policies().to_vec();
   if let Some((project_toml, _lock)) =
       ProjectConfig::resolve(Some(&cwd), context.project_path(), ocx_home, context.global()).await?
   {
       pool.extend(ProjectConfig::from_path(&project_toml).await?.trust_policies());
   }
   ```

   `context.project_path()` (line 285) + `context.global()` (line 296) already exist;
   `cwd` via `env::current_dir()`. **Plumbing gap:** `Context` exposes no `$OCX_HOME`
   dir accessor and `ConfigLoader::home_dir()` is private, so the `--global` verify case
   (needs `ocx_home`) can't resolve today. v1 recommendation: **do not support
   `--global` trust sourcing for verify** (global is a toolchain concept; verify is
   OCI-tier) — pass `ocx_home = None`, `global = false` and source project trust only via
   CWD walk / `--project`. Revisit if `--global verify` trust is ever requested.

Net: **add one field** (`config_trust: TrustConfig`) + **one accessor**
(`config_trust_policies`). No whole-`Config` retention. The two-source merge lives in the
verify command (or a thin `crate::trust` helper it calls), keeping the merge in one place.

---

## `ocx_schema` — TWO schema additions

`ocx_schema::schema_for` (`crates/ocx_schema/src/lib.rs:37-60`) emits **both**
`"config"` → `Config` (line 45) and `"project"` → `ProjectConfig` (line 46). Trust
policy lands on **both** structs, so **both** `config/v1.json` and `project/v1.json`
gain the new `trust` section — **two schema surfaces regenerate**, not one. No new
`schema_for` kind is needed (trust policy is a *field* on existing schemas, unlike the
standalone `patch` doc). `TrustPolicy` + `TrustConfig` must derive
`#[derive(schemars::JsonSchema)]` so both roots pick them up; regen via
`task schema:generate` (per `subsystem-metadata-schema.md`).

---

## Affected Code Surfaces (implementation touch-points)

| File | Change |
|---|---|
| `crates/ocx_lib/src/trust.rs` (new) + `lib.rs` | `pub mod trust;` — schema + compiled types, resolution, `TrustError` |
| `crates/ocx_lib/src/config.rs` | `Config.trust: Option<TrustConfig>` (`:24`); new **array-append** arm in `Config::merge` (`:88-113`) |
| `crates/ocx_lib/src/project/config.rs` | `trust` field on `ProjectConfig` (`:64`) **and** `RawProjectConfig` (`:150`, both `deny_unknown_fields`); manual `Clone`/`PartialEq`/`Eq` (`:110-137`); `from_parts` (`:171`); second-pass wiring (`:448`); **exclude from `declaration_hash`** (mirror `packages`) |
| `crates/ocx_cli/src/command/verify.rs` | flags `Option<String>` + mutual `requires` (`:49-56`); build `TrustPolicySet`; assemble two-source pool |
| `crates/ocx_lib/src/oci/verify/pipeline.rs` | `VerifyContext` field swap (`:48-50`); collapse steps 9-10 to one ANY-of eval (`:159-160`) |
| `crates/ocx_lib/src/oci/verify/identity.rs` | remove `IdentityMatcher`/`IssuerMatcher`; keep extraction helpers |
| `crates/ocx_lib/src/oci/verify/error.rs` | two new variants + `exit_code`/`kind_detail` arms; update frozen contract tests |
| `crates/ocx_cli/src/app/context.rs` | `config_trust: TrustConfig` field + `config_trust_policies()` accessor |

---

## Documentation Surfaces (enumerate — do not author here)

- `website/src/docs/reference/configuration.md` — `[[trust.policy]]` schema + tier-merge
  (union/append) semantics + specificity/ANY-of resolution.
- User-guide policy-authoring section (new) — scope patterns, rotation overlap,
  most-specific-wins, flag-vs-policy modes, security note on tier specificity.
- `website/src/docs/reference/exit-codes.md` — new `NoTrustPolicyMatch` (64) +
  `TrustPolicyInvalid` (78).
- `website/src/docs/reference/command-line.md` — `verify` options: flags now
  optional-when-a-policy-matches; both-or-neither.
- `subsystem-cli.md` + `subsystem-cli-commands.md` — one-line OCI-tier purity carve-out
  note (D7).
- `ocx_schema` regeneration — `config/v1.json` **and** `project/v1.json` (D-schema).
- `crates/ocx_cli/src/command/verify.rs` `///` help — update per `quality-cli-help.md`
  (user contract only; no ADR references in help text).
