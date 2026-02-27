use std::process::ExitCode;

use clap::Parser;

#[derive(Parser)]
pub struct Version;

impl Version {
    pub async fn execute(&self) -> anyhow::Result<ExitCode> {
        println!("{}", env!("CARGO_PKG_VERSION"));
        Ok(ExitCode::SUCCESS)
    }
}
