// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generates JSON Schemas for OCX types and writes them to stdout.

use ocx_schema::schema_for;

fn main() {
    let schema_type = std::env::args().nth(1).unwrap_or_else(|| "metadata".to_string());

    match schema_for(&schema_type) {
        Some(json) => println!("{json}"),
        None => {
            eprintln!(
                "Unknown schema type: {schema_type}. Available: metadata, profile, config, project, project-lock"
            );
            std::process::exit(1);
        }
    }
}
