// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Request/outcome vocabulary for the claim orchestration, and the fixed pull/merge-request template.
//!
//! The `Display` spellings of [`OwnerIdentitySource`] and [`ClaimStatus`] are shipped JSON-report
//! values; renaming one breaks every report consumer.

use std::path::PathBuf;

use super::owners::ResolvedOwner;
use crate::forge::{ForkIdentity, PullRequest, PushAccess, RepoCoordinate};

/// Where the claim writes its rendered root.
#[derive(Debug, Clone)]
pub enum ClaimTarget {
    /// Write the root under this directory with no forge mutation; the forge is still read.
    Out(PathBuf),
    /// Open (or update) a pull request from a fork of the index repository.
    Fork(RepoCoordinate),
    /// Commit the claim branch onto the index repository itself and open the pull request from it.
    Direct,
}

/// Whether the claim moved the claim branch; a `--out` run is always [`Self::Updated`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimStatus {
    /// The claim branch already carries a byte-identical root.
    Unchanged,
    /// The branch was created or moved, or `--out` wrote the tree.
    Updated,
}

impl ClaimStatus {
    /// Declaration order is the order a report consumer sees.
    pub const ALL: [Self; 2] = [Self::Unchanged, Self::Updated];
}

impl std::fmt::Display for ClaimStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unchanged => "unchanged",
            Self::Updated => "updated",
        })
    }
}

/// Which rule produced the owner list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerIdentitySource {
    /// Every pair came from the forge's users API or the token identity.
    Resolved,
    /// At least one `LOGIN:ID` was taken on the operator's word because the users API was unreachable.
    Asserted,
    /// The list came from `GITLAB_USER_*` / `GITHUB_ACTOR*` with no server confirmation.
    CiEnvironment,
}

impl OwnerIdentitySource {
    /// Declaration order is the order a report consumer sees.
    pub const ALL: [Self; 3] = [Self::Resolved, Self::Asserted, Self::CiEnvironment];
}

impl std::fmt::Display for OwnerIdentitySource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Resolved => "resolved",
            Self::Asserted => "asserted",
            Self::CiEnvironment => "ci-environment",
        })
    }
}

/// One `--owner` value, before the forge has (or has not) confirmed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerSpec {
    /// A bare `LOGIN`. Resolvable only while the users API is reachable.
    Login(String),
    /// A `LOGIN:ID` pair the operator asserted.
    Resolved { login: String, id: u64 },
}

/// The `upstream` object a third-party package carries.
///
/// `repository_url` and `disclaimer` are omitted when absent, never `null`: the root schema
/// refuses a `null` there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    /// `--upstream-org`. Always present when the object is.
    pub org: String,
    /// `--upstream-repository-url`.
    pub repository_url: Option<String>,
    /// `--upstream-disclaimer`.
    pub disclaimer: Option<String>,
}

/// Whether a value may be written into [`Upstream::repository_url`], which a catalog renders as an `href`.
///
/// Only `http`/`https`, or a `javascript:`/`data:` value becomes a live link downstream.
/// No userinfo: GitLab's `CI_REPOSITORY_URL` embeds the job token, which forwarding would publish.
/// A bare `bool`, because echoing a refused value would print that token in the CI log.
#[must_use]
pub fn upstream_repository_url_is_publishable(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https") && url.username().is_empty() && url.password().is_none()
}

/// One package claim.
#[derive(Debug, Clone)]
pub struct ClaimRequest {
    /// The logical `<namespace>/<package>` identifier, its registry already resolved by the caller.
    pub package: ocx_oci::PackageRef,
    /// `--repository`, the `oci://host/path` physical pointer, unparsed.
    pub repository: String,
    /// `--owner`, in the order given; empty means the ladder consults the CI environment and token.
    pub owners: Vec<OwnerSpec>,
    /// The `upstream` object, when `--upstream-org` was given.
    pub upstream: Option<Upstream>,
    /// The write target.
    pub target: ClaimTarget,
    /// The index repository coordinate (default `ocx-sh/index`).
    pub index_repo: RepoCoordinate,
    /// `[registries."<ns>"].trusted_hosts` for the package's logical registry: the sole SSRF escape
    /// hatch for dialing the physical repository.
    pub trusted_hosts: Vec<String>,
    /// The plain-HTTP allowance the client is built with; the pre-flight picks the dial scheme, and so
    /// the proxy variable, from it.
    pub insecure_hosts: Vec<String>,
}

