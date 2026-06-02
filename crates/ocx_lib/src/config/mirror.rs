// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-upstream-host registry mirror configuration.
//!
//! Home for all `[mirrors."<host>"]` settings. Each entry maps a canonical
//! upstream registry host (e.g. `"ghcr.io"`) to a replacement endpoint — a
//! corporate artifact-manager remote/proxy repository — so OCX routes read
//! traffic to the mirror instead of the firewall-blocked origin. The canonical
//! identifier, content-addressed digest, and every on-disk path stay keyed to
//! the upstream host; only the transport reference is rewritten.

use serde::Deserialize;

/// Configuration for a single `[mirrors."<host>"]` entry.
///
/// Re-exported from [`crate::config`] as `MirrorConfig`. `deny_unknown_fields`
/// is enforced so typos inside a known section fail fast.
///
/// `url` is kept `Option<String>` so [`Default`] and tier merge work, but an
/// absent or empty `url` is rejected when the mirror map is built — a no-op
/// mirror is a silent egress to a blocked host, the exact failure replace
/// semantics forbid.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MirrorConfig {
    /// `scheme://host[/repo-key-path-prefix]` for the mirror endpoint. This is
    /// the Docker/OCI **pull** path (`<host>/<repo-key>`), not the
    /// `/artifactory/api/docker/<repo-key>` admin REST path. OCX appends the
    /// upstream repository path verbatim after the prefix.
    ///
    /// Nexus Repository 3.83+ also uses path-based routing without the legacy
    /// `/repository/` prefix — the repo-key alone, the same shape as Artifactory.
    pub url: Option<String>,
}

/// A parsed mirror endpoint: scheme split out as `protocol`, the host, and the
/// optional repository-key path prefix.
///
/// Produced by [`MirrorConfig::parse_url`] in the config layer so the OCI
/// client receives an already-parsed value and never imports [`crate::config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMirror {
    /// URL scheme without the `://` separator, e.g. `"https"` or `"http"`.
    pub protocol: String,
    /// Mirror host (and optional port), e.g. `"company.jfrog.io"`.
    pub host: String,
    /// Repository-key path prefix, verbatim, with no leading or trailing `/`.
    /// Empty for a host-only (subdomain-method) mirror.
    pub path_prefix: String,
}

/// Error raised while parsing a [`MirrorConfig`] `url` into a [`ParsedMirror`],
/// or while resolving the per-host mirror map in [`resolve_mirror_map`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MirrorConfigError {
    /// The `url` field was absent or empty — a no-op mirror that would silently
    /// egress to the blocked upstream host.
    #[error("mirror url is missing or empty")]
    MissingUrl,
    /// The `url` had no host segment after stripping the scheme.
    #[error("mirror url has no host: {url}")]
    MissingHost {
        /// The offending `url` value.
        url: String,
    },
    /// A `[mirrors."<upstream>"]` entry could not be parsed. Carries the
    /// upstream host key for context and the underlying parse error as source.
    #[error("invalid [mirrors.\"{upstream}\"] configuration")]
    InvalidEntry {
        /// The upstream host key whose mirror entry failed to parse.
        upstream: String,
        /// The underlying parse failure.
        #[source]
        source: Box<MirrorConfigError>,
    },
    /// A mirror endpoint uses plain HTTP but its host is not listed in
    /// `OCX_INSECURE_REGISTRIES`. Replace semantics forbid silently downgrading
    /// to HTTP, so this fails loud at resolve time with an actionable hint
    /// (CWE-319) rather than as an opaque TLS error mid-transport.
    #[error(
        "mirror for '{upstream}' uses http:// but mirror host '{mirror_host}' is not in \
         OCX_INSECURE_REGISTRIES; add it to allow plain-HTTP transport"
    )]
    PlainHttpMirrorNotAllowed {
        /// The upstream host key whose mirror uses plain HTTP.
        upstream: String,
        /// The mirror host that would be contacted over plain HTTP.
        mirror_host: String,
    },
    /// The forwarded `OCX_MIRRORS` env value was not valid JSON. A malformed
    /// forwarded map is a hard error: silently degrading to an identity map
    /// would route reads to the firewall-blocked origin instead of the mirror.
    #[error("malformed OCX_MIRRORS env value")]
    MalformedEnvJson {
        /// The underlying JSON parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// A per-host value inside the forwarded `OCX_MIRRORS` JSON object was not a
    /// string. Dropping the entry silently would degrade the map for that host,
    /// so this fails loud and names the offending upstream host.
    #[error("OCX_MIRRORS entry for '{host}' is not a string url")]
    NonStringEnvValue {
        /// The upstream host whose env value was not a string.
        host: String,
    },
}

