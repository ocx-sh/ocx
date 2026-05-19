// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Human-readable size formatting for plain-text reports.
//!
//! Single source of truth so every report (`inspect` candidates, layers,
//! resolution chain, …) renders byte sizes identically. Wraps
//! [`indicatif::HumanBytes`] (binary units: KiB/MiB/GiB) — `indicatif` is
//! already a dependency for progress rendering, so no new crate is pulled
//! in. Machine surfaces (JSON) keep the raw integer; only plain-text notes
//! use this.

/// Formats a byte count as a binary-unit string (e.g. `4.21 MiB`).
///
/// A negative `size` means the value could not be determined (an
/// unexpectedly-absent on-disk blob) and renders as `unknown`.
pub fn human_bytes(size: i64) -> String {
    match u64::try_from(size) {
        Ok(bytes) => indicatif::HumanBytes(bytes).to_string(),
        Err(_) => "unknown".to_string(),
    }
}
