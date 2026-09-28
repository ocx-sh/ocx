// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Registry URL canonicalization; the credential read and write paths must both use it, or a stored login is
//! never found.

/// Canonicalize a registry argument into the `~/.docker/config.json` key form.
///
/// Matches docker's `normalizeRegistry`, including the `https://index.docker.io/v1/` key for Docker Hub.
pub fn canonicalize_registry(input: &str) -> String {
    let stripped = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);

    let trimmed = strip_trailing_api_version(stripped);

    let trimmed = trimmed.trim_end_matches('/');

    if trimmed == "docker.io" || trimmed == "index.docker.io" {
        return "https://index.docker.io/v1/".to_string();
    }

    trimmed.to_string()
}

/// Strip a trailing `/vN` or `/vN/` segment where N is one or more digits.
fn strip_trailing_api_version(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    if end == 0 {
        return s;
    }
    let mut cursor = end;
    if bytes[cursor - 1] == b'/' {
        cursor -= 1;
    }
    let digits_end = cursor;
    while cursor > 0 && bytes[cursor - 1].is_ascii_digit() {
        cursor -= 1;
    }
    if cursor == digits_end {
        return s;
    }
    if cursor == 0 || bytes[cursor - 1] != b'v' {
        return s;
    }
    cursor -= 1; // consume 'v'
    if cursor == 0 || bytes[cursor - 1] != b'/' {
        return s;
    }
    end = cursor; // up to (but excluding) the leading '/'
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalize_registry_matches_docker_normalization() {
        // Every row anchored to:
        //   - `docker/cli/cli/command/registry/login.go::normalizeRegistry`
        //   - oras-go `ServerAddressFromRegistry` (see research_oras_go_credentials_alignment.md §10)
        let cases = [
            ("ghcr.io", "ghcr.io"),
            ("https://ghcr.io", "ghcr.io"),
            ("https://ghcr.io/", "ghcr.io"),
            ("https://ghcr.io/v1/", "ghcr.io"),
            ("https://ghcr.io/v2/", "ghcr.io"),
            ("http://localhost:5000", "localhost:5000"),
            ("docker.io", "https://index.docker.io/v1/"),
            ("index.docker.io", "https://index.docker.io/v1/"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                canonicalize_registry(input),
                expected,
                "canonicalize_registry({input:?}) should equal {expected:?}",
            );
        }
    }
}