/// The result of a claim run.
#[derive(Debug)]
pub struct ClaimOutcome {
    /// The logical `<namespace>/<package>` identifier claimed.
    pub package: String,
    /// The logical name written into the root.
    pub name: String,
    /// Whether the claim branch moved.
    pub status: ClaimStatus,
    /// The resolved list written into the root — never a bare login.
    pub owners: Vec<ResolvedOwner>,
    /// Which rule produced [`Self::owners`]; the request body and the report both read this one value.
    pub owner_identity_source: OwnerIdentitySource,
    /// The token identity, else the CI-environment identity; `None` when neither is available.
    /// Authorship never substitutes for ownership, nor the reverse.
    pub author: Option<ResolvedOwner>,
    /// Which rung produced [`Self::author`]: `resolved` is forge-attested, `ci-environment` is
    /// settable by any earlier pipeline step. `None` exactly when [`Self::author`] is; never `asserted`.
    pub author_identity_source: Option<OwnerIdentitySource>,
    /// The claim branch, so a script need not re-derive the naming convention.
    pub branch: String,
    /// The opened/updated pull request — `None` under `--out`.
    pub pull_request: Option<PullRequest>,
    /// The verified fork identity — `None` under `--out` and on the direct path.
    pub fork: Option<ForkIdentity>,
    /// Relative paths written under the `--out` directory; empty otherwise.
    pub written_paths: Vec<String>,
    /// The preflight rows.
    pub push_access: PushAccess,
}

/// The pull/merge-request title: a fixed template over the logical name and nothing else.
#[must_use]
pub fn request_title(name: &str) -> String {
    format!("claim: {name}")
}

