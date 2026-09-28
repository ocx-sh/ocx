// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Operator-supplied extra CA roots (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`) and the process-wide Sigstore set.

pub mod embedded_roots;

pub use embedded_roots::seed_embedded_roots;

use std::sync::OnceLock;

/// Ceiling on an extra-CA-roots PEM bundle from any source.
///
/// Stays under the 64 KiB config ceiling, or an inline bundle makes `config.toml` itself unloadable, and under
/// the ~32 767-character Windows environment-variable limit `OCX_EXTRA_CA_CERTS` must fit.
pub const MAX_EXTRA_CA_CERTS_BYTES: usize = 32 * 1024;

/// Additional CA roots, appended to the platform and bundled Mozilla sets — never a replacement.
///
/// Only [`ExtraRoots::from_pem`] builds a non-empty value; the default is empty and seeds nothing.
#[derive(Clone, Default)]
pub struct ExtraRoots {
    der: Vec<pki_types::CertificateDer<'static>>,
    /// Built at parse time so [`ExtraRoots::seed`] has nothing left to fail on.
    certificates: Vec<reqwest::Certificate>,
}

impl std::fmt::Debug for ExtraRoots {
    /// Prints a certificate count, never the DER bytes.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtraRoots")
            .field("certificates", &self.der.len())
            .finish()
    }
}

impl PartialEq for ExtraRoots {
    fn eq(&self, other: &Self) -> bool {
        self.der == other.der
    }
}

/// Why [`ExtraRoots::from_pem`] refused a bundle, with no source attached.
///
/// No `Display` or `Error` impl, so it cannot reach an operator without the origin `ocx_config` attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PemBundleError {
    TooLarge {
        bytes: usize,
    },
    NotACertificate {
        tag: String,
    },
    Empty,
    /// `index` is the offending block, or `None` when the decoder or the platform probe refused the whole set.
    Malformed {
        index: Option<usize>,
    },
    /// A `-----BEGIN` line with no end line, at block `index`.
    Truncated {
        index: usize,
    },
}

impl ExtraRoots {
    /// The only constructor from bytes; a final probe client build means no production builder fails on CA input.
    ///
    /// Blocking: the probe loads the platform trust store.
    ///
    /// # Errors
    ///
    /// A [`PemBundleError`] naming which rule the bundle broke.
    pub fn from_pem(pem: &[u8]) -> Result<Self, PemBundleError> {
        use x509_cert::der::Decode as _;

        if pem.len() > MAX_EXTRA_CA_CERTS_BYTES {
            return Err(PemBundleError::TooLarge { bytes: pem.len() });
        }
        let blocks = pem::parse_many(pem).map_err(|_| PemBundleError::Malformed { index: None })?;
        // The decoder silently drops a block with no `-----END`, trusting fewer roots than configured.
        // The needle keeps the decoder's trailing space, so a bare `-----BEGIN` in a comment is not a block.
        let begins = pem
            .windows(b"-----BEGIN ".len())
            .filter(|window| *window == b"-----BEGIN ")
            .count();
        if begins > blocks.len() {
            return Err(PemBundleError::Truncated { index: blocks.len() });
        }
        if let Some(block) = blocks.iter().find(|block| block.tag() != "CERTIFICATE") {
            return Err(PemBundleError::NotACertificate {
                tag: block.tag().to_owned(),
            });
        }
        if blocks.is_empty() {
            return Err(PemBundleError::Empty);
        }
        let mut roots = Self {
            der: Vec::with_capacity(blocks.len()),
            certificates: Vec::with_capacity(blocks.len()),
        };
        for (index, block) in blocks.into_iter().enumerate() {
            let der = block.into_contents();
            let malformed = || PemBundleError::Malformed { index: Some(index) };
            x509_cert::Certificate::from_der(&der).map_err(|_| malformed())?;
            roots
                .certificates
                .push(reqwest::Certificate::from_der(&der).map_err(|_| malformed())?);
            roots.der.push(pki_types::CertificateDer::from(der));
        }
        roots
            .seed(reqwest::Client::builder())
            .build()
            .map_err(|_| PemBundleError::Malformed { index: None })?;
        Ok(roots)
    }

    /// Chains every root onto `builder` additively, so it composes with [`seed_embedded_roots`] in either order.
    pub fn seed(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        if self.is_empty() {
            return builder;
        }
        builder.tls_certs_merge(self.certificates.iter().cloned())
    }

    /// The parsed DER roots, for the registry transport's own certificate list.
    #[must_use]
    pub fn der(&self) -> &[pki_types::CertificateDer<'static>] {
        &self.der
    }

    /// Number of parsed roots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.der.len()
    }

    /// Whether no roots were parsed — the default.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.der.is_empty()
    }
}

/// Process-wide extra CA roots for the Sigstore trust-services client.
static SIGSTORE_ROOTS: OnceLock<ExtraRoots> = OnceLock::new();

/// Install the process-wide extra CA roots for Sigstore calls; a second install is ignored.
///
/// Install before any Sigstore call: `sigstore_http_client` captures the roots once, on first use.
pub fn install_sigstore_roots(roots: ExtraRoots) {
    if let Err(later) = SIGSTORE_ROOTS.set(roots)
        && SIGSTORE_ROOTS.get() != Some(&later)
    {
        // Unreachable with one `Context` per process; traced so a second set does not vanish silently.
        log::debug!(
            "a second install_sigstore_roots with a different set ({} certificates) is ignored",
            later.len()
        );
    }
}

