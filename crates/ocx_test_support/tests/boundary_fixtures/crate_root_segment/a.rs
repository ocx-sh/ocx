// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `ocx_project` as a MID-path segment, which names a module of
//! somebody else's path and is not a reach into the crate. The green half of
//! the leading-`::` case: the two differ only in what precedes the `::`.

pub fn stale() -> String {
    format!("{}", foo::ocx_project::MARKER) + &format!("{}", bar::ocx_project::X)
}
