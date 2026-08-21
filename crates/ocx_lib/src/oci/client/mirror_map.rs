// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-host registry mirror lookup applied on the OCI client read path.
//!
//! [`MirrorMap`] is the parsed, client-side form of the `[mirrors."<host>"]`
//! config table. It takes an **already-parsed** map of
//! ([`String`] upstream host → [`ParsedMirror`]) — parsing and validation
//! happen in the config layer ([`crate::config::mirror`]), so this module never
//! imports [`crate::config`], mirroring the `plain_http_registries(Vec<String>)`
//! precedent on [`super::ClientBuilder`].

use std::collections::HashMap;

use crate::config::mirror::ParsedMirror;

/// Resolves an upstream registry host to its configured mirror, rewriting the
/// transport host and repository path on the OCI client read path only.
///
/// Construction takes a parsed map; an empty map is the identity (no host is
/// mirrored). The default value is the empty map.
#[derive(Debug, Default, Clone)]
pub struct MirrorMap {
    entries: HashMap<String, ParsedMirror>,
}

impl MirrorMap {
    /// Builds a mirror map from already-parsed entries.
    ///
    /// Each entry is `(upstream host, parsed mirror endpoint)`. Parsing and the
    /// `url`-required validation happen in the config layer before this point,
    /// so this constructor is infallible.
    pub fn new(entries: impl IntoIterator<Item = (String, ParsedMirror)>) -> MirrorMap {
        MirrorMap {
            entries: entries.into_iter().collect(),
        }
    }

    /// Returns `true` when no host is mirrored (identity map).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up the parsed mirror for an upstream host, if one is configured.
    #[must_use]
    pub fn get(&self, registry: &str) -> Option<&ParsedMirror> {
        self.entries.get(registry)
    }

