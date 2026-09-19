// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Operator-supplied extra CA roots (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`,
//! ocx#448): the validated type family, the single choke point every root
//! byte passes through, and the process-wide Sigstore set.
//!
//! Domain-free by construction — nothing here reads a tier, a path or the
//! environment. The source a bundle came from, the refusal that names it and
//! the resolution ladder over the `config.toml` tiers live in
//! `ocx_config`'s `tls`, which wraps [`ExtraRoots::from_pem`] as `parse_pem`
//! to attach that origin. Beside `trust`, the other trust-material ladder; the
//! bundled Mozilla seed for hand-rolled `reqwest` builders is
//! [`embedded_roots`].

pub mod embedded_roots;

pub use embedded_roots::seed_embedded_roots;

use std::sync::OnceLock;

/// Ceiling on the combined size of an operator-supplied extra-CA-roots PEM
/// bundle, from any source (environment variable, file path, or inline
/// `config.toml` text) — D-10.
///
/// Sits under `config::loader::MAX_CONFIG_SIZE` (64 KiB) so an inline
/// `extra_ca_certs_pem` value can never make `config.toml` itself unloadable,
/// and under the ~32 767-character Windows environment-variable limit.
pub const MAX_EXTRA_CA_CERTS_BYTES: usize = 32 * 1024;

/// Additional CA root certificates, appended to the platform trust store and
/// the bundled Mozilla set — never a replacement (D-2, "extra" in every name).
///
/// The only way to build a non-empty value is [`ExtraRoots::from_pem`], the
/// single choke point every extra-CA-roots byte must pass through before it
/// reaches a [`reqwest::ClientBuilder`] or the registry transport's own
/// certificate list (C-004, D-9). A default-constructed value is empty and
/// [`ExtraRoots::seed`] is then a no-op — what every client is built with
/// when no source is configured.
#[derive(Clone, Default)]
pub struct ExtraRoots {
    /// The parsed roots, for the registry transport's own certificate list.
    der: Vec<pki_types::CertificateDer<'static>>,
    /// The same roots as reqwest wants them, built once at parse time so
    /// [`ExtraRoots::seed`] has nothing left to fail on.
    certificates: Vec<reqwest::Certificate>,
}

impl std::fmt::Debug for ExtraRoots {
    /// Prints a certificate count, never the DER bytes (D-11).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtraRoots")
            .field("certificates", &self.der.len())
            .finish()
    }
}

impl PartialEq for ExtraRoots {
    /// Two sets are equal when their DER bytes are — the reqwest copies are
    /// derived from them.
    fn eq(&self, other: &Self) -> bool {
        self.der == other.der
    }
}

/// Why [`ExtraRoots::from_pem`] refused a bundle — the discriminant alone,
/// with no source attached, because this tier has no vocabulary for where a
/// bundle came from.
///
/// **Never reaches an operator.** It implements neither `Display` nor
/// `std::error::Error`, so it cannot be formatted into a message, cannot be
/// carried as a `#[source]`, and cannot enter an error chain the CLI walks —
/// the compiler is what enforces that, not a convention. Its one consumer is
/// `config::tls::parse_pem`, which maps each
/// variant one-to-one onto the `TlsError` arm that renders it with the origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PemBundleError {
    /// The bundle exceeds [`MAX_EXTRA_CA_CERTS_BYTES`], carrying its length.
    TooLarge {
        /// The bundle's length in bytes.
        bytes: usize,
    },
    /// A PEM block's tag is not `CERTIFICATE`, carrying that tag.
    NotACertificate {
        /// The PEM block's own tag (the text between `-----BEGIN ` and `-----`).
        tag: String,
    },
    /// The bundle held zero `CERTIFICATE` blocks.
    Empty,
    /// A block failed to decode as X.509 (`Some(index)`, zero-based), or the
    /// PEM decoder refused the bundle / the platform probe refused the set
    /// (`None`).
    Malformed {
        /// The zero-based index of the offending block, or `None` when the
        /// failure is not attributable to one block.
        index: Option<usize>,
    },
    /// A `-----BEGIN` line the decoder could not complete, at that block.
    Truncated {
        /// The zero-based index of the first block with no end line.
        index: usize,
    },
}

