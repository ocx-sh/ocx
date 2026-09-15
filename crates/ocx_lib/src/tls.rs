// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Operator-supplied extra CA roots (`OCX_EXTRA_CA_CERTS` / `extra_ca_certs`,
//! ocx#448): the validated type family, the single choke point every root
//! byte passes through, the resolution ladder over the environment and the
//! `config.toml` tiers, and the process-wide Sigstore set. Beside
//! [`trust`](crate::trust), the other trust-material ladder; the bundled
//! Mozilla seed for hand-rolled `reqwest` builders stays in
//! [`utility::tls`](crate::utility::tls).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::config::{Config, ConfigTier};
use crate::log;
#[cfg(test)]
use crate::oci;

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
/// The only way to build a non-empty value is [`ExtraRoots::parse_pem`], the
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

/// Where an [`ExtraRoots`] value's bytes came from.
///
/// Carried for diagnostics only — every variant names a source, never holds
/// the PEM bytes themselves, so nothing reachable from here can leak
/// certificate (or, if an operator pastes the wrong secret, key) content into
/// an error message or a log line (D-11). `Debug` delegates to `Display` for
/// the same reason: a `{:?}` of a `TlsError` (or of anything carrying one)
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
    fn is_file(&self) -> bool {
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
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TlsError {
    /// The combined PEM bundle exceeds [`MAX_EXTRA_CA_CERTS_BYTES`] — in
    /// practice from inline text (an environment variable's value, or
    /// `extra_ca_certs_pem`): a file over the cap is refused by
    /// [`crate::utility::fs::read_bounded`] before its bytes reach
    /// [`ExtraRoots::parse_pem`], and surfaces as [`TlsError::Unreadable`].
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
    /// [`crate::utility::fs::read_bounded`].
    #[error("cannot read {origin}")]
    Unreadable {
        /// The path-typed source that was refused — `EnvPath` or `ConfigPath`
        /// already carry the path, so this variant holds no separate one.
        origin: ExtraRootsSource,
        /// What the read raised, with the path stripped: `origin` names it
        /// (and redacts it, D-11), and `{err:#}` at the CLI boundary renders
        /// this whole chain, so a second copy here would echo a value the
        /// origin refused to. See [`ExtraRoots::read_path`].
        #[source]
        io: std::io::Error,
    },
}

impl ClassifyExitCode for TlsError {
    /// C-010: a file the operator named that this process could not use as
    /// given is 74 (`Unreadable`, and `TooLarge` from a file — parity with
    /// `[trust.sigstore]`'s `trust_resolve.rs`); content refused from a file
    /// is the file's data being wrong, 65; the same refusal from inline text
    /// (the env value, `extra_ca_certs_pem`) is the configuration itself
    /// being wrong, 78.
    fn classify(&self) -> Option<ExitCode> {
        let code = match self {
            Self::Unreadable { .. } => ExitCode::IoError,
            Self::TooLarge { origin, .. } if origin.is_file() => ExitCode::IoError,
            Self::TooLarge { .. } => ExitCode::ConfigError,
            Self::NotACertificate { origin, .. }
            | Self::Empty { origin }
            | Self::Malformed { origin, .. }
            | Self::Truncated { origin, .. } => {
                if origin.is_file() {
                    ExitCode::DataError
                } else {
                    ExitCode::ConfigError
                }
            }
        };
        Some(code)
    }
}

impl ExtraRoots {
    /// C-005's environment arm, shared by `Context::try_init`'s resolution
    /// ladder and `ocx self setup`'s persistence phase: a non-empty
    /// `OCX_EXTRA_CA_CERTS` value containing `-----BEGIN` is the PEM text
    /// itself (D-4 — a path can never contain it); anything else is a path,
    /// read now through [`crate::utility::fs::read_bounded`] under
    /// [`MAX_EXTRA_CA_CERTS_BYTES`] (D-5, D-10). Returns the bytes as read —
    /// never re-encoded, so a caller persisting them stores the operator's
    /// bundle — with the [`ExtraRootsSource`] to validate them under. Bytes,
    /// not text: [`Self::parse_pem`] is bytes-native, and the config-path arm
    /// hands it the same raw read, so one file resolves identically through
    /// either door (C-005); a caller that needs text decodes for itself.
    ///
    /// Validation is the caller's next step ([`Self::parse_pem`]); an empty
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
        Self::read_path(&path, ExtraRootsSource::EnvPath(path.clone()))
    }

    /// C-005's path arm — the one bounded read behind every path-typed
    /// source: [`Self::from_env_value`], the `extra_ca_certs` arm of
    /// `Context::try_init`'s ladder, and `ocx config push`'s inlining read.
    /// Reads `path` through [`crate::utility::fs::read_bounded`] under
    /// [`MAX_EXTRA_CA_CERTS_BYTES`] (D-10) and returns the bytes as read with
    /// the `origin` to validate them under ([`Self::parse_pem`] is the
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
        use crate::utility::fs::BoundedReadError;

        let bytes = crate::utility::fs::read_bounded(path, MAX_EXTRA_CA_CERTS_BYTES as u64).map_err(|refused| {
            let io = match refused {
                BoundedReadError::Io { source, .. } => source,
                BoundedReadError::NotRegularFile { .. } => {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file")
                }
                BoundedReadError::TooLarge { cap, .. } => {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("over the {cap}-byte cap"))
                }
            };
            TlsError::Unreadable {
                origin: origin.clone(),
                io,
            }
        })?;
        Ok((bytes, origin))
    }

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
    /// [`TlsError::TooLarge`] over [`MAX_EXTRA_CA_CERTS_BYTES`],
    /// [`TlsError::Truncated`] for a `-----BEGIN` with no matching end (a
    /// bundle cut off mid-copy, naming that block's index),
    /// [`TlsError::NotACertificate`] for a non-`CERTIFICATE` PEM block,
    /// [`TlsError::Empty`] for zero blocks, [`TlsError::Malformed`] for a
    /// bundle the PEM decoder refuses, a block that fails to parse as X.509,
    /// or a set the platform TLS verifier's probe build rejects.
    pub fn parse_pem(pem: &[u8], origin: &ExtraRootsSource) -> Result<Self, TlsError> {
        use x509_cert::der::Decode as _;

        if pem.len() > MAX_EXTRA_CA_CERTS_BYTES {
            return Err(TlsError::TooLarge {
                origin: origin.clone(),
                bytes: pem.len(),
            });
        }
        let blocks = pem::parse_many(pem).map_err(|_| TlsError::Malformed {
            origin: origin.clone(),
            index: None,
        })?;
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
            return Err(TlsError::Truncated {
                origin: origin.clone(),
                index: blocks.len(),
            });
        }
        if let Some(block) = blocks.iter().find(|block| block.tag() != "CERTIFICATE") {
            return Err(TlsError::NotACertificate {
                origin: origin.clone(),
                tag: block.tag().to_owned(),
            });
        }
        if blocks.is_empty() {
            return Err(TlsError::Empty { origin: origin.clone() });
        }
        let mut roots = Self {
            der: Vec::with_capacity(blocks.len()),
            certificates: Vec::with_capacity(blocks.len()),
        };
        for (index, block) in blocks.into_iter().enumerate() {
            let der = block.into_contents();
            let malformed = || TlsError::Malformed {
                origin: origin.clone(),
                index: Some(index),
            };
            x509_cert::Certificate::from_der(&der).map_err(|_| malformed())?;
            roots
                .certificates
                .push(reqwest::Certificate::from_der(&der).map_err(|_| malformed())?);
            roots.der.push(pki_types::CertificateDer::from(der));
        }
        roots
            .seed(reqwest::Client::builder())
            .build()
            .map_err(|_| TlsError::Malformed {
                origin: origin.clone(),
                index: None,
            })?;
        Ok(roots)
    }

    /// Chains every root onto `builder` via `tls_certs_merge` (not the
    /// deprecated `add_root_certificate`) — additive, so it composes with
    /// [`seed_embedded_roots`](crate::utility::tls::seed_embedded_roots) in
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

