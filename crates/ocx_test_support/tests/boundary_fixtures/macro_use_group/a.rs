// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the forbidden module is only named inside a `use` group written
//! in a macro body, which `syn` hands the scanner as raw tokens.

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use crate::{shell::export, project::lock};

        pub fn stale() -> bool {
            let _ = export::render();
            lock::is_stale()
        }
    }
}
