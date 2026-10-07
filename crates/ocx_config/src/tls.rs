// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The configured half of the operator-supplied extra CA roots (`OCX_EXTRA_CA_CERTS` /
//! `extra_ca_certs`): where a bundle came from, the refusal naming it, and the resolution ladder.

use std::path::{Path, PathBuf};

use crate::loader::ConfigLoader;
use crate::{Config, ConfigTier};
use ocx_exit::{Pick, Row};
use ocx_util::tls::{ExtraRoots, MAX_EXTRA_CA_CERTS_BYTES, PemBundleError};

/// Where an [`ExtraRoots`] value's bytes came from.
///
/// No variant holds PEM bytes, or a pasted key could leak into an error message or log line.
#[derive(Clone)]
pub enum ExtraRootsSource {
    /// Inline PEM text in the `OCX_EXTRA_CA_CERTS` environment variable.
    Env,
    /// A path named by an environment variable.
    EnvPath(PathBuf),
    /// A path named by `extra_ca_certs` in a `config.toml`.
    ConfigPath {
        /// The path as the config named it (anchored absolute by the loader).
        path: PathBuf,
        /// The tier whose file set the key, or `None` when read outside the loader's tiers
        /// (`ocx config push` reading its payload).
        tier: Option<ConfigTier>,
    },
    /// Inline PEM text under `extra_ca_certs_pem`, with the tier that supplied it.
    ConfigInline(ConfigTier),
}

// Delegates to `Display`, or a `{:?}` of a `TlsError` bypasses the path redaction.
impl std::fmt::Debug for ExtraRootsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl ExtraRootsSource {
    /// Longest path echoed verbatim; anything longer is not a path and is never echoed.
    const MAX_ECHOED_PATH_BYTES: usize = 256;

    /// Whether the bytes came from a named file rather than inline text; a refusal's exit code
    /// turns on it.
    pub fn is_file(&self) -> bool {
        matches!(self, Self::EnvPath(_) | Self::ConfigPath { .. })
    }

    /// The byte count of a path too long or newline-bearing to echo (PEM text in a path-typed
    /// variable), or `None` for a path safe to print.
    fn redacted_len(path: &Path) -> Option<usize> {
        let bytes = path.as_os_str().as_encoded_bytes();
        (bytes.len() > Self::MAX_ECHOED_PATH_BYTES || bytes.contains(&b'\n')).then_some(bytes.len())
    }
}

impl std::fmt::Display for ExtraRootsSource {
    /// Names the door the operator used, never the value that came through it (CWE-532).
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Env => write!(formatter, "OCX_EXTRA_CA_CERTS"),
            Self::EnvPath(path) => match Self::redacted_len(path) {
                Some(bytes) => write!(formatter, "OCX_EXTRA_CA_CERTS ({bytes} bytes, not a readable path)"),
                None => write!(formatter, "OCX_EXTRA_CA_CERTS={}", path.display()),
            },
            Self::ConfigPath { path, tier } => {
                match Self::redacted_len(path) {
                    Some(bytes) => write!(formatter, "extra_ca_certs ({bytes} bytes, not a readable path)")?,
                    None => write!(formatter, "extra_ca_certs={}", path.display())?,
                }
                match tier {
                    Some(tier) => write!(formatter, " ({tier})"),
                    None => Ok(()),
                }
            }
            Self::ConfigInline(tier) => write!(formatter, "extra_ca_certs_pem ({tier})"),
        }
    }
}

/// The PEM tag `TlsError::NotACertificate` echoes, bounded and escaped, or a body line mistaken
/// for a header echoes up to the whole bundle.
fn truncated_tag(tag: &str) -> String {
    const MAX_TAG_BYTES: usize = 32;
    if tag.len() <= MAX_TAG_BYTES {
        return format!("{tag:?}");
    }
    // ASCII marker: U+2026 renders as mojibake on a Windows PowerShell 5.1 console.
    let prefix = &tag[..tag.floor_char_boundary(MAX_TAG_BYTES)];
    format!("{prefix:?}...")
}

/// Failure parsing, validating, or reading operator-supplied extra CA root material.
///
/// Variants carry no certificate or key bytes, only a source, count, index or truncated tag:
/// a reachable `Display` can land in CI logs.
#[derive(Debug, thiserror::Error, ocx_exit::Classify)]
#[exit(
    with = row_by_origin,
    rows(
        (DataError, slug = "extra_ca_file_invalid", summary = "The extra CA bundle file holds no usable certificate"),
        (ConfigError, slug = "extra_ca_value_invalid", summary = "The configured extra CA bundle value holds no usable certificate"),
    )
)]
pub enum TlsError {
    /// The PEM bundle exceeds [`MAX_EXTRA_CA_CERTS_BYTES`]; inline text only, since an
    /// over-cap file surfaces as [`TlsError::Unreadable`].
    #[error(
        "{origin} is {bytes} bytes, over the {}-byte limit for a CA bundle",
        MAX_EXTRA_CA_CERTS_BYTES
    )]
    #[exit(
        with = row_by_origin,
        rows(
            (IoError, slug = "extra_ca_file_too_large", summary = "The extra CA bundle file exceeds the allowed size"),
            (ConfigError, slug = "extra_ca_value_too_large", summary = "The configured extra CA bundle value exceeds the allowed size"),
        )
    )]
    TooLarge {
        /// Where the oversized text came from.
        origin: ExtraRootsSource,
        /// Its length in bytes.
        bytes: usize,
    },

    /// A PEM block's tag is not `CERTIFICATE` — refused rather than silently
    /// skipped, so a pasted private key is never dropped without a diagnostic.
    #[error("{origin} contains a PEM block tagged {}, not CERTIFICATE", truncated_tag(tag))]
    NotACertificate {
        /// Where the bundle came from.
        origin: ExtraRootsSource,
        /// The PEM block's own tag (the text between `-----BEGIN ` and `-----`).
        tag: String,
    },

    /// The bundle held zero `CERTIFICATE` blocks.
    #[error("{origin} contains no certificate")]
    Empty {
        /// Where the empty bundle came from.
        origin: ExtraRootsSource,
    },

    /// A block is not X.509, the PEM framing is undecodable, or the platform TLS verifier's
    /// probe build rejected the set as anchors.
    #[error(
        "{origin}: certificate {}",
        index.map_or_else(
            || "bundle is not one this platform accepts".to_owned(),
            |i| format!("block {} does not parse as an X.509 certificate", i + 1)
        )
    )]
    Malformed {
        /// Where the bundle came from.
        origin: ExtraRootsSource,
        /// Zero-based index of the failing block (one-based in the message), or `None` when no
        /// single block is at fault.
        index: Option<usize>,
    },

    /// A `-----BEGIN` line with no end — a bundle cut off mid-copy.
    #[error("{origin}: certificate block {} is incomplete (no -----END line)", index + 1)]
    Truncated {
        /// Where the bundle came from.
        origin: ExtraRootsSource,
        /// Zero-based index of the first block with no end line (one-based in the message).
        index: usize,
    },

    /// A path-typed source could not be read as a bounded, regular file.
    #[error("cannot read {origin}")]
    #[exit(
        IoError,
        slug = "extra_ca_unreadable",
        summary = "The extra CA bundle cannot be read"
    )]
    Unreadable {
        /// The refused path-typed source.
        origin: ExtraRootsSource,
        /// What the read raised, path stripped, or the `{err:#}` chain echoes a path `origin` redacted.
        #[source]
        io: std::io::Error,
    },
}

