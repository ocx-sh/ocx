// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The report vocabulary `ocx package claim` and `ocx package announce` share: one renderer, or
//! the two drift. No `Display` sits beside the `Serialize` derives here, for the same reason.

use ocx_announce::announce::AnnounceStatus;
use ocx_announce::claim::{ClaimStatus, OwnerIdentitySource};
use ocx_announce::forge::{CapabilityCheck, CapabilityName, CheckStatus, ForgeCredentials, ForgeKind, WriteTransport};
use ocx_util::wire_words;
use serde::Serialize;

/// The forge the run wrote to, after `--forge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Forge {
    /// GitHub.com or a GitHub Enterprise Server instance.
    Github,
    /// GitLab.com or a self-managed GitLab instance.
    Gitlab,
}

impl From<ForgeKind> for Forge {
    fn from(kind: ForgeKind) -> Self {
        match kind {
            ForgeKind::GitHub => Self::Github,
            ForgeKind::GitLab => Self::Gitlab,
        }
    }
}

wire_words! {
    /// The write transport the run used.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum Transport {
        /// The forge's REST API.
        Api = "api",
        /// A local `git` clone and one authenticated push.
        Git = "git",
    }
}

impl From<WriteTransport> for Transport {
    fn from(transport: WriteTransport) -> Self {
        match transport {
            WriteTransport::Api => Self::Api,
            WriteTransport::Git => Self::Git,
        }
    }
}

wire_words! {
    /// Whether a forge write moved anything.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum WriteStatus {
        /// The committed or proposed content already matched, so nothing was written.
        Unchanged = "unchanged",
        /// The run created or moved the content.
        Updated = "updated",
    }
}

impl From<AnnounceStatus> for WriteStatus {
    fn from(status: AnnounceStatus) -> Self {
        match status {
            AnnounceStatus::Unchanged => Self::Unchanged,
            AnnounceStatus::Updated => Self::Updated,
        }
    }
}

impl From<ClaimStatus> for WriteStatus {
    fn from(status: ClaimStatus) -> Self {
        match status {
            ClaimStatus::Unchanged => Self::Unchanged,
            ClaimStatus::Updated => Self::Updated,
        }
    }
}

wire_words! {
    /// Which rule produced an identity.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum IdentitySource {
        /// The forge answered: its users API, or its own account behind the credential.
        Resolved = "resolved",
        /// A `LOGIN:ID` pair was taken on the operator's word because the users API was unreachable.
        Asserted = "asserted",
        /// The CI environment's variables named the identity.
        CiEnvironment = "ci_environment",
    }
}

impl From<OwnerIdentitySource> for IdentitySource {
    fn from(source: OwnerIdentitySource) -> Self {
        match source {
            OwnerIdentitySource::Resolved => Self::Resolved,
            OwnerIdentitySource::Asserted => Self::Asserted,
            OwnerIdentitySource::CiEnvironment => Self::CiEnvironment,
        }
    }
}

wire_words! {
    /// The API credential's kind.
    ///
    /// ocx reports only a kind it can observe: a personal access token, a deploy
    /// token and an OAuth token all report as `token`.
    // No `Pat`/`DeployToken`/`Oauth` arm: ocx cannot observe the difference (`credential_kind_wire_spellings`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum CredentialKind {
        /// The credential is this environment's own `CI_JOB_TOKEN`.
        JobToken = "job_token",
        /// A credential ocx holds but cannot classify further.
        Token = "token",
        /// The ladder resolved nothing — the unauthenticated `--output` path.
        None = "none",
    }
}

wire_words! {
    /// The push credential's kind; absent when the `api` transport pushed nothing.
    ///
    /// `git_helper` is a different statement: a push happened and git's own
    /// credential helpers authenticated it.
    // Absent is `Option::None` at the call site, never `GitHelper`
    // (`push_credential_kind_git_helper_is_distinct_from_null`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum PushCredentialKind {
        /// The push secret is this environment's own `CI_JOB_TOKEN`.
        JobToken = "job_token",
        /// A push secret ocx injected but cannot classify further.
        Token = "token",
        /// Nothing was injected; git's own credential helpers are in charge.
        GitHelper = "git_helper",
    }
}

wire_words! {
    /// A write-preflight capability.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum Capability {
        /// The local `git` version against the floor the git transport needs.
        GitVersion = "git_version",
        /// The credential's push permission on the repository being written.
        PushAccess = "push_access",
        /// Whether the project lets a CI job token push to its repository.
        JobTokenPush = "job_token_push",
        /// Whether the index project's job-token allowlist admits the publishing project.
        JobTokenAllowlist = "job_token_allowlist",
    }
}

impl From<CapabilityName> for Capability {
    fn from(name: CapabilityName) -> Self {
        match name {
            CapabilityName::GitVersion => Self::GitVersion,
            CapabilityName::PushAccess => Self::PushAccess,
            CapabilityName::JobTokenPush => Self::JobTokenPush,
            CapabilityName::JobTokenAllowlist => Self::JobTokenAllowlist,
        }
    }
}

wire_words! {
    /// How one capability check came out. There is no `failed`: a check that fails raises an error
    /// and no report is rendered.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
    pub enum CapabilityStatus {
        /// The capability was read and is present.
        Passed = "passed",
        /// The forge did not let the credential read the capability; never fails the run.
        Unknown = "unknown",
        /// The check does not apply to this run's forge, transport or credential.
        Skipped = "skipped",
    }
}

impl From<CheckStatus> for CapabilityStatus {
    fn from(status: CheckStatus) -> Self {
        match status {
            CheckStatus::Passed => Self::Passed,
            CheckStatus::Unknown => Self::Unknown,
            CheckStatus::Skipped => Self::Skipped,
        }
    }
}

