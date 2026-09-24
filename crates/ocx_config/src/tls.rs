// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The configured half of the operator-supplied extra CA roots
//! (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`, ocx#448): where a bundle's bytes
//! came from, the refusal that names that door, and the resolution ladder over
//! the environment and the `config.toml` tiers.
//!
//! Everything here reads a tier, a path or the environment. The value type it
//! produces, the byte cap it enforces and the process-wide Sigstore set live in
//! [`tls`](ocx_util::tls), which knows nothing about configuration: the one
//! validated choke point every root byte passes through is
//! [`ExtraRoots::from_pem`], and [`parse_pem`] is that call plus the origin a
//! refusal has to name.

use std::path::{Path, PathBuf};

use crate::loader::ConfigLoader;
use crate::{Config, ConfigTier};
use ocx_util::tls::{ExtraRoots, MAX_EXTRA_CA_CERTS_BYTES, PemBundleError};

/// Where an [`ExtraRoots`] value's bytes came from.
///
/// Carried for diagnostics only — every variant names a source, never holds
/// the PEM bytes themselves, so nothing reachable from here can leak
/// certificate (or, if an operator pastes the wrong secret, key) content into
/// an error message or a log line (D-11). `Debug` delegates to `Display` for
/// the same reason: a `{:?}` of a [`TlsError`] (or of anything carrying one)
/// must not open a second door the redaction does not watch.
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
        /// The tier whose file set the key — the loader's own record
        /// (DX-16), never a guess from the value — or `None` when the path was
        /// read outside the loader's tiers: `ocx config push` reading the
        /// payload it is about to publish.
        tier: Option<ConfigTier>,
    },
    /// Inline PEM text under `extra_ca_certs_pem` in a `config.toml` tier,
    /// tagged with the tier that supplied it.
    ConfigInline(ConfigTier),
}

impl std::fmt::Debug for ExtraRootsSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl ExtraRootsSource {
    /// Longest path echoed verbatim into a message; anything longer is a
    /// value that is not a path (D-11).
    const MAX_ECHOED_PATH_BYTES: usize = 256;

    /// Whether the bytes came from a file the operator named (`EnvPath`,
    /// `ConfigPath`) rather than inline text — the split C-010's exit codes
    /// turn on.
    pub fn is_file(&self) -> bool {
        matches!(self, Self::EnvPath(_) | Self::ConfigPath { .. })
    }

    /// The byte count of a path that must not be echoed — over 256 bytes or
    /// carrying a newline, the shape of PEM text landed in a path-typed
    /// variable by mistake (D-11) — or `None` for a path safe to print.
    fn redacted_len(path: &Path) -> Option<usize> {
        let bytes = path.as_os_str().as_encoded_bytes();
        (bytes.len() > Self::MAX_ECHOED_PATH_BYTES || bytes.contains(&b'\n')).then_some(bytes.len())
    }
}

