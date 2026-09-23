// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error taxonomy for `ocx_python`.
//!
//! Following the one-enum-per-module convention (see `quality-rust-errors.md`),
//! each stage owns its error type in its own module rather than a single
//! crate-wide god enum:
//!
//! | Error | Module | Raised by |
//! |---|---|---|
//! | [`LockError`] | `lock` | [`parse_pylock`](crate::parse_pylock) |
//! | [`PlatformError`] | `platform` | tag parse / L2 encode (internal source) |
//! | [`SelectError`] | `select` | [`select_wheels`](crate::select_wheels) |
//! | [`RepackError`] | `repack` | [`repack_wheel`](crate::repack_wheel) |
//! | [`CollisionError`] | `collide` | [`check_collisions`](crate::check_collisions) |
//! | [`ComposeError`] | `compose` | [`compose_env`](crate::compose_env) |
//!
//! Every error type is `#[derive(thiserror::Error, Debug)]`, `#[non_exhaustive]`,
//! and carries wrapped sources via `#[source]`/`#[from]`. `PlatformError` is an
//! **internal** source type: it never surfaces to the consumer directly, only
//! wrapped inside [`SelectError`] or
//! [`ComposeError`].
//!
//! Consumers map these errors to their own exit codes; ocx-mirror's mapping
//! lives in its `ocx_mirror_error::pylock` module.
//!
//! This module intentionally declares no types of its own — it is the taxonomy
//! reference and re-export hub. The error types live with their stages.

pub use crate::collide::CollisionError;
pub use crate::compose::ComposeError;
pub use crate::lock::LockError;
pub use crate::platform::PlatformError;
pub use crate::repack::RepackError;
pub use crate::select::SelectError;
