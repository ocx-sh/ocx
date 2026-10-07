// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the `ocx_setup` family. Its types declare their own codes with `#[derive(Classify)]`.

use ocx_setup::error::Error as SetupError;
use ocx_setup::session_path::SessionPathError;

#[cfg(test)]
mod tests {
    use super::*;

    use ocx_exit::{ClassifyExitCode, ExitCode};
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

    /// Reds on: a setup slug, delegated or walked, naming another cause than the exit code does.
    #[test]
    fn setup_details_name_the_cause_that_decides_the_code() {
        use crate::exit::tests::assert_detail;

        let io = SetupError::Io {
            path: PathBuf::from("/home/user/.profile"),
            source: std::io::Error::other("disk full"),
        };
        assert_detail(&io, "setup_io");
        let spec = SetupError::InvalidVersionSpec {
            input: "1.2.3@".to_string(),
            reason: "empty digest".to_string(),
        };
        assert_detail(&spec, "invalid_version_spec");
        let refusal = SessionPathError::NotUtf8 {
            path: PathBuf::from("/opt"),
            format: SessionPathFormat::EnvironmentD,
        };
        // A bare `SessionPathError` is no ladder rung; only its wrapper reaches the classifier.
        assert_eq!(
            crate::exit::detail_slug(ocx_exit::ClassifyErrorKind::kind_detail(&refusal)),
            "session_path_not_utf8"
        );
        assert_detail(&SetupError::from(refusal), "session_path_not_utf8");
        let offline = SetupError::Bootstrap(ocx_package_manager::Error::OfflineMode);
        assert_detail(&offline, "offline_mode");
    }

    // ── moved from ocx_setup::session_path::linux with the impl ──

    // ── moved from ocx_setup::session_path::macos with the impl ──

    // ── moved from ocx_setup::session_path::windows with the impl ──

    // ── moved from ocx_setup::shell_config with the impl ──

    // ── moved from ocx_setup::bootstrap with the impl ──
}
