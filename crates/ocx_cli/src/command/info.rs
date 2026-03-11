// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{oci, shell};

#[derive(Parser)]
pub struct Info;

impl Info {
    pub async fn execute(&self) -> anyhow::Result<ExitCode> {
        println!("Version: {}", env!("CARGO_PKG_VERSION"));
        println!(
            "Supported Platforms: {}",
            crate::conventions::supported_platforms()
                .iter()
                .map(oci::Platform::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!(
            "Current Shell: {}",
            shell::Shell::from_process().map_or("n/a".to_string(), |s| format!("{}", s))
        );
        Ok(ExitCode::SUCCESS)
    }
}
