// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

/// A conflict detected when two packages set the same constant env var to different values.
#[derive(Debug)]
pub struct Conflict {
    /// The env var key.
    pub key: String,
    /// The package that previously claimed this key.
    pub previous_package: String,
    /// The value the previous package set.
    pub previous_value: String,
    /// The package that is now conflicting.
    pub current_package: String,
    /// The value the current package wants to set.
    pub current_value: String,
}

/// Tracks constant environment variable assignments across packages to detect conflicts.
///
/// Only constant-type vars are tracked (path-type vars are accumulated, so conflicts
/// don't apply).
#[derive(Debug, Default)]
pub struct ConstantTracker {
    /// key → (package, value)
    seen: HashMap<String, (String, String)>,
}

impl ConstantTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a constant env var assignment. Returns `Some(Conflict)` if a different
    /// package already set this key to a different value.
    pub fn track(&mut self, package: &str, key: &str, value: &str) -> Option<Conflict> {
        if let Some((prev_pkg, prev_val)) = self.seen.get(key) {
            if prev_pkg != package && prev_val != value {
                return Some(Conflict {
                    key: key.to_string(),
                    previous_package: prev_pkg.clone(),
                    previous_value: prev_val.clone(),
                    current_package: package.to_string(),
                    current_value: value.to_string(),
                });
            }
            None
        } else {
            self.seen
                .insert(key.to_string(), (package.to_string(), value.to_string()));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_conflict_same_package() {
        let mut tracker = ConstantTracker::new();
        assert!(tracker.track("cmake", "HOME", "/a").is_none());
        assert!(tracker.track("cmake", "HOME", "/b").is_none());
    }

    #[test]
    fn no_conflict_same_value() {
        let mut tracker = ConstantTracker::new();
        assert!(tracker.track("cmake", "HOME", "/a").is_none());
        assert!(tracker.track("node", "HOME", "/a").is_none());
    }

    #[test]
    fn conflict_different_package_different_value() {
        let mut tracker = ConstantTracker::new();
        assert!(tracker.track("cmake", "JAVA_HOME", "/cmake").is_none());
        let conflict = tracker.track("node", "JAVA_HOME", "/node").unwrap();
        assert_eq!(conflict.key, "JAVA_HOME");
        assert_eq!(conflict.previous_package, "cmake");
        assert_eq!(conflict.current_package, "node");
    }
}
