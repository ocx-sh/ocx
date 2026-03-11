// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use ocx_lib::oci::annotations;

/// Build OCI annotation key-value pairs for a mirrored package.
#[allow(dead_code)] // Will be used when OCI annotation support is wired into push
pub fn build_annotations(
    spec_name: &str,
    version: &str,
    source_url: &str,
    run_timestamp: &str,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    map.insert(annotations::TITLE.to_string(), spec_name.to_string());
    map.insert(annotations::VERSION.to_string(), version.to_string());
    map.insert(annotations::SOURCE.to_string(), source_url.to_string());
    map.insert(annotations::CREATED.to_string(), run_timestamp.to_string());
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_keys_present() {
        let result = build_annotations(
            "cmake",
            "3.28.0+20260310142359",
            "https://github.com/Kitware/CMake",
            "2026-03-10T14:23:59Z",
        );
        assert_eq!(result.len(), 4);
        assert_eq!(result[annotations::TITLE], "cmake");
        assert_eq!(result[annotations::VERSION], "3.28.0+20260310142359");
        assert_eq!(result[annotations::SOURCE], "https://github.com/Kitware/CMake");
        assert_eq!(result[annotations::CREATED], "2026-03-10T14:23:59Z");
    }
}
