// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the reach inside the macro-written module is a `use super::`
//! statement, so the module chain has to reach the `use`-expansion arm too,
//! not only the path scan.

macro_rules! declare_inner {
    () => {
        mod inner {
            use super::super::api::lock;

            pub fn stale() -> bool {
                lock::is_stale()
            }
        }
    };
}

declare_inner!();