impl std::fmt::Display for ExtraRootsSource {
    /// Names the door the operator used, never the value that came through it.
    ///
    /// Every variant names its door — the variable or the config key — and a
    /// config-sourced one names the tier's file too, so a refusal on a host
    /// with three `config.toml`s says which one to open. The two path forms
    /// render as `<door>=<path>`: `EnvPath` is the *same* variable as `Env`
    /// read as a path rather than inline text (D-4), and `ConfigPath` is the
    /// key `extra_ca_certs` — a bare path named neither.
    ///
    /// A path over 256 bytes or carrying a newline is not echoed at all — it
    /// renders as `<door> (<N> bytes, not a readable path)`, so an operator
    /// who exported PEM text (or the wrong secret) into the path-typed form
    /// never sees it copied into a CI log (D-11, CWE-532).
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

/// The PEM tag `TlsError::NotACertificate` echoes, bounded and `{:?}`-escaped:
/// the `pem` crate takes the tag as everything between `-----BEGIN ` and the
/// next `-----`, so a body line mistaken for a header would otherwise echo up
/// to the whole bundle (D-11).
fn truncated_tag(tag: &str) -> String {
    const MAX_TAG_BYTES: usize = 32;
    if tag.len() <= MAX_TAG_BYTES {
        return format!("{tag:?}");
    }
    // ASCII marker: a rendered string is what a Windows PowerShell 5.1
    // console shows, and U+2026 there is mojibake.
    let prefix = &tag[..tag.floor_char_boundary(MAX_TAG_BYTES)];
    format!("{prefix:?}...")
}

/// Failure parsing, validating, or reading operator-supplied extra CA root
/// material (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`, ocx#448).
///
/// Every variant carries only a source, a byte count, a block index, or a PEM
/// tag — never the certificate (or mistakenly-pasted key) bytes themselves
/// (D-11): a `Display` reachable from this type can land in CI logs. The tag
/// is the one field that is operator text; its `Display` truncates it to 32
/// bytes and escapes it, so even a bundle whose whole body was parsed as a
/// tag echoes a bounded prefix.
///
/// This is the *only* rendered form of a refused bundle. The origin-free
/// discriminant [`PemBundleError`] that [`ExtraRoots::from_pem`] raises is
/// converted here, in `parse_pem`, and implements neither `Display` nor
/// `std::error::Error` — so it cannot be printed, cannot be a `#[source]`, and
/// has no way to reach an operator at all.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// The combined PEM bundle exceeds [`MAX_EXTRA_CA_CERTS_BYTES`] — in
    /// practice from inline text (an environment variable's value, or
    /// `extra_ca_certs_pem`): a file over the cap is refused by
    /// [`ocx_util::fs::read_bounded`] before its bytes reach
    /// `parse_pem`, and surfaces as [`TlsError::Unreadable`].
    #[error(
        "{origin} is {bytes} bytes, over the {}-byte limit for a CA bundle",
        MAX_EXTRA_CA_CERTS_BYTES
    )]
    TooLarge {
        /// Where the oversized text came from.
        ///
        /// Named `origin`, not `source`: thiserror treats a field literally
        /// named `source` as the `#[source]` error (`Error::source()`), which
        /// would require [`ExtraRootsSource`] to implement
        /// `std::error::Error` — it is a plain diagnostic label, not an error.
        origin: ExtraRootsSource,
        /// Its length in bytes.
        bytes: usize,
    },

    /// A PEM block's tag is not `CERTIFICATE` — refused rather than silently
    /// skipped, so a pasted private key is never dropped without a diagnostic
    /// (D-10).
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

    /// A `CERTIFICATE` block failed to decode as X.509, the bundle's PEM
    /// framing could not be decoded (bad base64, mismatched `BEGIN`/`END`
    /// tags), or the platform TLS verifier's probe build rejected the set as
    /// anchors — the acceptance test D-9's fallback arms must never be
    /// reached from instead.
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
        /// The zero-based index of the PEM block that failed to parse
        /// (one-based in the message), or `None` when the failure is not
        /// attributable to one block: the PEM decoder refusing the bundle,
        /// or the platform-verifier probe build rejecting the set.
        index: Option<usize>,
    },

    /// A `-----BEGIN` line the decoder could not complete — a bundle cut off
    /// mid-copy. Its own variant, not a [`TlsError::Malformed`] index: the
    /// fix is to paste the rest, not to hunt an OS trust-store
    /// incompatibility. Classifies with `Malformed` (65 file / 78 inline).
    #[error("{origin}: certificate block {} is incomplete (no -----END line)", index + 1)]
    Truncated {
        /// Where the bundle came from.
        origin: ExtraRootsSource,
        /// The zero-based index of the first block with no end line
        /// (one-based in the message).
        index: usize,
    },

    /// A path-typed source could not be read as a bounded, regular file — see
    /// [`ocx_util::fs::read_bounded`].
    #[error("cannot read {origin}")]
    Unreadable {
        /// The path-typed source that was refused — `EnvPath` or `ConfigPath`
        /// already carry the path, so this variant holds no separate one.
        origin: ExtraRootsSource,
        /// What the read raised, with the path stripped: `origin` names it
        /// (and redacts it, D-11), and `{err:#}` at the CLI boundary renders
        /// this whole chain, so a second copy here would echo a value the
        /// origin refused to. See `read_path`.
        #[source]
        io: std::io::Error,
    },
}

/// C-005's environment arm, shared by `Context::try_init`'s resolution
/// ladder and `ocx self setup`'s persistence phase: a non-empty
/// `OCX_EXTRA_CA_CERTS` value containing `-----BEGIN` is the PEM text
/// itself (D-4 — a path can never contain it); anything else is a path,
/// read now through [`ocx_util::fs::read_bounded`] under
/// [`MAX_EXTRA_CA_CERTS_BYTES`] (D-5, D-10). Returns the bytes as read —
/// never re-encoded, so a caller persisting them stores the operator's
/// bundle — with the [`ExtraRootsSource`] to validate them under. Bytes,
/// not text: [`parse_pem`] is bytes-native, and the config-path arm
/// hands it the same raw read, so one file resolves identically through
/// either door (C-005); a caller that needs text decodes for itself.
///
/// Validation is the caller's next step ([`parse_pem`]); an empty
/// `value` is "unset" and never reaches here.
///
/// **Blocking** on the path arm (a file read).
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

