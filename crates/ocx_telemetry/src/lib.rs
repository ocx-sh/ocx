// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Usage export: the arming gate, destination selection, record builders and the one
//! OTLP/HTTP logs request a run sends.
//!
//! Nothing here builds an OpenTelemetry type, a client or a file until [`decide`] armed the run
//! *and* a destination was selected.

mod ci;
mod client;
mod destination;
mod detached;
mod export;
mod gate;
mod identity;
pub mod keys;
#[cfg(test)]
mod log_capture;
mod payload;
mod resource;
#[cfg(test)]
mod testing;

pub use ci::CiEnvironment;
pub use destination::{Destination, DestinationFamily};
pub use export::Slot;
pub use gate::{ArmedBy, Decision, OffReason, decide};
pub use payload::{CommandOutcome, CommandResult, PackageAction, PackageRecordInput, PackageRole};
pub use resource::ResourceInputs;
