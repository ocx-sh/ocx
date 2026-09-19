// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a `mod` opened inside a `macro_rules!` body is a module the
//! `super::` chain below has to pop through. `syn` parses no macro body, so
//! the module never becomes an `ItemMod`: a scanner that tracks only the
//! parsed `mod` items resolves `super::super::api` one level too shallow,
//! reaches past the crate root and reports nothing at all.

macro_rules! declare_inner {
    () => {
        mod inner {
            pub fn stale() -> bool {
                super::super::api()
            }
        }
    };
}

declare_inner!();