/// One row of the write preflight.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CapabilityCheckEntry {
    /// The capability this row reports on.
    pub name: Capability,
    /// How the check came out.
    pub status: CapabilityStatus,
    /// A human-readable qualifier where the forge exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl CapabilityCheckEntry {
    /// Projects every preflight row, `Skipped` included, in `CapabilityName` declaration order.
    #[must_use]
    pub fn from_checks(checks: &[CapabilityCheck]) -> Vec<Self> {
        checks
            .iter()
            .map(|check| Self {
                name: check.name.into(),
                status: check.status.into(),
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
    use ocx_announce::forge::{CapabilityName, ForgeCredentials, ForgeToken, PushAccess, WriteTransport};

    use super::{
        Capability, CapabilityCheckEntry, CapabilityStatus, CredentialKind, PushCredentialKind, credential_kind,
        push_credential_kind,
    };

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
    /// `#[serde(rename_all = "snake_case")]` so `job_token` becomes `JobToken`.
    #[test]
    fn credential_kind_wire_spellings() {
        let spellings: Vec<String> = CredentialKind::ALL.iter().copied().map(wire).collect();
        assert_eq!(
            spellings,
            vec!["job_token".to_string(), "token".to_string(), "none".to_string()],
            "the C-060 API credential vocabulary is exactly these three words, in this order"
        );
    }

    /// C-060: four push states, three of them words and the fourth an absent key.
    ///
    /// Red at the stub: the vocabulary is unrendered.
    /// Mutation once implemented: add a `Pat` arm; the arity pairing reds.
    #[test]
    fn push_credential_kind_wire_spellings() {
        let spellings: Vec<String> = PushCredentialKind::ALL.iter().copied().map(wire).collect();
        assert_eq!(
            spellings,
            vec!["job_token".to_string(), "token".to_string(), "git_helper".to_string()],
            "the C-060 push credential vocabulary is exactly these three words plus absence"
        );
    }

    /// The report's forge and transport words are the `--forge`/`--transport` flag values.
    #[test]
    fn forge_and_transport_wire_match_the_flag_values() {
        use clap::ValueEnum;
        use ocx_announce::forge::{ForgeKind, WriteTransport};
        for kind in ForgeKind::value_variants() {
            assert_eq!(wire(super::Forge::from(*kind)), kind.to_string());
        }
        for transport in WriteTransport::value_variants() {
            assert_eq!(wire(super::Transport::from(*transport)), transport.to_string());
        }
    }

    /// The report's identity-source word is the one stderr and the request body print.
    #[test]
    fn identity_source_wire_matches_the_library_word() {
        for source in ocx_announce::claim::OwnerIdentitySource::ALL {
            assert_eq!(wire(super::IdentitySource::from(source)), source.to_string());
        }
    }

    /// C-060 / C-063: the two credential states a unit test can construct
    /// without the process environment map to the right word.
    ///
    /// The `job_token` row is **not** here: `ForgeCredentials::api_is_job_token`
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

    /// C-060: `push_credential_kind` is absent under `api` — **even when a push
    /// credential was resolved**.
    ///
    /// `ForgeCredentials::resolve` populates the push half whenever an API
    /// credential is non-empty, with no transport guard, so a mapper written
    /// over `push()` alone reports a push kind for a REST claim that pushed
    /// nothing.
    ///
    /// Red at the stub: `push_credential_kind` is `unimplemented!()`.
    /// Mutation once implemented: drop the `WriteTransport::Api => None` gate.
    /// A credential with no push half then falls to `git_helper`, so this reds
    /// even without an environment-resolved push secret.
    #[test]
    fn push_credential_kind_is_null_under_the_api_transport() {
        assert_eq!(
            push_credential_kind(&resolved_token(), WriteTransport::Api),
            None,
            "the api transport pushes nothing, so it reports no push credential kind"
        );
    }

    /// C-060 / S-029: `git_helper` and an absent key are different statements.
    ///
    /// `git_helper` says a push happened and git's own helpers authenticated
    /// it; absence says no push was attempted. An implementation mapping
    /// `push().is_none()` straight onto `Option::None` collapses them.
    ///
    /// Red at the stub: `push_credential_kind` is `unimplemented!()`.
    /// Mutation once implemented: return `None` when `push().is_none()`.
    #[test]
    fn push_credential_kind_git_helper_is_distinct_from_null() {
        assert_eq!(
            push_credential_kind(&resolved_token(), WriteTransport::Git),
            Some(PushCredentialKind::GitHelper),
            "no injected push secret under the git transport is `git_helper`, never absent"
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
        let names: Vec<Capability> = entries.iter().map(|entry| entry.name).collect();
        assert_eq!(
            names,
            CapabilityName::ALL.map(Capability::from).to_vec(),
            "every capability row is projected, in CapabilityName declaration order"
        );
        assert!(
            entries.iter().all(|entry| entry.status == CapabilityStatus::Skipped),
            "a run that checked nothing still reports every row, as `skipped`"
        );
        // The wire half: snake_case words, and an absent `detail` rather than `null`.
        let row = serde_json::to_value(&entries[0]).expect("a row serializes");
        assert_eq!(row.get("name").and_then(serde_json::Value::as_str), Some("git_version"));
        assert!(
            row.get("detail").is_none(),
            "an unset detail is omitted, never null: {row}"
        );
        assert_eq!(
            row.get("status").and_then(serde_json::Value::as_str),
            Some("skipped"),
            "the status renders as the wire word, not as a Rust variant name"
        );
    }
}
