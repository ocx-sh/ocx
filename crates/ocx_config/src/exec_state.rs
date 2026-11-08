// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The execution state a child process inherits: the `__OCX_ENV_STATE` wire format, its codec, the
//! applied-value records and policy resolution.
//!
//! This is the ecosystem-tier half of the model: everything a descendant outside the `ocx` binary
//! (a plugin, `ocx-mirror`) must read lives here. Exec semantics, selection and the projection live
//! in internal crates.

pub mod decode;
pub mod display;
pub mod encode;
pub mod header;
pub mod policy;
pub mod received;
pub mod revert;
pub mod types;

pub use decode::decode;
pub use display::display_safe;
pub use encode::encode;
pub use policy::{Disagreement, FieldClass, PolicyInputs, PolicyLayer, RootFlags, WriteMode};
pub use revert::{RevertOutcome, revert};
pub use types::*;
