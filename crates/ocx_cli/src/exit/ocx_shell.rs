// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification tests of the CI-detection error family; its type declares its own codes.

use ocx_shell::ci::error::Error as CiError;

#[cfg(test)]
mod tests {
    use super::*;

    /// Reds on: a CI-export slug filed under another code than its variant exits with.
    #[test]
    fn ci_details_are_registered_under_their_codes() {
        use crate::exit::tests::assert_detail;

        assert_detail(&CiError::MissingEnv("GITHUB_ENV".to_string()), "ci_missing_env");
        let file = CiError::File {
            path: "/github/env".into(),
            source: std::io::Error::other("disk full"),
        };
        assert_detail(&file, "ci_file_write");
        assert_detail(&CiError::Write(std::io::Error::other("broken pipe")), "ci_export_write");
    }
}