/// C-005's path arm — the one bounded read behind every path-typed
/// source: [`from_env_value`], the `extra_ca_certs` arm of
/// `Context::try_init`'s ladder, and `ocx config push`'s inlining read.
/// Reads `path` through [`ocx_util::fs::read_bounded`] under
/// [`MAX_EXTRA_CA_CERTS_BYTES`] (D-10) and returns the bytes as read with
/// the `origin` to validate them under ([`parse_pem`] is the
/// caller's next step).
///
/// The refusal carries the path **once**, in `origin`: `read_bounded`'s
/// own error names the path in its `Display`, unredacted, and the CLI
/// prints the whole `source()` chain — so a PEM body pasted into a
/// path-typed value would be echoed through the chain even though
/// `origin` redacts it (D-11, CWE-532). The `std::io::Error` kept here is
/// the OS error itself, or an `InvalidInput` naming only the rule
/// (`not a regular file`, `over the <N>-byte cap`).
///
/// **Blocking.**
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

/// [`ExtraRoots::from_pem`] — the one validated choke point every
/// extra-CA-roots byte passes through (C-004, D-9) — with the `origin` a
/// refusal has to name attached.
///
/// The parse itself, its check order and its platform probe build live on
/// [`ExtraRoots`]; what this adds is the door. The mapping below is
/// one-to-one and total, so every discriminant [`ExtraRoots::from_pem`]
/// can raise has exactly one rendered form and no bundle is ever refused
/// without one.
///
/// **Blocking**, for [`ExtraRoots::from_pem`]'s reasons: the probe build loads
/// the platform trust store. Call from a blocking context; from async, wrap the
/// read and the parse together in `spawn_blocking`.
///
/// # Errors
///
/// [`TlsError::TooLarge`] over [`MAX_EXTRA_CA_CERTS_BYTES`],
/// [`TlsError::Truncated`] for a `-----BEGIN` with no matching end (a
/// bundle cut off mid-copy, naming that block's index),
/// [`TlsError::NotACertificate`] for a non-`CERTIFICATE` PEM block,
/// [`TlsError::Empty`] for zero blocks, [`TlsError::Malformed`] for a
/// bundle the PEM decoder refuses, a block that fails to parse as X.509,
/// or a set the platform TLS verifier's probe build rejects.
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

