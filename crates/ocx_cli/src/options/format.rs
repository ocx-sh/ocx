// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, ValueEnum, Default)]
pub enum Format {
    Json,
    #[default]
    Plain,
}