/// The installed extra CA roots, or an empty set; never `get_or_init`, which would pin the empty set before install.
#[must_use]
pub fn sigstore_roots() -> &'static ExtraRoots {
    static EMPTY: ExtraRoots = ExtraRoots {
        der: Vec::new(),
        certificates: Vec::new(),
    };
    SIGSTORE_ROOTS.get().unwrap_or(&EMPTY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::tls::seed_embedded_roots;
    use ocx_test_support::pki::{TestPki, assert_untrusted_root, error_chain, mint_root, pem_block, serve_https};

    fn parse(pem: &[u8]) -> Result<ExtraRoots, PemBundleError> {
        ExtraRoots::from_pem(pem)
    }

    /// D-11 / DX-9: `Debug` on the parsed set prints a count, never DER.
    #[test]
    fn extra_ca_roots_debug_prints_only_the_certificate_count() {
        let (_, first) = mint_root("CN=ocx-test-ca-1");
        let (_, second) = mint_root("CN=ocx-test-ca-2");
        let bundle = format!(
            "{}{}",
            pem_block("CERTIFICATE", &first),
            pem_block("CERTIFICATE", &second)
        );
        let roots = parse(bundle.as_bytes()).expect("two minted roots parse");

        assert_eq!(format!("{roots:?}"), "ExtraRoots { certificates: 2 }");
        assert_eq!(format!("{:?}", ExtraRoots::default()), "ExtraRoots { certificates: 0 }");
    }

    // ── S-002 / S-007 at unit tier: the handshake ───────────────────────────

    /// S-002 / S-007 (unit tier), C-004 `seed`: the cross-OS proof that a
    /// seeded root is consulted at handshake time. One in-process TLS server
    /// presenting a leaf signed by a minted root; the same client recipe with
    /// `.seed()` gets a 200, without it the chain ends in `UnknownIssuer`. Both
    /// outcomes in one function so the green cannot be a client that trusts
    /// everything, nor the red a server that answers nothing.
    #[tokio::test]
    async fn extra_ca_seeded_client_completes_the_handshake_and_unseeded_gets_unknown_issuer() {
        let pki = TestPki::mint();
        let addr = serve_https(&pki).await;
        let url = format!("https://localhost:{}/", addr.port());
        // Production shape: bundled roots first, operator roots on top; the
        // override pins `localhost` so the dial never depends on the host's
        // resolver, and `no_proxy` keeps an ambient HTTPS_PROXY out of it.
        let recipe = || {
            seed_embedded_roots(reqwest::Client::builder())
                .no_proxy()
                .resolve("localhost", addr)
                .timeout(Duration::from_secs(10))
        };

        let seeded = ExtraRoots::from_pem(pki.root_pem().as_bytes())
            .expect("a minted root parses")
            .seed(recipe())
            .build()
            .expect("seeded client builds");
        let response = seeded
            .get(&url)
            .send()
            .await
            .expect("the seeded client trusts the minted root");
        assert_eq!(response.status(), reqwest::StatusCode::OK);

        let unseeded = recipe().build().expect("unseeded client builds");
        let error = unseeded
            .get(&url)
            .send()
            .await
            .expect_err("without the root the handshake must fail");
        let chain = error_chain(&error);
        assert_untrusted_root(&chain);
    }

    // ── C-007: the Sigstore process-wide set ────────────────────────────────

    /// C-007 / DX-9: `sigstore_roots()` is the empty set until
    /// `install_sigstore_roots` runs — reading first must not pin it — and a
    /// second install is a no-op.
    ///
    /// Process-global state: `oci::endpoint.rs` has its own install test and
    /// shares this process under plain `cargo test` (unit tests across a
    /// crate link into one binary), so this is not the only test that
    /// installs — only the only one in this file. Under nextest every test
    /// is its own process anyway, which is what the gate runs, so ordering
    /// across files never bites. Mutations: pin via
    /// `get_or_init(Default)` in the getter → the install is lost, reds;
    /// overwrite on second install → the length becomes 1, reds.
    #[test]
    fn extra_ca_sigstore_roots_are_empty_until_installed_and_the_first_install_wins() {
        assert!(sigstore_roots().is_empty(), "a read before install sees the empty set");
        assert_eq!(sigstore_roots().len(), 0);

        let (_, first) = mint_root("CN=ocx-test-ca-1");
        let (_, second) = mint_root("CN=ocx-test-ca-2");
        let two = parse(
            format!(
                "{}{}",
                pem_block("CERTIFICATE", &first),
                pem_block("CERTIFICATE", &second)
            )
            .as_bytes(),
        )
        .expect("two minted roots parse");
        install_sigstore_roots(two);
        assert_eq!(
            sigstore_roots().len(),
            2,
            "the read before install did not pin the empty set"
        );
        assert_eq!(sigstore_roots().der()[0].as_ref(), first.as_slice());

        let one = parse(pem_block("CERTIFICATE", &second).as_bytes()).expect("one minted root parses");
        install_sigstore_roots(one);
        assert_eq!(sigstore_roots().len(), 2, "a second install is a no-op");
        assert_eq!(sigstore_roots().der()[0].as_ref(), first.as_slice());
    }
}
