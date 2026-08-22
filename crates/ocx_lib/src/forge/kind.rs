// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Which forge a coordinate lives on, and how that is decided.

use super::{ForgeError, ForgeToken, GitHubForge, GitLabForge, RepoCoordinate};

/// The forge implementations announce can talk to.
///
/// A closed internal enum with no `#[non_exhaustive]`, per the arch-principles
/// convention: the binary is the only consumer, and every match staying total is
/// what forces a third forge to be classified everywhere it matters — most
/// sharply in credential handling, where a wildcard would send a mutation
/// unauthenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeKind {
    /// GitHub.com or a GitHub Enterprise Server instance.
    GitHub,
    /// GitLab.com or a self-managed GitLab instance.
    GitLab,
}

impl ForgeKind {
    /// The forge a canonical host belongs to, or `None` for anything else.
    ///
    /// Only the two hosts whose forge is a fact are recognised. A self-hosted
    /// instance is deliberately **not** guessed: no unauthenticated probe
    /// distinguishes the forges reliably, hostnames carry no convention
    /// (`git.example.com` is equally likely to be either), and guessing wrong
    /// sends the announce credential to the wrong API in the wrong header. Every
    /// surveyed tool that supports both forges makes the operator declare the
    /// kind for a self-hosted host, and so does this.
    #[must_use]
    pub fn from_host(host: Option<&str>) -> Option<Self> {
        match host {
            // No host at all means the default index, which is on GitHub.
            None => Some(Self::GitHub),
            Some(host) if host.eq_ignore_ascii_case("github.com") => Some(Self::GitHub),
            Some(host) if host.eq_ignore_ascii_case("gitlab.com") => Some(Self::GitLab),
            Some(_) => None,
        }
    }

    /// Resolve the forge for `coordinate`, honouring an explicit `declared` kind.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ForgeKindUnknown`] when the host is self-hosted and
    /// nothing was declared.
    pub fn resolve(declared: Option<Self>, coordinate: &RepoCoordinate) -> Result<Self, ForgeError> {
        if let Some(kind) = declared {
            return Ok(kind);
        }
        Self::from_host(coordinate.host.as_deref()).ok_or_else(|| ForgeError::ForgeKindUnknown {
            host: coordinate.host.clone().unwrap_or_default(),
        })
    }

    /// The host this forge lives on when a coordinate names none.
    ///
    /// A coordinate's `host` is `None` for the canonical host, so `None` and
    /// `Some("github.com")` are two spellings of one instance. Anything
    /// comparing two coordinates' hosts must resolve both through here first, or
    /// it decides that `ocx-sh/index` and `github.com/ocx-sh/index` are on
    /// different servers.
    #[must_use]
    pub fn canonical_host(self) -> &'static str {
        match self {
            Self::GitHub => "github.com",
            Self::GitLab => "gitlab.com",
        }
    }

    /// Whether two coordinates name the same instance of this forge.
    ///
    /// Case-folded, and `None` resolved to [`Self::canonical_host`] on both
    /// sides — the same two normalisations [`Self::from_host`] and the API
    /// base-URL builders already apply, so this cannot disagree with where the
    /// requests actually go.
    #[must_use]
    pub fn same_host(self, left: &RepoCoordinate, right: &RepoCoordinate) -> bool {
        let resolve = |coordinate: &RepoCoordinate| {
            coordinate
                .host
                .clone()
                .unwrap_or_else(|| self.canonical_host().to_string())
        };
        resolve(left).eq_ignore_ascii_case(&resolve(right))
    }

    /// Refuse a coordinate this forge cannot express, before a request is made.
    ///
    /// The per-operation clients already refuse a nested namespace where they
    /// use it — but only for the *fork*, because that is the coordinate whose
    /// namespace they interpolate. A nested `--index-repo` on GitHub reached the
    /// wire and came back as a bare 404 reading "no such repository", which is
    /// the misdiagnosis the flatness rule exists to prevent. This is the check
    /// applied to every coordinate a run names, at the point they are all known.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::NestedNamespaceUnsupported`] when `coordinate` has a
    /// nested namespace and this forge does not nest.
    pub fn validate_coordinate(self, coordinate: &RepoCoordinate) -> Result<(), ForgeError> {
        match self {
            Self::GitHub => super::github::require_flat_namespace(coordinate).map(|_| ()),
            Self::GitLab => Ok(()),
        }
    }

    /// Build the client for this forge against `coordinate`'s host.
    ///
    /// # Errors
    ///
    /// Returns [`ForgeError::ClientBuild`] when the hardened HTTP client cannot
    /// be constructed.
    pub fn client(self, token: ForgeToken, coordinate: &RepoCoordinate) -> Result<Box<dyn super::Forge>, ForgeError> {
        let host = coordinate.host.as_deref();
        Ok(match self {
            Self::GitHub => Box::new(GitHubForge::new(token, host)?),
            Self::GitLab => Box::new(GitLabForge::new(token, host)?),
        })
    }
}