impl ExtraRoots {
    /// The only constructor from bytes. Every extra-CA-roots byte passes here
    /// before any [`reqwest::ClientBuilder`] or registry transport sees it
    /// (C-004, D-9).
    ///
    /// Checks run in this order, each on the whole input: the byte cap, then
    /// the `pem` decoder (leading text such as a `subject=` label and CRLF
    /// line endings are accepted), then that every `-----BEGIN ` line became
    /// a block (the decoder stops silently at the first one it cannot
    /// complete, so a bundle truncated after its first certificate would
    /// otherwise trust one root and drop the rest), then every block's tag,
    /// then zero blocks, then each block's X.509 parse, and finally one
    /// **probe build** of a `reqwest::Client` carrying the whole set. The
    /// probe is the platform's own verdict (webpki / CryptoAPI /
    /// Security.framework) on the roots as trust anchors: a set that fails it
    /// never becomes an `ExtraRoots`, so no production builder can fail on CA
    /// input and reach its degraded fallback arm with fewer roots than
    /// configured (D-9).
    ///
    /// **Blocking.** The probe build loads the platform trust store
    /// (`rustls-platform-verifier`, which on Linux reads the system bundle
    /// through `rustls-native-certs`). Call from a blocking context; from
    /// async, wrap the read and the parse together in `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// A [`PemBundleError`] naming which rule the bundle broke.
    /// `config::tls::parse_pem` is the door
    /// that turns it into the operator-facing refusal.
    pub fn from_pem(pem: &[u8]) -> Result<Self, PemBundleError> {
        use x509_cert::der::Decode as _;

        if pem.len() > MAX_EXTRA_CA_CERTS_BYTES {
            return Err(PemBundleError::TooLarge { bytes: pem.len() });
        }
        let blocks = pem::parse_many(pem).map_err(|_| PemBundleError::Malformed { index: None })?;
        // The decoder ends iteration at the first block it cannot complete
        // (no `-----END` line) and returns the blocks before it without an
        // error — so a bundle cut off mid-copy is malformed at that block,
        // never "the certificates that made it" and never absent. The needle
        // is the decoder's own (`pem` 3.0.6 `parser.rs` matches
        // `-----BEGIN ` with the trailing space), so a bare `-----BEGIN` in a
        // leading comment line is neither a block nor a missing one.
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

    /// Chains every root onto `builder` via `tls_certs_merge` (not the
    /// deprecated `add_root_certificate`) — additive, so it composes with
    /// [`seed_embedded_roots`] in
    /// either order. An empty set returns `builder` unchanged.
    pub fn seed(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        if self.is_empty() {
            return builder;
        }
        builder.tls_certs_merge(self.certificates.iter().cloned())
    }

    /// The parsed DER-encoded roots, for the registry transport's own
    /// certificate list (`oci::native::Certificate { encoding: Der, .. }`,
    /// C-006).
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

/// Process-wide extra CA roots for the Sigstore trust-services client
/// (`oci::endpoint::sigstore_http_client`).
static SIGSTORE_ROOTS: OnceLock<ExtraRoots> = OnceLock::new();

/// Install the process-wide extra CA roots for Sigstore calls.
///
/// Set once, from `Context::try_init`, before any Sigstore call (C-007). A
/// second install is a no-op — the roots validated at startup are what every
/// later Sigstore dial uses for the process's lifetime. A read before install
/// (e.g. unit tests calling [`sigstore_roots`] directly) sees the empty set
/// without pinning it — a later install still takes effect.
///
/// A `OnceLock` pair (this + [`sigstore_roots`]) rather than a parameter is
/// what lets `oci::endpoint::sigstore_http_client` stay a zero-argument
/// lazy static. The upgrade path, if that ever needs to change (e.g. roots
/// that can rotate mid-process), is threading the roots into
/// `sigstore_http_client` directly as an explicit argument and dropping this
/// pair — not adding a second global.
pub fn install_sigstore_roots(roots: ExtraRoots) {
    if let Err(later) = SIGSTORE_ROOTS.set(roots)
        && SIGSTORE_ROOTS.get() != Some(&later)
    {
        // Unreachable while one `Context` is built per process; a trace the
        // day that stops being true, since the second set would otherwise
        // vanish silently.
        log::debug!(
            "a second install_sigstore_roots with a different set ({} certificates) is ignored",
            later.len()
        );
    }
}

/// The installed extra CA roots, or an empty [`ExtraRoots`] if
/// [`install_sigstore_roots`] was never called.
///
/// Reads `SIGSTORE_ROOTS` without `get_or_init` — initializing the
/// [`OnceLock`] with a default would permanently pin the empty set for a
/// caller that reads before `install_sigstore_roots` runs.
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
