// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The report vocabulary the two forge-writing commands share (DX-59).
//!
//! C-060 and C-061 contract the **same** value vocabularies for
//! `credential_kind`, `push_credential_kind` and the capability-check rows
//! across `ocx package claim` and `ocx package announce`, so one renderer is
//! right and two would drift. A neutral module rather than one report importing
//! the other: pointing the older, more general announce report at the newer
//! claim report would make that inversion permanent.
//!
//! No `Printable` impl lives here, so this module owes no `report_roots!` row
//! (DX-57) — it is projected *into* the two reports that do.
//!
//! # Why the two credential kinds are enums with an `ALL` array
//!
//! Both are **wire vocabularies**: their serialized spellings are rendered into
//! a parsed report and are one-way once shipped. A bare `&'static str` mapper
//! over an ad-hoc `match` cannot be checked for completeness — a
//! `..._wire_spellings` test written against it counts only the arms the test
//! itself enumerated, which is green in every state (`quality-core.md`
//! § Unchecked Green). Pairing the spelling assertion against `ALL` makes a
//! newly added arm red instead, which is how `CapabilityName` (`forge/api.rs`)
//! and `ClaimStatus` (`claim/request.rs`) already hold theirs.
//!
//! Both derive `Serialize` and `JsonSchema` and are held **typed** on the
//! report, so the published schema carries the closed set rather than an open
//! `string`, and no report can spell a value the mapper cannot produce. There
//! is deliberately no `Display` impl beside the derive: one wire vocabulary
//! gets one renderer, and two would be free to drift.

use ocx_lib::forge::{CapabilityCheck, CapabilityName, CheckStatus, ForgeCredentials, WriteTransport};
use serde::{Serialize, Serializer};

/// Serialize a value by its [`std::fmt::Display`] spelling.
///
/// For the vocabularies C-060 closes that `ocx_lib` already owns —
/// [`CapabilityName`] and [`CheckStatus`] here, [`ClaimStatus`], [`ForgeKind`],
/// [`WriteTransport`] and [`OwnerIdentitySource`] on the claim report. None of
/// them derives `Serialize`: their `Display` impl **is** the wire spelling.
/// Holding the enum and rendering through this is what stops a report spelling a
/// value the library cannot produce, without minting a second vocabulary at this
/// layer.
///
/// Where each spelling is actually held — checked, not assumed, because this is
/// the file a later change consults before touching one:
///
/// | Vocabulary | Held by |
/// |---|---|
/// | [`ClaimStatus`], [`OwnerIdentitySource`], [`CapabilityName`], [`CheckStatus`] | an `ALL`-paired spelling test at library scope |
/// | [`WriteTransport`] | `write_transport_value_spellings`, paired against clap's `value_variants()` rather than an `ALL` |
/// | [`ForgeKind`] | **no library-scope spelling test** — `forge/kind.rs` declares no `ALL`. Its `github`/`gitlab` spellings are held only by this crate's golden claim-report document. |
///
/// [`ClaimStatus`]: ocx_lib::claim::ClaimStatus
/// [`ForgeKind`]: ocx_lib::forge::ForgeKind
/// [`OwnerIdentitySource`]: ocx_lib::claim::OwnerIdentitySource
pub fn serialize_display<T: std::fmt::Display, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// [`serialize_display`] for a key whose absence is `null`.
///
/// `serialize_with` replaces serde's handling of the **whole** field, `Option`
/// included, so the plain helper above would render `Some(x)`'s `Display` for a
/// present value and fail to compile for an absent one. A field that renders
/// `null` when the run produced nothing is DX-40.3's rule, not an option this
/// layer gets to take.
pub fn serialize_optional_display<T: std::fmt::Display, S: Serializer>(
    value: &Option<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.collect_str(value),
        None => serializer.serialize_none(),
    }
}

/// The API credential's wire kind (C-060).
///
/// ocx **may not report a kind it cannot observe**: there is deliberately no
/// `Pat`, no `DeployToken` and no `Oauth` arm, because nothing the forge tells
/// ocx distinguishes those from one another. Adding one is what
/// `credential_kind_wire_spellings` exists to red.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialKind {
    /// The credential is this environment's own `CI_JOB_TOKEN`.
    JobToken,
    /// A credential ocx holds but cannot classify further.
    Token,
    /// The ladder resolved nothing — the unauthenticated `--out` path.
    None,
}

