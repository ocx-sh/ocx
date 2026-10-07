// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Which environment variable stands in for which command-line flag.

use ocx_env::EnvVar;

/// Every variable with an equivalent flag, as `(variable, "--long")`; `cli.json` joins each side to the other.
pub static ENV_FLAGS: &[(&EnvVar, &str)] = &[
    (&ocx_env::OCX_CONFIG, "--config"),
    (&ocx_env::OCX_PROJECT, "--project"),
    (&ocx_env::OCX_GLOBAL, "--global"),
    (&ocx_env::OCX_REMOTE, "--remote"),
    (&ocx_env::OCX_OFFLINE, "--offline"),
    (&ocx_env::OCX_FROZEN, "--frozen"),
    (&ocx_env::OCX_QUIET, "--quiet"),
    (&ocx_env::OCX_JOBS, "--jobs"),
    (&ocx_env::OCX_INDEX, "--index"),
    (&ocx_env::OCX_LOG_LEVEL, "--log-level"),
];
