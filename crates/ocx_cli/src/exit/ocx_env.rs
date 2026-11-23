// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification test of the environment registry's refusal, whose type declares its own code.

use ocx_env::{InvalidEnv, ReferenceError};

#[cfg(test)]
mod tests {
    use super::*;

    /// Reds on: the slug filed under another code than an invalid variable exits with.
    #[test]
    fn invalid_env_detail_is_registered_under_its_code() {
        let invalid = InvalidEnv {
            key: "OCX_OFFLINE".to_string(),
        };
        crate::exit::tests::assert_detail(&invalid, "invalid_env");
    }

    /// Reds on: the invalid-reference slug filed under another code than a refused grammar exits with.
    #[test]
    fn a_refused_reference_detail_is_registered_under_its_code() {
        let refused = ReferenceError::EnvSelfReference {
            variable: "OCX_AUTH_TOKEN".to_string(),
        };
        crate::exit::tests::assert_detail(&refused, "secret_reference_invalid");
    }

    /// Reds on: a non-Unicode reference filed under another slug than the refused grammar's.
    #[test]
    fn a_non_unicode_reference_detail_is_registered_under_the_invalid_slug() {
        let refused = ReferenceError::ValueNotUnicode {
            variable: "OCX_AUTH_TOKEN".to_string(),
        };
        crate::exit::tests::assert_detail(&refused, "secret_reference_invalid");
    }

    /// Reds on: the unreadable-reference slug filed under another code than a missing source exits with.
    #[test]
    fn an_unreadable_reference_detail_is_registered_under_its_code() {
        let unreadable = ReferenceError::EnvTargetUnset {
            variable: "OCX_AUTH_TOKEN".to_string(),
            target: "MISSING".to_string(),
        };
        crate::exit::tests::assert_detail(&unreadable, "secret_reference_unreadable");
    }

    /// Reds on: the malformed-reference slug filed under another code than over-cap or non-UTF-8 data exits with.
    #[test]
    fn a_malformed_reference_detail_is_registered_under_its_code() {
        let malformed = ReferenceError::FileNotUtf8 {
            variable: "OCX_AUTH_TOKEN".to_string(),
            path: "/run/secrets/token".to_string(),
            utf16: false,
        };
        crate::exit::tests::assert_detail(&malformed, "secret_reference_malformed");
    }
}
