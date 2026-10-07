// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Built against the library without `cfg(test)`: under `__testing` the test seams are declared.
//!
//! Every lane builds with `__testing`, so the converse (a release build declares no seam) is proven by
//! the release-binary byte scan, not here.

#![cfg(feature = "__testing")]

use ocx_env::{Visibility, all};

#[test]
fn testing_feature_declares_the_test_seams() {
    let declared: Vec<_> = all().collect();
    assert!(declared.len() >= 120, "{} declarations", declared.len());
    let seams = declared
        .iter()
        .filter(|var| var.visibility == Visibility::Testing)
        .count();
    assert!(seams > 0, "__testing is on but no test seam is declared");
}