/// C-005: the extra-CA-roots resolution ladder for one view (`merged` or
/// `local` — the caller decides which `config` and which `env` value to pass;
/// `sigstore` is a fold over the two, [`sigstore_extra_roots`], not a third
/// walk of this ladder).
///
/// Ladder: `env` set and non-empty → `from_env_value` (a value
/// containing `-----BEGIN` is inline PEM text, `ExtraRootsSource::Env`, D-4;
/// else a path, `ExtraRootsSource::EnvPath`, read via
/// `read_path`) — unless `config.extra_ca_certs_system_locked`
/// (ocx#469), which ignores `env` with a warning; `env` unset or `""` →
/// `config.extra_ca_certs_pem` inline (`ExtraRootsSource::ConfigInline`), else
/// `config.extra_ca_certs` as a path (`ExtraRootsSource::ConfigPath`, the same
/// `read_path`); neither set → the empty [`ExtraRoots`]. Every
/// non-empty candidate passes through `parse_pem` (C-004), the
/// single validated choke point (D-9).
///
/// `tier` is the loader's own record of which tier set the config key
/// (`LoadedConfig::extra_ca_certs_tier`, DX-16) — the origin a refusal names
/// is the file that set the value, never a guess from the value. A `Config`
/// carrying `extra_ca_certs_pem` with no recorded tier cannot come out of the
/// loader; a hand-built one is attributed to `$OCX_HOME/config.toml`.
///
/// **Blocking** (DX-11): a path read plus `parse_pem`'s probe
/// build both do blocking I/O. The CLI's `Context::try_init` is `async fn`,
/// so it runs this inside `tokio::task::spawn_blocking`, mapping a
/// `JoinError` to an I/O-class failure — never `NotFound` — matching
/// `oci/verify/trust_resolve.rs`'s precedent.
///
/// # Errors
///
/// [`TlsError::Unreadable`] when a path-typed value cannot be read as a
/// bounded regular file, else any [`TlsError`] `parse_pem`
/// raises, propagated verbatim — a `TlsError` from any view aborts
/// `Context::try_init` (D-9).
pub fn resolve_extra_roots(
    config: &Config,
    env: Option<&str>,
    tier: Option<ConfigTier>,
) -> Result<ExtraRoots, TlsError> {
    let env = env.filter(|value| !value.is_empty());
    // ocx#469: a system-locked pair outranks the environment too — the one
    // rung the loader's lock cannot reach, since the env is not a tier it
    // folds. Same voice as the loader's per-tier warning; never the value
    // (D-11).
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
                // The loader records a tier for every key it folds in
                // (`load_and_merge_recording`, `fold_managed_tier`); a
                // `None` here is a hand-built `Config`, never a loaded one.
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

/// D-7: the Sigstore trust-services view, folded from the two resolved views.
///
/// `merged` when the resolved `[managed] source` carries a digest pin
/// (`managed_source_pinned` — the caller passes `.digest().is_some()`, or
/// `false` when no managed tier resolved), **or** when the managed payload's
/// key did not win the fold — `extra_ca_tier` is the loader's record of which
/// tier set the surviving key (DX-16), so anything but `Managed` means the
/// merged and local views resolved the same material; else `local`.
///
/// Pure so the fold is unit-testable (`extra_ca_sigstore_view_follows_managed_pin`);
/// the CLI's `Context::try_init` calls it once and installs the result via
/// [`install_sigstore_roots`](ocx_util::tls::install_sigstore_roots) before any
/// client is built. Takes a `bool` rather than `Option<&ocx_oci::PackageRef>` —
/// this module has no other reason to know that type, and the caller already
/// has the pin question answered.
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

    // ── C-004: the parse matrix ──────────────────────────────────────────────

    /// C-004 / edge case "bundle with 2+ certificates → all appended".
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

    /// C-004 / edge case "CRLF PEM, leading `subject=` labels before
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

    /// C-004 / D-10: a non-`CERTIFICATE` tag refuses — a pasted private key is
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

    /// C-004 / D-10: the same refusal when a valid certificate sits beside the
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

    /// C-004 / D-10: OpenSSL's `-trustout` output is tagged
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

    /// C-005 env arm: a UTF-8 BOM ahead of `-----BEGIN` (a bundle saved by a
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

    /// C-004: zero blocks → `Empty`, for both the literal empty value and a
    /// whitespace-only one (an env var set to a blank line).
    #[test]
    fn extra_ca_parse_pem_refuses_empty_and_whitespace_only_input() {
        assert!(matches!(parse(b""), Err(TlsError::Empty { .. })), "empty");
        assert!(
            matches!(parse(b"  \r\n\t\n"), Err(TlsError::Empty { .. })),
            "whitespace only"
        );
    }

    /// C-004 (L2): a bundle cut off after `-----BEGIN` — the `pem` decoder
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

    /// C-004: a complete certificate followed by a truncated one is refused
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

    /// C-004: the truncation count uses the decoder's own needle. A leading
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

    /// C-004 / D-10: the size check runs first and is exact — one byte over
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

    /// C-004: a `CERTIFICATE` block whose body is not an X.509 certificate is
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

    /// C-004 / S-007: a block that parses as X.509 but that the platform
    /// verifier refuses as an anchor — a pre-v3 certificate — is `Malformed`
    /// from the probe build, so no client is ever built with fewer roots than
    /// configured (D-9). Linux-only by design: only webpki refuses it;
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

    // ── C-043: every arm's rendered text, against a literal ─────────────────

    /// C-043: the operator-facing wording of every `TlsError` arm, pinned to
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

    // ── D-11: nothing reachable from an error echoes certificate bytes ──────

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

    /// D-11: every `TlsError` variant, over every source shape including the
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

    /// D-11 through the real reader: `Unreadable` built by [`read_path`]
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

    /// D-11 / DX-9: an `EnvPath` over 256 bytes or carrying a newline renders
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

    /// D-11: the same redaction for a `config.toml` path — the rendering names
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

    /// D-11 / DX-9: `NotACertificate` echoes the offending tag truncated — a
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

    /// C-005's env arm: `-----BEGIN` anywhere is the text itself under the
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

    // ── C-005 / D-7: the resolution ladder and the Sigstore fold ────────────

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

    /// C-005: an env value carrying `-----BEGIN` is inline PEM text — parsed
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

    /// C-005: an env value without `-----BEGIN` is a path, read through the
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

    /// C-005 / S-007 / D-10: a path to a non-regular file is `Unreadable`
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

    /// C-005 / S-005: env set → config never consulted. The config holds a
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

    /// ocx#469: a system-locked pair ignores the env value — the config's
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

    /// C-005 / S-005: `""` is unset — falls through to config.
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

    /// C-005: `extra_ca_certs_pem` is the `ConfigInline` origin.
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

    /// C-005: `extra_ca_certs` (already anchored absolute by the loader) is
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

    /// C-005: neither env nor config → the empty set, and nothing is read.
    #[test]
    fn extra_ca_resolve_neither_is_empty() {
        let roots = resolve_extra_roots(&Config::default(), None, Some(ConfigTier::Home)).expect("nothing configured");
        assert!(roots.is_empty());
    }

    /// D-7: [`sigstore_extra_roots`] folds to the `merged` view when
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
