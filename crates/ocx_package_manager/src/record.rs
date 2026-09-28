// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pre-exec resolution records: one JSON record per launching frame, naming the
//! resolved package closure and executable, written before the child starts.
//!
//! `ocx package test` and `ocx patch test` run unpublished artifacts and never
//! record ([`crate::launch::ExemptionReason`]).

pub mod environment;
pub mod error;
pub mod execution_record;
pub mod name_template;
pub mod policy;
pub mod purl;
pub mod sink;

pub use error::RecordsError;
pub use execution_record::{
    ExecutionRecord, Frame, FrameCommand, PackageBinding, RecordInputs, ResourceDescriptor, Scope,
};
pub use name_template::{NameContext, NameTemplate};
pub use ocx_config::records::RecordsOptions;
pub use policy::{RecordingPolicy, resolve_records};
pub use sink::{emit, probe_writable};
