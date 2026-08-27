// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod bin_scan;
mod canonical_tag;
mod completion;
mod compression_level;
mod content_path;
mod env_override;
mod format;
mod group_selection;
// `pub mod` rather than the sibling `mod` + `pub use` idiom: `Hook` has no
// consumer until WP-11 flattens it into `self activate`, and a `pub use` with
// no consumer is an `unused_imports` failure under `warnings = "deny"`. WP-9
// adds the re-export alongside the first call site.
pub mod hook;
mod identifier;
mod interactive;
mod lazy_mode;
mod lazy_report;
mod platform;
mod pull;
mod referrers;
mod verification;
mod verify;

pub use bin_scan::{BinScan, BinScanMode};
pub use canonical_tag::CanonicalTag;
pub use completion::Completion;
pub use compression_level::CompressionLevel;
pub use content_path::ContentPath;
pub use env_override::EnvOverride;
pub use format::{Format, FormatMode};
pub use group_selection::GroupSelection;
pub use identifier::Identifier;
pub use interactive::Interactive;
pub use lazy_mode::LazyMode;
pub use lazy_report::LazyReport;
pub use platform::PlatformOption;
pub use pull::Pull;
pub use referrers::Referrers;
pub use verification::Verification;
pub use verify::{SignatureVerify, Verify};
