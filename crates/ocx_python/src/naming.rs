// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Conventional wheel repo naming, `<scope>/<index-host>/<package>:<sha256>`: a one-way door, since published
//! names cannot move.

use crate::select::WheelRef;

/// Index-host segment for a URL-less `WheelRef`, which `select` already rejects; keeps `wheel_reference` infallible.
const NO_URL_INDEX_HOST: &str = "unknown-index-host";

/// The default wheel scope when the maintainer configures none.
pub const DEFAULT_WHEEL_SCOPE: &str = "pip-packages";

/// The maintainer-configured scope prefix for mirrored wheel repos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelScope(String);

impl WheelScope {
    /// Wraps a maintainer-configured scope string.
    pub fn new(scope: impl Into<String>) -> Self {
        Self(scope.into())
    }

    /// The scope as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for WheelScope {
    fn default() -> Self {
        Self(DEFAULT_WHEEL_SCOPE.to_string())
    }
}

/// A rendered, repo-relative wheel reference; [`Display`](std::fmt::Display) renders `<repository>:<tag>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelReference {
    /// The repo-relative repository path (`<scope>/<index-host>/<package>`), no registry host.
    pub repository: String,
    /// The tag: the wheel `sha256` (hex, no `sha256:` prefix).
    pub tag: String,
}

impl std::fmt::Display for WheelReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.repository, self.tag)
    }
}

/// Renders the conventional repo-relative [`WheelReference`] for a wheel.
pub fn wheel_reference(scope: &WheelScope, wheel: &WheelRef) -> WheelReference {
    let index_host = wheel.url.as_deref().and_then(extract_host).unwrap_or(NO_URL_INDEX_HOST);
    // Only `[a-z0-9-]` survives, or a hostile lock smuggles `:`/`@`/`%` into the OCI reference.
    let package: String = normalize_package_name(&wheel.name)
        .chars()
        .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '-')
        .collect();
    // Edge hyphens survive the filter but break the OCI path-component grammar.
    let package = package.trim_matches('-').to_string();
    let package = if package.is_empty() {
        "invalid-package-name".to_string()
    } else {
        package
    };
    WheelReference {
        repository: format!("{}/{index_host}/{package}", scope.as_str()),
        tag: wheel.sha256.clone(),
    }
}

/// Extracts the host from a URL, stripping scheme, userinfo, port, and path.
fn extract_host(url: &str) -> Option<&str> {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority_end = after_scheme.find(['/', '?', '#']).unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or(host);
    // `.`/`..` would become a path-traversal segment in the repository path.
    (!host.is_empty() && host != "." && host != "..").then_some(host)
}

