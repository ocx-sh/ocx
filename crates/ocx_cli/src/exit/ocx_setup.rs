// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the setup error family — the `ocx_setup` rung of the
//! ladder, here rather than in that crate because classification is `ocx_cli`'s alone.

use ocx_exit::ExitCode;

use ocx_setup::error::Error as SetupError;
use ocx_setup::session_path::SessionPathError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for SetupError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // Delegate to the inner package-manager error so the existing
            // ladder decides (offline → 81, registry → 69, …). Returning
            // `None` lets the chain walker reach the inner cause via `source()`.
            // `Bootstrap` wraps the inner package_manager error via `#[from]`
            // (see the variant above), so `source()` exposes it for the walk —
            // do not "fix" this by returning a specific code here.
            SetupError::Bootstrap(_) => None,
            SetupError::Io { .. } => Some(ExitCode::IoError),
            SetupError::Subprocess(_) => Some(ExitCode::Unavailable),
            // Parsed via clap value_parser → rendered as usage error (exit 64).
            SetupError::InvalidVersionSpec { .. } => Some(ExitCode::UsageError),
            // tag@digest mismatch is a found-but-inconsistent error (exit 65),
            // not a "not found" (79). Fail-closed per plan D9.
            SetupError::PinDigestMismatch { .. } => Some(ExitCode::DataError),
            SetupError::InvalidManagedConfigSource { .. } => Some(ExitCode::ConfigError),
            // Delegate to the inner error so the existing ManagedConfigUpdateError
            // ladder decides (Unavailable/AuthError/DataError); `#[from]` exposes
            // it via `source()` for the chain walker, mirroring `Error::Bootstrap`.
            SetupError::ManagedConfigUpdateFailed(_) => None,
            // A locked-tier override rejection is a configuration policy error.
            SetupError::ManagedConfigLocked(_) => Some(ExitCode::ConfigError),
            // Delegate rather than restate: `SessionPathError` owns the mapping
            // from its own variants to a code, and this arm existing is what
            // makes exit 78 reachable from `argv` at all — `SetupError` is
            // already registered in `crate::exit::classify`, the inner type is not.
            SetupError::SessionPath(inner) => inner.classify(),
            // Delegate to `TlsError::classify` (74/65/78 per C-010) directly —
            // `#[error(transparent)]` makes `source()` skip this variant
            // entirely, so returning `None` here (as `Bootstrap` /
            // `ManagedConfigUpdateFailed` do) would leave the chain walker
            // with no registered type to downcast and fall through to
            // `ExitCode::Failure`. Same shape as `Error::SessionPath` above.
            SetupError::ExtraCaCerts(inner) => inner.classify(),
            SetupError::ConfigEdit(inner) => inner.classify(),
            SetupError::ExtraCaCertsNotUtf8 { .. } => Some(ExitCode::DataError),
            SetupError::RenderedConfigTooLarge { .. } => Some(ExitCode::ConfigError),
        }
    }
}

impl ClassifyExitCode for SessionPathError {
    /// An exhaustive match with no wildcard arm, copying
    /// [`ocx_config::env::CommandResolutionError`]'s shape: under a blanket arm a
    /// variant added later is silently classified and no test can catch it.
    fn classify(&self) -> Option<ExitCode> {
        match self {
            Self::Unencodable { .. } | Self::NotUtf8 { .. } | Self::NotAbsolute { .. } => Some(ExitCode::ConfigError),
        }
    }
}

pub(super) fn try_downcast(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    downcast_arm!(cause, SetupError);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use ocx_setup::session_path::SessionPathFormat;

    use std::path::PathBuf;

    // ── moved from ocx_setup with the impl ──

    // ── moved from ocx_setup::session_path with the impl ──

    /// The refusal is a configuration fault, exit 78 — the code the ADR's
    /// error table gives "`OCX_HOME` cannot be encoded for a session-PATH
    /// format".
    #[test]
    fn an_encoding_refusal_classifies_as_a_configuration_error() {
        let refusal = SessionPathError::Unencodable {
            path: PathBuf::from("/opt/100%real"),
            format: SessionPathFormat::WindowsRegistry,
            character: '%',
            reason: "and REG_EXPAND_SZ has no escape for it",
        };
        assert_eq!(refusal.classify(), Some(ExitCode::ConfigError));
        assert_eq!(
            SessionPathError::NotUtf8 {
                path: PathBuf::from("/opt"),
                format: SessionPathFormat::EnvironmentD,
            }
            .classify(),
            Some(ExitCode::ConfigError)
        );

        // The hop that makes 78 reachable from `argv` rather than merely
        // classifiable: `SetupError` is what `crate::exit::classify` downcasts, and its
        // `SessionPath` arm delegates back to the impl above. Without the
        // wrapper variant this type would be unreachable from every command.
        assert_eq!(
            ocx_setup::error::Error::from(refusal).classify(),
            Some(ExitCode::ConfigError)
        );
    }

    // ── moved from ocx_setup::session_path::linux with the impl ──

    // ── moved from ocx_setup::session_path::macos with the impl ──

    // ── moved from ocx_setup::session_path::windows with the impl ──

    // ── moved from ocx_setup::shell_config with the impl ──

    // ── moved from ocx_setup::bootstrap with the impl ──
}
