// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! REST-only forge client for `ocx package announce`.
//!
//! Copy-and-own port of grimoire's forge module (`src/catalog/forge.rs`, same
//! owner), transport-adjusted to REST-only and owned by OCX — no shared crate,
//! no cross-repo dependency (design register S5). The git-subprocess transport
//! and the tri-state push-permission probe are deliberately dropped (design
//! register S1/S3); GitLab, dropped from the first cut, is back as a peer
//! implementation rather than a branch inside one client.
//!
//! The operation set announce drives is [`Forge`]; the public vocabulary types
//! name no forge and expose no forge-specific flag grammar (design register S7).
//! [`GitHubForge`] and [`GitLabForge`] are its two implementations, each
//! covering both its canonical host and self-hosted instances of it, selected by
//! [`ForgeKind`]. The test seam stays the HTTP base-URL override
//! (`with_base_url` + `__OCX_TESTING_FORGE_BASE_URL`), which the acceptance fake
//! forge substitutes against — the trait carries a second *forge*, it is not a
//! mocking seam for the first one.
//!
//! Security invariants (design register X5) are owed by **every** implementation
//! and re-proved in each: no-redirect client (the credential is never replayed
//! on a cross-host 3xx), credential via header only, fork parent verified
//! against the upstream, fork identity built only from API response bodies, and
//! a bounded fork-readiness wait.

mod api;
mod error;
mod github;
mod gitlab;
mod http;
mod identity;
mod kind;
mod poll;

pub use api::{BranchComparison, CommitBase, Forge, RefUpdate};
pub use error::ForgeError;
pub use github::GitHubForge;
pub use gitlab::GitLabForge;
pub use kind::ForgeKind;

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

/// A forge repository coordinate: `[HOST/]NAMESPACE/PROJECT`.
///
/// The namespace may itself hold slashes. GitHub's namespace is always one
/// segment, but GitLab nests groups arbitrarily deep
/// (`acme/platform/tooling/index`), and a coordinate type that cannot spell that
/// makes a whole class of GitLab index repositories unaddressable. The type is
/// therefore forge-neutral and permissive; a forge that does not nest rejects a
/// multi-segment namespace itself, where the rule actually belongs.
///
/// `host` is `None` for the forge's canonical host, `Some` for a self-hosted
/// instance. Whether a leading segment is a host is decided by the same rule
/// OCI identifiers use ([`crate::oci::identifier::segment_is_host`]) — one
/// spelling of "that looks like a host", not a second one that can drift.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoCoordinate {
    /// The forge host, or `None` for the forge's canonical host.
    pub host: Option<String>,
    /// The owning account, organization, or (possibly nested) group path.
    pub namespace: String,
    /// The repository/project name.
    pub project: String,
}

impl RepoCoordinate {
    /// The `namespace/project` path, without the host.
    #[must_use]
    pub fn full_path(&self) -> String {
        format!("{}/{}", self.namespace, self.project)
    }

    /// The first namespace segment — the account or top-level group that owns
    /// the path. This is the unit a fork lives under and the unit an ownership
    /// check compares, on both forges.
    #[must_use]
    pub fn namespace_root(&self) -> &str {
        self.namespace.split('/').next().unwrap_or(&self.namespace)
    }

    /// The same coordinate under a different `namespace`, keeping the host and
    /// project — how a fork's conventional location is named.
    #[must_use]
    pub fn with_namespace(&self, namespace: impl Into<String>) -> Self {
        Self {
            host: self.host.clone(),
            namespace: namespace.into(),
            project: self.project.clone(),
        }
    }
}

