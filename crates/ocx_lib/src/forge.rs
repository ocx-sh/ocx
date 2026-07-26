// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! REST-only forge client for `ocx package announce`.
//!
//! Copy-and-own port of grimoire's forge module (`src/catalog/forge.rs`, same
//! owner), transport-adjusted to REST-only and owned by OCX — no shared crate,
//! no cross-repo dependency (design register S5). The git-subprocess transport,
//! the tri-state push-permission probe, and every GitLab code path from the
//! donor are deliberately dropped (design register S1/S3/S7).
//!
//! GitHub-only v1 behind a **forge-neutral** surface (design register S7): the
//! public types name no forge and expose no `--github-*` vocabulary.
//! [`GitHubForge`] is the single concrete forge client — a second forge would
//! be its own future track, not a trait-dispatched variant (design register
//! D2). The test seam is the HTTP base-URL override
//! ([`GitHubForge::with_base_url`] + `__OCX_TESTING_FORGE_BASE_URL`), which the
//! acceptance fake forge substitutes against — not a trait object.
//!
//! Security invariants (design register X5), enforced in [`github`]:
//! no-redirect client (bearer never replayed on a cross-host 3xx), bearer via
//! header only, fork parent verified against the upstream, fork identity built
//! only from API response bodies, and a bounded fork-readiness poll.

mod error;
mod github;
mod identity;
mod poll;

pub use error::ForgeError;
pub use github::{BranchComparison, GitHubForge, RefUpdate};

/// Announce credential, sourced from `OCX_ANNOUNCE_TOKEN`.
///
/// The secret is only ever sent as a bearer header — never logged, never placed
/// in a URL, never in argv. `Debug` is redacted so it cannot leak into logs or
/// error chains (design register X6).
#[derive(Clone)]
pub struct ForgeToken(String);

impl ForgeToken {
    /// Wrap a raw credential string.
    #[must_use]
    pub fn new(secret: String) -> Self {
        Self(secret)
    }
}

impl From<String> for ForgeToken {
    fn from(secret: String) -> Self {
        Self(secret)
    }
}

impl std::fmt::Debug for ForgeToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ForgeToken(***)")
    }
}

/// A forge repository coordinate (`owner/repo`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoCoordinate {
    /// The owning account or organization.
    pub owner: String,
    /// The repository name.
    pub repo: String,
}

impl RepoCoordinate {
    /// The canonical `owner/repo` form.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

impl std::str::FromStr for RepoCoordinate {
    type Err = ForgeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (owner, repo) = value
            .split_once('/')
            .filter(|(owner, repo)| !owner.is_empty() && !repo.is_empty() && !repo.contains('/'))
            .ok_or_else(|| ForgeError::InvalidRepoCoordinate {
                value: value.to_string(),
            })?;
        Ok(Self {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }
}

impl std::fmt::Display for RepoCoordinate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.owner, self.repo)
    }
}

/// A verified fork identity.
///
/// Every field is read from a forge API **response body** — never composed from
/// `{login}/{basename}` — so a renamed fork resolves to its real name (design
/// register X5). Callers rebuild every subsequent endpoint from this identity,
/// never from an API-returned URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkIdentity {
    /// The fork's canonical `owner/repo`.
    pub full_name: String,
    /// The fork's owning account or organization.
    pub owner: String,
    /// The fork's repository name.
    pub repo: String,
}

impl ForkIdentity {
    /// The fork as a [`RepoCoordinate`] for building endpoints.
    #[must_use]
    pub fn coordinate(&self) -> RepoCoordinate {
        RepoCoordinate {
            owner: self.owner.clone(),
            repo: self.repo.clone(),
        }
    }
}

/// The result of opening or updating a pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    /// The pull-request number.
    pub number: u64,
    /// The pull-request web URL, reported to the user.
    pub html_url: String,
    /// True when an existing open pull request was reused, false when freshly
    /// opened.
    pub updated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_token_debug_is_redacted() {
        let token = ForgeToken::new("ghp_supersecret_value".to_string());
        assert_eq!(format!("{token:?}"), "ForgeToken(***)");
        assert_eq!(format!("{token:#?}"), "ForgeToken(***)");
        assert!(!format!("{token:?}").contains("supersecret"));
    }

    #[test]
    fn repo_coordinate_parses_and_round_trips() {
        let coordinate: RepoCoordinate = "ocx-sh/index".parse().expect("valid coordinate");
        assert_eq!(coordinate.owner, "ocx-sh");
        assert_eq!(coordinate.repo, "index");
        assert_eq!(coordinate.full_name(), "ocx-sh/index");
        assert_eq!(coordinate.to_string(), "ocx-sh/index");
    }

    #[test]
    fn repo_coordinate_rejects_malformed_input() {
        for value in ["", "owner", "owner/", "/repo", "owner/repo/extra", "owner//repo"] {
            assert!(
                value.parse::<RepoCoordinate>().is_err(),
                "expected {value:?} to be rejected"
            );
        }
    }
}
