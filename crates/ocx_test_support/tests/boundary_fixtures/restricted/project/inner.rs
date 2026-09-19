// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: `pub(super)` and `pub(crate)` scope items; they name no module
//! even though `super` here is `crate::project`.

pub(super) fn scoped() -> u8 {
    1
}

pub(crate) fn wide() -> u8 {
    2
}