    /// Rewrites `(registry, repository)` to the mirror's
    /// `(host, <path-prefix>/<repository>)` when `registry` has a configured
    /// mirror; returns `None` (identity — no mirror for this host) otherwise.
    ///
    /// The repository is appended verbatim after the path prefix — OCX does no
    /// `library/` short-name expansion. A host-only mirror (empty prefix)
    /// yields the repository unchanged.
    ///
    /// An **empty** `repository` (the registry-scoped catalog case, fed by
    /// [`Client::transport_registry`](super::Client)) yields the path prefix
    /// alone, with no trailing slash. The naive `format!("{prefix}/{repository}")`
    /// join would otherwise produce `"<prefix>/"`, which the OCI distribution
    /// auth flow stamps into the token scope as `repository:<prefix>/:pull` — a
    /// malformed scope that can break catalog auth against a mirror keying
    /// tokens off the repo-key path segment.
    #[must_use]
    pub fn rewrite_repository(&self, registry: &str, repository: &str) -> Option<(String, String)> {
        let mirror = self.entries.get(registry)?;
        let rewritten = match (mirror.path_prefix.is_empty(), repository.is_empty()) {
            (true, _) => repository.to_string(),
            (false, true) => mirror.path_prefix.clone(),
            (false, false) => format!("{}/{}", mirror.path_prefix, repository),
        };
        Some((mirror.host.clone(), rewritten))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror(host: &str, path_prefix: &str) -> ParsedMirror {
        ParsedMirror {
            protocol: "https".to_string(),
            host: host.to_string(),
            path_prefix: path_prefix.to_string(),
        }
    }

    // ── Step 3.1 specification tests ─────────────────────────────────────────

    /// `rewrite_repository` returns `None` for a host with no configured mirror
    /// (identity — leave the caller to use the canonical reference).
    ///
    /// Traces: plan Testing Strategy — "miss → None (identity)".
    #[test]
    fn rewrite_repository_returns_none_on_miss() {
        let map = MirrorMap::new([("ghcr.io".to_string(), mirror("company.jfrog.io", "ghcr-remote"))]);
        // quay.io has no mirror entry — must return None.
        let result = map.rewrite_repository("quay.io", "owner/tool");
        assert!(
            result.is_none(),
            "a host with no mirror entry must return None (identity), got {result:?}"
        );
    }

    /// `rewrite_repository("ghcr.io", "owner/tool")` → `(mirror_host, "ghcr-remote/owner/tool")`.
    ///
    /// Traces: plan Testing Strategy — "rewrite_repository join
    /// (ghcr.io, owner/tool → mirror host, <prefix>/owner/tool)".
    #[test]
    fn rewrite_repository_joins_prefix_and_repository() {
        let map = MirrorMap::new([("ghcr.io".to_string(), mirror("company.jfrog.io", "ghcr-remote"))]);
        let (host, repo) = map
            .rewrite_repository("ghcr.io", "owner/tool")
            .expect("ghcr.io must have a mirror entry");
        assert_eq!(host, "company.jfrog.io");
        assert_eq!(repo, "ghcr-remote/owner/tool");
    }

    /// Repository is appended verbatim, including `library/` segment — OCX
    /// does no Docker-style short-name expansion.
    ///
    /// Traces: plan Testing Strategy — "repository appended verbatim incl. a
    /// `library/` segment (docker.io, library/nginx → <prefix>/library/nginx,
    /// no munging)"; ADR R2.
    #[test]
    fn rewrite_repository_library_segment_appended_verbatim() {
        let map = MirrorMap::new([("docker.io".to_string(), mirror("corp.jfrog.io", "dockerhub-remote"))]);
        let (host, repo) = map
            .rewrite_repository("docker.io", "library/nginx")
            .expect("docker.io must have a mirror entry");
        assert_eq!(host, "corp.jfrog.io");
        assert_eq!(
            repo, "dockerhub-remote/library/nginx",
            "library/ prefix must be preserved verbatim, not stripped"
        );
    }

    /// Host-only mirror (empty prefix) → repository unchanged.
    ///
    /// Traces: plan Testing Strategy — "host-only mirror (empty prefix)";
    /// ADR degenerate case.
    #[test]
    fn rewrite_repository_empty_prefix_yields_repository_unchanged() {
        let map = MirrorMap::new([("ghcr.io".to_string(), mirror("ghcr-mirror.corp", ""))]);
        let (host, repo) = map
            .rewrite_repository("ghcr.io", "owner/tool")
            .expect("ghcr.io must have a mirror entry");
        assert_eq!(host, "ghcr-mirror.corp");
        assert_eq!(repo, "owner/tool", "empty prefix must yield repository unchanged");
    }

    /// An empty repository (the registry-scoped catalog case) yields the path
    /// prefix verbatim — NOT `"<prefix>/"` with a trailing slash. The trailing
    /// slash would corrupt the OCI auth token scope
    /// (`repository:<prefix>/:pull`).
    ///
    /// Traces: finding #5 — `transport_registry` catalog auth-scope fix.
    #[test]
    fn rewrite_repository_empty_repository_yields_prefix_without_trailing_slash() {
        let map = MirrorMap::new([("ghcr.io".to_string(), mirror("company.jfrog.io", "ghcr-catalog"))]);
        let (host, repo) = map
            .rewrite_repository("ghcr.io", "")
            .expect("ghcr.io must have a mirror entry");
        assert_eq!(host, "company.jfrog.io");
        assert_eq!(
            repo, "ghcr-catalog",
            "empty repository must yield the path prefix verbatim, never <prefix>/ with a trailing slash"
        );
    }

    /// An empty repository against a host-only mirror (empty prefix) yields an
    /// empty repository — no spurious slash.
    ///
    /// Traces: finding #5 — host-only catalog case.
    #[test]
    fn rewrite_repository_empty_repository_host_only_yields_empty() {
        let map = MirrorMap::new([("ghcr.io".to_string(), mirror("ghcr-mirror.corp", ""))]);
        let (host, repo) = map
            .rewrite_repository("ghcr.io", "")
            .expect("ghcr.io must have a mirror entry");
        assert_eq!(host, "ghcr-mirror.corp");
        assert_eq!(repo, "", "host-only mirror with empty repository must stay empty");
    }

    /// Empty map has no entries — `is_empty()` returns true; `rewrite_repository`
    /// returns `None` for any input.
    ///
    /// Traces: plan Testing Strategy — identity map is the default.
    #[test]
    fn empty_map_is_identity() {
        let map = MirrorMap::default();
        assert!(map.is_empty(), "default MirrorMap must be empty");
        assert!(
            map.rewrite_repository("ghcr.io", "owner/tool").is_none(),
            "empty map must return None for any registry"
        );
    }

    /// T-A2: multi-host `MirrorMap` — each upstream host rewrites to ITS OWN
    /// mirror endpoint with no cross-contamination, and an unconfigured host
    /// returns `None`.
    ///
    /// Builds a 3-entry map (`ghcr.io`, `docker.io`, `quay.io`) and asserts:
    /// - `ghcr.io` rewrites exclusively to the `ghcr.io` mirror host/prefix.
    /// - `docker.io` rewrites exclusively to the `docker.io` mirror host/prefix.
    /// - `registry.k8s.io` (not configured) returns `None`.
    #[test]
    fn multi_host_map_routes_each_registry_independently() {
        let map = MirrorMap::new([
            ("ghcr.io".to_string(), mirror("ghcr-mirror.corp", "ghcr-remote")),
            ("docker.io".to_string(), mirror("docker-mirror.corp", "dockerhub-proxy")),
            ("quay.io".to_string(), mirror("quay-mirror.corp", "quay-proxy")),
        ]);

        assert!(!map.is_empty(), "map must not report itself empty with 3 entries");

        // ghcr.io → its own mirror, not docker.io's or quay.io's.
        let (ghcr_host, ghcr_repo) = map
            .rewrite_repository("ghcr.io", "owner/tool")
            .expect("ghcr.io must have a mirror entry");
        assert_eq!(
            ghcr_host, "ghcr-mirror.corp",
            "ghcr.io must rewrite to its own mirror host, not docker.io's or quay.io's"
        );
        assert_eq!(
            ghcr_repo, "ghcr-remote/owner/tool",
            "ghcr.io repository must be prefixed with ghcr-remote"
        );

        // docker.io → its own mirror, not ghcr.io's or quay.io's.
        let (docker_host, docker_repo) = map
            .rewrite_repository("docker.io", "library/nginx")
            .expect("docker.io must have a mirror entry");
        assert_eq!(
            docker_host, "docker-mirror.corp",
            "docker.io must rewrite to its own mirror host, not ghcr.io's or quay.io's"
        );
        assert_eq!(
            docker_repo, "dockerhub-proxy/library/nginx",
            "docker.io repository must be prefixed with dockerhub-proxy"
        );

        // registry.k8s.io is not configured → None (identity).
        let result = map.rewrite_repository("registry.k8s.io", "pause");
        assert!(
            result.is_none(),
            "unconfigured host registry.k8s.io must return None (identity), got {result:?}"
        );
    }
}
