// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the macro opens `mod $name`, whose name is not knowable before
//! expansion. The depth still is: `super::super::api` pops through one module
//! either way, so the chain has to resolve one level shallower than the file.

macro_rules! declare_inner {
    ($name:ident) => {
        mod $name {
            pub fn stale() -> bool {
                super::super::api()
            }
        }
    };
}

declare_inner!(inner);