impl std::fmt::Display for ForgeKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
        })
    }
}

impl clap_builder::ValueEnum for ForgeKind {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::GitHub, Self::GitLab]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        Some(match self {
            Self::GitHub => clap_builder::builder::PossibleValue::new("github"),
            Self::GitLab => clap_builder::builder::PossibleValue::new("gitlab"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coordinate(value: &str) -> RepoCoordinate {
        value.parse().expect("valid coordinate")
    }

    #[test]
    fn canonical_hosts_resolve_without_a_declaration() {
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("ocx-sh/index")).expect("no host is the default index"),
            ForgeKind::GitHub
        );
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("github.com/ocx-sh/index")).expect("canonical GitHub"),
            ForgeKind::GitHub
        );
        assert_eq!(
            ForgeKind::resolve(None, &coordinate("gitlab.com/acme/index")).expect("canonical GitLab"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn a_self_hosted_host_is_refused_rather_than_guessed() {
        let error = ForgeKind::resolve(None, &coordinate("git.example.com/acme/index"))
            .expect_err("a self-hosted host must not be guessed");
        assert!(matches!(error, ForgeError::ForgeKindUnknown { .. }), "got {error:?}");
        // ...and is accepted the moment the operator says which forge it is.
        assert_eq!(
            ForgeKind::resolve(Some(ForgeKind::GitLab), &coordinate("git.example.com/acme/team/index"))
                .expect("declared kind wins"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn a_declaration_overrides_even_a_canonical_host() {
        // Not a hypothetical: an instance can be reverse-proxied under a name
        // that looks canonical, and the operator's word beats the heuristic.
        assert_eq!(
            ForgeKind::resolve(Some(ForgeKind::GitLab), &coordinate("github.com/acme/index"))
                .expect("declaration wins"),
            ForgeKind::GitLab
        );
    }

    #[test]
    fn an_omitted_host_is_the_canonical_host_not_a_different_one() {
        // The regression this pins: `--index-repo ocx-sh/index --fork
        // github.com/me/index` names ONE instance twice. Comparing the two
        // `Option<String>` hosts directly makes them differ, and the run is
        // refused with a message that contradicts itself.
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("ocx-sh/index"), &coordinate("github.com/me/index")),
            "an omitted host must resolve to the forge's canonical host"
        );
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("github.com/me/index"), &coordinate("ocx-sh/index")),
            "and symmetrically"
        );
        assert!(
            ForgeKind::GitLab.same_host(&coordinate("acme/index"), &coordinate("gitlab.com/me/index")),
            "the canonical host is per forge, not a single constant"
        );
        assert!(
            ForgeKind::GitHub.same_host(&coordinate("GitHub.COM/a/b"), &coordinate("github.com/c/d")),
            "hosts are compared case-insensitively, as DNS and every URL builder here do"
        );
        // The falsifying half: genuinely different instances still differ, and a
        // canonical host is not the same as a self-hosted one.
        assert!(
            !ForgeKind::GitHub.same_host(&coordinate("github.example.com/a/b"), &coordinate("ocx-sh/index")),
            "a self-hosted host must not collapse onto the canonical one"
        );
        assert!(
            !ForgeKind::GitLab.same_host(&coordinate("a.example.com/x/y"), &coordinate("b.example.com/x/y")),
            "two self-hosted instances must stay distinct"
        );
    }
}
