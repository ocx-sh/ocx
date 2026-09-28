// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index publication workflow: announce pipeline, forge drivers, index claim.
//!
//! No root `Error`: each subtree's error has its own `downcast_arm!` rung in
//! `ocx_cli/src/exit/ocx_announce.rs`, which a wrapper would hide.

pub mod announce;
pub mod claim;
pub mod forge;
