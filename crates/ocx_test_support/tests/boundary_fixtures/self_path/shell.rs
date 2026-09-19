// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: both syn-parsed spellings of a `self::` reach — the `use`
//! statement, whose leading `self` the expander used to drop, and the
//! qualified expression path.

use self::project::lock;

pub mod project;

pub fn stale() -> bool {
    lock::is_stale() || self::project::lock::is_stale()
}
