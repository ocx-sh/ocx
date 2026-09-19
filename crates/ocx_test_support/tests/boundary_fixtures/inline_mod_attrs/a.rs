// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the reaches decorate inline modules — an attribute list and a
//! `pub(in …)` restriction — and sit outside the modules they name.

#[cfg_attr(feature = "x", derive(crate::project::D))]
mod decorated {}

pub(in crate::project) mod restricted {}
