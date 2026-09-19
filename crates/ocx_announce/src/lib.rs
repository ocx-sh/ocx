// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Index publication workflow: announce pipeline, forge drivers, index claim.
//!
//! Three co-equal subtrees rather than one promoted root, the shape
//! `ocx_shell` set for the same reason: [`announce`] decides what the index
//! should say about a package, [`claim`] decides who may say it, and [`forge`]
//! is the write transport that carries either to a git host. None is the
//! others' detail — they share the forge's request vocabulary and nothing
//! else — so the subtrees stay rather than flattening onto the crate root the
//! way `ocx_project` and `ocx_oci` did with their single tier.
//!
//! **This crate names no root `Error`, and that is a measurement rather than an
//! omission.** Every extraction before it minted one because its tier borrowed
//! `ocx_lib::Error` for failures it raised itself. This tier never did: it owns
//! [`announce::AnnounceError`], [`claim::ClaimError`] and [`forge::ForgeError`],
//! each already carried by its own `downcast_arm!` rung in
//! `ocx_cli/src/exit/ocx_announce.rs`. A fourth root would wrap three armed
//! enums in a conversion nothing calls, and every wrapper between an error and
//! the ladder is a place a `source()` walk can lose its subject (DEC-87).
//!
//! The one place the tier did borrow was `AnnounceError::ListTags`, whose
//! `source` was a `Box<ocx_lib::Error>` built by the hand-written flattening
//! `From<ocx_package::error::Error>`. It now carries the producer's own type,
//! because `Publisher::list_tags` is what raises it and no conversion adds
//! anything on the way.
//!
//! Nothing here may reach `ocx_lib`. `scripts/crate_map.toml` allows this crate
//! `ocx_index`, `ocx_package`, `ocx_config`, `ocx_oci`, `ocx_util` and
//! `ocx_exit`, and no more; the root error the split dissolved is not among
//! them and left no successor to name.

pub mod announce;
pub mod claim;
pub mod forge;