/// Picks the first row for a file origin (an unusable file) and the second for inline text (the configuration itself).
///
/// Exhaustive over [`TlsError`], so a new variant must name its origin or pick its own row.
fn row_by_origin(error: &TlsError, rows: [Row; 2]) -> Pick<'_> {
    let (TlsError::TooLarge { origin, .. }
    | TlsError::NotACertificate { origin, .. }
    | TlsError::Empty { origin }
    | TlsError::Malformed { origin, .. }
    | TlsError::Truncated { origin, .. }
    | TlsError::Unreadable { origin, .. }) = error;
    if origin.is_file() {
        Pick::row(rows[0])
    } else {
        Pick::row(rows[1])
    }
}

/// The environment arm of the extra-CA ladder: a non-empty value containing `-----BEGIN` is PEM
/// text, anything else a path read through [`read_path`]. **Blocking** on the path arm.
///
/// Returns the bytes as read, never re-encoded, so a caller persisting them stores the operator's
/// bundle; validate them with [`parse_pem`].
///
/// # Errors
///
/// [`TlsError::Unreadable`] when the path cannot be read as a bounded
/// regular file.
pub fn from_env_value(value: &str) -> Result<(Vec<u8>, ExtraRootsSource), TlsError> {
    if value.contains("-----BEGIN") {
        return Ok((value.as_bytes().to_vec(), ExtraRootsSource::Env));
    }
    let path = PathBuf::from(value);
    read_path(&path, ExtraRootsSource::EnvPath(path.clone()))
}

/// The bounded read behind every path-typed source, under [`MAX_EXTRA_CA_CERTS_BYTES`];
/// validate the bytes with [`parse_pem`]. **Blocking.**
///
/// The refusal names the path only in `origin`: `read_bounded`'s error names it unredacted, so
/// keeping that would leak a PEM body in a path-typed value through the source chain (CWE-532).
///
/// # Errors
///
/// [`TlsError::Unreadable`] when the path cannot be read as a bounded
/// regular file.
pub fn read_path(path: &Path, origin: ExtraRootsSource) -> Result<(Vec<u8>, ExtraRootsSource), TlsError> {
    let bytes =
        ocx_util::fs::read_bounded(path, MAX_EXTRA_CA_CERTS_BYTES as u64).map_err(|refused| TlsError::Unreadable {
            origin: origin.clone(),
            io: refused.into_io_error(),
        })?;
    Ok((bytes, origin))
}

/// [`ExtraRoots::from_pem`], the one validated choke point for every extra-CA byte, with the
/// `origin` a refusal names attached.
///
/// **Blocking**: the probe build loads the platform trust store; from async, `spawn_blocking`
/// the read and the parse together.
///
/// # Errors
///
/// [`TlsError::TooLarge`], [`TlsError::Truncated`], [`TlsError::NotACertificate`],
/// [`TlsError::Empty`] or [`TlsError::Malformed`], one per refusal kind.
pub fn parse_pem(pem: &[u8], origin: &ExtraRootsSource) -> Result<ExtraRoots, TlsError> {
    ExtraRoots::from_pem(pem).map_err(|refused| {
        let origin = origin.clone();
        match refused {
            PemBundleError::TooLarge { bytes } => TlsError::TooLarge { origin, bytes },
            PemBundleError::NotACertificate { tag } => TlsError::NotACertificate { origin, tag },
            PemBundleError::Empty => TlsError::Empty { origin },
            PemBundleError::Malformed { index } => TlsError::Malformed { origin, index },
            PemBundleError::Truncated { index } => TlsError::Truncated { origin, index },
        }
    })
}

/// The extra-CA-roots resolution ladder for one view: a non-empty `env` (unless the config pair
/// is system-locked), then `extra_ca_certs_pem`, then `extra_ca_certs` as a path, else empty.
/// `tier` is the loader's record of the key's tier. **Blocking.**
///
/// # Errors
///
/// [`TlsError::Unreadable`] for an unreadable path, else any [`TlsError`] `parse_pem` raises.
pub fn resolve_extra_roots(
    config: &Config,
    env: Option<&str>,
    tier: Option<ConfigTier>,
) -> Result<ExtraRoots, TlsError> {
    let env = env.filter(|value| !value.is_empty());
    // A system-locked pair outranks the env too, the one rung the loader's lock cannot reach.
    let env = if config.extra_ca_certs_system_locked && env.is_some() {
        log::warn!(
            "ignoring OCX_EXTRA_CA_CERTS: extra_ca_certs / extra_ca_certs_pem are locked by {}; edit the system tier \
             or ask its owner",
            ConfigLoader::system_path().display()
        );
        None
    } else {
        env
    };
    match env {
        Some(value) => {
            let (pem, origin) = from_env_value(value)?;
            parse_pem(&pem, &origin)
        }
        None => match (config.extra_ca_certs_pem.as_deref(), config.extra_ca_certs.as_deref()) {
            (Some(pem), _) => {
                debug_assert!(
                    tier.is_some(),
                    "a loaded Config carrying extra_ca_certs_pem has a recorded tier"
                );
                parse_pem(
                    pem.as_bytes(),
                    &ExtraRootsSource::ConfigInline(tier.unwrap_or(ConfigTier::Home)),
                )
            }
            (None, Some(path)) => {
                let origin = ExtraRootsSource::ConfigPath {
                    path: path.to_path_buf(),
                    tier,
                };
                let (pem, origin) = read_path(path, origin)?;
                parse_pem(&pem, &origin)
            }
            (None, None) => Ok(ExtraRoots::default()),
        },
    }
}

