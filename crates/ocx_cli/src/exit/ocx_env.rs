// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Test-only: the classification test of the environment registry's refusal, whose type declares its own code.

use ocx_env::InvalidEnv;

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
}
