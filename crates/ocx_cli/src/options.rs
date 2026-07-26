// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod bin_scan;
mod completion;
mod compression_level;
mod content_path;
mod env_override;
mod format;
mod group_selection;
mod keep_tag;
// `pub mod` rather than the sibling `mod` + `pub use` idiom: `Hook` has no
// consumer until WP-11 flattens it into `self activate`, and a `pub use` with
// no consumer is an `unused_imports` failure under `warnings = "deny"`. WP-9
// adds the re-export alongside the first call site.
pub mod hook;
mod identifier;
mod interactive;
// The cosign-parity option groups, on the same `pub mod` footing as `hook` and
// for a second reason on top of the `unused_imports` one: nothing attaches them
// until the sign-side and verify-side command work lands, and those are two
// independent efforts. The module path IS the frozen import spelling
// (`crate::options::key::KeyOpt`, `crate::options::rekor_upload::RekorUploadOpt`,
// `crate::options::signature_format::SignatureFormatOpt`,
// `crate::options::tags::TagsOpt`), so neither effort has to edit this file to
// reach them -- two of them editing one options file is the collision this
// layout exists to prevent. Do not add a `pub use` here later either: the path
// is the contract, and shortening it would put both back in this file.
pub mod key;
mod lazy_mode;
mod lazy_report;
mod platform;
mod pull;
mod records;
mod referrers;
pub mod rekor_upload;
pub mod signature_format;
pub mod tags;
mod verification;
mod verify;

pub use bin_scan::{BinScan, BinScanMode};
pub use completion::Completion;
pub use compression_level::CompressionLevel;
pub use content_path::ContentPath;
pub use env_override::EnvOverride;
pub use format::{Format, FormatMode};
pub use group_selection::GroupSelection;
pub use identifier::Identifier;
pub use interactive::Interactive;
pub use keep_tag::KeepTag;
pub use lazy_mode::LazyMode;
pub use lazy_report::LazyReport;
pub use platform::PlatformOption;
pub use pull::Pull;
pub use records::Records;
pub use referrers::Referrers;
pub use verification::Verification;
pub use verify::{SignatureVerify, Verify};
