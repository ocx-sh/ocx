// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `#[path]` on a macro-written module with a body. The attribute
//! redirects where the module's children are looked up; it does not rename the
//! module, so the chain is unchanged and the module must still be counted.

macro_rules! declare_inner {
    () => {
        #[path = "inner_impl"]
        mod inner {
            pub fn stale() -> bool {
                super::super::api()
            }
        }
    };
}

declare_inner!();
