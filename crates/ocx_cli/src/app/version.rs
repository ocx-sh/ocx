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
        let parts: Vec<&str> = v.split('.').collect();
        assert_eq!(parts.len(), 3, "version must be MAJOR.MINOR.PATCH, got: {v}");
        for part in &parts {
            assert!(
                part.parse::<u32>().is_ok(),
                "version component must be numeric, got: {v}"
            );
        }
    }
}
