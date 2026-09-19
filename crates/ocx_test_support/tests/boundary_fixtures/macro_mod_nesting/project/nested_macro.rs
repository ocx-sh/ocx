// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the `mod` is written inside a macro invocation that is itself
//! inside a `macro_rules!` body — one token stream, two macros deep.

macro_rules! declare_inner {
    () => {
        cfg_if::cfg_if! {
            if #[cfg(unix)] {
                mod inner {
                    pub fn stale() -> bool {
                        super::super::api()
                    }
                }
            }
        }
    };
}

declare_inner!();
