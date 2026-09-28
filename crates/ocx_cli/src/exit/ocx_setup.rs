// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Exit-code classification for the `ocx_setup` error family.

use ocx_exit::ExitCode;

use ocx_setup::error::Error as SetupError;
use ocx_setup::session_path::SessionPathError;

use super::{ClassifyExitCode, downcast_arm};

impl ClassifyExitCode for SetupError {
    fn classify(&self) -> Option<ExitCode> {
        match self {
            // `None` lets the chain walker reach the root cause; a `Some` here would replace its code.
            SetupError::Bootstrap(_) => None,
            SetupError::Io { .. } => Some(ExitCode::IoError),
            SetupError::Subprocess(_) => Some(ExitCode::Unavailable),
            SetupError::InvalidVersionSpec { .. } => Some(ExitCode::UsageError),
            SetupError::PinDigestMismatch { .. } => Some(ExitCode::DataError),
            SetupError::InvalidManagedConfigSource { .. } => Some(ExitCode::ConfigError),
            SetupError::ManagedConfigUpdateFailed(_) => None,
            SetupError::ManagedConfigLocked(_) => Some(ExitCode::ConfigError),
            // `SessionPathError` is not in the downcast ladder; without this arm it exits 1.
            SetupError::SessionPath(inner) => inner.classify(),
            // `#[error(transparent)]` hides this node from the chain walker, so `None` would exit 1.
            SetupError::ExtraCaCerts(inner) => inner.classify(),
            SetupError::ConfigEdit(inner) => inner.classify(),
            SetupError::ExtraCaCertsNotUtf8 { .. } => Some(ExitCode::DataError),
            SetupError::RenderedConfigTooLarge { .. } => Some(ExitCode::ConfigError),
        }
    }
}

impl ClassifyExitCode for SessionPathError {
    /// Exhaustive, not a blanket `Some`, so a new variant cannot ship unclassified.
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
