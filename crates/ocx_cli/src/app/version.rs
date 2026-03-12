// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Returns the compiled OCX version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_set() {
        let v = version();
        assert!(!v.is_empty(), "CARGO_PKG_VERSION must not be empty");
        assert_ne!(v, "0.0.0", "workspace version must be set in Cargo.toml");
    }
}