/// Whether a coordinate segment is a well-formed `host` or `host:port`.
///
/// Deliberately narrower than [`crate::oci::identifier::segment_is_host`], which
/// only answers "does this look like a host" for the *shape* of an identifier.
/// This one guards a different thing: the value ends up in the API base URL the
/// credential is sent to, so anything that could shift the URL's authority —
/// userinfo (`@`), a query or fragment, a nested path, an IPv6 literal — is
/// rejected rather than interpreted.
fn is_valid_host(segment: &str) -> bool {
    let (name, port) = match segment.split_once(':') {
        Some((name, port)) => (name, Some(port)),
        None => (segment, None),
    };
    // `u16::from_str` also rejects a sign, whitespace and an out-of-range value
    // such as `80443`, which a digits-only length check would wave through.
    if let Some(port) = port
        && !(port.bytes().all(|byte| byte.is_ascii_digit()) && port.parse::<u16>().is_ok())
    {
        return false;
    }
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// Whether a namespace or project segment is a legal forge path segment.
///
/// Both forges restrict a namespace and a project path to letters, digits, `_`,
/// `-` and `.`, so this rejects nothing either forge would accept. It is a
/// *security* check as much as a validation one: the GitHub client interpolates
/// `full_path()` into a URL raw (GitLab percent-encodes it), so a segment
/// carrying `?`, `#` or `/` would silently retarget the request —
/// `acme?x=1/index` becomes a call to `/repos/acme` with the rest as a query
/// string. Refusing the character is one check for both clients; encoding at
/// sixteen GitHub call sites is sixteen chances to miss one.
fn is_valid_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

impl std::str::FromStr for RepoCoordinate {
    type Err = ForgeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || ForgeError::InvalidRepoCoordinate {
            value: value.to_string(),
        };
        let mut segments: Vec<&str> = value.split('/').collect();
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(invalid());
        }
        // A leading host is only recognised when something is left to be a
        // `namespace/project` after it — `acme/index` is a two-segment path, never
        // a host with a bare project.
        let host = if segments.len() >= 3 && crate::oci::identifier::segment_is_host(segments[0]) {
            // A segment that looks like a host but is not a well-formed one is
            // REFUSED, never demoted to a namespace segment: the host is
            // interpolated straight into the API base URL that carries the
            // announce credential, so `gitlab.com@evil.example` would put
            // `gitlab.com` in the userinfo and send the token to
            // `evil.example`. Fail closed at the parse boundary (design
            // register X6).
            if !is_valid_host(segments[0]) {
                return Err(invalid());
            }
            Some(segments.remove(0).to_string())
        } else {
            None
        };
        if segments.len() < 2 {
            return Err(invalid());
        }
        // Every remaining segment is a namespace or project name and lands in a
        // request URL. Checked here, once, for both forges.
        if !segments.iter().all(|segment| is_valid_path_segment(segment)) {
            return Err(invalid());
        }
        let project = segments.pop().ok_or_else(invalid)?.to_string();
        Ok(Self {
            host,
            namespace: segments.join("/"),
            project,
        })
    }
}

impl std::fmt::Display for RepoCoordinate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.host {
            Some(host) => write!(formatter, "{host}/{}/{}", self.namespace, self.project),
            None => write!(formatter, "{}/{}", self.namespace, self.project),
        }
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
    /// The fork's canonical `namespace/project` path.
    pub full_path: String,
    /// The fork's owning account, organization, or group path.
    pub namespace: String,
    /// The fork's repository/project name.
    pub project: String,
    /// The forge's own opaque handle for the fork, where it has one that is
    /// cheaper or more precise than the path — GitLab's numeric project id.
    /// `None` on a forge addressed purely by path.
    pub id: Option<u64>,
}

impl ForkIdentity {
    /// The fork as a [`RepoCoordinate`] for building endpoints, carrying
    /// `upstream`'s host so a self-hosted fork stays on its own instance.
    #[must_use]
    pub fn coordinate(&self, upstream: &RepoCoordinate) -> RepoCoordinate {
        RepoCoordinate {
            host: upstream.host.clone(),
            namespace: self.namespace.clone(),
            project: self.project.clone(),
        }
    }

    /// The first namespace segment — see [`RepoCoordinate::namespace_root`].
    #[must_use]
    pub fn namespace_root(&self) -> &str {
        self.namespace.split('/').next().unwrap_or(&self.namespace)
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
        assert_eq!(coordinate.host, None);
        assert_eq!(coordinate.namespace, "ocx-sh");
        assert_eq!(coordinate.project, "index");
        assert_eq!(coordinate.full_path(), "ocx-sh/index");
        assert_eq!(coordinate.to_string(), "ocx-sh/index");
    }