/// PEP 503 normalization, equivalent to `re.sub(r"[-_.]+", "-", name).lower()`.
pub fn normalize_package_name(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut last_was_separator = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !last_was_separator {
                normalized.push('-');
                last_was_separator = true;
            }
        } else {
            normalized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel_ref(name: &str, filename: &str, url: Option<&str>, sha256: &str) -> WheelRef {
        WheelRef {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            filename: filename.to_string(),
            url: url.map(str::to_string),
            sha256: sha256.to_string(),
        }
    }

    #[test]
    fn strips_non_repository_chars_from_a_hostile_package_name() {
        // A hostile lock's package name must not smuggle OCI reference
        // grammar (`:`/`@`/`%`) into the repository path; the guard drops
        // everything outside `[a-z0-9-]` post-normalization, and an
        // all-invalid name falls back to the sentinel segment.
        let scope = WheelScope::default();
        let wheel = wheel_ref(
            "evil:name@sha",
            "x-1.0.0-py3-none-any.whl",
            Some("https://pypi.org/x"),
            "aa",
        );
        let reference = wheel_reference(&scope, &wheel);
        assert_eq!(reference.repository, "pip-packages/pypi.org/evilnamesha");

        let wheel = wheel_ref("@@@", "x-1.0.0-py3-none-any.whl", Some("https://pypi.org/x"), "aa");
        let reference = wheel_reference(&scope, &wheel);
        assert_eq!(reference.repository, "pip-packages/pypi.org/invalid-package-name");

        let wheel = wheel_ref("-evil-", "x-1.0.0-py3-none-any.whl", Some("https://pypi.org/x"), "aa");
        let reference = wheel_reference(&scope, &wheel);
        assert_eq!(
            reference.repository, "pip-packages/pypi.org/evil",
            "edge hyphens trimmed"
        );
    }

    #[test]
    fn normalizes_package_names_per_pep_503() {
        assert_eq!(normalize_package_name("Flask_Cors"), "flask-cors");
        assert_eq!(normalize_package_name("foo..bar"), "foo-bar");
        assert_eq!(normalize_package_name("foo---bar"), "foo-bar");
        assert_eq!(normalize_package_name("A.B_C-D"), "a-b-c-d");
    }

    #[test]
    fn renders_full_reference_for_pythonhosted_wheel() {
        let wheel = wheel_ref(
            "Flask-Cors",
            "flask_cors-4.0.0-py2.py3-none-any.whl",
            Some("https://files.pythonhosted.org/packages/aa/bb/flask_cors-4.0.0-py2.py3-none-any.whl"),
            "deadbeef",
        );

        let reference = wheel_reference(&WheelScope::default(), &wheel);

        assert_eq!(reference.repository, "pip-packages/files.pythonhosted.org/flask-cors");
        assert_eq!(reference.tag, "deadbeef");
        assert_eq!(
            reference.to_string(),
            "pip-packages/files.pythonhosted.org/flask-cors:deadbeef"
        );
    }

    #[test]
    fn differing_wheels_share_one_repo_distinguished_by_content_tag() {
        // Two wheels of the same package differing by ABI/platform get the same
        // repository (no slug) and are told apart purely by their sha256 tag.
        let manylinux = wheel_ref(
            "numpy",
            "numpy-1.26.2-cp311-cp311-manylinux_2_17_x86_64.whl",
            Some("https://files.pythonhosted.org/packages/aa/numpy-manylinux.whl"),
            "1111",
        );
        let musllinux = wheel_ref(
            "numpy",
            "numpy-1.26.2-cp311-cp311-musllinux_1_2_x86_64.whl",
            Some("https://files.pythonhosted.org/packages/bb/numpy-musllinux.whl"),
            "2222",
        );

        let a = wheel_reference(&WheelScope::default(), &manylinux);
        let b = wheel_reference(&WheelScope::default(), &musllinux);

        assert_eq!(a.repository, b.repository, "same package → same repo");
        assert_eq!(a.repository, "pip-packages/files.pythonhosted.org/numpy");
        assert_ne!(a.tag, b.tag, "distinct content → distinct tag");
    }

    #[test]
    fn scope_defaults_to_pip_packages_and_can_be_overridden() {
        let wheel = wheel_ref(
            "foo",
            "foo-1.2.3-py3-none-any.whl",
            Some("https://example.com/foo.whl"),
            "cafebabe",
        );

        let default_reference = wheel_reference(&WheelScope::default(), &wheel);
        assert!(default_reference.repository.starts_with("pip-packages/"));

        let custom_reference = wheel_reference(&WheelScope::new("acme-wheels"), &wheel);
        assert!(custom_reference.repository.starts_with("acme-wheels/"));
    }

    #[test]
    fn missing_url_falls_back_to_documented_host() {
        let wheel = wheel_ref("foo", "foo-1.2.3-py3-none-any.whl", None, "cafebabe");

        let reference = wheel_reference(&WheelScope::default(), &wheel);

        assert_eq!(reference.repository, format!("pip-packages/{NO_URL_INDEX_HOST}/foo"));
    }

    #[test]
    fn dot_dot_host_falls_back_to_documented_host_not_path_traversal() {
        let wheel = wheel_ref(
            "foo",
            "foo-1.2.3-py3-none-any.whl",
            Some("https://../evil/foo.whl"),
            "cafebabe",
        );

        let reference = wheel_reference(&WheelScope::default(), &wheel);

        assert!(
            !reference.repository.contains(".."),
            "rendered repository must not contain a path-traversal segment: {}",
            reference.repository
        );
        assert_eq!(reference.repository, format!("pip-packages/{NO_URL_INDEX_HOST}/foo"));
    }
}
