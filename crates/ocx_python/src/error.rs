// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for `ocx_python`: one enum per stage, re-exported here.
//!
//! [`PlatformError`] never surfaces directly; it arrives wrapped in [`SelectError`] or [`ComposeError`].

pub use crate::collide::CollisionError;
pub use crate::compose::ComposeError;
pub use crate::lock::LockError;
pub use crate::platform::PlatformError;
pub use crate::repack::RepackError;
pub use crate::select::SelectError;