impl CredentialKind {
    /// Every kind, in declaration order.
    ///
    /// The single source of the vocabulary's size: the spelling test pairs its
    /// expected list against this array, so a fourth arm reds rather than
    /// passing unmentioned.
    pub const ALL: [Self; 3] = [Self::JobToken, Self::Token, Self::None];
}

/// The push credential's wire kind (C-060).
///
/// `null` is **not** a variant: it is `Option::None` at the call site, and it
/// means "the `api` transport pushes nothing". [`Self::GitHelper`] is a
/// different statement — a push happened and git's own credential helpers
/// authenticated it — and collapsing the two is the defect
/// `push_credential_kind_git_helper_is_distinct_from_null` exists to catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PushCredentialKind {
    /// The push secret is this environment's own `CI_JOB_TOKEN`.
    JobToken,
    /// A push secret ocx injected but cannot classify further.
    Token,
    /// Nothing was injected; git's own credential helpers are in charge.
    GitHelper,
}

impl PushCredentialKind {
    /// Every kind, in declaration order. Same role as [`CredentialKind::ALL`].
    pub const ALL: [Self; 3] = [Self::JobToken, Self::Token, Self::GitHelper];
}

/// One row of the write preflight.
///
/// A report-side projection of [`CapabilityCheck`] rather than a re-export of
/// it: the wire shape is this layer's contract, and the library type carries no
/// `Serialize`. The two closed vocabularies are held as the library's own
/// enums and rendered through [`serialize_display`], so a row cannot carry a
/// name or a status the library has no variant for.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CapabilityCheckEntry {
    /// The capability's wire name — `git-version`, `push-access`,
    /// `job-token-push` or `job-token-allowlist`.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub name: CapabilityName,
    /// `"passed"`, `"unknown"` or `"skipped"`. There is no `"failed"`: a check
    /// that fails raises an error and no report is rendered.
    #[serde(serialize_with = "serialize_display")]
    #[schemars(with = "String")]
    pub status: CheckStatus,
    /// A human-readable qualifier where the forge exposes one, `null`
    /// otherwise.
    pub detail: Option<String>,
}

impl CapabilityCheckEntry {
    /// Project the preflight rows into their wire shape, in
    /// `CapabilityName` declaration order.
    ///
    /// Takes the slice
    /// [`PushAccess::checks`](ocx_lib::forge::PushAccess::checks) hands out, not
    /// a `Vec`: `PushAccess` owns its rows and the privacy of that vector is
    /// what makes "non-empty on every run" unrepresentable-otherwise (C-069).
    /// **Every** row is projected, `Skipped` ones included — filtering the
    /// inapplicable rows out is what S-011 exists to forbid.
    #[must_use]
    pub fn from_checks(checks: &[CapabilityCheck]) -> Vec<Self> {
        checks
            .iter()
            .map(|check| Self {
                name: check.name,
                status: check.status,
                detail: check.detail.clone(),
            })
            .collect()
    }
}

/// The API credential's wire kind, read off the credential the **ladder**
/// resolved.
///
/// Never off a direct `OCX_ANNOUNCE_TOKEN` read: that misses the job-token rung
/// entirely, so an operator inside a GitLab job with an empty ocx variable would
/// be reported as `none` for a run that authenticated perfectly well.
/// [`ForgeCredentials::api_is_present`] (DX-52) is the only way to observe the
/// terminal rung, and it is the same read C-063's exit-80 refusal branches on.
#[must_use]
pub fn credential_kind(credentials: &ForgeCredentials) -> CredentialKind {
    if !credentials.api_is_present() {
        return CredentialKind::None;
    }
    if credentials.api_is_job_token() {
        CredentialKind::JobToken
    } else {
        CredentialKind::Token
    }
}

