// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the forbidden module sits inside a nested `use` group.

use crate::{
    shell::{export, hook::Hook},
    project::{lock, ProjectConfig as Config},
};

pub fn stale(config: &Config, hook: &Hook) -> bool {
    let _ = export::render(hook);
    lock::is_stale(config)
}
