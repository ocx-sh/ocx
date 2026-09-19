// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the module is an argument of the macro invocation, not part of a
//! `macro_rules!` body — the token stream starts at `mod`.

with_module!(mod inner {
    pub fn stale() -> bool {
        super::super::api()
    }
});
