// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-filesystem file placement: reflink (CoW clone) with a full-copy
//! fallback. THE one place `reflink_copy` is called — mirror of `hardlink.rs`
//! for the cross-device case.
//!
//! Unlike [`crate::hardlink::create`], which shares an inode between source and
//! destination, `reflink::create` always produces an **independent inode**. The
//! destination is either a copy-on-write clone of the source (when the
//! underlying filesystem supports reflinking — e.g. btrfs, APFS, XFS with
//! `reflink=1`) or a full byte-for-byte copy otherwise. In both cases:
//!
//! - Executable bits and content are duplicated, not shared.
//! - Modifying the destination through one inode does not affect the source
//!   (unlike a hardlink).
//! - On macOS, a package assembled cross-device may need re-signing even if
//!   the signature bytes were copied, because the inode is independent — this
//!   is handled in P2.4.
//!
//! **When to use this module vs `hardlink::create`:**
//! - Same filesystem → use `crate::hardlink::create` (shared inode, zero-copy).
//! - Different filesystem → use `crate::reflink::create` (independent inode,
//!   CoW or copy). The assembly walker probes `same_filesystem` and dispatches
//!   through [`crate::utility::fs::assemble::AssemblyMode`].

use crate::prelude::*;

/// Places a file at `link` as an independent inode copy of `source`.
///
/// Uses `reflink_copy::reflink_or_copy`: on filesystems that support CoW
/// reflinking (btrfs, APFS, XFS with `reflink=1`) the kernel clones the
/// extents instantly. On all other filesystems a full byte-for-byte copy is
/// performed. Either way the result is an **independent inode** — modifying
/// `link` does not affect `source`.
///
/// Creates any missing parent directories of `link`. Fails if `link` already
/// exists.
///
/// # Errors
///
/// Returns `crate::Error::InternalFile` wrapping the underlying `io::Error`
/// if:
/// - A parent directory of `link` cannot be created.
/// - `source` does not exist or is not readable.
/// - `link` already exists (`io::ErrorKind::AlreadyExists`).
/// - Any other I/O failure during the copy or reflink syscall.
pub fn create(_source: impl AsRef<std::path::Path>, _link: impl AsRef<std::path::Path>) -> Result<()> {
    unimplemented!("P2.3: reflink_copy::reflink_or_copy")
}
