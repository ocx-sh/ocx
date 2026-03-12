// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
pub struct Version;

impl Version {
    pub async fn execute(&self) -> anyhow::Result<ExitCode> {
        println!("{}", crate::app::version());
        Ok(ExitCode::SUCCESS)
    }
}