/// The pull/merge-request body.
///
/// A fixed template over structured values only, so no operator free text reaches a body a human
/// merges.
#[must_use]
pub fn request_body(
    name: &str,
    repository: &str,
    branch: &str,
    owners: &[ResolvedOwner],
    source: OwnerIdentitySource,
) -> String {
    // Bare `login:id`, never `@login`, or the claim fires a mention in a repository humans review.
    let pairs = owners
        .iter()
        .map(|owner| format!("{}:{}", owner.login, owner.id))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Package claim for `{name}`.\n\n\
         - name: {name}\n\
         - repository: {repository}\n\
         - branch: {branch}\n\
         - owners: {pairs}\n\
         - owner identity source: {source}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owners() -> Vec<ResolvedOwner> {
        vec![
            ResolvedOwner {
                login: "alice".to_string(),
                id: 7,
            },
            ResolvedOwner {
                login: "bob".to_string(),
                id: 8,
            },
        ]
    }

    const NAME: &str = "ocx.sh/acme/widget";
    const REPOSITORY: &str = "oci://ghcr.io/acme/widget";
    const BRANCH: &str = "indexbot-claim-acme-widget";

    /// C-046 — [`OwnerIdentitySource`]'s three renderings, paired against `ALL`.
    ///
    /// These strings land in a parsed JSON report and are **one-way once
    /// shipped**; `CapabilityName` got the same guard in WP-5 for exactly that
    /// reason. The pairing is what makes the arity a guard rather than a number
    /// the test wrote for itself: a variant added to `ALL` without a spelling reds
    /// on the length, and one renamed in `Display` reds on its own row. Note the
    /// **hyphen** in `ci-environment` — an underscore is the natural Rust-side
    /// spelling and the wrong wire one.
    ///
    /// Reds on: renaming any spelling, reordering `ALL`, or changing its length.
    #[test]
    fn owner_identity_source_wire_spellings_and_arity() {
        let expected = [
            (OwnerIdentitySource::Resolved, "resolved"),
            (OwnerIdentitySource::Asserted, "asserted"),
            (OwnerIdentitySource::CiEnvironment, "ci-environment"),
        ];
        assert_eq!(
            OwnerIdentitySource::ALL.len(),
            expected.len(),
            "every provenance owes a contracted wire spelling; ALL is {:?}",
            OwnerIdentitySource::ALL
        );
        for (index, (source, spelling)) in expected.into_iter().enumerate() {
            assert_eq!(
                OwnerIdentitySource::ALL[index],
                source,
                "declaration order is the order a report consumer sees"
            );
            assert_eq!(source.to_string(), spelling);
        }
    }

    /// C-046 — [`ClaimStatus`]'s two renderings, paired against `ALL`.
    ///
    /// Deliberately the same two words announce uses, over a different subject:
    /// announce compares against the *committed* root, claim against the *open
    /// claim branch*. The `--out`-is-always-`updated` consequence is asserted in
    /// the orchestration's own tests; this pins the vocabulary.
    ///
    /// Reds on: renaming a spelling, reordering `ALL`, or changing its length.
    #[test]
    fn claim_status_wire_spellings_and_arity() {
        let expected = [(ClaimStatus::Unchanged, "unchanged"), (ClaimStatus::Updated, "updated")];
        assert_eq!(ClaimStatus::ALL.len(), expected.len(), "ALL is {:?}", ClaimStatus::ALL);
        for (index, (status, spelling)) in expected.into_iter().enumerate() {
            assert_eq!(ClaimStatus::ALL[index], status);
            assert_eq!(status.to_string(), spelling);
        }
    }

    /// C-067's positive half — the body **contains** every structured value.
    ///
    /// A denylist ("the disclaimer is absent") passes for an empty body, which is
    /// why the contract needs a positive assertion beside it: the logical name,
    /// the physical repository, the branch, every `login:id` pair, and the
    /// provenance word. `quality-rust.md` § Structural guards names this exact
    /// failure — a negative assertion fails silently where a positive one fails
    /// loudly.
    ///
    /// The body's exact prose is deliberately **not** pinned here. No literal
    /// template exists in the ADR — only the value list — so an assertion on the
    /// builder's own wording would be a regression test dressed up as a contract.
    ///
    /// Reds on: dropping the owners, the repository, the branch, or the
    /// provenance word from the body.
    #[test]
    fn request_body_carries_every_structured_value() {
        let body = request_body(NAME, REPOSITORY, BRANCH, &owners(), OwnerIdentitySource::CiEnvironment);

        for value in [NAME, REPOSITORY, BRANCH, "alice:7", "bob:8", "ci-environment"] {
            assert!(body.contains(value), "the body must carry {value}: {body}");
        }

        let title = request_title(NAME);
        assert!(title.contains(NAME), "the title names the package: {title}");
    }

    /// C-067 — owners render as bare `login:id`, **never** `@login`.
    ///
    /// A claim must fire no mentions in a repository humans review. The negative
    /// assertion is safe here precisely because every other value in the body is
    /// charset-constrained: the logical name and the repository path come from the
    /// identifier grammar, and the login charset guard admits no `@`. The positive
    /// half is what makes it a check rather than a test of an empty string.
    ///
    /// Reds on: `@{login}` in the renderer.
    #[test]
    fn owner_logins_render_without_at_sign() {
        let body = request_body(NAME, REPOSITORY, BRANCH, &owners(), OwnerIdentitySource::Resolved);

        assert!(body.contains("alice:7"), "the pair is rendered: {body}");
        assert!(body.contains("bob:8"), "for every owner: {body}");
        assert!(!body.contains('@'), "and no owner becomes a mention: {body}");
        assert!(
            !request_title(NAME).contains('@'),
            "nor does the title: {}",
            request_title(NAME)
        );
    }

    /// SEC — [`upstream_repository_url_is_publishable`] refuses userinfo, and
    /// refuses a non-`http(s)` scheme.
    ///
    /// The userinfo row is the one that costs a secret: GitLab's own
    /// `CI_REPOSITORY_URL` is `https://gitlab-ci-token:<token>@host/p.git`, and
    /// the value is written into a **committed** root, so accepting it publishes
    /// the job token (CWE-522). The scheme rows are DX-64's stored-link guard.
    ///
    /// Both polarities in one function: the refusal list alone passes for a
    /// predicate that refuses everything.
    ///
    /// Reds on: dropping either the `username()`/`password()` conjunct (the
    /// userinfo rows), or the `matches!` on the scheme (the `javascript:` row).
    #[test]
    fn an_upstream_repository_url_is_publishable_only_without_credentials() {
        for accepted in [
            "https://example.com/acme/widget",
            "http://example.com/acme/widget",
            "HTTPS://example.com/acme/widget",
            "https://gitlab.example/group/project.git",
            // Measured, not assumed: WHATWG folds the extra slash away, so this
            // is host `acme`, path `/widget` -- an ordinary https URL. The
            // prefix test this predicate replaced refused it.
            "https:///acme/widget",
        ] {
            assert!(
                upstream_repository_url_is_publishable(accepted),
                "{accepted} is an ordinary repository URL"
            );
        }

        for refused in [
            // The reaching input: a forwarded `CI_REPOSITORY_URL`.
            "https://gitlab-ci-token:glcbt-notarealjobtoken@gitlab.example/group/project.git",
            // A login with no password is not a repository URL either.
            "https://alice@gitlab.example/group/project.git",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "ssh://git@gitlab.example/group/project.git",
            "example.com/acme/widget",
            "//example.com/acme/widget",
            "https://",
        ] {
            assert!(
                !upstream_repository_url_is_publishable(refused),
                "{refused} must not reach a committed index root"
            );
        }
    }
}
