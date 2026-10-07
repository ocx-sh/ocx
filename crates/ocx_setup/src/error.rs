// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the `ocx self setup` subsystem.
//!
//! A dirty RC block refused without `--force` is the outcome
//! [`crate::ProfileOutcome::SkippedDirty`], not an error.

use std::path::PathBuf;

/// Error raised while creating or refreshing shell integration.
#[derive(thiserror::Error, Debug, ocx_exit::Classify)]
#[exit(family = "SetupError")]
pub enum Error {
    /// Self-install bootstrap failed; the CAS could not be populated.
    #[error("bootstrap failed")]
    // `chain` defers to the walker so the root cause decides the code.
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "setup_bootstrap_failed",
            summary = "Installing ocx into the content store failed with an unclassified cause"
        )
    )]
    Bootstrap(#[from] ocx_package_manager::Error),
    /// A shim or profile file could not be read or written.
    #[error("I/O error for {path}")]
    #[exit(IoError, slug = "setup_io", summary = "Reading or writing a setup file failed")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A profile-detection subprocess (PowerShell `$PROFILE` / exec-policy probe) failed.
    // Never constructed today: the `profiles.rs` probes degrade failure to `None`/`false`.
    #[error("profile subprocess failed")]
    #[exit(
        Unavailable,
        slug = "setup_profile_subprocess",
        summary = "A shell subprocess setup needs could not run"
    )]
    Subprocess(#[source] std::io::Error),
    /// The VERSION argument could not be parsed as a valid version spec.
    ///
    /// Returned from the clap `value_parser`, so it exits 64 as a usage error.
    #[error("invalid version spec {input:?}: {reason}")]
    #[exit(
        UsageError,
        slug = "invalid_version_spec",
        summary = "The version argument is not a tag, a digest, or a tag pinned to a digest"
    )]
    InvalidVersionSpec {
        /// The raw input string that was rejected.
        input: String,
        /// Human-readable description of why the input was rejected.
        reason: String,
    },
    /// A `tag@digest` pin was specified but the tag resolved to a different digest.
    #[error(
        "pin digest mismatch for {tag}: expected {expected} but registry resolved {resolved}{hint}",
        hint = if let Some(h) = hint { format!("; {h}") } else { String::new() }
    )]
    #[exit(
        DataError,
        slug = "pin_digest_mismatch",
        summary = "The registry resolved the pinned tag to a different digest"
    )]
    PinDigestMismatch {
        /// The tag component of the `tag@digest` spec.
        tag: String,
        /// The digest that was pinned in the VERSION argument.
        expected: ocx_oci::Digest,
        /// The digest the registry (or local index) resolved for the tag.
        resolved: ocx_oci::Digest,
        /// Stale-index hint; `ensure_pinned` always sets it, since it cannot tell whether the local index resolved
        /// the tag.
        hint: Option<String>,
    },
    /// The `--managed-config` ref failed re-validation as an OCI identifier at write time.
    #[error("managed config source '{value}' is not a valid OCI identifier")]
    #[exit(
        ConfigError,
        slug = "invalid_managed_config_source",
        summary = "The managed-config source is not a valid OCI identifier"
    )]
    InvalidManagedConfigSource {
        /// The rejected `--managed-config` value.
        value: String,
        /// The underlying identifier parse failure.
        #[source]
        source: ocx_oci::package_ref::error::IdentifierError,
    },
    /// Fetching and persisting the managed-config snapshot failed; no fence is
    /// written (`adr_managed_config_tier.md` § Setup ordering).
    #[error("failed to sync the managed-config snapshot")]
    #[exit(
        chain,
        fallback(
            Failure,
            slug = "managed_config_update_failed",
            summary = "Syncing the managed-config snapshot failed with an unclassified cause"
        )
    )]
    ManagedConfigUpdateFailed(#[from] ocx_config::managed_config::ManagedConfigUpdateError),
    /// A system-locked managed tier refused an override that would clear or
    /// redirect it (exit 78).
    // Re-checked in `apply_managed_config`, or a direct library caller bypasses the CLI's check.
    #[error(transparent)]
    #[exit(delegate = 0)]
    ManagedConfigLocked(#[from] ocx_config::managed::ManagedConfigError),
    /// A directory cannot be encoded for this host's session-PATH format;
    /// refused before anything was written (exit 78).
    ///
    /// A write that fails is [`crate::SessionPathOutcome::Failed`] in the `Ok` arm, not this.
    #[error(transparent)]
    // `SessionPathError` is not in the downcast ladder; without the delegate it exits 1.
    #[exit(delegate = 0)]
    SessionPath(#[from] crate::session_path::SessionPathError),
    /// `OCX_EXTRA_CA_CERTS` resolved to material `parse_pem` refused, before any write.
    // Transparent: `TlsError` is never a walker link, so the classifier matches this variant.
    #[error(transparent)]
    // `#[error(transparent)]` hides this node from the chain walker, so delegate explicitly.
    #[exit(delegate = 0)]
    ExtraCaCerts(#[from] ocx_config::tls::TlsError),
    /// A `config.toml` read-modify-write ([`ocx_config::edit`]) did not land:
    /// lock timeout (75), or read, parse, shape check or write failure (74).
    #[error(transparent)]
    #[exit(delegate = 0)]
    ConfigEdit(#[from] ocx_config::edit::EditError),
    /// The `OCX_EXTRA_CA_CERTS` bundle is valid PEM but not UTF-8, so it cannot
    /// be persisted as a TOML string (exit 65).
    // Carries the byte count only, never the bytes.
    #[error(
        "OCX_EXTRA_CA_CERTS names a bundle that is not UTF-8 ({bytes} bytes) and cannot be persisted; strip the \
         non-UTF-8 label lines outside the -----BEGIN/-----END blocks, or set `extra_ca_certs = \"<path>\"` in \
         config.toml instead (a path is read as bytes)"
    )]
    #[exit(
        DataError,
        slug = "extra_ca_certs_not_utf8",
        summary = "The extra CA bundle is not UTF-8 and cannot be persisted"
    )]
    ExtraCaCertsNotUtf8 {
        /// The bundle's length.
        bytes: usize,
    },
    /// Persisting `extra_ca_certs_pem` would leave `config.toml` over the
    /// loader's size ceiling; refused before any write, also when the file is
    /// already over it.
    #[error(
        "persisting OCX_EXTRA_CA_CERTS would leave {path} at {bytes} bytes, over the {}-byte config limit; shrink the \
         file, or set `extra_ca_certs = \"<path>\"` in it instead",
        ocx_config::loader::MAX_CONFIG_SIZE
    )]
    #[exit(
        ConfigError,
        slug = "rendered_config_too_large",
        summary = "Persisting the extra CA bundle would exceed the config size limit"
    )]
    RenderedConfigTooLarge {
        /// The `config.toml` path the write was refused for.
        path: PathBuf,
        /// The size the document has, or would have had.
        bytes: usize,
    },
}
