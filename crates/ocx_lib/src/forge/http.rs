// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The hardened HTTP client both forge clients are built on.
//!
//! One copy on purpose. The redirect policy here is the guard that stops the
//! announce credential from being replayed at another host, and a security
//! control that exists twice is a security control that can drift: the two
//! clients were byte-identical until someone edited one of them. Anything that
//! genuinely differs per forge — which header carries the credential, which
//! status codes may be replayed — stays in that forge's own module.

use std::time::Duration;

use super::ForgeError;

/// Client user-agent, shared so a forge cannot be identified by a stale one.
const USER_AGENT_VALUE: &str = concat!("ocx/", env!("CARGO_PKG_VERSION"));

/// Build the no-redirect, embedded-roots HTTP client the forge clients use.
///
/// Redirects are disabled because reqwest otherwise replays the credential
/// header on a cross-host 3xx `Location`, exfiltrating the token — the same
/// hazard for GitHub's `Authorization` and GitLab's `PRIVATE-TOKEN`. These REST
/// endpoints never legitimately redirect; a non-2xx surfaces as an error, never
/// chased. Embedded Mozilla roots are seeded so TLS works with no system trust
/// store (minimal CI runner), mirroring the index HTTP client's hardening
/// (`oci/index/ocx_index.rs`).
///
/// # Errors
///
/// Returns [`ForgeError::ClientBuild`] when reqwest cannot build the client.
pub fn build_forge_http_client(timeout: Duration) -> Result<reqwest::Client, ForgeError> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT_VALUE);
    crate::utility::tls::seed_embedded_roots(builder)
        .build()
        .map_err(|source| ForgeError::ClientBuild { source })
}

#[cfg(test)]
mod tests {
    /// The redirect policy is the one line in this file that is a security
    /// control rather than a convenience, and `reqwest::Client` exposes no way
    /// to read its policy back — so this is a structural guard over the source.
    ///
    /// Two exclusions, and both were earned rather than anticipated. Comments are
    /// stripped so the rationale above cannot satisfy the needle. **And the test
    /// module is cut off before scanning**, because the needle is a string
    /// literal in the assertion below — which is code, not a comment. Without the
    /// split, the guard matched itself: a mutation to `Policy::limited(3)` was
    /// applied to the function above and the test still passed. The split was
    /// added after seeing that green, and the mutation now reds.
    #[test]
    fn the_forge_client_disables_redirects() {
        let source = include_str!("http.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first part");
        let code: String = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // The needle must be present...
        assert!(
            code.contains("redirect(reqwest::redirect::Policy::none())"),
            "the forge HTTP client must refuse redirects so the credential cannot be replayed at another host"
        );
        // ...and no OTHER redirect policy may be configured beside it, or a
        // second `.redirect(...)` call would override it while the assertion
        // above still passed.
        assert_eq!(
            code.matches(".redirect(").count(),
            1,
            "exactly one redirect policy may be configured on the forge client"
        );
    }
}
