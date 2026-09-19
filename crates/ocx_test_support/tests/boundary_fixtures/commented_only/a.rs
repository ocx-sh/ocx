// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: every `crate::project` mention here is inert.
//! use crate::project::ProjectConfig;

/* use crate::project::lock; */
use std::fmt; // was: use crate::project::Consent;

pub fn describe() -> String {
    let doc = r#"the forbidden form is `crate::project::X`"#;
    let msg = "crate::project::lock::is_stale";
    let quote = '"';
    format!("{doc}{msg}{quote}")
}

pub fn width(f: &mut fmt::Formatter<'_>) -> usize {
    f.width().unwrap_or(0) /* crate::project */
}
