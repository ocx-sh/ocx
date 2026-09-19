// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `$crate` — the hygienic root every macro that becomes
//! cross-module is forced onto, which is what the crate split does to them.
//! The group form reaches neither arm on its own: `$crate::{…}` is not a
//! valid `syn::ItemUse`, so the `use` arm's re-lex fails, and the token path
//! scan stops at the group's `{`. The bare path below is caught by the path
//! arm alone and is pinned so that arm cannot quietly stop carrying it.
//!
//! `$crate` occurs 0× in `crates/**`, so a differential run of the old and
//! new scanners over the live tree was arithmetically guaranteed to be blind
//! to this — which is why it is a committed fixture, not a one-off probe.

macro_rules! declare {
    () => {
        use $crate::{project::api, shell::export};

        pub fn stale() -> bool {
            $crate::project::lock::is_stale()
        }
    };
}

declare!();
