// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-sdkgen`: writes an SDK's sources for the documents this build embeds.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use ocx_sdkgen::{Documents, GenerateError, Lang, ReadError, generate_sdk};

/// The documents of the `ocx` this generator was built from.
const REPORTS: &str = include_str!("../../ocx_schema/tests/golden/reports.json");
const ERRORS: &str = include_str!("../../ocx_schema/tests/golden/errors.json");
const CLI: &str = include_str!("../../ocx_schema/tests/golden/cli.json");

/// Generate an OCX SDK from the published machine-interface documents.
#[derive(Debug, Parser)]
#[command(name = "ocx-sdkgen", version)]
struct Cli {
    /// Language of the generated SDK.
    #[arg(long, value_enum)]
    lang: Lang,
    /// Directory the generated sources are written to; created if absent.
    #[arg(long)]
    out: PathBuf,
    /// Read `reports.json`, `errors.json` and `cli.json` from this directory instead of the embedded copies.
    #[arg(long)]
    contract: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("the embedded documents are not JSON: {0}")]
    Embedded(#[from] serde_json::Error),
    #[error(transparent)]
    Generate(#[from] GenerateError),
}

fn main() -> ExitCode {
    let Cli { lang, out, contract } = Cli::parse();
    match run(lang, &out, contract.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ocx-sdkgen: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(lang: Lang, out: &Path, contract: Option<&Path>) -> Result<(), Error> {
    let documents = match contract {
        Some(directory) => Documents::read_dir(directory)?,
        None => Documents::parse(REPORTS, ERRORS, CLI)?,
    };
    generate_sdk(lang, &documents, out)?;
    Ok(())
}