impl MirrorConfig {
    /// Merge `other` into `self` field-by-field. `other`'s `Some` values
    /// override `self`'s; `other`'s `None` values do not clobber `self`.
    pub fn merge(&mut self, other: MirrorConfig) {
        if other.url.is_some() {
            self.url = other.url;
        }
    }

    /// Parse the `url` into a [`ParsedMirror`].
    ///
    /// Dedicated split: strip the scheme into `protocol`, take the first `/`
    /// boundary as the host/path-prefix split, keep the remainder verbatim as
    /// the path prefix, and trim one trailing `/`. Deliberately does **not**
    /// reuse `auth::registry_url::canonicalize_registry`, whose `/vN` strip and
    /// `docker.io` special-case would corrupt a repo-key prefix or a
    /// `docker.io` mirror host.
    ///
    /// # Errors
    ///
    /// Returns [`MirrorConfigError::MissingUrl`] when `url` is absent or empty,
    /// and [`MirrorConfigError::MissingHost`] when the `url` has no host segment.
    pub fn parse_url(&self) -> Result<ParsedMirror, MirrorConfigError> {
        let url = self.url.as_deref().unwrap_or_default();
        if url.is_empty() {
            return Err(MirrorConfigError::MissingUrl);
        }

        // 1. Strip the scheme into `protocol`. Default to https when no scheme
        //    is present (matches the registry-auth convention without pulling
        //    in `canonicalize_registry`'s /vN and docker.io special-cases).
        let (protocol, rest) = match url.split_once("://") {
            Some((scheme, rest)) => (scheme.to_string(), rest),
            None => ("https".to_string(), url),
        };

        // 2. First `/` boundary splits host from the verbatim path prefix.
        let (host, raw_prefix) = match rest.split_once('/') {
            Some((host, prefix)) => (host, prefix),
            None => (rest, ""),
        };

        if host.is_empty() {
            return Err(MirrorConfigError::MissingHost { url: url.to_string() });
        }

        // 3. Trim exactly one trailing `/` from the prefix (verbatim otherwise).
        let path_prefix = raw_prefix.strip_suffix('/').unwrap_or(raw_prefix);

        Ok(ParsedMirror {
            protocol,
            host: host.to_string(),
            path_prefix: path_prefix.to_string(),
        })
    }
}

/// Output of [`resolve_mirror_map`]: the parsed `(upstream host, ParsedMirror)`
/// entries (for [`MirrorMap::new`](crate::oci::MirrorMap)) and the resolved
/// `(host, url)` pairs (for forwarding via
/// [`OcxConfigView`](crate::env::OcxConfigView)).
pub type ResolvedMirrors = (Vec<(String, ParsedMirror)>, Vec<(String, String)>);

