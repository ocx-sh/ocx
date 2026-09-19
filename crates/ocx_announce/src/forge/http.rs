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
/// (`oci/index/ocx_index.rs`). `extra_roots` chains operator-supplied CA roots
/// (ocx#448, C-007) on top — `ForgeKind::client` resolves the CLI's merged
/// view and hands it to the forge's `new`, so a configured host builds its
/// client once.
///
/// # Errors
///
/// Returns [`ForgeError::ClientBuild`] when reqwest cannot build the client.
pub fn build_forge_http_client(
    timeout: Duration,
    extra_roots: &ocx_util::tls::ExtraRoots,
) -> Result<reqwest::Client, ForgeError> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(USER_AGENT_VALUE);
    extra_roots
        .seed(ocx_util::tls::seed_embedded_roots(builder))
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

    /// C-007 / DX-5 / S-002 (unit tier): `build_forge_http_client` with a
    /// non-empty set succeeds, and the client it returns trusts that set — a
    /// 200 from an in-process server signed by a minted root; the same build
    /// with the default (empty) set ends in `UnknownIssuer`. Dialed by IP so
    /// no resolver is involved.
    #[tokio::test]
    async fn extra_ca_forge_client_builds_with_and_trusts_a_non_empty_root_set() {
        use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, serve_https};
        use ocx_util::tls::ExtraRoots;

        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let url = format!("https://{addr}/repos/ocx-sh/ocx/releases");

        let seeded = super::build_forge_http_client(
            std::time::Duration::from_secs(5),
            &ocx_util::tls::ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
        )
        .expect("a non-empty root set builds");
        let response = seeded
            .get(&url)
            .send()
            .await
            .expect("the seeded forge client trusts the minted root");
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let unseeded = super::build_forge_http_client(std::time::Duration::from_secs(5), &ExtraRoots::default())
            .expect("the default set builds");
        let error = unseeded
            .get(&url)
            .send()
            .await
            .expect_err("without the root the forge client must refuse the handshake");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);
    }
}