/// The push credential's wire kind, or `None` under the `api` transport.
///
/// The transport gate is load-bearing and is **not** derivable from the
/// credential alone: [`ForgeCredentials::resolve`] populates the push half
/// whenever an API credential exists, with no transport guard, so a mapper
/// reading `push()` alone reports `"token"` for an ordinary REST claim that
/// pushed nothing.
#[must_use]
pub fn push_credential_kind(credentials: &ForgeCredentials, transport: WriteTransport) -> Option<PushCredentialKind> {
    match transport {
        // The gate R-23 exists for: `resolve` populates the push half whenever
        // an API credential exists, so reading `push()` alone would report a
        // push kind for a REST run that pushed nothing.
        WriteTransport::Api => None,
        WriteTransport::Git => Some(if credentials.push_is_job_token() {
            PushCredentialKind::JobToken
        } else if credentials.push().is_some() {
            PushCredentialKind::Token
        } else {
            // Nothing injected is NOT "no push": the push happened and git's
            // own helpers authenticated it.
            PushCredentialKind::GitHelper
        }),
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::forge::{CapabilityName, CheckStatus, ForgeCredentials, ForgeToken, PushAccess, WriteTransport};

    use super::{CapabilityCheckEntry, CredentialKind, PushCredentialKind, credential_kind, push_credential_kind};

    /// The wire spelling of one vocabulary value, read off `Serialize` — which
    /// is the renderer the report uses, so nothing else can be asserted here by
    /// mistake.
    fn wire(value: impl serde::Serialize) -> String {
        serde_json::to_value(value)
            .expect("a fieldless enum serializes")
            .as_str()
            .expect("each variant renders as a JSON string")
            .to_string()
    }

    /// A credential the ladder resolved something for, built without touching
    /// the process environment.
    ///
    /// `ForgeCredentials::new` derives `api_is_job_token` from `CI_JOB_TOKEN`,
    /// which is unset in a unit-test process, so this is the "a token, but not
    /// this job's" state — [`CredentialKind::Token`].
    fn resolved_token() -> ForgeCredentials {
        ForgeCredentials::new(ForgeToken::new("not-a-job-token".to_string()))
    }

    /// The terminal rung: the ladder resolved nothing and left an empty token.
    fn no_credential() -> ForgeCredentials {
        ForgeCredentials::new(ForgeToken::new(String::new()))
    }

    /// C-060: the API credential vocabulary is exactly three words, and the
    /// three ocx **may not** report (`pat`, `deploy-token`, `oauth`) are held
    /// out by the arity of `ALL`, never by a source-text scan.
    ///
    /// A grep for the forbidden words would live in the same file as the doc
    /// comment explaining why they are absent — a detector matching its own
    /// invocation (`quality-core.md` § Unchecked Green). Pairing the spellings
    /// against `ALL` instead means a `Pat` arm reds here.
    ///
    /// Red at the stub: the vocabulary is unrendered.
    /// Mutation once implemented: add a fourth `CredentialKind` arm, or drop
    /// `#[serde(rename_all = "kebab-case")]` so `job-token` becomes `JobToken`.
    #[test]
    fn credential_kind_wire_spellings() {
        let spellings: Vec<String> = CredentialKind::ALL.iter().copied().map(wire).collect();
        assert_eq!(
            spellings,
            vec!["job-token".to_string(), "token".to_string(), "none".to_string()],
            "the C-060 API credential vocabulary is exactly these three words, in this order"
        );
    }

    /// C-060: four push values, three of them words and the fourth `null`.
    ///
    /// Red at the stub: the vocabulary is unrendered.
    /// Mutation once implemented: add a `Pat` arm; the arity pairing reds.
    #[test]
    fn push_credential_kind_wire_spellings() {
        let spellings: Vec<String> = PushCredentialKind::ALL.iter().copied().map(wire).collect();
        assert_eq!(
            spellings,
            vec!["job-token".to_string(), "token".to_string(), "git-helper".to_string()],
            "the C-060 push credential vocabulary is exactly these three words plus null"
        );
    }

    /// C-060 / C-063: the two credential states a unit test can construct
    /// without the process environment map to the right word.
    ///
    /// The `job-token` row is **not** here: `ForgeCredentials::api_is_job_token`
    /// is derived from `CI_JOB_TOKEN` inside the library, and `ocx_cli`'s test
    /// binary links a non-`cfg(test)` `ocx_lib`, so it has no override seam and
    /// would have to mutate the real process environment. That rung is covered
    /// at library scope by
    /// `credentials::tests::the_api_ladder_prefers_a_non_empty_ocx_token_over_the_job_token`
    /// and end-to-end by WP-16.
    ///
    /// Red at the stub: `credential_kind` is `unimplemented!()`.
    /// Mutation once implemented: derive the word from "was `OCX_ANNOUNCE_TOKEN`
    /// set" instead of from the resolved credential — the `none` row then reds
    /// in any process where that variable happens to be exported, and the
    /// `token` row reds in every process where it is not.
    #[test]
    fn credential_kind_maps_the_resolved_credential() {
        assert_eq!(
            credential_kind(&resolved_token()),
            CredentialKind::Token,
            "a credential the ladder resolved is `token` unless it is this job's own"
        );
        assert_eq!(
            credential_kind(&no_credential()),
            CredentialKind::None,
            "the terminal rung leaves an empty token, which reports `none`"
        );
    }

    /// C-060: `push_credential_kind` is `null` under `api` — **even when a push
    /// credential was resolved**.
    ///
    /// `ForgeCredentials::resolve` populates the push half whenever an API
    /// credential is non-empty, with no transport guard, so a mapper written
    /// over `push()` alone reports a push kind for a REST claim that pushed
    /// nothing.
    ///
    /// Red at the stub: `push_credential_kind` is `unimplemented!()`.
    /// Mutation once implemented: drop the `WriteTransport::Api => None` gate.
    /// A credential with no push half then falls to `git-helper`, so this reds
    /// even without an environment-resolved push secret.
    #[test]
    fn push_credential_kind_is_null_under_the_api_transport() {
        assert_eq!(
            push_credential_kind(&resolved_token(), WriteTransport::Api),
            None,
            "the api transport pushes nothing, so it reports no push credential kind"
        );
    }

    /// C-060 / S-029: `git-helper` and `null` are different statements.
    ///
    /// `git-helper` says a push happened and git's own helpers authenticated
    /// it; `null` says no push was attempted. An implementation mapping
    /// `push().is_none()` straight onto `Option::None` collapses them.
    ///
    /// Red at the stub: `push_credential_kind` is `unimplemented!()`.
    /// Mutation once implemented: return `None` when `push().is_none()`.
    #[test]
    fn push_credential_kind_git_helper_is_distinct_from_null() {
        assert_eq!(
            push_credential_kind(&resolved_token(), WriteTransport::Git),
            Some(PushCredentialKind::GitHelper),
            "no injected push secret under the git transport is `git-helper`, never null"
        );
    }

    /// C-060 / C-069 / S-011 / S-036: every preflight row is projected,
    /// including the ones that did not apply, in `CapabilityName` declaration
    /// order.
    ///
    /// Asserted as a **sequence**, not a set: the contract promises the array is
    /// stable across runs, and a set assertion is order-blind.
    ///
    /// Red at the stub: `from_checks` is `unimplemented!()`.
    /// Mutation once implemented: filter `status == "skipped"` out of the
    /// projection (the "omit what does not apply" instinct), or sort the rows
    /// alphabetically.
    #[test]
    fn capability_rows_are_projected_in_declaration_order_including_skipped() {
        let entries = CapabilityCheckEntry::from_checks(PushAccess::skipped_all().checks());
        let names: Vec<CapabilityName> = entries.iter().map(|entry| entry.name).collect();
        assert_eq!(
            names,
            CapabilityName::ALL.to_vec(),
            "every capability row is projected, in CapabilityName declaration order"
        );
        assert!(
            entries.iter().all(|entry| entry.status == CheckStatus::Skipped),
            "a run that checked nothing still reports every row, as `skipped`"
        );
        // The wire half, so the typed field cannot silently change spelling:
        // the row's `name`/`status` render as the library's own words.
        let row = serde_json::to_value(&entries[0]).expect("a row serializes");
        assert_eq!(
            row.get("name").and_then(serde_json::Value::as_str),
            Some(CapabilityName::ALL[0].to_string().as_str())
        );
        assert_eq!(
            row.get("status").and_then(serde_json::Value::as_str),
            Some("skipped"),
            "the status renders as the wire word, not as a Rust variant name"
        );
    }
}
