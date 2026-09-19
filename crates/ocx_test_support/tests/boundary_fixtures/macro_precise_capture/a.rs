// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `use<'_>` — edition-2024 precise capturing, written where the
//! `use`-statement scan sees it. It is not a `use` statement and must not be
//! refused. Five live occurrences in `crates/**` today, none of them yet
//! inside macro tokens, so only this fixture keeps the carve-out exercised.

macro_rules! declare_iter {
    () => {
        pub fn names(&self) -> impl Iterator<Item = &str> + use<'_> {
            std::iter::empty()
        }
    };
}

declare_iter!();