/// C-005: the extra-CA-roots resolution ladder for one view (`merged` or
/// `local` — the caller decides which `config` and which `env` value to pass;
/// `sigstore` is a fold over the two, [`sigstore_extra_roots`], not a third
/// walk of this ladder).
///
/// Ladder: `env` set and non-empty → [`ExtraRoots::from_env_value`] (a value
/// containing `-----BEGIN` is inline PEM text, `ExtraRootsSource::Env`, D-4;
/// else a path, `ExtraRootsSource::EnvPath`, read via
/// [`ExtraRoots::read_path`]) — unless `config.extra_ca_certs_system_locked`
/// (ocx#469), which ignores `env` with a warning; `env` unset or `""` →
/// `config.extra_ca_certs_pem` inline (`ExtraRootsSource::ConfigInline`), else
/// `config.extra_ca_certs` as a path (`ExtraRootsSource::ConfigPath`, the same
/// [`ExtraRoots::read_path`]); neither set → the empty [`ExtraRoots`]. Every
/// non-empty candidate passes through [`ExtraRoots::parse_pem`] (C-004), the
/// single validated choke point (D-9).
///
/// `tier` is the loader's own record of which tier set the config key
/// (`LoadedConfig::extra_ca_certs_tier`, DX-16) — the origin a refusal names
/// is the file that set the value, never a guess from the value. A `Config`
/// carrying `extra_ca_certs_pem` with no recorded tier cannot come out of the
/// loader; a hand-built one is attributed to `$OCX_HOME/config.toml`.
///
/// **Blocking** (DX-11): a path read plus [`ExtraRoots::parse_pem`]'s probe
/// build both do blocking I/O. The CLI's `Context::try_init` is `async fn`,
/// so it runs this inside `tokio::task::spawn_blocking`, mapping a
/// `JoinError` to an I/O-class failure — never `NotFound` — matching
/// `oci/verify/trust_resolve.rs`'s precedent.
///
/// # Errors
///
/// [`TlsError::Unreadable`] when a path-typed value cannot be read as a
/// bounded regular file, else any [`TlsError`] [`ExtraRoots::parse_pem`]
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
            crate::config::loader::ConfigLoader::system_path().display()
        );
        None
    } else {
        env
    };
    match env {
        Some(value) => {
            let (pem, origin) = ExtraRoots::from_env_value(value)?;
            ExtraRoots::parse_pem(&pem, &origin)
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
                ExtraRoots::parse_pem(
                    pem.as_bytes(),
                    &ExtraRootsSource::ConfigInline(tier.unwrap_or(ConfigTier::Home)),
                )
            }
            (None, Some(path)) => {
                let origin = ExtraRootsSource::ConfigPath {
                    path: path.to_path_buf(),
                    tier,
                };
                let (pem, origin) = ExtraRoots::read_path(path, origin)?;
                ExtraRoots::parse_pem(&pem, &origin)
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
/// [`install_sigstore_roots`] before any client is built. Takes a `bool`
/// rather than `Option<&oci::Identifier>` — this module has no other reason
/// to know that type, and the caller already has the pin question answered.
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

/// Test-only PKI for the extra-CA-roots tests (ocx#448): a minted P-256 root,
/// a leaf for `localhost` / `127.0.0.1` signed by it, and an in-process
/// `tokio-rustls` server that answers any request with an empty `200 OK`.
///
/// Shared by the `tls`, `oci::index::ocx_index`, `oci::endpoint` and
/// `forge::http` handshake tests — the one cross-OS proof that a seeded root
/// is consulted at handshake time rather than merely stored (S-002 / S-007 at
/// unit tier). The certificate recipe is `oci/verify/trust_root.rs`'s
/// `real_cert_der`, extended with a leaf profile and a SAN.
#[cfg(test)]
pub(crate) mod test_pki {
    use std::net::SocketAddr;
    use std::str::FromStr as _;
    use std::sync::Arc;
    use std::time::Duration;

    use p256::ecdsa::SigningKey;
    use p256::elliptic_curve::rand_core::OsRng;
    use p256::pkcs8::EncodePrivateKey as _;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use x509_cert::builder::{Builder as _, CertificateBuilder, Profile};
    use x509_cert::der::Encode as _;
    use x509_cert::der::asn1::{Ia5String, OctetString};
    use x509_cert::ext::pkix::name::GeneralName;
    use x509_cert::ext::pkix::{ExtendedKeyUsage, SubjectAltName};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::spki::SubjectPublicKeyInfoOwned;
    use x509_cert::time::Validity;

    use super::{ExtraRoots, ExtraRootsSource};

    /// A root plus a leaf it signed, with the leaf's private key — everything
    /// an in-process TLS server needs, and the root PEM a client is seeded with.
    pub(crate) struct TestPki {
        pub(crate) root_der: Vec<u8>,
        pub(crate) leaf_der: Vec<u8>,
        pub(crate) leaf_key_pkcs8: Vec<u8>,
    }

    fn spki(key: &SigningKey) -> SubjectPublicKeyInfoOwned {
        SubjectPublicKeyInfoOwned::from_key(*key.verifying_key()).expect("spki")
    }

    fn validity() -> Validity {
        Validity::from_now(Duration::from_secs(3600)).expect("validity")
    }

    /// A self-signed CA root — `Profile::Root` adds `basicConstraints ca=true`
    /// and `keyCertSign`, which is what makes it acceptable as a trust anchor.
    pub(crate) fn mint_root(name: &str) -> (SigningKey, Vec<u8>) {
        let key = SigningKey::random(&mut OsRng);
        let builder = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(1_u32),
            validity(),
            Name::from_str(name).expect("name"),
            spki(&key),
            &key,
        )
        .expect("builder");
        let der = builder
            .build::<p256::ecdsa::DerSignature>()
            .expect("build")
            .to_der()
            .expect("der");
        (key, der)
    }

    /// A version-2 self-signed certificate: no v3 extensions, an
    /// `issuerUniqueID` present. x509-cert decodes it; webpki's trust-anchor
    /// parser allows only v3 (with a dedicated v1 fallback, so a v1 root is
    /// *accepted*) and a v2 body fails both parsers — `BadEncoding` from the
    /// probe build. CryptoAPI and Security.framework decode any version, so
    /// this is the block the *platform* probe rejects, and only on Linux.
    ///
    /// The builder's opt-out of the v3 extension set (`Profile::Manual`) is
    /// behind x509-cert's `hazmat` feature, which the workspace does not
    /// enable, so the TBS is assembled by hand and signed with the same P-256
    /// key type the builder recipe uses — the version `x509_cert`'s own
    /// `finalize` would pick for a unique id and no extensions.
    ///
    /// Gated like its one consumer: on Windows and macOS the platform probe
    /// accepts a v2 root, so the test does not exist there and an ungated
    /// helper is `dead_code` under `-D warnings`.
    #[cfg(not(any(windows, target_os = "macos")))]
    pub(crate) fn mint_v2_root() -> Vec<u8> {
        use p256::ecdsa::signature::Signer as _;
        use x509_cert::der::asn1::BitString;
        use x509_cert::spki::DynSignatureAlgorithmIdentifier as _;

        let key = SigningKey::random(&mut OsRng);
        let name = Name::from_str("CN=ocx-test-v2-ca").expect("name");
        let tbs = x509_cert::TbsCertificate {
            version: x509_cert::Version::V2,
            serial_number: SerialNumber::from(3_u32),
            signature: key.signature_algorithm_identifier().expect("algorithm"),
            issuer: name.clone(),
            validity: validity(),
            subject: name,
            subject_public_key_info: spki(&key),
            issuer_unique_id: Some(BitString::from_bytes(&[0xAB]).expect("bit string")),
            subject_unique_id: None,
            extensions: None,
        };
        let tbs_der = tbs.to_der().expect("tbs der");
        let signature: p256::ecdsa::DerSignature = key.sign(&tbs_der);
        let certificate = x509_cert::Certificate {
            signature_algorithm: tbs.signature.clone(),
            tbs_certificate: tbs,
            signature: BitString::from_bytes(signature.as_bytes()).expect("bit string"),
        };
        certificate.to_der().expect("der")
    }

    impl TestPki {
        pub(crate) fn mint() -> Self {
            let (root_key, root_der) = mint_root("CN=ocx-test-ca");
            let leaf_key = SigningKey::random(&mut OsRng);
            let mut builder = CertificateBuilder::new(
                Profile::Leaf {
                    issuer: Name::from_str("CN=ocx-test-ca").expect("name"),
                    enable_key_agreement: false,
                    enable_key_encipherment: false,
                },
                SerialNumber::from(2_u32),
                validity(),
                Name::from_str("CN=localhost").expect("name"),
                spki(&leaf_key),
                &root_key,
            )
            .expect("builder");
            builder
                .add_extension(&SubjectAltName(vec![
                    GeneralName::DnsName(Ia5String::new("localhost").expect("ia5")),
                    GeneralName::IpAddress(OctetString::new([127, 0, 0, 1]).expect("octets")),
                ]))
                .expect("san");
            // `serverAuth` is load-bearing on macOS: Security.framework's SSL
            // policy refuses a leaf without it (`EkuError`), while webpki and
            // CryptoAPI tolerate an absent extension. Every backend accepts
            // it present, so the one recipe serves all three verifiers.
            builder
                .add_extension(&ExtendedKeyUsage(vec![
                    x509_cert::der::oid::db::rfc5280::ID_KP_SERVER_AUTH,
                ]))
                .expect("eku");
            let leaf_der = builder
                .build::<p256::ecdsa::DerSignature>()
                .expect("build")
                .to_der()
                .expect("der");
            let leaf_key_pkcs8 = leaf_key.to_pkcs8_der().expect("pkcs8").as_bytes().to_vec();
            Self {
                root_der,
                leaf_der,
                leaf_key_pkcs8,
            }
        }

        /// The root as a one-block LF PEM bundle — what an operator would put
        /// in `OCX_EXTRA_CA_CERTS`.
        pub(crate) fn root_pem(&self) -> String {
            pem_block("CERTIFICATE", &self.root_der)
        }

        /// The root through the choke point (C-004) — the only constructor.
        pub(crate) fn roots(&self) -> ExtraRoots {
            ExtraRoots::parse_pem(self.root_pem().as_bytes(), &ExtraRootsSource::Env).expect("a minted root parses")
        }
    }

    /// One PEM block, LF line endings (the `pem` crate defaults to CRLF).
    pub(crate) fn pem_block(tag: &str, der: &[u8]) -> String {
        pem::encode_config(
            &pem::Pem::new(tag, der),
            pem::EncodeConfig::new().set_line_ending(pem::LineEnding::LF),
        )
    }

    /// An HTTPS server on an ephemeral loopback port, presenting `pki`'s leaf,
    /// answering every request with an empty `200 OK`. A client that does not
    /// trust the root aborts during the handshake and never reaches the
    /// request loop — that is the negative half of every handshake test.
    pub(crate) async fn serve_https(pki: &TestPki) -> SocketAddr {
        use tokio_rustls::TlsAcceptor;
        use tokio_rustls::rustls::ServerConfig;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![
                    pki_types::CertificateDer::from(pki.leaf_der.clone()),
                    pki_types::CertificateDer::from(pki.root_der.clone()),
                ],
                pki_types::PrivateKeyDer::Pkcs8(pki_types::PrivatePkcs8KeyDer::from(pki.leaf_key_pkcs8.clone())),
            )
            .expect("the minted leaf and key form a server identity");
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local address");
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut request = Vec::new();
                    let mut chunk = [0_u8; 1024];
                    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                        match tls.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(read) => request.extend_from_slice(&chunk[..read]),
                        }
                    }
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        addr
    }

    /// Every `Display` down the `source` chain, joined — so an assertion sees
    /// rustls's `UnknownIssuer` under reqwest's "error sending request".
    /// The unseeded half of every handshake proof: the platform verifier
    /// refused the minted root. webpki (Linux) and CryptoAPI (Windows) both
    /// surface `UnknownIssuer`; Security.framework (macOS) reports
    /// `errSecNotTrusted` (-67843) as "“<CN>” certificate is not trusted".
    /// A timeout, a refused connection or a name mismatch matches neither,
    /// so the assertion still discriminates a missing root from a dead
    /// server.
    pub(crate) fn assert_untrusted_root(chain: &str) {
        assert!(
            chain.contains("UnknownIssuer") || chain.contains("certificate is not trusted"),
            "expected an untrusted-root refusal in: {chain}"
        );
    }

    pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
        let mut text = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            text.push_str(": ");
            text.push_str(&cause.to_string());
            source = cause.source();
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    #[cfg(not(any(windows, target_os = "macos")))]
    use super::test_pki::mint_v2_root;
    use super::test_pki::{TestPki, assert_untrusted_root, error_chain, mint_root, pem_block, serve_https};
    use super::*;
    use crate::utility::tls::seed_embedded_roots;

    const ENV: ExtraRootsSource = ExtraRootsSource::Env;

    fn parse(pem: &[u8]) -> Result<ExtraRoots, TlsError> {
        ExtraRoots::parse_pem(pem, &ENV)
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

        assert_eq!(roots, pki.roots(), "the BOM changes nothing about the parsed set");
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

    /// D-11 through the real reader: `Unreadable` built by [`ExtraRoots::read_path`]
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
            ExtraRoots::from_env_value(&headerless),
            ExtraRoots::read_path(
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
            match ExtraRoots::read_path(
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
        match ExtraRoots::read_path(
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

    // ── C-010: exit codes — 74 / 65 / 78 by variant and by file-vs-inline ───

    fn file_origins() -> [ExtraRootsSource; 2] {
        [
            ExtraRootsSource::EnvPath(PathBuf::from("/etc/ssl/corp.pem")),
            ExtraRootsSource::ConfigPath {
                path: PathBuf::from("/etc/ssl/corp.pem"),
                tier: Some(ConfigTier::User),
            },
        ]
    }

    fn inline_origins() -> [ExtraRootsSource; 2] {
        [ExtraRootsSource::Env, ExtraRootsSource::ConfigInline(ConfigTier::User)]
    }

    /// C-010: `Unreadable` is 74 whatever the read raised — a path the
    /// operator pointed at that this process could not use as given —
    /// including the reader's own `TooLarge` and `NotRegularFile`, which
    /// `read_path` maps to `InvalidInput`, and the OS's own `NotFound` /
    /// `PermissionDenied`, which `config push` splits but the runtime does not.
    #[test]
    fn extra_ca_unreadable_classifies_as_io_error_for_every_bounded_read_error() {
        for origin in file_origins() {
            let ios = [
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a regular file"),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("over the {MAX_EXTRA_CA_CERTS_BYTES}-byte cap"),
                ),
                std::io::Error::from(std::io::ErrorKind::NotFound),
                std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            ];
            for io in ios {
                let error = TlsError::Unreadable {
                    origin: origin.clone(),
                    io,
                };
                assert_eq!(error.classify(), Some(ExitCode::IoError), "{error:?}");
            }
        }
    }

    /// C-010: content refused from a **file** the operator named is data the
    /// file holds — 65.
    #[test]
    fn extra_ca_content_errors_from_a_file_classify_as_data_error() {
        for origin in file_origins() {
            let errors = [
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
                TlsError::NotACertificate {
                    origin: origin.clone(),
                    tag: "PRIVATE KEY".to_owned(),
                },
                TlsError::Empty { origin: origin.clone() },
            ];
            for error in errors {
                assert_eq!(error.classify(), Some(ExitCode::DataError), "{error:?}");
            }
        }
    }

    /// C-010: the same content refused from **inline** text — the env value
    /// or `extra_ca_certs_pem` — is the configuration itself being wrong — 78.
    #[test]
    fn extra_ca_content_errors_from_inline_text_classify_as_config_error() {
        for origin in inline_origins() {
            let errors = [
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
                TlsError::NotACertificate {
                    origin: origin.clone(),
                    tag: "PRIVATE KEY".to_owned(),
                },
                TlsError::Empty { origin: origin.clone() },
            ];
            for error in errors {
                assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");
            }
        }
    }

    /// C-010 / DX-9: `TooLarge` splits on the source too — inline text is a
    /// config error (78); a file origin is the file being unusable (74), for
    /// parity with `Unreadable`'s file row.
    #[test]
    fn extra_ca_too_large_classifies_by_inline_versus_file_origin() {
        for origin in inline_origins() {
            let error = TlsError::TooLarge {
                origin,
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            };
            assert_eq!(error.classify(), Some(ExitCode::ConfigError), "{error:?}");
        }
        for origin in file_origins() {
            let error = TlsError::TooLarge {
                origin,
                bytes: MAX_EXTRA_CA_CERTS_BYTES + 1,
            };
            assert_eq!(error.classify(), Some(ExitCode::IoError), "{error:?}");
            // The message names the bound it enforces (review r1).
            assert!(
                error
                    .to_string()
                    .contains(&format!("over the {MAX_EXTRA_CA_CERTS_BYTES}-byte limit")),
                "{error}"
            );
        }
    }

    /// C-004 / C-010: the chain walker reaches a `TlsError` behind a wrapper —
    /// the `try_downcast!` registration in `cli/classify.rs`, without which 78
    /// is unreachable from `Context::try_init`.
    #[test]
    fn extra_ca_tls_error_is_classified_through_the_error_chain_walker() {
        #[derive(Debug, thiserror::Error)]
        #[error("initialising the context")]
        struct Wrapped(#[source] TlsError);

        let wrapped = Wrapped(TlsError::Empty {
            origin: ExtraRootsSource::ConfigInline(ConfigTier::System),
        });
        assert_eq!(crate::cli::classify_error(&wrapped), ExitCode::ConfigError);

        let wrapped = Wrapped(TlsError::Malformed {
            origin: ExtraRootsSource::ConfigPath {
                path: PathBuf::from("/etc/ssl/corp.pem"),
                tier: Some(ConfigTier::Home),
            },
            index: Some(2),
        });
        assert_eq!(crate::cli::classify_error(&wrapped), ExitCode::DataError);
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

        let seeded = pki.roots().seed(recipe()).build().expect("seeded client builds");
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

    /// C-005's env arm: `-----BEGIN` anywhere is the text itself under the
    /// `Env` origin; anything else is a path read as-is under `EnvPath`;
    /// a non-regular file is `Unreadable` under that same `EnvPath`.
    #[test]
    fn extra_ca_from_env_value_sniffs_inline_text_reads_a_path_and_refuses_a_device() {
        let pem = TestPki::mint().root_pem();
        let inline = format!("subject=CN=corp\n{pem}");
        let (bytes, origin) = ExtraRoots::from_env_value(&inline).expect("inline text");
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
        let (bytes, origin) = ExtraRoots::from_env_value(path.to_str().unwrap()).expect("path read");
        assert_eq!(bytes, on_disk, "a path's bytes are returned as read, UTF-8 or not");
        assert!(ExtraRoots::parse_pem(&bytes, &origin).is_ok(), "and they validate");
        assert!(
            matches!(&origin, ExtraRootsSource::EnvPath(read) if read == &path),
            "{origin:?}"
        );

        #[cfg(unix)]
        match ExtraRoots::from_env_value("/dev/zero") {
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
        match ExtraRoots::from_env_value("  \n") {
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
        let unpinned = oci::Identifier::parse("registry.corp/managed-config:stable").unwrap();
        let pinned =
            oci::Identifier::parse(&format!("registry.corp/managed-config@sha256:{}", "a".repeat(64))).unwrap();
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
