// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Starlark LSP server bootstrap (firewall side of `ocx lsp`).
//!
//! Lives inside the engine firewall so `ocx_cli` carries no `starlark*`
//! dependency (the hidden `lsp` command delegates to
//! [`run_lsp_server`]). The custom [`OcxLspContext::get_environment`] returns
//! the SAME `#[starlark_module]` doc metadata the evaluator uses (single
//! source of truth, zero drift). `load()` stays disabled.
//!
//! v1 minimal scope (documented, INTERNAL/UNSTABLE per ADR R4): completion +
//! hover for the `ocx.*` / `expect.*` host API work via `get_environment`;
//! inline syntax-error diagnostics are not emitted in v1 (would require an
//! `lsp_types` diagnostic-conversion surface — deferred, not on the plan).
//! The server is otherwise a real, dispatchable stdio LSP server.

use std::path::Path;

use starlark::docs::DocModule;
use starlark::environment::GlobalsBuilder;
use starlark::syntax::{AstModule, Dialect};
use starlark_lsp::server::{LspContext, LspEvalResult, LspUrl, StringLiteralResult, stdio_server};

/// Builds the same globals the evaluator uses (extended stdlib + `ocx.*` +
/// `expect.*`) so LSP completion/hover reflect the live host API.
fn lsp_globals() -> starlark::environment::Globals {
    GlobalsBuilder::extended_by(super::engine::SCRIPT_EXTENSIONS)
        .with(super::ocx_module::ocx_module)
        .with(super::expect_module::expect_module)
        .build()
}

/// `load()`-disabled dialect, matching the evaluator.
fn lsp_dialect() -> Dialect {
    Dialect {
        enable_load: false,
        ..Dialect::Standard
    }
}

/// Minimal `LspContext` for OCX test scripts: single-file (no `load()`),
/// host-API docs from the live `#[starlark_module]` definitions.
struct OcxLspContext {
    globals: starlark::environment::Globals,
}

impl LspContext for OcxLspContext {
    fn parse_file_with_contents(&self, _uri: &LspUrl, content: String) -> LspEvalResult {
        match AstModule::parse("<lsp>", content, &lsp_dialect()) {
            Ok(ast) => LspEvalResult {
                diagnostics: Vec::new(),
                ast: Some(ast),
            },
            // v1: no inline diagnostic conversion (see module doc).
            Err(_) => LspEvalResult::default(),
        }
    }

    fn resolve_load(
        &self,
        path: &str,
        _current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<LspUrl> {
        // `load()` is disabled for OCX test scripts.
        anyhow::bail!("load() is not supported in ocx test scripts: {path}")
    }

    fn render_as_load(
        &self,
        _target: &LspUrl,
        _current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("load() is not supported in ocx test scripts")
    }

    fn resolve_string_literal(
        &self,
        _literal: &str,
        _current_file: &LspUrl,
        _workspace_root: Option<&Path>,
    ) -> anyhow::Result<Option<StringLiteralResult>> {
        Ok(None)
    }

    fn get_load_contents(&self, _uri: &LspUrl) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    fn get_environment(&self, _uri: &LspUrl) -> DocModule {
        // Single source of truth: the same Globals the evaluator builds.
        self.globals.documentation()
    }

    fn get_url_for_global_symbol(&self, _current_file: &LspUrl, _symbol: &str) -> anyhow::Result<Option<LspUrl>> {
        Ok(None)
    }
}

/// Runs the OCX Starlark LSP server over stdio until the client disconnects.
///
/// Engine-neutral public entry point (the `ocx_cli` `lsp` command
/// delegates here so the firewall holds — no `starlark*` dep in `ocx_cli`).
pub fn run_lsp_server() -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ctx = OcxLspContext { globals: lsp_globals() };
    stdio_server(ctx).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
}
