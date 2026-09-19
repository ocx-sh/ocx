// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: an attribute sits between the macro's brace and the `mod`, so the
//! two tokens before the module's `{` are still `mod <ident>`.

macro_rules! declare_inner {
    () => {
        #[cfg(unix)]
        mod inner {
            pub fn stale() -> bool {
                super::super::api()
            }
        }
    };
}

declare_inner!();
