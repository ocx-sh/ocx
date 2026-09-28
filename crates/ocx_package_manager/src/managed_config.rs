// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Building the managed-config package — `ocx config push` ([`publish`]) and
//! `ocx config test` ([`preview`], which writes nothing).

pub mod preview;
pub mod publish;

pub use preview::{ManagedConfigPreview, preview_managed_config};
pub use publish::{
    ManagedConfigPublishError, ManagedConfigPublishOptions, publish_managed_config, read_candidate_payload,
    validate_managed_config_payload,
};
