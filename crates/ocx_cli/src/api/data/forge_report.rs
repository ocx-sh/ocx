// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The report vocabulary `ocx package claim` and `ocx package announce` share: one renderer, or
//! the two drift. No `Display` sits beside the `Serialize` derives here, for the same reason.

use ocx_announce::forge::{CapabilityCheck, CapabilityName, CheckStatus, ForgeCredentials, WriteTransport};
use serde::{Serialize, Serializer};

/// Serializes a library-owned closed vocabulary by its `Display` spelling, which is its wire form.
/// `ForgeKind` has no library spelling test: only the golden claim-report document holds `github`/`gitlab`.
pub fn serialize_display<T: std::fmt::Display, S: Serializer>(value: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(value)
}

/// [`serialize_display`] for an `Option` field, whose absence renders `null`.
pub fn serialize_optional_display<T: std::fmt::Display, S: Serializer>(
    value: &Option<T>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => serializer.collect_str(value),
        None => serializer.serialize_none(),
    }
}

/// The API credential's kind.
///
/// ocx reports only a kind it can observe: a personal access token, a deploy
/// token and an OAuth token all report as `token`.
// No `Pat`/`DeployToken`/`Oauth` arm: ocx cannot observe the difference (`credential_kind_wire_spellings`).
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
    /// Every kind, in declaration order; the spelling test pairs against this array, so a new arm reds.
    pub const ALL: [Self; 3] = [Self::JobToken, Self::Token, Self::None];
}

/// The push credential's kind; `null` means the `api` transport pushed nothing.
///
/// `git-helper` is a different statement: a push happened and git's own
/// credential helpers authenticated it.
// `null` is `Option::None` at the call site, never `GitHelper`
// (`push_credential_kind_git_helper_is_distinct_from_null`).
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
    /// Projects every preflight row, `Skipped` included, in `CapabilityName` declaration order.
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

/// The API credential's wire kind, read off the ladder's credential: a direct `OCX_ANNOUNCE_TOKEN`
/// read misses the job-token rung and reports `none` for an authenticated run.
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
#[must_use]
pub fn push_credential_kind(credentials: &ForgeCredentials, transport: WriteTransport) -> Option<PushCredentialKind> {
    match transport {
        // `resolve` fills the push half whenever an API credential exists, so `push()` alone misreports REST.
        WriteTransport::Api => None,
        WriteTransport::Git => Some(if credentials.push_is_job_token() {
            PushCredentialKind::JobToken
        } else if credentials.push().is_some() {
            PushCredentialKind::Token
        } else {
            // Nothing injected is not "no push": git's own helpers authenticated it.
            PushCredentialKind::GitHelper
        }),
    }
}

#[cfg(test)]
mod tests {
    use ocx_announce::forge::{CapabilityName, CheckStatus, ForgeCredentials, ForgeToken, PushAccess, WriteTransport};

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