/// Resolves the per-host mirror map from `[mirrors]` config merged with the
/// inherited `OCX_MIRRORS` env, then parses and validates every resolved entry.
///
/// This is the single Config→mirror-map transform shared by the CLI
/// (`Context::try_init`) and the OCI client (`ClientBuilder::from_env*`). It
/// owns the one-way-door precedence rule and the plain-HTTP security gate so
/// they cannot drift between layers:
///
/// - **Per-host merge** — `env_pairs` (the inherited `OCX_MIRRORS`) win per-host
///   key; a host present only in `[mirrors]` config survives. Order is stable
///   (a `BTreeMap`), so forwarding to a child ocx is deterministic.
/// - **Parse + validate** — each merged entry is parsed via
///   [`MirrorConfig::parse_url`]; a missing/empty `url` is a hard error (F9: a
///   no-op mirror silently egresses to the blocked upstream host).
/// - **Plain-HTTP gate (CWE-319)** — an `http://` mirror whose host is not in
///   `insecure_hosts` is a hard error naming `OCX_INSECURE_REGISTRIES`, so the
///   failure surfaces loud at resolve time rather than as an opaque TLS error
///   mid-transport.
///
/// Returns the parsed `(upstream host, ParsedMirror)` entries (for
/// [`MirrorMap::new`](crate::oci::MirrorMap) on the client side) and the
/// resolved `(host, url)` pairs (for forwarding via
/// [`OcxConfigView`](crate::env::OcxConfigView)) so the parent and any forwarded
/// child agree on one resolved map. The mirror map itself is built by the
/// caller in the `oci` layer, keeping `config` free of any `oci` import.
///
/// # Errors
///
/// Returns [`MirrorConfigError::InvalidEntry`] when a configured mirror `url` is
/// missing/empty or has no host, and
/// [`MirrorConfigError::PlainHttpMirrorNotAllowed`] when a mirror uses plain
/// HTTP without its host in `insecure_hosts`.
pub fn resolve_mirror_map(
    config: &crate::config::Config,
    env_pairs: Vec<(String, String)>,
    insecure_hosts: &[String],
) -> Result<ResolvedMirrors, MirrorConfigError> {
    // Start from `[mirrors]` config (host → url), then overlay the env below.
    // Each entry is parsed exactly once — in the shared pass further down — so
    // there is no redundant eager parse. A config entry with an absent `url`
    // is carried as an empty string; the shared pass then surfaces it as
    // `MissingUrl` (wrapped `InvalidEntry`), so a config-only no-op mirror still
    // errors even when the env does not overlay it (F9: a no-op mirror silently
    // egresses to the blocked upstream host).
    let mut merged: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if let Some(mirrors) = &config.mirrors {
        for (upstream, mirror_config) in mirrors {
            merged.insert(upstream.clone(), mirror_config.url.clone().unwrap_or_default());
        }
    }

    // Overlay the inherited `OCX_MIRRORS` env: env wins per-host key; hosts
    // absent from the env keep their config value (one-way-door precedence).
    for (upstream, url) in env_pairs {
        merged.insert(upstream, url);
    }

    // Parse + validate the resolved set once. The parsed entries feed the
    // client-side MirrorMap; the pairs feed the forwarding view.
    let mut entries = Vec::with_capacity(merged.len());
    let mut pairs = Vec::with_capacity(merged.len());
    for (upstream, url) in merged {
        let parsed =
            MirrorConfig { url: Some(url.clone()) }
                .parse_url()
                .map_err(|source| MirrorConfigError::InvalidEntry {
                    upstream: upstream.clone(),
                    source: Box::new(source),
                })?;

        // Plain-HTTP gate: an http:// mirror only composes with the existing
        // plain-HTTP set when the operator lists the mirror host explicitly.
        // The scheme alone never opts out of TLS (ADR F2).
        if parsed.protocol == "http" && !insecure_hosts.iter().any(|h| h == &parsed.host) {
            return Err(MirrorConfigError::PlainHttpMirrorNotAllowed {
                upstream,
                mirror_host: parsed.host,
            });
        }

        entries.push((upstream.clone(), parsed));
        pairs.push((upstream, url));
    }

    Ok((entries, pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_config_merge_none_in_higher_does_not_clobber_lower_url() {
        let mut lower = MirrorConfig {
            url: Some("https://company.jfrog.io/ghcr-remote".to_string()),
        };
        let higher = MirrorConfig { url: None };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("https://company.jfrog.io/ghcr-remote"),
            "None in higher should not clobber lower's Some(url)"
        );
    }

    #[test]
    fn mirror_config_merge_some_in_higher_overrides_lower_url() {
        let mut lower = MirrorConfig {
            url: Some("https://old.example/remote".to_string()),
        };
        let higher = MirrorConfig {
            url: Some("https://new.example/remote".to_string()),
        };
        lower.merge(higher);
        assert_eq!(
            lower.url.as_deref(),
            Some("https://new.example/remote"),
            "Some in higher should override lower's url"
        );
    }

    // ── Step 3.1 specification tests ─────────────────────────────────────────

    /// `[mirrors."ghcr.io"] url = "https://..."` round-trips through TOML.
    ///
    /// Traces: plan Testing Strategy "MirrorConfig" row — TOML de/serialize.
    #[test]
    fn mirror_config_toml_roundtrip() {
        let toml_str = r#"
            [mirrors."ghcr.io"]
            url = "https://company.jfrog.io/ghcr-remote"
        "#;
        #[derive(serde::Deserialize)]
        struct Wrapper {
            mirrors: std::collections::HashMap<String, MirrorConfig>,
        }
        let w: Wrapper = toml::from_str(toml_str).expect("valid TOML must parse");
        let cfg = w.mirrors.get("ghcr.io").expect("ghcr.io key must exist");
        assert_eq!(
            cfg.url.as_deref(),
            Some("https://company.jfrog.io/ghcr-remote"),
            "TOML round-trip must preserve url exactly"
        );
    }

    /// `deny_unknown_fields` — a typo in a known section (e.g. `urll = ...`)
    /// must produce a deserialization error instead of silently ignoring it.
    ///
    /// Traces: plan Testing Strategy — "unknown field → error (deny_unknown_fields)".
    #[test]
    fn mirror_config_unknown_field_rejected() {
        let toml_str = r#"
            [mirrors."ghcr.io"]
            urll = "https://company.jfrog.io/ghcr-remote"
        "#;
        // Deserialize through the real `Config` type so the test pins the
        // production contract: a typo inside `[mirrors."<host>"]` must be
        // rejected by `MirrorConfig`'s `deny_unknown_fields`.
        let result = toml::from_str::<crate::config::Config>(toml_str);
        assert!(
            result.is_err(),
            "unknown field 'urll' must be rejected by deny_unknown_fields, but parse succeeded"
        );
    }

    /// `url = None` (absent in TOML, i.e. empty struct) → `parse_url` returns
    /// `MirrorConfigError::MissingUrl`.
    ///
    /// Traces: plan review F9 — "url=None/empty → parse_url errors"; ADR
    /// "url is required at map-build time" to prevent silent egress.
    #[test]
    fn parse_url_errors_on_none_url() {
        let cfg = MirrorConfig { url: None };
        let result = cfg.parse_url();
        assert!(
            matches!(result, Err(MirrorConfigError::MissingUrl)),
            "url=None must yield MirrorConfigError::MissingUrl, got: {result:?}"
        );
    }

    /// `url = ""` (empty string) → `parse_url` returns
    /// `MirrorConfigError::MissingUrl`.
    ///
    /// Traces: plan review F9 — empty url is the same footgun as absent url.
    #[test]
    fn parse_url_errors_on_empty_url() {
        let cfg = MirrorConfig {
            url: Some(String::new()),
        };
        let result = cfg.parse_url();
        assert!(
            matches!(result, Err(MirrorConfigError::MissingUrl)),
            "url='' must yield MirrorConfigError::MissingUrl, got: {result:?}"
        );
    }

    /// `https://host/a/b/c` → protocol `https`, host `host`, prefix `a/b/c`.
    ///
    /// Traces: plan Testing Strategy — url split `https://h/a/b/c → (https,h,a/b/c)`.
    #[test]
    fn parse_url_splits_scheme_host_and_path_prefix() {
        let cfg = MirrorConfig {
            url: Some("https://host/a/b/c".to_string()),
        };
        let parsed = cfg.parse_url().expect("valid url must parse");
        assert_eq!(parsed.protocol, "https");
        assert_eq!(parsed.host, "host");
        assert_eq!(parsed.path_prefix, "a/b/c");
    }

    /// Host-only url `https://ghcr-remote.jfrog.io` → empty prefix.
    ///
    /// Traces: plan Testing Strategy — "host-only → empty prefix".
    #[test]
    fn parse_url_host_only_yields_empty_prefix() {
        let cfg = MirrorConfig {
            url: Some("https://ghcr-remote.jfrog.io".to_string()),
        };
        let parsed = cfg.parse_url().expect("valid host-only url must parse");
        assert_eq!(parsed.host, "ghcr-remote.jfrog.io");
        assert_eq!(parsed.path_prefix, "", "host-only url must yield empty prefix");
    }

    /// `http://` scheme is preserved verbatim in `protocol`.
    ///
    /// Traces: plan Testing Strategy — "http:// scheme preserved".
    #[test]
    fn parse_url_http_scheme_preserved() {
        let cfg = MirrorConfig {
            url: Some("http://local-mirror.corp/docker-remote".to_string()),
        };
        let parsed = cfg.parse_url().expect("http url must parse");
        assert_eq!(
            parsed.protocol, "http",
            "http scheme must be preserved, not forced to https"
        );
        assert_eq!(parsed.host, "local-mirror.corp");
        assert_eq!(parsed.path_prefix, "docker-remote");
    }

    /// Artifactory repo-key prefix survives byte-for-byte — no `/vN` strip,
    /// no docker.io special-case.
    ///
    /// Traces: plan Testing Strategy — "Artifactory repo-key prefix survives
    /// byte-for-byte (`https://art.corp/ghcr-remote` → prefix `ghcr-remote`,
    /// NOT `/vN`-stripped)"; ADR review A4 — dedicated split, no
    /// `canonicalize_registry`.
    #[test]
    fn parse_url_artifactory_repo_key_survives_verbatim() {
        let cfg = MirrorConfig {
            url: Some("https://art.corp/ghcr-remote".to_string()),
        };
        let parsed = cfg.parse_url().expect("Artifactory url must parse");
        assert_eq!(parsed.host, "art.corp");
        assert_eq!(
            parsed.path_prefix, "ghcr-remote",
            "repo-key prefix must survive verbatim; /vN stripping must NOT apply"
        );
    }

    /// A `docker.io` *mirror host* is NOT special-cased or rewritten.
    ///
    /// Proves that `canonicalize_registry` (which expands `docker.io` →
    /// `index.docker.io`) is NOT in the path — a corporate mirror whose
    /// hostname happens to be `docker.io` must pass through unchanged.
    ///
    /// Traces: plan Testing Strategy — "a `docker.io` *mirror host* is NOT
    /// rewritten (proves canonicalize_registry absent)"; ADR review A4.
    #[test]
    fn parse_url_docker_io_mirror_host_not_special_cased() {
        let cfg = MirrorConfig {
            url: Some("https://docker.io/my-proxy".to_string()),
        };
        let parsed = cfg.parse_url().expect("docker.io mirror url must parse");
        // The host must remain "docker.io", NOT "index.docker.io".
        assert_eq!(
            parsed.host, "docker.io",
            "docker.io mirror host must NOT be rewritten to index.docker.io"
        );
        assert_eq!(parsed.path_prefix, "my-proxy");
    }

    /// Trailing slash on the prefix is trimmed.
    ///
    /// Traces: plan Testing Strategy — "trim one trailing `/`".
    #[test]
    fn parse_url_trailing_slash_trimmed_from_prefix() {
        let cfg = MirrorConfig {
            url: Some("https://mirror.corp/proxy-repo/".to_string()),
        };
        let parsed = cfg.parse_url().expect("url with trailing slash must parse");
        assert_eq!(
            parsed.path_prefix, "proxy-repo",
            "trailing slash must be stripped from path prefix"
        );
    }

    /// T-A1: double trailing slash — `strip_suffix('/')` is single-slash-only, NOT
    /// greedy (`trim_end_matches('/')`). A url like `"https://mirror.corp/proxy-repo//"`
    /// must yield `path_prefix == "proxy-repo/"` (one slash remains), not `"proxy-repo"`.
    ///
    /// This guards against a future regression where `strip_suffix` is swapped for
    /// `trim_end_matches`, which would greedily strip all trailing slashes and corrupt
    /// the bearer-token auth scope when the prefix genuinely ends with `/`.
    #[test]
    fn parse_url_double_trailing_slash_only_strips_one() {
        let cfg = MirrorConfig {
            url: Some("https://mirror.corp/proxy-repo//".to_string()),
        };
        let parsed = cfg.parse_url().expect("url with double trailing slash must parse");
        assert_eq!(
            parsed.host, "mirror.corp",
            "host must be unaffected by double trailing slash"
        );
        assert_eq!(
            parsed.path_prefix, "proxy-repo/",
            "double trailing slash must strip only one slash, leaving proxy-repo/ (not proxy-repo)"
        );
    }

    /// T-A1 (host-only variant): `"https://host/"` — the path after the host is
    /// an empty string `""`, which exercises the `split_once('/') → ("host", "")`
    /// branch. The empty raw_prefix has no trailing slash, so `strip_suffix` is a
    /// no-op, and `path_prefix` must be empty (not `"/"` or any other artifact).
    #[test]
    fn parse_url_host_with_single_trailing_slash_yields_empty_prefix() {
        let cfg = MirrorConfig {
            url: Some("https://host/".to_string()),
        };
        let parsed = cfg.parse_url().expect("host-only url with trailing slash must parse");
        assert_eq!(parsed.host, "host", "host must be extracted correctly");
        assert_eq!(
            parsed.path_prefix, "",
            "a host-only url with a single trailing slash must yield an empty path_prefix"
        );
    }

    /// T-A5: `"https://"` (scheme present, host absent) → `MirrorConfigError::MissingHost`.
    ///
    /// After stripping `"https://"` the remainder is `""`, so
    /// `split_once('/')` returns `None` and `rest` itself is `""`, which fails
    /// the `host.is_empty()` guard. This proves that the guard fires on an
    /// actually-empty host, not just on a `split_once` miss.
    #[test]
    fn parse_url_errors_on_scheme_only_missing_host() {
        let cfg = MirrorConfig {
            url: Some("https://".to_string()),
        };
        let result = cfg.parse_url();
        assert!(
            matches!(result, Err(MirrorConfigError::MissingHost { ref url }) if url == "https://"),
            "url with scheme but no host must yield MirrorConfigError::MissingHost, got: {result:?}"
        );
    }

    /// T-A5 (with prefix): `"https:///prefix"` — host segment between `://` and
    /// the first `/` is empty. Must yield `MirrorConfigError::MissingHost`.
    #[test]
    fn parse_url_errors_on_empty_host_before_prefix() {
        let cfg = MirrorConfig {
            url: Some("https:///prefix".to_string()),
        };
        let result = cfg.parse_url();
        assert!(
            matches!(result, Err(MirrorConfigError::MissingHost { .. })),
            "url with empty host segment before prefix must yield MirrorConfigError::MissingHost, got: {result:?}"
        );
    }

    /// T-A5 (resolve path): `MissingHost` surfaced through `resolve_mirror_map`
    /// must be wrapped in `InvalidEntry` naming the upstream host, parallel to the
    /// existing `resolve_errors_on_missing_url_naming_upstream` test for
    /// `MissingUrl`.
    #[test]
    fn resolve_errors_on_missing_host_naming_upstream() {
        // A url with a scheme but no host: parse_url returns MissingHost, which
        // resolve_mirror_map must wrap in InvalidEntry naming the upstream key.
        let config = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "docker.io".to_string(),
                    MirrorConfig {
                        url: Some("https://".to_string()),
                    },
                );
                m
            }),
            ..Default::default()
        };

        let err = resolve_mirror_map(&config, Vec::new(), &[]).expect_err("a url with no host must error");
        assert!(
            matches!(err, MirrorConfigError::InvalidEntry { ref upstream, .. } if upstream == "docker.io"),
            "expected InvalidEntry naming docker.io as the upstream, got: {err:?}"
        );
        // Source of the InvalidEntry must be MissingHost.
        let message = err.to_string();
        assert!(
            message.contains("docker.io"),
            "error message must name the upstream host; got: {message}"
        );
    }

    /// Tier merge: higher tier (`Some(url)`) replaces the lower tier's entry
    /// for the same host key when merged into `Config`.
    ///
    /// Traces: plan Testing Strategy — "tier merge (higher tier replaces host
    /// key-by-key)".
    #[test]
    fn config_mirrors_tier_merge_higher_wins_per_host_key() {
        use std::collections::HashMap;

        use crate::config::Config;

        let mut lower = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        url: Some("https://old-mirror.corp/ghcr-remote".to_string()),
                    },
                );
                m
            }),
            ..Default::default()
        };

        let higher = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        url: Some("https://new-mirror.corp/ghcr-remote".to_string()),
                    },
                );
                m
            }),
            ..Default::default()
        };

        lower.merge(higher);

        let merged_url = lower
            .mirrors
            .as_ref()
            .and_then(|m| m.get("ghcr.io"))
            .and_then(|c| c.url.as_deref());

        assert_eq!(
            merged_url,
            Some("https://new-mirror.corp/ghcr-remote"),
            "higher-tier url must win over lower-tier url for the same host key"
        );
    }

    // ── resolve_mirror_map: precedence + plain-HTTP gate ─────────────────────

    use std::collections::HashMap;

    use crate::config::Config;

    /// Build a `Config` whose `[mirrors]` table maps each `(host, url)`.
    fn config_with_mirrors(entries: &[(&str, &str)]) -> Config {
        let mut map = HashMap::new();
        for (host, url) in entries {
            map.insert(
                (*host).to_string(),
                MirrorConfig {
                    url: Some((*url).to_string()),
                },
            );
        }
        Config {
            mirrors: Some(map),
            ..Default::default()
        }
    }

    /// `OCX_MIRRORS` (env) wins over `[mirrors]` (config) per-host key, and a
    /// host present in only one source survives. This is the one-way-door
    /// public-contract precedence rule — relocated from `context.rs` to live
    /// next to the resolver that now owns the merge.
    ///
    /// Config: `{ghcr.io→A, docker.io→C}`; env: `{ghcr.io→B}`.
    /// Result: `{ghcr.io→B (env wins), docker.io→C (config survives)}`.
    #[test]
    fn env_mirrors_win_over_config_per_host_key() {
        let config = config_with_mirrors(&[
            ("ghcr.io", "https://config-mirror.corp/ghcr-A"),
            ("docker.io", "https://config-mirror.corp/docker-C"),
        ]);
        let env_pairs = vec![("ghcr.io".to_string(), "https://env-mirror.corp/ghcr-B".to_string())];

        let (_entries, pairs) =
            resolve_mirror_map(&config, env_pairs, &[]).expect("https mirrors must resolve without the insecure gate");

        let pairs: HashMap<String, String> = pairs.into_iter().collect();
        assert_eq!(
            pairs.get("ghcr.io").map(String::as_str),
            Some("https://env-mirror.corp/ghcr-B"),
            "env must win over config for a host present in both"
        );
        assert_eq!(
            pairs.get("docker.io").map(String::as_str),
            Some("https://config-mirror.corp/docker-C"),
            "a host present only in config must survive the env overlay"
        );
        assert_eq!(pairs.len(), 2, "no spurious hosts; only ghcr.io + docker.io");
    }

    /// An `http://` mirror whose host is NOT in `insecure_hosts` fails loud at
    /// resolve time (CWE-319) with an error that NAMES `OCX_INSECURE_REGISTRIES`
    /// and the offending mirror host — not an opaque TLS failure mid-transport.
    ///
    /// Traces: Finding B (security) — fail loud + actionable on plain-HTTP
    /// mirror not in the insecure list.
    #[test]
    fn resolve_errors_on_http_mirror_host_absent_from_insecure_list() {
        let config = config_with_mirrors(&[("ghcr.io", "http://local-mirror.corp:5001/ghcr-remote")]);

        let err = resolve_mirror_map(&config, Vec::new(), &[])
            .expect_err("http mirror without its host in the insecure list must error");

        assert!(
            matches!(
                err,
                MirrorConfigError::PlainHttpMirrorNotAllowed { ref mirror_host, .. }
                    if mirror_host == "local-mirror.corp:5001"
            ),
            "expected PlainHttpMirrorNotAllowed naming the mirror host, got: {err:?}"
        );

        let message = err.to_string();
        assert!(
            message.contains("OCX_INSECURE_REGISTRIES"),
            "error must name OCX_INSECURE_REGISTRIES so the operator knows the fix; got: {message}"
        );
        assert!(
            message.contains("local-mirror.corp:5001"),
            "error must name the offending mirror host; got: {message}"
        );
        assert!(
            message.contains("ghcr.io"),
            "error must name the upstream host the mirror serves; got: {message}"
        );
    }

    /// The same `http://` mirror resolves cleanly once its host IS listed in
    /// `insecure_hosts` — the gate is host-membership, not scheme alone.
    ///
    /// Traces: Finding B — scenario 6a (http mirror WITH host in
    /// OCX_INSECURE_REGISTRIES must still succeed).
    #[test]
    fn resolve_allows_http_mirror_host_in_insecure_list() {
        let config = config_with_mirrors(&[("ghcr.io", "http://local-mirror.corp:5001/ghcr-remote")]);
        let insecure = vec!["local-mirror.corp:5001".to_string()];

        let (entries, _pairs) = resolve_mirror_map(&config, Vec::new(), &insecure)
            .expect("http mirror with host in insecure list resolves");

        assert_eq!(entries.len(), 1, "the single mirror entry must resolve");
        let (upstream, parsed) = &entries[0];
        assert_eq!(upstream, "ghcr.io");
        assert_eq!(parsed.protocol, "http");
        assert_eq!(parsed.host, "local-mirror.corp:5001");
        assert_eq!(parsed.path_prefix, "ghcr-remote");
    }

    /// A missing/empty `url` in a configured entry surfaces as
    /// `InvalidEntry` naming the upstream host, with the `MissingUrl` parse
    /// failure carried as the error source (F9 — no silent egress).
    #[test]
    fn resolve_errors_on_missing_url_naming_upstream() {
        let config = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert("ghcr.io".to_string(), MirrorConfig { url: None });
                m
            }),
            ..Default::default()
        };

        let err = resolve_mirror_map(&config, Vec::new(), &[]).expect_err("a None url must error (F9)");
        assert!(
            matches!(err, MirrorConfigError::InvalidEntry { ref upstream, .. } if upstream == "ghcr.io"),
            "expected InvalidEntry naming ghcr.io, got: {err:?}"
        );
    }

    /// Tier merge across multiple hosts: a host present only in the lower tier
    /// survives, while the higher tier wins for a host present in both.
    ///
    /// Lower: `{ghcr.io, docker.io}`; higher: `{ghcr.io}`. Merged must keep
    /// `docker.io` (lower-only survives) AND take the higher tier's `ghcr.io`.
    ///
    /// Traces: plan Testing Strategy — "tier merge (higher tier replaces host
    /// key-by-key)", lower-tier-only-host-survives case.
    #[test]
    fn config_mirrors_tier_merge_lower_only_host_survives() {
        use std::collections::HashMap;

        use crate::config::Config;

        let mut lower = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        url: Some("https://old-mirror.corp/ghcr-remote".to_string()),
                    },
                );
                m.insert(
                    "docker.io".to_string(),
                    MirrorConfig {
                        url: Some("https://old-mirror.corp/dockerhub-remote".to_string()),
                    },
                );
                m
            }),
            ..Default::default()
        };

        let higher = Config {
            mirrors: Some({
                let mut m = HashMap::new();
                m.insert(
                    "ghcr.io".to_string(),
                    MirrorConfig {
                        url: Some("https://new-mirror.corp/ghcr-remote".to_string()),
                    },
                );
                m
            }),
            ..Default::default()
        };

        lower.merge(higher);

        let mirrors = lower.mirrors.as_ref().expect("merged config must keep mirrors");
        assert_eq!(
            mirrors.get("ghcr.io").and_then(|c| c.url.as_deref()),
            Some("https://new-mirror.corp/ghcr-remote"),
            "higher tier must win for ghcr.io (present in both tiers)"
        );
        assert_eq!(
            mirrors.get("docker.io").and_then(|c| c.url.as_deref()),
            Some("https://old-mirror.corp/dockerhub-remote"),
            "docker.io is only in the lower tier and must survive the merge"
        );
        assert_eq!(mirrors.len(), 2, "merge must keep both host keys, no host dropped");
    }
}
