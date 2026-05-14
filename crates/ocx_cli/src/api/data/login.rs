// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// Successful `ocx login` result.
///
/// Plain format: nothing on stdout — the human "Login succeeded" line is a
/// stderr diagnostic emitted via `Api::success`. stdout is the CLI's
/// machine interface and stays empty when there is no parseable payload.
///
/// JSON format: `{"registry": "...", "username": "..."}` on stdout.
#[derive(Serialize)]
pub struct LoginResult {
    pub registry: String,
    pub username: String,
}

impl Printable for LoginResult {
    fn print_plain(&self, _printer: &ocx_lib::cli::DataInterface) {
        // Intentionally empty: success is reported on stderr. Only the JSON
        // path writes to stdout (the data interface).
    }
}

/// Successful `ocx logout` result.
///
/// Plain format: nothing on stdout (see [`LoginResult`]).
///
/// JSON format: `{"registry": "..."}` on stdout.
#[derive(Serialize)]
pub struct LogoutResult {
    pub registry: String,
}

impl Printable for LogoutResult {
    fn print_plain(&self, _printer: &ocx_lib::cli::DataInterface) {
        // Intentionally empty: success is reported on stderr.
    }
}