    #[test]
    fn repo_coordinate_rejects_malformed_input() {
        for value in ["", "owner", "owner/", "/repo", "owner//repo"] {
            assert!(
                value.parse::<RepoCoordinate>().is_err(),
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn a_leading_host_is_recognised_only_when_a_path_follows_it() {
        let hosted: RepoCoordinate = "gitlab.example.com/acme/index".parse().expect("host and path");
        assert_eq!(hosted.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(hosted.namespace, "acme");
        assert_eq!(hosted.project, "index");
        assert_eq!(hosted.to_string(), "gitlab.example.com/acme/index");

        // Three segments where the first is NOT host-shaped is a nested
        // namespace, not a host — the distinction the whole grammar turns on.
        let nested: RepoCoordinate = "acme/platform/index".parse().expect("nested namespace");
        assert_eq!(nested.host, None);
        assert_eq!(nested.namespace, "acme/platform");
        assert_eq!(nested.project, "index");
        assert_eq!(nested.namespace_root(), "acme");

        // A port makes a segment host-shaped too (the loopback case every
        // acceptance test runs against).
        let ported: RepoCoordinate = "localhost:8080/acme/index".parse().expect("host with a port");
        assert_eq!(ported.host.as_deref(), Some("localhost:8080"));

        // Two segments are ALWAYS namespace/project, even when the first is
        // host-shaped: there would be no project left otherwise, and a coordinate
        // with no project is not a coordinate. A host-shaped namespace is legal
        // on both forges, so this is a real path, not a near-miss to reject.
        let two: RepoCoordinate = "host.example.com/owner".parse().expect("two segments are a path");
        assert_eq!(two.host, None);
        assert_eq!(two.namespace, "host.example.com");
        assert_eq!(two.project, "owner");
    }

    #[test]
    fn a_deeply_nested_hosted_coordinate_round_trips() {
        let value = "gitlab.example.com/acme/platform/tooling/index";
        let coordinate: RepoCoordinate = value.parse().expect("deeply nested");
        assert_eq!(coordinate.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(coordinate.namespace, "acme/platform/tooling");
        assert_eq!(coordinate.project, "index");
        assert_eq!(coordinate.full_path(), "acme/platform/tooling/index");
        assert_eq!(coordinate.to_string(), value);
        // A fork's conventional location keeps the host and the project name,
        // and replaces only the namespace.
        let fork = coordinate.with_namespace("contrib/forks");
        assert_eq!(fork.to_string(), "gitlab.example.com/contrib/forks/index");
    }

    #[test]
    fn a_host_shaped_segment_that_is_not_a_host_is_refused_not_reinterpreted() {
        // Each of these satisfies `segment_is_host` (it has a dot or a colon) and
        // would otherwise be interpolated into the API base URL that carries the
        // announce credential. `gitlab.com@evil.example` is the sharp one: as a
        // URL authority it means userinfo `gitlab.com` at host `evil.example`,
        // so the token would be sent to `evil.example`.
        for value in [
            "gitlab.com@evil.example/acme/index",
            "gitlab.com:443@evil.example/acme/index",
            "gitlab.com?x=1/acme/index",
            "gitlab.com#f/acme/index",
            "-lead.example.com/acme/index",
            "trail-.example.com/acme/index",
            "git.example.com:/acme/index",
            "git.example.com:80443/acme/index",
            "git.example.com:80a/acme/index",
            "[2001:db8::1]/acme/index",
        ] {
            assert!(
                value.parse::<RepoCoordinate>().is_err(),
                "expected {value:?} to be refused rather than parsed"
            );
        }
        // The refusal must not have swallowed the legitimate ported host it sits
        // next to — a check that only ever goes red is not a check.
        let ported: RepoCoordinate = "git.example.com:8443/acme/index".parse().expect("a real ported host");
        assert_eq!(ported.host.as_deref(), Some("git.example.com:8443"));
    }

    #[test]
    fn a_path_segment_that_could_retarget_a_request_is_refused() {
        // The GitHub client interpolates `full_path()` into a URL raw, so each
        // of these would address a different endpoint than the one named:
        // `acme?x=1/index` reaches `/repos/acme` with the rest as a query.
        for value in [
            "acme?x=1/index",
            "acme#frag/index",
            "acme/index?ref=x",
            "acme/index#frag",
            "acme /index",
            "acme%2Fevil/index",
            "../index",
            "acme/../index",
            "acme/./index",
        ] {
            assert!(
                value.parse::<RepoCoordinate>().is_err(),
                "expected {value:?} to be refused rather than interpolated into a URL"
            );
        }
        // The falsifying half: the character set both forges actually allow —
        // letters, digits, `_`, `-`, `.` — still parses, nested and all. The
        // host is written out because a dotted FIRST segment is read as a host
        // (the D16 ambiguity), which is a different rule from this one.
        let ok: RepoCoordinate = "gitlab.com/acme.team/plat_form/sub-group/index.js"
            .parse()
            .expect("legal segments");
        assert_eq!(ok.namespace, "acme.team/plat_form/sub-group");
        assert_eq!(ok.project, "index.js");
    }

    #[test]
    fn a_dotted_top_level_group_is_addressed_by_writing_the_host_out() {
        // GitLab group paths may contain dots, so `acme.team/platform/index` is
        // genuinely ambiguous: the grammar reads the dotted first segment as a
        // host, which is the documented behaviour and not a silent misroute —
        // `acme.team` is not a forge OCX knows, so the run stops and asks for
        // `--forge` rather than sending the credential anywhere.
        let read_as_host: RepoCoordinate = "acme.team/platform/index".parse().expect("parses");
        assert_eq!(read_as_host.host.as_deref(), Some("acme.team"));
        assert!(
            ForgeKind::from_host(Some("acme.team")).is_none(),
            "an unknown host must not resolve to a forge on its own"
        );
        // Writing the canonical host out is how the nested group is expressed,
        // and it round-trips.
        let value = "gitlab.com/acme.team/platform/index";
        let explicit: RepoCoordinate = value.parse().expect("explicit host");
        assert_eq!(explicit.host.as_deref(), Some("gitlab.com"));
        assert_eq!(explicit.namespace, "acme.team/platform");
        assert_eq!(explicit.project, "index");
        assert_eq!(explicit.to_string(), value);
        assert_eq!(ForgeKind::from_host(explicit.host.as_deref()), Some(ForgeKind::GitLab));
    }
}
