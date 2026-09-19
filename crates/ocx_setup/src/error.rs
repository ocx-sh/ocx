// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error type for the `ocx self setup` subsystem.
//!
//! A dirty RC block refused without `--force` is **not** an error — it is a
//! non-error [`crate::ProfileOutcome::SkippedDirty`] outcome, and the
//! CLI decides exit code 82 by inspecting the outcomes, not by matching an
//! error variant.

use std::path::PathBuf;

/// Error raised while creating or refreshing shell integration.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// Self-install bootstrap failed; the CAS could not be populated.
    #[error("bootstrap failed")]
    Bootstrap(#[from] ocx_package_manager::Error),
    /// A shim or profile file could not be read or written.
    #[error("I/O error for {path}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A profile-detection subprocess (PowerShell `$PROFILE` / exec-policy probe) failed.
    ///
    // reserved: no current caller — the PowerShell probes in `profiles.rs`
    // degrade subprocess failure to `None` / `false` by contract (PowerShell
    // absence is non-fatal), so this variant is never constructed today. Kept
    // because plan contract 6 declares it with a `Subprocess → 69` classify
    // mapping for a future probe site that surfaces the failure as a typed error.
    #[error("profile subprocess failed")]
    Subprocess(#[source] std::io::Error),
    /// The VERSION argument could not be parsed as a valid version spec.
    ///
    /// Surfaced via the clap `value_parser` so clap renders it as a usage error
    /// (exit 64). `reason` describes which part of the syntax was invalid.
    #[error("invalid version spec {input:?}: {reason}")]
    InvalidVersionSpec {
        /// The raw input string that was rejected.
        input: String,
        /// Human-readable description of why the input was rejected.
        reason: String,
    },
    /// A `tag@digest` pin was specified but the tag resolved to a different
    /// digest than the one pinned (fail-closed immutability assertion, plan D9).
    ///
    /// The error message names both digests so the operator can diagnose
    /// whether the index is stale (see `hint`).
    #[error(
        "pin digest mismatch for {tag}: expected {expected} but registry resolved {resolved}{hint}",
        hint = if let Some(h) = hint { format!("; {h}") } else { String::new() }
    )]
    PinDigestMismatch {
        /// The tag component of the `tag@digest` spec.
        tag: String,
        /// The digest that was pinned in the VERSION argument.
        expected: ocx_oci::Digest,
        /// The digest the registry (or local index) resolved for the tag.
        resolved: ocx_oci::Digest,
        /// Optional hint shown when resolution was against the local index and
        /// the mismatch may be caused by a stale index
        /// (e.g. `"run \`ocx index update\` to refresh the local index"`).
        hint: Option<String>,
    },
    /// The `--managed-config` ref could not be re-parsed as a valid OCI
    /// identifier at write time (defensive re-validation, CWE-74 guard —
    /// the fence body is real TOML serialization, never `format!`
    /// interpolation of the raw ref, but the ref itself must still be a
    /// well-formed identifier before it is adopted at all).
    #[error("managed config source '{value}' is not a valid OCI identifier")]
    InvalidManagedConfigSource {
        /// The rejected `--managed-config` value.
        value: String,
        /// The underlying identifier parse failure.
        #[source]
        source: ocx_oci::identifier::error::IdentifierError,
    },
    /// The synchronous fetch+persist step during `--managed-config` adoption
    /// failed. Per ADR "Setup ordering", no fence is written on failure — the
    /// caller sees zero partial state.
    #[error("failed to sync the managed-config snapshot")]
    ManagedConfigUpdateFailed(#[from] ocx_config::managed_config::ManagedConfigUpdateError),
    /// A system-locked managed tier refused an explicit override that would
    /// clear or redirect it (locks only tighten — exit 78). The CLI seam
    /// (`resolve_managed_config_arg`) rejects this before calling in, but
    /// [`crate::apply_managed_config`] re-checks so the public library
    /// function cannot be bypassed by a direct caller.
    #[error(transparent)]
    ManagedConfigLocked(#[from] ocx_config::managed::ManagedConfigError),
    /// A directory cannot be encoded for this host's session-PATH format, so
    /// the session-PATH registration was refused **before** anything was
    /// written (C-037, exit 78).
    ///
    /// This is the only way a session-PATH failure reaches an `Err`. A write
    /// that is attempted and fails is
    /// [`crate::SessionPathOutcome::Failed`] — an outcome carried in the
    /// `Ok` arm, warned about, and exit 0 (C-036). The split is the contract:
    /// a refused run left the machine byte-identical, whereas a failed write
    /// left it as it was and is worth a warning, not an abort.
    #[error(transparent)]
    SessionPath(#[from] crate::session_path::SessionPathError),
    /// Phase 0.5 (C-008, ocx#448): `OCX_EXTRA_CA_CERTS` resolved to material
    /// `config::tls::parse_pem` refused (C-004) — the
    /// single choke point every extra-CA-roots byte passes through before any
    /// write. `#[error(transparent)]` forwards `source()` straight past this
    /// variant to `TlsError`'s own cause, so `TlsError` is never itself a
    /// chain link the walker can downcast — classification is delegated
    /// inline instead, mirroring [`Error::SessionPath`] above (not
    /// `Bootstrap`/`ManagedConfigUpdateFailed`, whose `#[from]` field carries
    /// its own `#[error("...")]` message and so IS a walkable link).
    #[error(transparent)]
    ExtraCaCerts(#[from] ocx_config::tls::TlsError),
    /// A `config.toml` read-modify-write ([`ocx_config::edit`]) did not
    /// land: the lock timed out (75), or the read, parse, shape check or
    /// atomic write failed (74). Transparent and delegating like
    /// [`Error::ExtraCaCerts`], so the edit's own message and code reach the
    /// CLI unwrapped — the path is named once, by the edit.
    #[error(transparent)]
    ConfigEdit(#[from] ocx_config::edit::EditError),
    /// Phase 0.5: the bundle `OCX_EXTRA_CA_CERTS` names is valid PEM but not
    /// UTF-8 (a Latin-1 label line ahead of a block), so it cannot be
    /// persisted as a TOML string. The registry/index/forge clients still
    /// trust it — `parse_pem` is bytes-native — only the persistence refuses,
    /// and as data (65, the `ManagedConfigPublishError::ExtraCaCertsNotUtf8`
    /// precedent): the file's bytes are what is wrong. Carries the byte count
    /// only, never the bytes (D-11).
    #[error(
        "OCX_EXTRA_CA_CERTS names a bundle that is not UTF-8 ({bytes} bytes) and cannot be persisted; strip the \
         non-UTF-8 label lines outside the -----BEGIN/-----END blocks, or set `extra_ca_certs = \"<path>\"` in \
         config.toml instead (a path is read as bytes)"
    )]
    ExtraCaCertsNotUtf8 {
        /// The bundle's length.
        bytes: usize,
    },
    /// Phase 0.5: the `config.toml` document `extra_ca_certs_pem` would
    /// render into is, or would be, over the loader's own size ceiling
    /// (64 KiB, `config::loader::MAX_CONFIG_SIZE`) — refused before any
    /// write, so a value that would make the file unloadable is never
    /// persisted. "Would leave", not "would grow": a file already over the
    /// ceiling is refused with its size on disk, before the bundle is added.
    #[error(
        "persisting OCX_EXTRA_CA_CERTS would leave {path} at {bytes} bytes, over the {}-byte config limit; shrink the \
         file, or set `extra_ca_certs = \"<path>\"` in it instead",
        ocx_config::loader::MAX_CONFIG_SIZE
    )]
    RenderedConfigTooLarge {
        /// The `config.toml` path the write was refused for.
        path: PathBuf,
        /// The size the document has, or would have had.
        bytes: usize,
    },
}
