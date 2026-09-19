// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the reach and the needle sit inside macro arguments only.

pub fn stale() -> String {
    format!("{}", crate::project::lock::is_stale())
}

pub fn here() -> String {
    format!("{}", module_path!())
}

pub fn field() -> Vec<Foo> {
    vec![Foo { field: crate::project::X }]
}

pub fn key() -> Value {
    json!({ "k": crate::project::Y })
}

pub fn ascription() -> impl Fn(crate::shell::Hook) {
    debug_assert!(matches!(|a: crate::project::T| a, _));
    |_| ()
}
