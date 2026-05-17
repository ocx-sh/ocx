// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx lsp` subcommand — INTERNAL + UNSTABLE.
//!
//! A thin wrapper over the `starlark_lsp` stdio LSP server with a custom
//! `LspContext` whose `get_environment()` is populated from the SAME
//! `#[starlark_module]` doc metadata used by the evaluator (single source of
//! truth, zero drift). `load()` stays disabled.
//!
//! It MUST be a subcommand (editors point `starlark.lspPath` at a binary +
//! subcommand; a flag cannot serve this), but it carries
//! `#[command(hide = true)]` so it NEVER appears in `ocx --help`. The wire
//! contract is exactly the `lsp` subcommand name; everything else is
//! unstable. It is documented only in the authoring/IDE docs, never in the
//! command-line reference (OCX is a backend tool; the LSP name/wire is not a
//! stability promise).
//!
//! Design intent (comments only — no code yet): `ocx lsp` is intentionally
//! named generically so that a future dialect-selecting argument (e.g.
//! `--dialect starlark`) can be added without renaming the subcommand. Do not
//! bake "starlark test script" assumptions into this command name or its
//! top-level doc.
//!
//! It speaks LSP over stdio — it does NOT use `Printable` and has no
//! `api/data/` type. It never writes to the package store.

use std::process::ExitCode;

use clap::Parser;

/// Internal LSP server for IDE integration (hidden from `ocx --help`).
///
/// Long-lived stdio server. Editors invoke `ocx lsp` and pipe LSP
/// over stdin/stdout.
#[derive(Parser)]
#[command(hide = true)]
pub struct Lsp {}

impl Lsp {
    /// D2: the dispatcher (`command.rs`) passes the already-constructed
    /// `Context` uniformly to every subcommand. The LSP server needs none of
    /// its heavy state (no store, no index, no registry), so we accept it for
    /// dispatch-shape consistency but deliberately do not touch it — no extra
    /// initialization is triggered on this path. The server is a long-lived
    /// stdio loop; it speaks LSP, not `Printable`, and never writes the store.
    pub async fn execute(&self, _context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `stdio_server` blocks (owns the stdio connection + io threads). Run
        // it via `block_in_place` so it does not stall the multi-thread
        // runtime's other tasks; the firewall keeps all `starlark*` usage in
        // `ocx_lib::script` (no `starlark*` dep in `ocx_cli`).
        tokio::task::block_in_place(|| {
            ocx_lib::script::run_lsp_server().map_err(|e| anyhow::anyhow!("starlark LSP server failed: {e}"))
        })?;
        Ok(ExitCode::SUCCESS)
    }
}