/// The Sigstore trust-services view: `merged` when the managed source is digest-pinned or the
/// managed payload did not win the extra-CA key, else `local`.
#[must_use]
pub fn sigstore_extra_roots(
    merged: &ExtraRoots,
    local: &ExtraRoots,
    extra_ca_tier: Option<ConfigTier>,
    managed_source_pinned: bool,
) -> ExtraRoots {
    let payload_set_nothing = extra_ca_tier != Some(ConfigTier::Managed);
    if managed_source_pinned || payload_set_nothing {
        merged.clone()
    } else {
        local.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    use ocx_test_support::pki::{TestPki, error_chain, mint_root, pem_block};

    const ENV: ExtraRootsSource = ExtraRootsSource::Env;

    fn parse(pem: &[u8]) -> Result<ExtraRoots, TlsError> {
        parse_pem(pem, &ENV)
    }

    // ── the parse matrix ─────────────────────────────────────────────────────

    /// Edge case "bundle with 2+ certificates → all appended".
    #[test]
    fn extra_ca_parse_pem_accepts_a_two_certificate_bundle_in_order() {
        let (_, first) = mint_root("CN=ocx-test-ca-1");
        let (_, second) = mint_root("CN=ocx-test-ca-2");
        let bundle = format!(
            "{}{}",
            pem_block("CERTIFICATE", &first),
            pem_block("CERTIFICATE", &second)
        );

        let roots = parse(bundle.as_bytes()).expect("two minted roots parse");

        assert_eq!(roots.len(), 2);
        assert!(!roots.is_empty());
        assert_eq!(roots.der()[0].as_ref(), first.as_slice(), "first block, first root");
        assert_eq!(roots.der()[1].as_ref(), second.as_slice(), "second block, second root");
    }

    /// Edge case "CRLF PEM, leading `subject=` labels before
    /// `-----BEGIN` (Fedora/RHEL bundles) → accepted".
    #[test]
    fn extra_ca_parse_pem_accepts_crlf_and_a_leading_subject_label_line() {
        let (_, root) = mint_root("CN=ocx-test-ca");
        let crlf = pem::encode_config(
            &pem::Pem::new("CERTIFICATE", root.as_slice()),
            pem::EncodeConfig::new().set_line_ending(pem::LineEnding::CRLF),
        );
        assert!(crlf.contains("\r\n"), "fixture must actually carry CRLF");
        let bundle = format!("subject=CN=ocx-test-ca\r\nissuer=CN=ocx-test-ca\r\n{crlf}");

        let roots = parse(bundle.as_bytes()).expect("a labelled CRLF bundle parses");

        assert_eq!(roots.len(), 1);
        assert_eq!(roots.der()[0].as_ref(), root.as_slice());
    }

    /// A non-`CERTIFICATE` tag refuses — a pasted private key is
    /// never silently skipped the way `reqwest::from_pem_bundle` would.
    #[test]
    fn extra_ca_parse_pem_refuses_a_private_key_block_alone() {
        let pki = TestPki::mint();
        let key = pem_block("PRIVATE KEY", &pki.leaf_key_pkcs8);

        match parse(key.as_bytes()) {
            Err(TlsError::NotACertificate { tag, .. }) => assert_eq!(tag, "PRIVATE KEY"),
            other => panic!("expected NotACertificate, got {other:?}"),
        }
    }

    /// The same refusal when a valid certificate sits beside the
    /// key — a bundle is all-or-nothing, never "the certificates that parsed".
    #[test]
    fn extra_ca_parse_pem_refuses_a_private_key_block_beside_a_certificate() {
        let pki = TestPki::mint();
        let bundle = format!("{}{}", pki.root_pem(), pem_block("PRIVATE KEY", &pki.leaf_key_pkcs8));

        match parse(bundle.as_bytes()) {
            Err(TlsError::NotACertificate { tag, .. }) => assert_eq!(tag, "PRIVATE KEY"),
            other => panic!("expected NotACertificate, got {other:?}"),
        }
    }

    /// OpenSSL's `-trustout` output is tagged
    /// `TRUSTED CERTIFICATE` — a certificate with trust attributes appended,
    /// which no TLS stack takes as an anchor. Refused by its tag like any
    /// other non-`CERTIFICATE` block, so the operator is told what to convert
    /// (`openssl x509 -in trusted.pem -out plain.pem`) rather than handed a
    /// "malformed" verdict on a well-formed file.
    #[test]
    fn extra_ca_parse_pem_refuses_an_openssl_trusted_certificate_block() {
        let pki = TestPki::mint();
        for tag in ["TRUSTED CERTIFICATE", "X509 CERTIFICATE"] {
            match parse(pem_block(tag, &pki.root_der).as_bytes()) {
                Err(TlsError::NotACertificate { tag: seen, .. }) => assert_eq!(seen, tag),
                other => panic!("expected NotACertificate for a {tag} block, got {other:?}"),
            }
        }
    }

    /// Env arm: a UTF-8 BOM ahead of `-----BEGIN` (a bundle saved by a
    /// Windows editor) is leading text to the decoder, so the bundle parses —
    /// and to the same root as without it, so a persisted copy stays
    /// idempotent on the next run.
    #[test]
    fn extra_ca_parse_pem_accepts_a_utf8_bom_ahead_of_the_first_block() {
        let pki = TestPki::mint();
        let with_bom = [b"\xEF\xBB\xBF".as_slice(), pki.root_pem().as_bytes()].concat();

        let roots = parse(&with_bom).expect("a BOM is leading text, not a certificate byte");

        assert_eq!(
            roots,
            ExtraRoots::from_pem(pki.root_pem().as_bytes()).expect("a minted root parses"),
            "the BOM changes nothing about the parsed set"
        );
    }

    /// Zero blocks → `Empty`, for both the literal empty value and a
    /// whitespace-only one (an env var set to a blank line).
    #[test]
    fn extra_ca_parse_pem_refuses_empty_and_whitespace_only_input() {
        assert!(matches!(parse(b""), Err(TlsError::Empty { .. })), "empty");
        assert!(
            matches!(parse(b"  \r\n\t\n"), Err(TlsError::Empty { .. })),
            "whitespace only"
        );
    }

    /// A bundle cut off after `-----BEGIN` — the `pem` decoder
    /// yields no block for it — is `Truncated` at block 0, not `Empty`: the
    /// operator supplied a certificate, just not all of it — and not
    /// `Malformed` either, whose wording sends them hunting an OS trust-store
    /// incompatibility for a cut-off paste.
    #[test]
    fn extra_ca_parse_pem_refuses_a_truncated_bundle_as_truncated_not_empty() {
        let pki = TestPki::mint();
        let whole = pki.root_pem();
        let cut = whole.find("-----END").expect("a PEM block has an end line");
        let truncated = &whole[..cut];
        assert!(truncated.contains("-----BEGIN") && !truncated.contains("-----END"));

        match parse(truncated.as_bytes()) {
            Err(error @ TlsError::Truncated { index, .. }) => {
                assert_eq!(index, 0);
                assert!(error.to_string().contains("no -----END line"), "{error}");
            }
            other => panic!("expected Truncated for a truncated bundle, got {other:?}"),
        }
        // Positive control: text with no `-----BEGIN` at all is still `Empty`.
        assert!(matches!(parse(b"subject=CN=ocx\n"), Err(TlsError::Empty { .. })));
    }

    /// A complete certificate followed by a truncated one is refused
    /// naming the truncated block — `pem::parse_many` stops silently at the
    /// first block it cannot complete, so without the `-----BEGIN` count a
    /// bundle cut off mid-copy after cert 1 of 2 would trust one root and
    /// drop the other without a word.
    ///
    /// Mutation: delete the `begins > blocks.len()` check — the bundle parses
    /// as one root and this reds.
    #[test]
    fn extra_ca_parse_pem_refuses_a_bundle_whose_last_block_is_truncated() {
        let (_, first) = mint_root("CN=ocx-test-ca-1");
        let (_, second) = mint_root("CN=ocx-test-ca-2");
        let second_pem = pem_block("CERTIFICATE", &second);
        let cut = second_pem.find("-----END").expect("a PEM block has an end line");
        let bundle = format!("{}{}", pem_block("CERTIFICATE", &first), &second_pem[..cut]);

        match parse(bundle.as_bytes()) {
            Err(error @ TlsError::Truncated { index, .. }) => {
                assert_eq!(index, 1, "the truncated block is named");
                assert!(
                    error.to_string().contains("block 2 is incomplete"),
                    "the message counts blocks from one: {error}"
                );
            }
            other => panic!("expected Truncated for a bundle with a truncated tail, got {other:?}"),
        }
        // Positive control: the same two blocks, both complete, parse to two.
        let whole = format!("{}{second_pem}", pem_block("CERTIFICATE", &first));
        assert_eq!(parse(whole.as_bytes()).expect("two complete blocks parse").len(), 2);
    }

    /// The truncation count uses the decoder's own needle. A leading
    /// comment carrying a bare `-----BEGIN` (`# delimited by
    /// -----BEGIN/-----END`) is not a block the decoder would have parsed, so
    /// a complete bundle behind it is accepted, not refused as `Truncated`.
    ///
    /// Mutation: needle back to `b"-----BEGIN"` — this reds with
    /// `Truncated { index: 1 }`.
    #[test]
    fn extra_ca_parse_pem_ignores_a_bare_begin_in_leading_text() {
        let pki = TestPki::mint();
        let bundle = format!("# blocks are delimited by -----BEGIN/-----END\n{}", pki.root_pem());

        match parse(bundle.as_bytes()) {
            Ok(roots) => assert_eq!(roots.len(), 1),
            Err(error) => panic!("a bare -----BEGIN in a comment is not a block: {error}"),
        }
    }

    /// The size check runs first and is exact — one byte over
    /// `MAX_EXTRA_CA_CERTS_BYTES` is `TooLarge` carrying the byte count; a
    /// bundle of exactly the cap still parses.
    #[test]
    fn extra_ca_parse_pem_refuses_one_byte_over_the_cap_and_accepts_the_cap() {
        let pki = TestPki::mint();
        let mut at_cap = pki.root_pem().into_bytes();
        assert!(
            at_cap.len() < MAX_EXTRA_CA_CERTS_BYTES,
            "fixture must fit under the cap"
        );
        at_cap.resize(MAX_EXTRA_CA_CERTS_BYTES, b'\n');
        let mut over_cap = at_cap.clone();
        over_cap.push(b'\n');

        match parse(&over_cap) {
            Err(TlsError::TooLarge { bytes, .. }) => assert_eq!(bytes, MAX_EXTRA_CA_CERTS_BYTES + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
        match parse(&at_cap) {
            Ok(roots) => assert_eq!(roots.len(), 1),
            Err(TlsError::TooLarge { .. }) => panic!("exactly the cap is not over the cap"),
            Err(other) => panic!("a padded valid bundle at the cap must parse, got {other}"),
        }
    }

    /// A `CERTIFICATE` block whose body is not an X.509 certificate is
    /// `Malformed` naming the zero-based block — both for arbitrary bytes and
    /// for valid DER that is not a certificate.
    #[test]
    fn extra_ca_parse_pem_refuses_a_certificate_block_that_is_not_x509_with_its_index() {
        let pki = TestPki::mint();
        // `SEQUENCE { INTEGER 1 }` — the `trust_root.rs` fixture: valid DER,
        // never a certificate.
        let not_a_certificate_der: &[u8] = &[0x30, 0x03, 0x02, 0x01, 0x01];

        let text_body = pem_block("CERTIFICATE", b"this is not DER at all");
        match parse(text_body.as_bytes()) {
            Err(error @ TlsError::Malformed { index, .. }) => {
                assert_eq!(index, Some(0));
                assert!(
                    error
                        .to_string()
                        .contains("block 1 does not parse as an X.509 certificate"),
                    "the Display wording a caller matches on: {error}"
                );
            }
            other => panic!("expected Malformed at block 0, got {other:?}"),
        }

        let der_body = pem_block("CERTIFICATE", not_a_certificate_der);
        match parse(der_body.as_bytes()) {
            Err(TlsError::Malformed { index, .. }) => assert_eq!(index, Some(0)),
            other => panic!("expected Malformed at block 0, got {other:?}"),
        }

        let second_bad = format!("{}{der_body}", pki.root_pem());
        match parse(second_bad.as_bytes()) {
            Err(error @ TlsError::Malformed { index, .. }) => {
                assert_eq!(index, Some(1));
                assert!(
                    error.to_string().contains("block 2 does not parse"),
                    "the message counts blocks from one: {error}"
                );
            }
            other => panic!("expected Malformed at block 1, got {other:?}"),
        }
    }

    /// A block that parses as X.509 but that the platform
    /// verifier refuses as an anchor — a pre-v3 certificate — is `Malformed`
    /// from the probe build, so no client is ever built with fewer roots than
    /// configured. Linux-only by design: only webpki refuses it;
    /// CryptoAPI and Security.framework accept any certificate version.
    ///
    /// The design record names a v1 root; rustls-webpki 0.103's
    /// `anchor_from_trusted_cert` has an explicit v1 fallback parser and
    /// accepts one (probed), so the case is a v2 root instead — the version
    /// webpki's "v3 only" rule refuses and its v1 parser cannot read.
    ///
    /// Mutation: delete the probe build — this reds (x509-cert parses v2).
    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn extra_ca_parse_pem_refuses_a_pre_v3_root_the_platform_verifier_rejects() {
        use ocx_test_support::pki::mint_v2_root;
        use x509_cert::der::Decode as _;

        let v2 = mint_v2_root();
        assert!(
            x509_cert::Certificate::from_der(&v2).is_ok(),
            "the fixture must pass the X.509 parse so only the probe can refuse it"
        );

        match parse(pem_block("CERTIFICATE", &v2).as_bytes()) {
            Err(TlsError::Malformed { index, .. }) => {
                assert_eq!(index, None, "the probe rejects the set, not one block");
            }
            other => panic!("expected Malformed from the probe, got {other:?}"),
        }
    }

    // ── every arm's rendered text, against a literal ────────────────────────

    /// The operator-facing wording of every `TlsError` arm, pinned to
    /// the literal string rather than to a shape.
    ///
    /// These are the sentences an operator reads when trust material is
    /// refused, and a script may `grep` one. A relocation that re-derived a
    /// message — or a `thiserror` attribute reflowed by hand — would leave
    /// every `matches!`-style assertion in this file green while the text
    /// changed underneath it, which is precisely what moving this enum between
    /// modules puts at risk.
    ///
    /// One row per variant, all six; `Unreadable`'s `#[source]` is asserted
    /// through the whole chain, the way `main.rs` prints it (`{err:#}`), so a
    /// lost `#[source]` reds here too.
    #[test]
    fn extra_ca_every_tls_error_arm_renders_its_pinned_literal() {
        let path = PathBuf::from("/etc/ssl/corp.pem");
        let rows: [(TlsError, &str); 6] = [
            (
                TlsError::TooLarge {
                    origin: ExtraRootsSource::Env,
                    bytes: 40_000,
                },
                "OCX_EXTRA_CA_CERTS is 40000 bytes, over the 32768-byte limit for a CA bundle",
            ),
            (
                TlsError::NotACertificate {
                    origin: ExtraRootsSource::ConfigInline(ConfigTier::System),
                    tag: "PRIVATE KEY".to_owned(),
                },
                "extra_ca_certs_pem (system config.toml) contains a PEM block tagged \"PRIVATE KEY\", not CERTIFICATE",
            ),
            (
                TlsError::Empty {
                    origin: ExtraRootsSource::ConfigPath {
                        path: path.clone(),
                        tier: Some(ConfigTier::Home),
                    },
                },
                "extra_ca_certs=/etc/ssl/corp.pem ($OCX_HOME/config.toml) contains no certificate",
            ),
            (
                TlsError::Malformed {
                    origin: ExtraRootsSource::EnvPath(path.clone()),
                    index: Some(1),
                },
                "OCX_EXTRA_CA_CERTS=/etc/ssl/corp.pem: certificate block 2 does not parse as an X.509 certificate",
            ),
            (
                TlsError::Malformed {
                    origin: ExtraRootsSource::Env,
                    index: None,
                },
                "OCX_EXTRA_CA_CERTS: certificate bundle is not one this platform accepts",
            ),
            (
                TlsError::Truncated {
                    origin: ExtraRootsSource::ConfigPath { path, tier: None },
                    index: 0,
                },
                "extra_ca_certs=/etc/ssl/corp.pem: certificate block 1 is incomplete (no -----END line)",
            ),
        ];
        for (error, expected) in rows {
            assert_eq!(error.to_string(), expected);
        }

        // `Unreadable` carries a cause, so its pinned text is the whole chain.
        let unreadable = TlsError::Unreadable {
            origin: ExtraRootsSource::EnvPath(PathBuf::from("/etc/ssl/corp.pem")),
            io: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
        };
        assert_eq!(
            unreadable.to_string(),
            "cannot read OCX_EXTRA_CA_CERTS=/etc/ssl/corp.pem"
        );
        assert_eq!(
            error_chain(&unreadable),
            "cannot read OCX_EXTRA_CA_CERTS=/etc/ssl/corp.pem: not a regular file"
        );
    }

    // ── nothing reachable from an error echoes certificate bytes ────────────

    /// A PEM body the way an operator's mistake would land it: the whole text
    /// in a path-typed variable. Returns the text and a slice of its base64
    /// body that must never appear in any rendering.
    fn pem_text_and_body_probe() -> (String, String) {
        let pki = TestPki::mint();
        let text = pki.root_pem();
        let body = text
            .lines()
            .find(|line| !line.starts_with("-----"))
            .expect("a PEM block has a body line")
            .to_owned();
        assert!(body.len() >= 32, "a body line is 64 base64 chars");
        (text, body[..32].to_owned())
    }

    fn all_variants(origin: &ExtraRootsSource) -> Vec<TlsError> {
        vec![
            TlsError::TooLarge {
                origin: origin.clone(),
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            },
            TlsError::NotACertificate {
                origin: origin.clone(),
                tag: "PRIVATE KEY".to_owned(),
            },
            TlsError::Empty { origin: origin.clone() },
            TlsError::Malformed {
                origin: origin.clone(),
                index: Some(0),
            },
            TlsError::Malformed {
                origin: origin.clone(),
                index: None,
            },
            TlsError::Truncated {
                origin: origin.clone(),
                index: 1,
            },
            TlsError::Unreadable {
                origin: origin.clone(),
                io: std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
            },
        ]
    }

    /// Every `TlsError` variant, over every source shape including the
    /// pathological "PEM text landed in the path variable" one, renders no
    /// `-----BEGIN` and no base64 body — a `Display` reachable from here lands
    /// in CI logs (CWE-532). Asserted over the whole `source()` chain, the
    /// way `main.rs` prints it (`{err:#}`), not the top message alone.
    #[test]
    fn extra_ca_error_display_never_echoes_pem_bytes_from_any_source() {
        let (pem_text, body_probe) = pem_text_and_body_probe();
        let sources = [
            ExtraRootsSource::Env,
            ExtraRootsSource::EnvPath(PathBuf::from(&pem_text)),
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from(&pem_text),
                tier: Some(ConfigTier::Home),
            },
            ExtraRootsSource::ConfigInline(ConfigTier::Home),
        ];
        for source in &sources {
            // `{:?}` too: a derived `Debug` would print the variant's field
            // verbatim, and a `log::debug!("{err:?}")` is one line away.
            for rendered_source in [source.to_string(), format!("{source:?}")] {
                assert!(!rendered_source.contains("-----BEGIN"), "source {source} echoes PEM");
                assert!(!rendered_source.contains(&body_probe), "source {source} echoes a body");
            }
            for error in all_variants(source) {
                for rendered in [error_chain(&error), format!("{error:?}")] {
                    assert!(!rendered.contains("-----BEGIN"), "{error} echoes PEM: {rendered}");
                    assert!(!rendered.contains(&body_probe), "{error} echoes a body: {rendered}");
                }
            }
        }
    }

    /// No PEM echo through the real reader: `Unreadable` built by [`read_path`]
    /// on the pathological value (PEM text as the path — over 256 bytes and
    /// newline-bearing) carries the path once, redacted, and its `#[source]`
    /// chain never names it. `read_bounded`'s own error renders the path
    /// verbatim, and the CLI prints the whole chain, so the mapping in
    /// `read_path` is what keeps the value out of a CI log.
    ///
    /// Mutation: carry `read_bounded`'s error as the source (or put the path
    /// back into the `io` message) — the chain echoes the body and this reds.
    #[test]
    fn extra_ca_unreadable_chain_never_echoes_a_pathological_path_from_either_door() {
        use std::error::Error as _;

        let (pem_text, body_probe) = pem_text_and_body_probe();
        let path = PathBuf::from(&pem_text);
        // The env door sniffs `-----BEGIN` as inline text, so its pathological
        // path is the body without the header line — a pasted secret.
        let headerless: String = pem_text
            .lines()
            .filter(|line| !line.starts_with("-----BEGIN"))
            .map(|line| format!("{line}\n"))
            .collect();
        assert!(headerless.contains(&body_probe) && headerless.len() > 256);
        let doors = [
            from_env_value(&headerless),
            read_path(
                &path,
                ExtraRootsSource::ConfigPath {
                    path: path.clone(),
                    tier: Some(ConfigTier::Home),
                },
            ),
        ];
        for door in doors {
            let error = match door {
                Err(error @ TlsError::Unreadable { .. }) => error,
                other => panic!("a PEM body is not a readable path, got {other:?}"),
            };
            let chain = error_chain(&error);
            assert!(!chain.contains("-----BEGIN"), "the chain echoes PEM: {chain}");
            assert!(!chain.contains(&body_probe), "the chain echoes a body: {chain}");
            assert!(
                chain.contains("bytes, not a readable path"),
                "the origin is redacted: {chain}"
            );
            assert!(
                error.source().is_some_and(|io| !io.to_string().contains(&body_probe)),
                "the source is kept, without the path: {chain}"
            );
        }

        // Positive control on the mapping itself: the two `read_bounded`
        // refusals that are not OS errors keep their rule, not the path.
        #[cfg(unix)]
        {
            let device = PathBuf::from("/dev/zero");
            match read_path(
                &device,
                ExtraRootsSource::ConfigPath {
                    path: device.clone(),
                    tier: None,
                },
            ) {
                Err(TlsError::Unreadable { io, .. }) => {
                    assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
                    assert_eq!(io.to_string(), "not a regular file");
                }
                other => panic!("/dev/zero must be Unreadable, got {other:?}"),
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let huge = dir.path().join("huge.pem");
        std::fs::write(&huge, vec![b'\n'; MAX_EXTRA_CA_CERTS_BYTES + 1]).unwrap();
        match read_path(
            &huge,
            ExtraRootsSource::ConfigPath {
                path: huge.clone(),
                tier: None,
            },
        ) {
            Err(TlsError::Unreadable { io, .. }) => {
                assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput);
                assert_eq!(io.to_string(), format!("over the {MAX_EXTRA_CA_CERTS_BYTES}-byte cap"));
            }
            other => panic!("an over-cap file must be Unreadable, got {other:?}"),
        }
    }

    /// An `EnvPath` over 256 bytes or carrying a newline renders
    /// as `OCX_EXTRA_CA_CERTS (<N> bytes, not a readable path)`; at exactly
    /// 256 bytes and newline-free it is still a path and prints verbatim.
    #[test]
    fn extra_ca_env_path_source_redacts_a_pathological_value_to_a_byte_count() {
        let (pem_text, _) = pem_text_and_body_probe();
        let expected = format!("OCX_EXTRA_CA_CERTS ({} bytes, not a readable path)", pem_text.len());
        assert_eq!(
            ExtraRootsSource::EnvPath(PathBuf::from(&pem_text)).to_string(),
            expected
        );

        let with_newline = "/etc/ssl/corp\n.pem";
        assert_eq!(
            ExtraRootsSource::EnvPath(PathBuf::from(with_newline)).to_string(),
            format!("OCX_EXTRA_CA_CERTS ({} bytes, not a readable path)", with_newline.len())
        );

        let long = "/".repeat(257);
        assert_eq!(
            ExtraRootsSource::EnvPath(PathBuf::from(&long)).to_string(),
            "OCX_EXTRA_CA_CERTS (257 bytes, not a readable path)"
        );

        let at_limit = "/".repeat(256);
        assert_eq!(
            ExtraRootsSource::EnvPath(PathBuf::from(&at_limit)).to_string(),
            format!("OCX_EXTRA_CA_CERTS={at_limit}"),
            "256 bytes is a path, not 'longer than 256 bytes'"
        );
    }

    /// The same redaction for a `config.toml` path — the rendering names
    /// the key, the tier and a byte count, never the value; an ordinary path
    /// prints as `extra_ca_certs=<path> (<tier>)`, so a refusal from any of a
    /// host's three `config.toml`s says which door and which file — the bare
    /// path it used to print named neither (review r1, H1).
    ///
    /// Mutation: drop the `extra_ca_certs=` prefix from the `ConfigPath` arm
    /// and every `assert_eq!` on an ordinary path here reds.
    #[test]
    fn extra_ca_config_path_source_names_the_key_and_tier_and_redacts_a_pathological_value() {
        let (pem_text, body_probe) = pem_text_and_body_probe();
        let rendered = ExtraRootsSource::ConfigPath {
            path: PathBuf::from(&pem_text),
            tier: Some(ConfigTier::System),
        }
        .to_string();
        assert!(!rendered.contains("-----BEGIN"), "{rendered}");
        assert!(!rendered.contains(&body_probe), "{rendered}");
        assert_eq!(
            rendered,
            format!(
                "extra_ca_certs ({} bytes, not a readable path) ({})",
                pem_text.len(),
                ConfigTier::System
            ),
            "redaction names the key, the byte count and the tier"
        );

        let ordinary = "/etc/ssl/corp.pem";
        assert_eq!(
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from(ordinary),
                tier: Some(ConfigTier::Home),
            }
            .to_string(),
            format!("extra_ca_certs={ordinary} ({})", ConfigTier::Home)
        );
        // `ocx config push` reads the payload it is about to publish — no
        // tier to name, and none invented.
        assert_eq!(
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from(ordinary),
                tier: None,
            }
            .to_string(),
            format!("extra_ca_certs={ordinary}")
        );
        assert_eq!(ExtraRootsSource::Env.to_string(), "OCX_EXTRA_CA_CERTS");
        assert_eq!(
            ExtraRootsSource::EnvPath(PathBuf::from(ordinary)).to_string(),
            format!("OCX_EXTRA_CA_CERTS={ordinary}")
        );
        assert_eq!(
            ExtraRootsSource::ConfigInline(ConfigTier::Home).to_string(),
            format!("extra_ca_certs_pem ({})", ConfigTier::Home)
        );
    }

    /// `NotACertificate` echoes the offending tag truncated — a
    /// 300-byte "tag" (the shape of a body line mistaken for a header) renders
    /// to a bounded, `{:?}`-escaped stub, never the whole thing.
    #[test]
    fn extra_ca_not_a_certificate_display_truncates_a_long_tag() {
        let tag: String = "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo"
            .chars()
            .cycle()
            .take(300)
            .collect();
        let rendered = TlsError::NotACertificate {
            origin: ExtraRootsSource::Env,
            tag: tag.clone(),
        }
        .to_string();

        assert!(
            !rendered.contains(&tag[..48]),
            "the tag is echoed untruncated: {rendered}"
        );
        assert!(rendered.contains("..."), "truncation is marked: {rendered}");
        assert!(
            rendered.is_ascii(),
            "a rendered message stays ASCII (WinPS 5.1): {rendered}"
        );
        // Origin (18) + wording + at most ~48 for the tag stub.
        assert!(rendered.len() <= 128, "{} chars: {rendered}", rendered.len());
        // Positive control: a short tag renders whole.
        let short = TlsError::NotACertificate {
            origin: ExtraRootsSource::Env,
            tag: "PRIVATE KEY".to_owned(),
        }
        .to_string();
        assert!(short.contains("PRIVATE KEY"), "{short}");
    }

    // ── The `TooLarge` message. Its exit codes moved to ocx_cli::exit ───────

    fn file_origins() -> [ExtraRootsSource; 2] {
        [
            ExtraRootsSource::EnvPath(PathBuf::from("/etc/ssl/corp.pem")),
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from("/etc/ssl/corp.pem"),
                tier: Some(ConfigTier::User),
            },
        ]
    }

    /// The 74/78 split by origin is asserted where `classify()` now lives —
    /// `ocx_cli::exit::ocx_config`. What stays here is the message.
    #[test]
    fn extra_ca_too_large_message_names_the_byte_limit() {
        for origin in file_origins() {
            let error = TlsError::TooLarge {
                origin,
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            };
            // The message names the bound it enforces (review r1).
            assert!(
                error
                    .to_string()
                    .contains(&format!("over the {MAX_EXTRA_CA_CERTS_BYTES}-byte limit")),
                "{error}"
            );
        }
    }

    /// The env arm: `-----BEGIN` anywhere is the text itself under the
    /// `Env` origin; anything else is a path read as-is under `EnvPath`;
    /// a non-regular file is `Unreadable` under that same `EnvPath`.
    #[test]
    fn extra_ca_from_env_value_sniffs_inline_text_reads_a_path_and_refuses_a_device() {
        let pem = TestPki::mint().root_pem();
        let inline = format!("subject=CN=corp\n{pem}");
        let (bytes, origin) = from_env_value(&inline).expect("inline text");
        assert_eq!(bytes, inline.as_bytes(), "inline text is returned as given");
        assert!(matches!(origin, ExtraRootsSource::Env), "{origin:?}");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corp-ca.pem");
        // A Latin-1 `subject=` label ahead of the block: not UTF-8, which the
        // env-path arm must hand through untouched — `parse_pem` skips leading
        // text, and the config-path arm reads the same file the same way.
        let mut on_disk = b"subject=CN=corp caf\xe9\n".to_vec();
        on_disk.extend_from_slice(pem.as_bytes());
        std::fs::write(&path, &on_disk).unwrap();
        let (bytes, origin) = from_env_value(path.to_str().unwrap()).expect("path read");
        assert_eq!(bytes, on_disk, "a path's bytes are returned as read, UTF-8 or not");
        assert!(parse_pem(&bytes, &origin).is_ok(), "and they validate");
        assert!(
            matches!(&origin, ExtraRootsSource::EnvPath(read) if read == &path),
            "{origin:?}"
        );

        #[cfg(unix)]
        match from_env_value("/dev/zero") {
            Err(TlsError::Unreadable {
                origin: ExtraRootsSource::EnvPath(read),
                io,
            }) => {
                assert_eq!(read, PathBuf::from("/dev/zero"));
                assert_eq!(io.kind(), std::io::ErrorKind::InvalidInput, "{io}");
            }
            other => panic!("/dev/zero must be Unreadable under EnvPath, got {other:?}"),
        }

        // A whitespace-only value is not "unset" (only `""` is) and holds no
        // `-----BEGIN`, so it is a path — one that does not exist: 74, the
        // same door as any other typo, never a silent no-op. Windows refuses
        // the name's syntax (`InvalidFilename`, os error 123) before it ever
        // looks the path up; both kinds are the same door.
        match from_env_value("  \n") {
            Err(TlsError::Unreadable {
                origin: ExtraRootsSource::EnvPath(_),
                io,
            }) => assert!(
                matches!(
                    io.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
                ),
                "{io}"
            ),
            other => panic!("a whitespace-only value must be Unreadable under EnvPath, got {other:?}"),
        }
    }

    // ── the resolution ladder and the Sigstore fold ─────────────────────────

    /// A real self-signed CA (the test stack's Fulcio root, `basicConstraints
    /// CA:TRUE`) — the parser runs a real X.509 parse and a probe build, so
    /// only real material passes it.
    const EXTRA_CA_PEM: &str = include_str!("../../../test/sigstore/keys/fulcio-ca.crt.pem");
    /// A `PRIVATE KEY` block: valid PEM, not a certificate — the value that
    /// makes every arm of the ladder REFUSE, so the refusal's `origin` tells
    /// which arm was consulted.
    const NOT_A_CA_PEM: &str = include_str!("../../../test/sigstore/keys/fulcio-ca.key.pem");

    fn config_with_pem(pem: &str) -> Config {
        Config {
            extra_ca_certs_pem: Some(pem.to_string()),
            ..Default::default()
        }
    }

    fn config_with_path(path: &Path) -> Config {
        Config {
            extra_ca_certs: Some(path.to_path_buf()),
            ..Default::default()
        }
    }

    fn write_pem(dir: &tempfile::TempDir, name: &str, pem: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, pem).expect("fixture write");
        path
    }

    /// An env value carrying `-----BEGIN` is inline PEM text — parsed
    /// as the `Env` origin, never read as a path.
    #[test]
    fn extra_ca_resolve_env_inline_text_is_the_env_origin() {
        let roots = resolve_extra_roots(&Config::default(), Some(EXTRA_CA_PEM), Some(ConfigTier::Home))
            .expect("inline CA parses");
        assert_eq!(roots.len(), 1);

        match resolve_extra_roots(&Config::default(), Some(NOT_A_CA_PEM), Some(ConfigTier::Home)) {
            Err(TlsError::NotACertificate {
                origin: ExtraRootsSource::Env,
                ..
            }) => {}
            other => panic!("inline text must refuse with the Env origin, got {other:?}"),
        }
    }

    /// An env value without `-----BEGIN` is a path, read through the
    /// bounded read as the `EnvPath` origin.
    #[test]
    fn extra_ca_resolve_env_path_is_read_as_the_env_path_origin() {
        let dir = tempfile::tempdir().unwrap();
        let ca = write_pem(&dir, "corp-ca.pem", EXTRA_CA_PEM);
        let roots = resolve_extra_roots(&Config::default(), Some(ca.to_str().unwrap()), Some(ConfigTier::Home))
            .expect("path CA reads");
        assert_eq!(roots.len(), 1);

        let key = write_pem(&dir, "not-a-ca.pem", NOT_A_CA_PEM);
        match resolve_extra_roots(&Config::default(), Some(key.to_str().unwrap()), Some(ConfigTier::Home)) {
            Err(TlsError::NotACertificate {
                origin: ExtraRootsSource::EnvPath(path),
                ..
            }) => assert_eq!(path, key),
            other => panic!("a path value must refuse with the EnvPath origin, got {other:?}"),
        }
    }

    /// A path to a non-regular file is `Unreadable`
    /// (the bounded read refuses it), never a hang and never an empty set.
    #[cfg(unix)]
    #[test]
    fn extra_ca_resolve_env_path_to_dev_zero_is_unreadable() {
        match resolve_extra_roots(&Config::default(), Some("/dev/zero"), Some(ConfigTier::Home)) {
            Err(TlsError::Unreadable {
                origin: ExtraRootsSource::EnvPath(path),
                ..
            }) => assert_eq!(path, Path::new("/dev/zero")),
            other => panic!("/dev/zero must be Unreadable with the EnvPath origin, got {other:?}"),
        }
    }

    /// Env set → config never consulted. The config holds a
    /// value every arm would refuse; the env value wins and resolves.
    #[test]
    fn extra_ca_resolve_env_wins_over_config() {
        let roots = resolve_extra_roots(
            &config_with_pem(NOT_A_CA_PEM),
            Some(EXTRA_CA_PEM),
            Some(ConfigTier::Home),
        )
        .expect("env wins");
        assert_eq!(roots.len(), 1, "the env value alone must be what resolved");
    }

    /// A system-locked pair ignores the env value — the config's
    /// root is the one that resolves, and the env value (one every arm
    /// refuses) is never parsed. Red state: drop the lock guard and the env
    /// value wins as in `extra_ca_resolve_env_wins_over_config`.
    #[test]
    fn extra_ca_resolve_system_lock_beats_env() {
        let config = Config {
            extra_ca_certs_system_locked: true,
            ..config_with_pem(EXTRA_CA_PEM)
        };
        let roots = resolve_extra_roots(&config, Some(NOT_A_CA_PEM), Some(ConfigTier::System))
            .expect("the locked config pair resolves; the env value is never consulted");
        assert_eq!(roots.len(), 1, "the system root alone");
    }

    /// `""` is unset — falls through to config.
    #[test]
    fn extra_ca_resolve_empty_env_falls_through_to_config() {
        let roots = resolve_extra_roots(&config_with_pem(EXTRA_CA_PEM), Some(""), Some(ConfigTier::Home))
            .expect("config applies");
        assert_eq!(roots.len(), 1);
        let none =
            resolve_extra_roots(&Config::default(), Some(""), Some(ConfigTier::Home)).expect("nothing configured");
        assert!(
            none.is_empty(),
            "\"\" with no config key is the empty set, not an error"
        );
    }

    /// `extra_ca_certs_pem` is the `ConfigInline` origin.
    #[test]
    fn extra_ca_resolve_config_pem_is_the_config_inline_origin() {
        let roots = resolve_extra_roots(&config_with_pem(EXTRA_CA_PEM), None, Some(ConfigTier::Home))
            .expect("config pem parses");
        assert_eq!(roots.len(), 1);

        match resolve_extra_roots(&config_with_pem(NOT_A_CA_PEM), None, Some(ConfigTier::Home)) {
            Err(TlsError::NotACertificate {
                origin: ExtraRootsSource::ConfigInline(_),
                ..
            }) => {}
            other => panic!("extra_ca_certs_pem must refuse with the ConfigInline origin, got {other:?}"),
        }

        // `extra_ca_certs_pem = ""` is the one reachable inline `Empty`: the
        // key is set (no fallthrough to the path form, no silent no-op) and
        // the text holds no block — 78, naming the tier.
        match resolve_extra_roots(&config_with_pem(""), None, Some(ConfigTier::System)) {
            Err(TlsError::Empty {
                origin: ExtraRootsSource::ConfigInline(ConfigTier::System),
            }) => {}
            other => panic!("an empty extra_ca_certs_pem must be Empty under ConfigInline, got {other:?}"),
        }
    }

    /// `extra_ca_certs` (already anchored absolute by the loader) is
    /// read as the `ConfigPath` origin.
    #[test]
    fn extra_ca_resolve_config_path_is_the_config_path_origin() {
        let dir = tempfile::tempdir().unwrap();
        let ca = write_pem(&dir, "corp-ca.pem", EXTRA_CA_PEM);
        let roots =
            resolve_extra_roots(&config_with_path(&ca), None, Some(ConfigTier::Home)).expect("config path reads");
        assert_eq!(roots.len(), 1);

        let absent = dir.path().join("absent-corp-ca.pem");
        match resolve_extra_roots(&config_with_path(&absent), None, Some(ConfigTier::Home)) {
            Err(TlsError::Unreadable {
                origin: ExtraRootsSource::ConfigPath { path, tier },
                ..
            }) => {
                assert_eq!(path, absent);
                assert_eq!(tier, Some(ConfigTier::Home), "the loader's tier rides the origin");
            }
            other => {
                panic!("an absent extra_ca_certs path must be Unreadable with the ConfigPath origin, got {other:?}")
            }
        }
    }

    /// Neither env nor config → the empty set, and nothing is read.
    #[test]
    fn extra_ca_resolve_neither_is_empty() {
        let roots = resolve_extra_roots(&Config::default(), None, Some(ConfigTier::Home)).expect("nothing configured");
        assert!(roots.is_empty());
    }

    /// [`sigstore_extra_roots`] folds to the `merged` view when
    /// the managed source is digest-pinned or its key did not win the fold
    /// (the loader's tier record is anything but `Managed`), else `local`.
    #[test]
    fn extra_ca_sigstore_view_follows_managed_pin() {
        let merged_config = config_with_pem(EXTRA_CA_PEM);
        let merged = resolve_extra_roots(&merged_config, None, Some(ConfigTier::Managed)).expect("managed root parses");
        let local = ExtraRoots::default();
        let unpinned = ocx_oci::PackageRef::parse("registry.corp/managed-config:stable").unwrap();
        let pinned =
            ocx_oci::PackageRef::parse(&format!("registry.corp/managed-config@sha256:{}", "a".repeat(64))).unwrap();
        assert!(
            unpinned.digest().is_none() && pinned.digest().is_some(),
            "fixture pins must differ"
        );

        // Unpinned tag + the payload set `_pem` → local only (the root reaches
        // registry/index/forge, never the Sigstore client).
        let sigstore = sigstore_extra_roots(&merged, &local, Some(ConfigTier::Managed), unpinned.digest().is_some());
        assert!(
            sigstore.is_empty(),
            "an unpinned managed source must not lend its root to the Sigstore client"
        );

        // Digest-pinned + the payload set `_pem` → merged.
        let sigstore = sigstore_extra_roots(&merged, &local, Some(ConfigTier::Managed), pinned.digest().is_some());
        assert_eq!(
            sigstore.der(),
            merged.der(),
            "a digest-pinned managed source is honoured for Sigstore"
        );

        // Unpinned, but a local tier's key won the fold (the payload set
        // none, or `--config` outranked it) → merged; nothing to gate.
        let local_root = resolve_extra_roots(&merged_config, None, Some(ConfigTier::Home)).expect("local root parses");
        for tier in [Some(ConfigTier::Home), Some(ConfigTier::Explicit), None] {
            let sigstore = sigstore_extra_roots(&merged, &local_root, tier, unpinned.digest().is_some());
            assert_eq!(
                sigstore.der(),
                merged.der(),
                "a root a local tier set is not managed material (tier {tier:?})"
            );
        }

        // No managed tier at all → merged.
        let sigstore = sigstore_extra_roots(&merged, &local_root, Some(ConfigTier::Home), false);
        assert_eq!(sigstore.der(), merged.der());
    }
}
