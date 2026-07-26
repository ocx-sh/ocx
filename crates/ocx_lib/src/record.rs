// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-exec resolution records.
//!
//! One JSON record per ocx frame that launches a tool: the resolved package
//! closure with digests plus the resolved executable, written to an
//! operator-designated directory immediately before the child starts. The point
//! is to bind a job's outputs to approved published package versions without a
//! wrapper script being the control point.
//!
//! Module singular, config section plural — the `patch` / `[patches]` precedent.
//!
//! `ocx package test` and `ocx patch test` deliberately do not record: both are
//! maintainer previews over local unpublished artifacts, so a record from them
//! would describe something that was never published — noise in exactly the
//! audit this exists to keep clean. The exclusion is enumerated in
//! [`crate::launch::ExemptionReason`], not left implicit.

pub mod environment;
pub mod error;
pub mod execution_record;
pub mod name_template;
pub mod options;
pub mod policy;
pub mod purl;
pub mod sink;

pub use error::RecordsError;
pub use execution_record::{
    ExecutionRecord, Frame, FrameCommand, PackageBinding, RecordInputs, ResourceDescriptor, Scope,
};
pub use name_template::{NameContext, NameTemplate};
pub use options::RecordsOptions;
pub use policy::{RecordingPolicy, resolve_records};
pub use sink::{emit, probe_writable};
