// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Error -> [`ExitCode`] classification for the `ocx` binary.
//!
//! Classification is a CLI concern: only a process that exits owns the mapping
//! from an error to an exit code. The library crates define the error types;
//! this module decides what each one costs the caller. Keeping the two apart is
//! what `no_classification_in_libraries` enforces (C-030).
//!
//! The mapping is distributed across `ClassifyExitCode` impls, one per error
//! type, grouped into one submodule per owning library crate
//! (`exit::ocx_config`, `exit::ocx_oci`, ...). Each submodule exports a
//! `try_downcast` that walks its own types; `try_classify` is the dispatch over
//! those submodules. A wrapper variant carrying an inner classifiable error
//! calls `inner.classify()` and either delegates or overrides based on its own
//! context.
//!
//! [`classify_error`] walks a [`std::error::Error`] chain via `source()`.
//! Binaries using anyhow call `classify_error(err.as_ref())` via
//! `anyhow::Error`'s `AsRef<dyn std::error::Error + 'static>` impl.
//!
//! When no subtree matches, the classifier falls through to
//! [`ExitCode::Failure`]. A locked-in fall-through test in `exit::classify`
//! prevents silent drift.

use ocx_exit::ExitCode;

use crate::app::CommandError;
use crate::app::project_context::ProjectContextError;

mod classify;
mod cli_input;
mod ocx_announce;
mod ocx_config;
mod ocx_index;
mod ocx_oci;
mod ocx_package;
mod ocx_package_manager;
mod ocx_project;
mod ocx_setup;
mod ocx_shell;
mod ocx_sign;
mod ocx_store;
mod ocx_util;

pub(crate) use classify::{ClassifyErrorKind, ClassifyExitCode};

/// Classify an error chain into an [`ExitCode`], CLI-local types first.
///
/// The single exit-code authority for `main.rs`; commands return typed errors
/// rather than hand-mapping exit codes.
///
/// # Two passes, on purpose
///
/// The CLI-local types are swept over the **whole** chain before the library
/// types are swept over the whole chain again. That ordering is observable: a
/// CLI-local cause *anywhere* in the chain outranks a library cause *earlier*
/// in it. Merging the two sweeps into one walk - which reads like the obvious
/// simplification - silently inverts that precedence for every chain carrying
/// both kinds, and an inverted precedence is a changed exit code. The two
/// passes are the contract; leave them as two.
pub fn classify_error(err: &(dyn std::error::Error + 'static)) -> ExitCode {
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(pce) = cause.downcast_ref::<ProjectContextError>()
            && let Some(code) = pce.classify()
        {
            return code;
        }
        if let Some(ce) = cause.downcast_ref::<CommandError>()
            && let Some(code) = ce.classify()
        {
            return code;
        }
    }
    classify_library_error(err)
}

/// Classify a [`std::error::Error`] chain against the library ladder alone.
///
/// The second of [`classify_error`]'s two passes, kept callable on its own for
/// the aggregating commands that fold a per-package library failure into one
/// exit code: those chains carry no CLI-local cause, and reaching them through
/// the full [`classify_error`] would put a precedence rung in front of them
/// that they never had.
///
/// Walks the error chain via [`std::error::Error::source`] and downcasts each
/// cause to each known classifiable type. The first cause with a non-`None`
/// `ClassifyExitCode::classify` result wins; otherwise the function falls back
/// to [`ExitCode::Failure`].
pub fn classify_library_error(err: &(dyn std::error::Error + 'static)) -> ExitCode {
    // `successors` walks `err -> err.source() -> ...` without allocating, giving
    // the same reach as `anyhow::Error::chain()` through the std boundary type.
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(code) = try_classify(cause) {
            return code;
        }
    }
    ExitCode::Failure
}

/// Downcast `cause` to every known classifiable type and return the first
/// [`ExitCode`] produced by `ClassifyExitCode::classify`.
///
/// One line per owning library crate, plus `cli_input` for this crate's own
/// input-validation errors — see that module for why they are answered in
/// this pass rather than in [`classify_error`]'s CLI-local one. A new error
/// type joins its crate's submodule ladder, not this list; a new *crate*
/// adds a line here. Each
/// downcast behind these calls is O(1) (`TypeId` check), and `TypeId` equality
/// is exact, so at most one arm in the whole ladder can match a given cause -
/// the order below is a reading aid, not a precedence rule.
fn try_classify(cause: &(dyn std::error::Error + 'static)) -> Option<ExitCode> {
    cli_input::try_downcast(cause)
        .or_else(|| ocx_package::try_downcast(cause))
        .or_else(|| ocx_util::try_downcast(cause))
        .or_else(|| ocx_project::try_downcast(cause))
        .or_else(|| ocx_config::try_downcast(cause))
        .or_else(|| ocx_oci::try_downcast(cause))
        .or_else(|| ocx_index::try_downcast(cause))
        .or_else(|| ocx_announce::try_downcast(cause))
        .or_else(|| ocx_package_manager::try_downcast(cause))
        .or_else(|| ocx_store::try_downcast(cause))
        .or_else(|| ocx_shell::try_downcast(cause))
        .or_else(|| ocx_setup::try_downcast(cause))
        .or_else(|| ocx_sign::try_downcast(cause))
        // Last, as it was: the `std::io::Error` tail that cannot be an impl
        // at all (orphan rule).
        .or_else(|| classify::try_downcast(cause))
}

/// One rung of an `exit/<crate>.rs` ladder.
///
/// Expands to the downcast-and-classify step the flat ladder used to spell out
/// by hand: exact `TypeId` match, then the type's own `classify`, then fall
/// through to the next rung.
macro_rules! downcast_arm {
    ($cause:expr, $ty:ty) => {
        if let Some(e) = $cause.downcast_ref::<$ty>()
            && let Some(code) = $crate::exit::ClassifyExitCode::classify(e)
        {
            return Some(code);
        }
    };
}

pub(crate) use downcast_arm;

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use ocx_util::boolean_string::BooleanStringError;

    use super::{ClassifyExitCode, ExitCode};

    /// The classification table as it stood before this work package moved it.
    ///
    /// Recovered from `7adaea62`, not regenerated from the current tree —
    /// regenerating would re-baseline the very thing it pins.
    const BASELINE: &str = include_str!("exit/classify_baseline_7adaea62.json");

    /// `ocx_lib` names a dozen error enums `Error`, so the baseline's `type`
    /// field is ambiguous on its own and its `source` file is what tells them
    /// apart. Each entry maps a declaring file to the name the relocated
    /// classifier imports it under.
    const ALIASES: &[(&str, &str)] = &[
        ("crates/ocx_lib/src/archive/error.rs", "ArchiveError"),
        ("crates/ocx_lib/src/ci/error.rs", "CiError"),
        ("crates/ocx_lib/src/compression/error.rs", "CompressionError"),
        ("crates/ocx_lib/src/config/error.rs", "ConfigError"),
        // The key stays the dead spelling — it joins against the frozen
        // baseline's `source` field, and re-pointing it would break the join
        // silently. The value is a live Rust name, and WP-37 deleted the type it
        // used to spell, so it names what the rows under it now are: the root
        // error the split dissolved, whose arms `DISSOLVED` and `STANDS_IN_FOR`
        // answer for.
        ("crates/ocx_lib/src/error.rs", "DissolvedRootError"),
        ("crates/ocx_lib/src/file_structure/error.rs", "FileStructureError"),
        ("crates/ocx_lib/src/oci/index/error.rs", "OciIndexError"),
        ("crates/ocx_lib/src/package/error.rs", "PackageError"),
        ("crates/ocx_lib/src/package_manager/error.rs", "PackageManagerError"),
        ("crates/ocx_lib/src/project/error.rs", "ProjectError"),
        ("crates/ocx_lib/src/project/registry/error.rs", "ProjectRegistryError"),
        ("crates/ocx_lib/src/setup/error.rs", "SetupError"),
        ("crates/ocx_lib/src/utility/singleflight.rs", "SingleflightError"),
    ];

    /// Stands in for every spelling of the enum an `impl` is for. Both sides of
    /// the comparison pass through here, so the token only has to be one no
    /// error enum uses.
    const SELF_TY: &str = "SelfTy";

    /// One `pattern => value` arm of a classifier `match`.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ClassifierArm {
        /// The type the `impl` is for, as written — `PackageError`,
        /// `ocx_lib::Error`.
        target: String,
        /// `ClassifyExitCode` or `ClassifyErrorKind`.
        trait_name: String,
        /// The `impl` function the arm sits in: `classify`, `exit_code` or
        /// `kind_detail`. A `ClassifyErrorKind` impl maps one variant twice —
        /// once to a code and once to a slug — so the function is what tells
        /// those two apart.
        func: String,
        /// Which `match` expression within that function, counted in source
        /// order. Two arms of *one* `match` may never share a pattern, so a
        /// repeat under this key means the canonicaliser collapsed two distinct
        /// patterns, not that the code has a duplicate.
        match_id: usize,
        /// Where the arm starts, for a drift message that can be opened.
        line: usize,
        /// Rendered as written, guard included — `Error::Io { .. } if x`.
        /// Canonicalising happens in `canonical_row`, which the baseline
        /// side goes through too: one pipeline, so neither side can be
        /// normalised in a way the other is not.
        pattern: String,
        value: String,
    }

    /// Rewrites the spellings the relocation changed, on parsed paths rather
    /// than on raw text.
    ///
    /// The textual pass this replaces called `.replace("Error::", "Self::")`
    /// over the whole arm, which rewrote `SomeError::Io` to `SomeSelf::Io` and
    /// folded distinct patterns onto one key — and because the consumer indexes
    /// a *set* of values per key, a collided arm that kept its old value hid the
    /// one that changed whenever the two shared a value, which for
    /// `Some(ExitCode::DataError)` is the common case. Rewriting only a path's
    /// head segment cannot do that.
    struct Relocation {
        /// The last identifier of the `impl` target — `Error` for
        /// `ocx_lib::Error`.
        target: String,
    }

    /// The `ocx_lib` path an extracted crate's module used to be, one row per
    /// extracted *module path* — not per crate, because a crate does not
    /// always take a whole top-level module with it. `ocx_sign` took four
    /// submodules of `oci` plus `sbom`, so `ocx_sign::sign` canonicalises to
    /// `oci::sign` and `ocx_sign::sbom` to the bare `sbom`: a head-identifier
    /// rename cannot express either, and both sides are therefore paths that
    /// are matched and spliced segment-wise.
    ///
    /// A row is only safe while nothing else can produce the canonical names
    /// it produces — otherwise the rewrite folds two different types onto one
    /// canonical row, and a real exit-code move would then compare equal.
    /// Checked from the tree rather than asserted here:
    ///
    /// - `ocx_oci` / `oci`: what `ocx_lib::oci` still held left with
    ///   `ocx_index` and `ocx_sign`, so nothing answers to `oci` inside
    ///   `ocx_lib` at all, and `ocx_oci`'s own surface must stay clear of the
    ///   four names the `ocx_sign` rows splice underneath `oci`.
    /// - `ocx_trust` / `trust`: the whole module left, so nothing answers to
    ///   `trust` inside `ocx_lib` at all and no collision is possible.
    /// - the five `ocx_sign` rows: the modules left `ocx_lib` whole, and their
    ///   old parent `oci` left with them, so the residual side is empty for
    ///   every one of them.
    ///
    /// Three rows carry a rename some arm actually writes — `ocx_oci`,
    /// `ocx_trust` and `ocx_sign::sign`; deleting any one of them reds
    /// [`every_classification_matches_the_pre_split_baseline`], measured row by
    /// row. The other four are not decoration: each carries the residual
    /// assertion in
    /// [`every_extracted_module_row_renames_onto_an_unoccupied_name`], which
    /// reds on an `ocx_lib` tree that answers to its old name again, and each
    /// is the bridge already in place for the first arm to name it. Eleven
    /// crates have left `ocx_lib` and three have rows: a row is owed by what a
    /// classification arm *spells*, never by an extraction happening, and the
    /// coverage assertion beside the baseline pin is what says so from the tree
    /// rather than from this comment.
    ///
    /// **The premise this table rests on, stated where a row is written.** The
    /// baseline is frozen at `7adaea62` and is never regenerated — its own
    /// `__README` forbids it. So a row's job is permanently to map *today's*
    /// spelling back to that one frozen spelling, and a row never goes stale:
    /// once the last arm naming its crate is gone the row simply stops being
    /// reached. Re-freeze the baseline at a later commit and that stops being
    /// true — every row becomes live against a tree that already uses the new
    /// spelling, and the renames start moving paths *away* from the baseline
    /// rather than onto it. Whoever re-freezes must delete this table in the
    /// same commit.
    ///
    /// DEC-46 added `func` and `match_id` to every baseline row, which is a
    /// **resolution** change at an unchanged revision, not a re-freeze: the
    /// arms, patterns, values and sources are byte-identical to what was
    /// recovered, and the enrichment is a join on `source`, a bijection over
    /// all 372 rows. The revision is still `7adaea62`, so the premise above is
    /// untouched. The distinction is worth keeping straight, because the next
    /// reader will otherwise see a baseline change and conclude the rule was
    /// broken.
    const EXTRACTED_MODULES: &[(&str, &str)] = &[
        ("ocx_oci", "oci"),
        ("ocx_trust", "trust"),
        ("ocx_sign::sign", "oci::sign"),
        ("ocx_sign::attest", "oci::attest"),
        ("ocx_sign::verify", "oci::verify"),
        ("ocx_sign::simplesigning", "oci::simplesigning"),
        ("ocx_sign::sbom", "sbom"),
        ("ocx_shell::shell", "shell"),
        ("ocx_shell::ci", "ci"),
    ];

    /// Splits a `::`-joined row side into its segments.
    fn segments(path: &str) -> Vec<&str> {
        path.split("::").collect()
    }

    /// The baseline spelling of `head`, if some row's new side is a prefix of
    /// it, as (segments matched, replacement segments).
    ///
    /// Longest match wins, so a row for a crate and a row for one of its
    /// modules can coexist and the more specific one decides. Ties are
    /// impossible: two rows with the same new side are refused by
    /// [`every_extracted_module_row_renames_onto_an_unoccupied_name`].
    fn extracted_module(head: &[String]) -> Option<(usize, Vec<&'static str>)> {
        EXTRACTED_MODULES
            .iter()
            .map(|(new, old)| (segments(new), segments(old)))
            .filter(|(new, _)| head.len() > new.len() && head.iter().zip(new).all(|(have, want)| have == want))
            .max_by_key(|(new, _)| new.len())
            .map(|(new, old)| (new.len(), old))
    }

    /// Every `ocx_*` path head in `text`.
    ///
    /// After canonicalisation this must return nothing. The baseline predates
    /// the split and spells no crate name anywhere in a pattern or a value —
    /// measured, not assumed: zero `ocx_*::` occurrences over its 372 arms. So
    /// a head that survives is a spelling the bridge does not carry, which is
    /// what a crate with no [`EXTRACTED_MODULES`] row looks like from here.
    ///
    /// Heads rather than crate names: an `ocx_` head that is not a crate at all
    /// is a typo, and this refuses that too.
    fn crate_heads(text: &str) -> BTreeSet<String> {
        let bytes = text.as_bytes();
        let mut out = BTreeSet::new();
        for (start, _) in text.match_indices("ocx_") {
            if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
                continue;
            }
            let end = text[start..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .map_or(text.len(), |offset| start + offset);
            // `text` is `to_token_stream().to_string()`, which renders a path
            // as `a :: b`. Requiring the separator to sit flush against the
            // identifier is how the first draft of this read every canonical
            // string as clean and found nothing in either state.
            if text[end..].trim_start().starts_with("::") {
                out.insert(text[start..end].to_owned());
            }
        }
        out
    }

    impl Relocation {
        fn for_target(target: &str) -> Self {
            Self {
                target: target.rsplit("::").next().unwrap_or(target).to_owned(),
            }
        }
    }

    impl syn::visit_mut::VisitMut for Relocation {
        fn visit_path_mut(&mut self, path: &mut syn::Path) {
            syn::visit_mut::visit_path_mut(self, path);
            path.leading_colon = None;
            // `crate::`, `ocx_lib::` and `super::` name the same item from
            // either tree; the split moved only which one is in scope.
            while path.segments.len() > 1
                && matches!(
                    path.segments[0].ident.to_string().as_str(),
                    "crate" | "ocx_lib" | "super" | "cli"
                )
            {
                path.segments = path.segments.iter().skip(1).cloned().collect();
            }
            // An extraction is the same items at a new address, so a path the
            // baseline spelled `oci::sign::…` now reads `ocx_sign::sign::…`.
            // Same type, same arm, same code — only the route to it changed.
            // Splicing the old prefix back is the bridge DEC-24 asks for;
            // refreshing the baseline instead would re-baseline the very thing
            // it pins. Prefix-only, so a *different* enum whose path merely
            // contains the segments is untouched. See `extracted_module` for
            // why each row is safe.
            let head: Vec<String> = path.segments.iter().map(|segment| segment.ident.to_string()).collect();
            if let Some((matched, old)) = extracted_module(&head) {
                let span = path.segments[0].ident.span();
                let tail: Vec<syn::PathSegment> = path.segments.iter().skip(matched).cloned().collect();
                path.segments = old
                    .into_iter()
                    .map(|segment| syn::PathSegment::from(syn::Ident::new(segment, span)))
                    .chain(tail)
                    .collect();
            }
            // The matched enum names itself three ways across the two trees:
            // `Self::`, the bare `Error::` the library wrote, and the alias the
            // binary imports. Only as a path *head* does any of them mean it —
            // `docker_credential::CredentialRetrievalError::Timeout` is a
            // different enum that merely ends in the same word.
            if path.segments.len() > 1 {
                let head = path.segments[0].ident.to_string();
                if head == "Self" || head == "Error" || head == self.target {
                    path.segments[0].ident = syn::Ident::new(SELF_TY, path.segments[0].ident.span());
                }
            }
            // The chain walker was renamed where it moved.
            if let Some(last) = path.segments.last_mut()
                && last.ident == "classify_error"
            {
                last.ident = syn::Ident::new("classify_library_error", last.ident.span());
            }
        }

        fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
            syn::visit_mut::visit_macro_mut(self, mac);
            mac.tokens = strip_crate_prefixes(std::mem::take(&mut mac.tokens));
        }
    }

    /// A macro body is an opaque token stream that `syn` does not descend into,
    /// and one classification arm's value is
    /// `matches!(client, crate::oci::…::ClientError::RegistryTransient(_))`.
    /// The crate prefix inside it has to come off at the token level or the two
    /// trees never agree on that arm.
    ///
    /// A whole `a::b::c` run is read at once rather than one identifier at a
    /// time, because [`EXTRACTED_MODULES`] matches a path prefix and
    /// `ocx_sign::sign` is two segments. The run ends at the last identifier,
    /// so a turbofish or a `::{…}` group after it is left to the caller's loop
    /// untouched.
    fn strip_crate_prefixes(tokens: proc_macro2::TokenStream) -> proc_macro2::TokenStream {
        /// The identifiers of the `a :: b :: c` run starting at `start`, and
        /// the index one past its last token.
        fn path_run(trees: &[proc_macro2::TokenTree], start: usize) -> (Vec<String>, usize) {
            let mut names = Vec::new();
            let mut index = start;
            // `get`, not `[]`: the advance below only steps onto a position it
            // has already seen an identifier at, but the bound is worth
            // keeping where the loop reads it rather than where it advances.
            while let Some(proc_macro2::TokenTree::Ident(ident)) = trees.get(index) {
                names.push(ident.to_string());
                let separated = matches!(trees.get(index + 1), Some(proc_macro2::TokenTree::Punct(punct)) if punct.as_char() == ':')
                    && matches!(trees.get(index + 2), Some(proc_macro2::TokenTree::Punct(punct)) if punct.as_char() == ':')
                    && matches!(trees.get(index + 3), Some(proc_macro2::TokenTree::Ident(_)));
                if !separated {
                    return (names, index + 1);
                }
                index += 3;
            }
            (names, index)
        }

        let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
        let mut out: Vec<proc_macro2::TokenTree> = Vec::new();
        let mut index = 0;
        while index < trees.len() {
            if let proc_macro2::TokenTree::Group(group) = &trees[index] {
                let mut rebuilt = proc_macro2::Group::new(group.delimiter(), strip_crate_prefixes(group.stream()));
                rebuilt.set_span(group.span());
                out.push(proc_macro2::TokenTree::Group(rebuilt));
                index += 1;
                continue;
            }
            if !matches!(&trees[index], proc_macro2::TokenTree::Ident(_)) {
                out.push(trees[index].clone());
                index += 1;
                continue;
            }

            let span = trees[index].span();
            let (mut names, next) = path_run(&trees, index);
            let before = names.clone();
            // The `visit_path_mut` rewrites, applied where `syn` cannot reach.
            while names.len() > 1 && matches!(names[0].as_str(), "crate" | "ocx_lib" | "super" | "cli") {
                names.remove(0);
            }
            if let Some((matched, old)) = extracted_module(&names) {
                let tail = names.split_off(matched);
                names = old.iter().map(|segment| (*segment).to_owned()).chain(tail).collect();
            }

            if names == before {
                out.extend(trees[index..next].iter().cloned());
            } else {
                for (position, name) in names.iter().enumerate() {
                    if position > 0 {
                        out.push(proc_macro2::TokenTree::Punct(proc_macro2::Punct::new(
                            ':',
                            proc_macro2::Spacing::Joint,
                        )));
                        out.push(proc_macro2::TokenTree::Punct(proc_macro2::Punct::new(
                            ':',
                            proc_macro2::Spacing::Alone,
                        )));
                    }
                    out.push(proc_macro2::TokenTree::Ident(proc_macro2::Ident::new(name, span)));
                }
            }
            index = next;
        }
        out.into_iter().collect()
    }

    /// A block-form arm (`… => { value }`) and the one-line arm it was reflowed
    /// from are the same mapping, so the braces come off before comparison.
    /// Only when the block is one expression and nothing else — a block that
    /// computes is a different value, not a reflow.
    fn unwrap_reflowed_block(mut expr: syn::Expr) -> syn::Expr {
        while let syn::Expr::Block(block) = &expr {
            if block.attrs.is_empty()
                && block.label.is_none()
                && block.block.stmts.len() == 1
                && let syn::Stmt::Expr(inner, None) = &block.block.stmts[0]
            {
                expr = inner.clone();
                continue;
            }
            break;
        }
        expr
    }

    /// How an arm's left-hand side is keyed: the pattern, and the guard that
    /// tells it apart from its siblings.
    ///
    /// `ForgeError::Status { status, .. }` is three arms of one `match`,
    /// separated only by `if *status == 429` and friends, mapping to three
    /// different exit codes. Keying on the pattern alone folds all three onto
    /// one entry whose value set then satisfies any of their baseline rows.
    fn render_arm_pattern(pat: &syn::Pat, guard: Option<&syn::Expr>) -> String {
        let mut rendered = quote::ToTokens::to_token_stream(pat).to_string();
        if let Some(guard) = guard {
            rendered.push_str(" if ");
            rendered.push_str(&quote::ToTokens::to_token_stream(guard).to_string());
        }
        rendered
    }

    /// The baseline stores each side as the text it had at the base revision.
    /// Reading it back through the same parser and the same canonicaliser the
    /// source goes through is what makes a reflowed arm and the one-line arm it
    /// came from compare equal, without either being compared as text.
    fn canonical_row(pattern: &str, value: &str, target: &str) -> Result<(String, String), syn::Error> {
        let pattern = pattern.trim().trim_end_matches(',');
        let value = value.trim().trim_end_matches(',').trim_end_matches(';').trim();
        let parsed: syn::ExprMatch = syn::parse_str(&format!("match __subject {{ {pattern} => {value} }}"))?;
        let arm = parsed
            .arms
            .into_iter()
            .next()
            .ok_or_else(|| syn::Error::new(proc_macro2::Span::call_site(), "the row is not one match arm"))?;
        let syn::Arm {
            mut pat,
            guard,
            body: mut value,
            ..
        } = arm;
        let mut guard = guard.map(|(_, expr)| *expr);
        let mut relocation = Relocation::for_target(target);
        syn::visit_mut::VisitMut::visit_pat_mut(&mut relocation, &mut pat);
        if let Some(guard) = guard.as_mut() {
            syn::visit_mut::VisitMut::visit_expr_mut(&mut relocation, guard);
        }
        syn::visit_mut::VisitMut::visit_expr_mut(&mut relocation, &mut value);
        Ok((
            render_arm_pattern(&pat, guard.as_ref()),
            quote::ToTokens::to_token_stream(&unwrap_reflowed_block(*value)).to_string(),
        ))
    }

    /// Every `pattern => value` arm inside a `ClassifyExitCode` /
    /// `ClassifyErrorKind` impl.
    ///
    /// Parsed, not scanned. The line-oriented scan this replaces had three
    /// defects that all read as success: a rustfmt-wrapped header
    /// (`impl ClassifyExitCode\n    for <LongType>\n{`) matched no `impl … {`
    /// line and hid the whole impl while the aggregate arm count stayed above
    /// the baseline; splitting every body line on `=>` manufactured an arm from
    /// any `=>` inside a closure, a string or a comment; and a block-form arm
    /// recorded `{` as its value.
    fn arms_in(source: &str) -> Vec<ClassifierArm> {
        arms_in_file(&syn::parse_file(source).expect("a classifier source file parses as Rust"))
    }

    /// [`arms_in`] over a tree someone else parsed.
    ///
    /// The split exists because the pin reads every classifier source twice —
    /// once for the trait impls, once for the `downcast_arm!` ladder — and
    /// `syn::parse_file` is that test's entire cost. One parse, two readers.
    fn arms_in_file(file: &syn::File) -> Vec<ClassifierArm> {
        let mut scan = ImplScan { out: Vec::new() };
        syn::visit::Visit::visit_file(&mut scan, file);
        scan.out
    }

    /// Every type the `downcast_arm!` ladder registers, as written.
    ///
    /// A rung is not a `match` arm, so [`arms_in_file`] cannot see it — and
    /// after WP-37 a rung is the only surviving form of some baseline
    /// delegations. Read as syntax rather than grepped: a macro invocation
    /// inside a function body is a `syn::Macro`, and the commented-out or
    /// string-embedded spellings a text scan would count are not.
    ///
    /// Takes a parsed tree, like [`arms_in_file`] — see that function for why
    /// the parse is separated from the read.
    fn armed_types_in_file(file: &syn::File) -> Vec<String> {
        struct MacroScan {
            out: Vec<String>,
        }
        impl<'ast> syn::visit::Visit<'ast> for MacroScan {
            fn visit_macro(&mut self, node: &'ast syn::Macro) {
                if node
                    .path
                    .segments
                    .last()
                    .is_some_and(|last| last.ident == "downcast_arm")
                    && let Some((_, ty)) = node.tokens.to_string().split_once(',')
                {
                    self.out.push(ty.trim().to_owned());
                }
                syn::visit::visit_macro(self, node);
            }
        }
        let mut scan = MacroScan { out: Vec::new() };
        syn::visit::Visit::visit_file(&mut scan, file);
        scan.out
    }

    struct ImplScan {
        out: Vec<ClassifierArm>,
    }

    impl<'ast> syn::visit::Visit<'ast> for ImplScan {
        fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
            syn::visit::visit_item_impl(self, node);
            let Some((_, path, _)) = &node.trait_ else {
                return;
            };
            let Some(trait_name) = path.segments.last().map(|segment| segment.ident.to_string()) else {
                return;
            };
            if trait_name != "ClassifyExitCode" && trait_name != "ClassifyErrorKind" {
                return;
            }
            let target = quote::ToTokens::to_token_stream(&node.self_ty)
                .to_string()
                .replace(' ', "");
            for item in &node.items {
                let syn::ImplItem::Fn(function) = item else {
                    continue;
                };
                let mut matches = MatchScan { arms: Vec::new() };
                syn::visit::Visit::visit_block(&mut matches, &function.block);
                for (match_id, pat, guard, value) in matches.arms {
                    self.out.push(ClassifierArm {
                        target: target.clone(),
                        trait_name: trait_name.clone(),
                        func: function.sig.ident.to_string(),
                        match_id,
                        line: syn::spanned::Spanned::span(&pat).start().line,
                        pattern: render_arm_pattern(&pat, guard.as_ref()),
                        value: quote::ToTokens::to_token_stream(&value).to_string(),
                    });
                }
            }
        }
    }

    /// Every `match` in one function, nested ones included: an arm that
    /// delegates to an inner `match` classifies through it, and the baseline
    /// records both levels.
    struct MatchScan {
        arms: Vec<(usize, syn::Pat, Option<syn::Expr>, syn::Expr)>,
    }

    impl<'ast> syn::visit::Visit<'ast> for MatchScan {
        fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
            let match_id = self.arms.last().map_or(0, |(id, ..)| id + 1);
            for arm in &node.arms {
                let guard = arm.guard.as_ref().map(|(_, expr)| (**expr).clone());
                self.arms.push((match_id, arm.pat.clone(), guard, (*arm.body).clone()));
            }
            syn::visit::visit_expr_match(self, node);
        }
    }

    /// Every Rust file of the binary crate.
    ///
    /// This was `exit/*.rs` plus one named file, and `CommandError` classifies
    /// from `app.rs`, which that list did not name. Nothing was lost — its
    /// `classify` is `Some(self.code)` with no match, so it carries no arm —
    /// but which files the pin reads should not be a list someone has to
    /// remember to extend.
    fn classifier_sources() -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).expect("a source directory is readable") {
                let path = entry.expect("a directory entry is readable").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    out.push(path);
                }
            }
        }
        let mut files = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        files.sort();
        files
    }

    /// The three spellings a classifier is written with.
    ///
    /// Both readers compare the **last path segment** against one of these
    /// literal idents — [`ImplScan`] against the trait name, `MacroScan`
    /// against `downcast_arm`. So a file whose text carries none of them
    /// cannot hold anything either reader would find, including through an
    /// aliased `use`: an alias is invisible to the visitors for exactly the
    /// reason it is invisible to this filter. The filter has the readers' own
    /// blind spots and no others, which is what makes skipping the parse sound
    /// rather than a text shortcut standing in for a parser.
    const CLASSIFIER_SPELLINGS: [&str; 3] = ["ClassifyExitCode", "ClassifyErrorKind", "downcast_arm"];

    /// How many of the crate's sources may carry a classifier, and their parsed
    /// trees — plus how many files were walked to find them.
    ///
    /// Parsing every source of this crate twice was the whole cost of
    /// [`every_classification_matches_the_pre_split_baseline`]: 191 files, two
    /// `syn::parse_file` calls each. Twenty-nine of them mention a classifier
    /// at all, so the pin now parses those once and reads both scans off one
    /// tree.
    ///
    /// The walked count is returned rather than discarded because a filter
    /// that stopped matching and a tree that genuinely lost its classifiers
    /// produce the same short list; the caller floors both numbers.
    fn parsed_classifier_sources() -> (usize, Vec<(PathBuf, syn::File)>) {
        let paths = classifier_sources();
        let walked = paths.len();
        let parsed = paths
            .into_iter()
            .filter_map(|path| {
                let source = std::fs::read_to_string(&path).expect("a classifier source file is readable");
                if !CLASSIFIER_SPELLINGS.iter().any(|spelling| source.contains(spelling)) {
                    return None;
                }
                let file = syn::parse_file(&source).expect("a classifier source file parses as Rust");
                Some((path, file))
            })
            .collect();
        (walked, parsed)
    }

    /// Every [`EXTRACTED_MODULES`] row renames onto a name nothing else answers
    /// to (DEC-43).
    ///
    /// The rewrite is prefix-only, so a row turns `<new>::rest` into
    /// `<old>::rest`. That is sound exactly while no second thing answers to
    /// `<old>`: where one does, two *different* types canonicalise to one
    /// string, and [`a_fold_onto_one_canonical_name_hides_a_moved_exit_code`]
    /// shows what that costs — the pin goes green over an exit code that
    /// moved.
    ///
    /// Two ways a second thing can answer, and both are read off the tree:
    ///
    /// - `crates/ocx_lib/src/<old>` still exists, so the residual module
    ///   reaches the canonical space as itself, unrewritten. An extraction
    ///   that left part of a module behind must give the row the *moved*
    ///   sub-path, not the parent. Retired at WP-37 with the directory it
    ///   read; the body says what replaced it.
    /// - another row emits `<old>` as one of its own next segments. The path
    ///   form is what makes this reachable: `ocx_sign::sign` canonicalises to
    ///   `oci::sign`, which is a name the `ocx_oci` row would also emit the day
    ///   `crates/ocx_oci/src/sign.rs` appears.
    ///
    /// Derived from the rows rather than listed beside them, because the table
    /// gains rows per extraction and ten crates are still ahead. The prose on
    /// [`EXTRACTED_MODULES`] records *why* each row is safe; this records
    /// *that* it is, and reds when it stops being.
    #[test]
    fn every_extracted_module_row_renames_onto_an_unoccupied_name() {
        /// The names a module directory answers to at its next segment: one
        /// entry per `foo.rs` and per `foo/`. A leaf module is a file and has
        /// no such directory, so it contributes nothing here — its own items
        /// are not on disk to be read.
        fn surface(dir: &Path) -> BTreeSet<String> {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return BTreeSet::new();
            };
            entries
                .filter_map(|entry| {
                    let path = entry.expect("a directory entry is readable").path();
                    let name = path.file_stem()?.to_str()?.to_owned();
                    let is_module = path.is_dir() || path.extension().is_some_and(|extension| extension == "rs");
                    (is_module && name != "lib" && name != "mod").then_some(name)
                })
                .collect()
        }

        /// `base` walked down `path`'s segments, as Rust resolves a module: a
        /// directory, or the `.rs` file of the same name.
        fn resolve(base: &Path, path: &str) -> Option<PathBuf> {
            let mut at = base.to_path_buf();
            for segment in segments(path) {
                at = at.join(segment);
            }
            if at.is_dir() {
                return Some(at);
            }
            let file = at.with_extension("rs");
            file.is_file().then_some(file)
        }

        let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ocx_cli has a parent");
        // The residual side, retired with its subject at WP-37. Each row used
        // to assert `resolve(&crates/ocx_lib/src, old).is_none()` — that
        // nothing inside the monolith still answered to the pre-extraction
        // name. `crates/ocx_lib` is deleted, so that call now returns `None`
        // for every row whatever the tree looks like: a green indistinguishable
        // from the check never having run. Replaced by the one thing still
        // worth stating, which reds if the tree grows the directory back.
        assert!(
            !crates.join("ocx_lib").exists(),
            "crates/ocx_lib is back; the residual-module check below was retired on the premise that \
             it is gone, and every row's `<old>` name is unguarded again"
        );

        assert!(
            !EXTRACTED_MODULES.is_empty(),
            "EXTRACTED_MODULES is empty, so this guard checks nothing; the bridge is not optional \
             while the baseline is frozen at a pre-split commit"
        );

        // Canonical next-segment name -> the row that emits it.
        let mut emitted: BTreeMap<String, &str> = BTreeMap::new();
        let mut seen_new: BTreeSet<&str> = BTreeSet::new();
        let mut seen_old: BTreeSet<&str> = BTreeSet::new();

        for (new, old) in EXTRACTED_MODULES {
            assert!(
                seen_new.insert(new),
                "`{new}` has two rows, and the longest-prefix lookup would have to break the tie \
                 arbitrarily"
            );
            assert!(
                seen_old.insert(old),
                "two rows rename onto `{old}`, which folds both onto one canonical prefix"
            );

            // Without this a mis-spelled row reads an absent directory, and
            // every check below passes having compared nothing against
            // nothing.
            let crate_name = segments(new)[0];
            assert!(
                crates.join(crate_name).join("src").join("lib.rs").is_file(),
                "`{crate_name}` is not a crate root, so `{new}` names nothing"
            );
            let source = resolve(
                &crates.join(crate_name).join("src"),
                new[crate_name.len()..].trim_start_matches(':'),
            )
            .or_else(|| (segments(new).len() == 1).then(|| crates.join(crate_name).join("src")));
            let source = source.unwrap_or_else(|| {
                panic!("`{new}` resolves to no module under crates/{crate_name}/src, so the row is a typo")
            });

            for name in surface(&source) {
                let key = format!("{old}::{name}");
                if let Some(previous) = emitted.insert(key.clone(), new) {
                    panic!("`{previous}` and `{new}` both emit the canonical name `{key}`");
                }
            }
        }

        let buried: Vec<(&&str, &&str)> = EXTRACTED_MODULES
            .iter()
            .filter_map(|(_, old)| emitted.get(*old).map(|by| (old, by)))
            .collect();
        assert!(
            buried.is_empty(),
            "these rows rename onto a name another row already emits, so one canonical prefix is \
             reachable two ways and two different types would fold onto it: {buried:?}"
        );
    }

    /// Two spellings folded onto one canonical name hide a moved exit code
    /// (DEC-43's subject, shown rather than asserted).
    ///
    /// The `collapsed` check in
    /// [`every_classification_matches_the_pre_split_baseline`] catches a fold
    /// *inside one `match`*, because one match cannot carry a pattern twice.
    /// It keys on the function and the match, so two arms in different
    /// functions are two legitimate keys — while the index that the baseline is
    /// compared against keys on neither. The value sets merge, the comparison
    /// is membership, and the arm whose value moved is satisfied by the arm
    /// that did not.
    ///
    /// So the in-match check is not the guard for this; disjointness is, which
    /// is why [`every_extracted_module_row_renames_onto_an_unoccupied_name`]
    /// exists. This pins the hazard so that guard's subject cannot be argued
    /// away.
    #[test]
    fn a_fold_onto_one_canonical_name_hides_a_moved_exit_code() {
        let source = concat!(
            "impl ClassifyExitCode for ocx_lib::Error {\n",
            "    fn classify(&self) -> Option<ExitCode> {\n",
            "        match self {\n",
            "            Self::A(ocx_oci::ssrf::Probe::X) => Some(ExitCode::DataError),\n",
            "        }\n",
            "    }\n",
            "    fn classify_elsewhere(&self) -> Option<ExitCode> {\n",
            "        match self {\n",
            "            Self::A(ocx_lib::oci::ssrf::Probe::X) => Some(ExitCode::Unavailable),\n",
            "        }\n",
            "    }\n",
            "}\n",
        );

        let mut index: BTreeMap<(String, String, String), BTreeSet<String>> = BTreeMap::new();
        let mut per_match: BTreeMap<(String, String, String, usize, String), usize> = BTreeMap::new();
        let mut canonical = BTreeSet::new();
        for arm in arms_in(source) {
            let (pattern, value) =
                canonical_row(&arm.pattern, &arm.value, &arm.target).expect("the probe re-reads as one arm");
            canonical.insert(pattern.clone());
            *per_match
                .entry((
                    arm.target.clone(),
                    arm.trait_name.clone(),
                    arm.func,
                    arm.match_id,
                    pattern.clone(),
                ))
                .or_default() += 1;
            index
                .entry((arm.target, arm.trait_name, pattern))
                .or_default()
                .insert(value);
        }

        assert_eq!(
            canonical.len(),
            1,
            "the two spellings must fold onto one name for this hazard to exist at all, got {canonical:?}"
        );
        assert_eq!(
            per_match.values().filter(|count| **count > 1).count(),
            0,
            "the in-match collapse check must NOT fire here — it keys on the function, and that is              precisely the gap this test records"
        );
        let values = index.values().next().expect("the probe produced one key");
        assert_eq!(values.len(), 2, "both arms land in one value set: {values:?}");
        assert!(
            values.contains("Some (ExitCode :: DataError)"),
            "a baseline row wanting DataError is satisfied by membership, so the arm that now answers              Unavailable never reds: {values:?}"
        );

        // And the same input under DEC-46's key and comparison, which is what
        // the pin now runs: two arms, two keys, one value each, so the arm that
        // answers `Unavailable` is compared against its own baseline row rather
        // than against the union that was covering for it.
        let mut fine: BTreeMap<(String, String, String, usize, String), BTreeSet<String>> = BTreeMap::new();
        for arm in arms_in(source) {
            let (pattern, value) =
                canonical_row(&arm.pattern, &arm.value, &arm.target).expect("the probe re-reads as one arm");
            fine.entry((arm.target, arm.trait_name, arm.func, arm.match_id, pattern))
                .or_default()
                .insert(value);
        }
        assert_eq!(
            fine.len(),
            2,
            "the finer key separates what the coarse key merged: {fine:?}"
        );
        assert!(
            fine.values().all(|values| values.len() == 1),
            "each arm is alone under its own key, so equality is a well-posed question: {fine:?}"
        );
        let want = "Some (ExitCode :: DataError)".to_owned();
        let hidden = fine
            .values()
            .find(|values| !values.contains(&want))
            .expect("the arm whose value moved is visible on its own under the finer key");
        assert_eq!(
            hidden.iter().next().map(String::as_str),
            Some("Some (ExitCode :: Unavailable)"),
            "the moved value is the one the merged set was hiding"
        );
    }

    /// DEC-46's enrichment added columns to the frozen baseline and altered no
    /// value in it.
    ///
    /// The join that added `func` and `match_id` was keyed on `source`, which is
    /// a bijection over every row — but a bijection on the key says nothing
    /// about what the writer did to the values, and "resolution at an unchanged
    /// revision, not a re-freeze" is indistinguishable from its opposite unless
    /// something checks it. So the five columns that existed before DEC-46 are
    /// hashed and the digest is recorded in the file, computed from the frozen
    /// one: any edit to a pattern, a value or a source reds here, whether it
    /// arrives with an enrichment or on its own.
    #[test]
    fn the_enrichment_added_columns_and_changed_no_frozen_value() {
        use sha2::{Digest, Sha256};

        const FROZEN_COLUMNS: [&str; 5] = ["type", "trait", "pattern", "value", "source"];

        let baseline: serde_json::Value = serde_json::from_str(BASELINE).expect("the baseline fixture is valid JSON");
        let rows = baseline["arms"]
            .as_array()
            .expect("the baseline carries an `arms` array");
        assert!(
            rows.len() > 300,
            "the baseline shrank to {} arms, so the digest below would cover almost nothing",
            rows.len()
        );

        let mut hasher = Sha256::new();
        for row in rows {
            let line = FROZEN_COLUMNS
                .iter()
                .map(|column| {
                    row[*column]
                        .as_str()
                        .unwrap_or_else(|| panic!("every row carries `{column}`"))
                })
                .collect::<Vec<_>>()
                .join("\t");
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        // sha2 0.11 returns a `GenericArray`, which has no `LowerHex`.
        let digest: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();

        let recorded = baseline["frozen_columns_sha256"]
            .as_str()
            .expect("the baseline records the digest of its frozen columns");
        assert_eq!(
            digest, recorded,
            "the frozen columns no longer hash to what the file records. DEC-46 added `func` and \
             `match_id` and nothing else; a value that moved here is a re-freeze wearing an \
             enrichment's clothes, and the pin would then be measuring the tree against itself"
        );
    }

    /// Every variant this binary classifies maps to exactly the code and slug
    /// it mapped to before the relocation.
    ///
    /// **Why variant-to-value and not a vocabulary count.** At `7adaea62` 92
    /// string-valued `kind_detail` arms produced 83 distinct strings, so
    /// many-to-one is normal and a *new* collapse — two variants folded onto one
    /// string that is already present — leaves an 83-versus-83 literal census
    /// completely unchanged while changing what `ocx` emits. Only the mapping
    /// catches that, so the mapping is what is pinned, for both traits: an exit
    /// code is the more load-bearing of the two contracts.
    ///
    /// **The positive control.** A source-text scan that stops recognising
    /// anything reports zero arms and finds no disagreements, which reads
    /// exactly like success. So the extractor is first run over text this test
    /// owns and must return the arm in it; only then is a clean diff evidence.
    ///
    /// Reds on: any arm whose pattern disappears, and any arm whose value
    /// changes — including a wildcard swallowing a variant, which removes the
    /// pattern. Both were run.
    #[test]
    fn every_classification_matches_the_pre_split_baseline() {
        // The positive control. A scan that stops recognising anything reports
        // zero arms and finds no disagreements, which reads exactly like
        // success — so the extractor first runs over text this test owns. The
        // header is the shape rustfmt gives a target too long for one line, and
        // one arm delegates to a nested `match`: the line scan this replaced
        // saw neither.
        let probe = concat!(
            "impl ClassifyExitCode\n",
            "    for ocx_lib::deeply::nested::ProbeError\n",
            "{\n",
            "    fn classify(&self) -> Option<ExitCode> {\n",
            "        match self {\n",
            "            ProbeError::One => Some(ExitCode::DataError),\n",
            "            Self::Two(inner) => match inner {\n",
            "                Inner::Deep => None,\n",
            "            },\n",
            "        }\n",
            "    }\n",
            "}\n",
        );
        let probed: Vec<(String, String, usize, String, String)> = arms_in(probe)
            .into_iter()
            .map(|arm| (arm.target, arm.func, arm.match_id, arm.pattern, arm.value))
            .collect();
        assert_eq!(
            probed,
            PROBE_ARMS
                .iter()
                .map(|(target, func, id, pattern, value)| {
                    (
                        (*target).to_owned(),
                        (*func).to_owned(),
                        *id,
                        (*pattern).to_owned(),
                        (*value).to_owned(),
                    )
                })
                .collect::<Vec<_>>(),
            "the arm extractor must read a wrapped header and a nested match; a scanner that reads nothing \
             would find no disagreement below and the clean diff would mean nothing"
        );

        let baseline: serde_json::Value = serde_json::from_str(BASELINE).expect("the baseline fixture is valid JSON");
        let rows = baseline["arms"]
            .as_array()
            .expect("the baseline carries an `arms` array");
        assert!(rows.len() > 300, "the baseline shrank to {} arms", rows.len());

        let aliases: BTreeMap<&str, &str> = ALIASES.iter().copied().collect();
        // Keyed on the same tuple the collapse check uses (DEC-46). Keyed on
        // `(target, trait, pattern)` it merged 372 arms into 358 keys, and the
        // comparison below asked whether the baseline's value was *among* the
        // merged candidates — so one variant matched in two functions of one
        // impl had each arm satisfying the other's row.
        let mut index: BTreeMap<(String, String, String, usize, String), BTreeSet<String>> = BTreeMap::new();
        let mut per_match: BTreeMap<(String, String, String, usize, String), usize> = BTreeMap::new();
        let mut arm_count = 0usize;
        // Crate heads the canonicaliser did not remove, mapped to where they
        // were written. Asserted empty below.
        let mut uncanonicalised: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        // The `downcast_arm!` rungs, for DEC-106's `ArmedDelegation` rows:
        // each spelling mapped to the file(s) whose ladder registers it. A set
        // of bare spellings would let a row naming `AuthError` be satisfied by
        // some other crate's `AuthError` — the name-match that keeps costing
        // this plan findings.
        let mut armed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let (walked, sources) = parsed_classifier_sources();
        // Floored on the reader, twice. A walk that found no files and a
        // filter that stopped matching both shorten this list, and either
        // leaves every comparison below ranging over nothing — the shape the
        // baseline's own row floor cannot tell from a tree that lost its
        // classifications.
        assert!(
            walked > 150,
            "the source walk read {walked} file(s); this crate carries well over a hundred, so the \
             walk stopped and the arms below are whatever it happened to reach"
        );
        assert!(
            sources.len() > 20,
            "only {} of {walked} source(s) were parsed for classifiers; the tree carries around \
             thirty, so the spelling filter stopped matching and the clean comparison below \
             would mean nothing",
            sources.len()
        );

        for (path, parsed) in &sources {
            for ty in armed_types_in_file(parsed) {
                let file = path
                    .file_name()
                    .expect("a source path has a file name")
                    .to_string_lossy()
                    .into_owned();
                armed.entry(ty).or_default().insert(file);
            }
            for arm in arms_in_file(parsed) {
                arm_count += 1;
                let (pattern, value) = canonical_row(&arm.pattern, &arm.value, &arm.target).unwrap_or_else(|error| {
                    panic!(
                        "{}:{}: the extractor rendered an arm it cannot re-read: {error}",
                        path.display(),
                        arm.line
                    )
                });
                for head in crate_heads(&pattern).into_iter().chain(crate_heads(&value)) {
                    uncanonicalised
                        .entry(head)
                        .or_default()
                        .insert(format!("{}:{}", path.display(), arm.line));
                }
                let key = (
                    arm.target.clone(),
                    arm.trait_name.clone(),
                    arm.func.clone(),
                    arm.match_id,
                    pattern.clone(),
                );
                *per_match.entry(key.clone()).or_default() += 1;
                index.entry(key).or_default().insert(value);
            }
        }

        // A canonicalisation that folds two distinct patterns onto one key does
        // not red by itself: the index holds a *set* of values per key, so the
        // collided arm that kept its value satisfies the row of the arm that
        // changed whenever the two shared a value — and for
        // `Some(ExitCode::DataError)` that is the common case. One `match`
        // cannot carry the same pattern twice, so a repeat under this key is
        // that fold, made visible.
        let collapsed: Vec<String> = per_match
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|((target, trait_name, func, match_id, pattern), count)| {
                format!("{target}/{trait_name}::{func} match #{match_id}: `{pattern}` is {count} arms under one key")
            })
            .collect();
        assert!(
            collapsed.is_empty(),
            "the canonicaliser collapsed {} distinct pattern(s):\n  {}",
            collapsed.len(),
            collapsed.join("\n  ")
        );

        // C-042 (WP-17): a baseline variant that became a **type of its own**.
        // `BooleanStringError` replaced `config::Error::InvalidBooleanString`,
        // and a whole-type `ClassifyExitCode` impl carries no `match` — so
        // `arms_in` reads no arm for it and the row below would report
        // `pattern is gone`, which is exactly what a *deleted* classification
        // looks like. The row is satisfied by **calling** the relocated
        // classifier and rendering its answer through the same `canonical_row`
        // both sides already go through, so the baseline's own recorded value
        // stays the thing compared: a relocation that changed the exit code
        // reds precisely as an in-place edit would.
        //
        // The baseline is not edited to absorb this — its `__README` forbids
        // regenerating or refreshing it, and a row removed to make a red go
        // away is the same coverage loss the file exists to prevent. The
        // mapping is bridged here instead, at the one place both sides meet.
        /// A baseline row whose classification became a type of its own, named
        /// by the coordinates the baseline records it under so the bridged row
        /// lands on exactly the key that row looks up (DEC-46).
        struct Relocated {
            target: &'static str,
            trait_name: &'static str,
            func: &'static str,
            match_id: usize,
            pattern: &'static str,
            code: Option<ExitCode>,
        }

        let relocated = [Relocated {
            target: "ConfigError",
            trait_name: "ClassifyExitCode",
            func: "classify",
            match_id: 0,
            pattern: "Self::InvalidBooleanString { .. }",
            code: BooleanStringError {
                value: "maybe".to_owned(),
                possible: "1, y, yes, true, 0, n, no, false".to_owned(),
            }
            .classify(),
        }];
        for row in &relocated {
            let rendered = match row.code {
                Some(code) => format!("ExitCode::{code:?}"),
                None => "None".to_owned(),
            };
            let (pattern, value) = canonical_row(row.pattern, &rendered, row.target)
                .expect("the bridged row renders as one readable match arm");
            index
                .entry((
                    row.target.to_owned(),
                    row.trait_name.to_owned(),
                    row.func.to_owned(),
                    row.match_id,
                    pattern,
                ))
                .or_default()
                .insert(value);
        }

        // Floored on the reader, not only on its subject: a ladder walk that
        // read nothing and a ladder with no rungs produce the same empty set,
        // and every `ArmedDelegation` row below would then red for the wrong
        // reason — or, worse, a future row would be excused by a set that was
        // never populated.
        assert!(
            armed.len() > 40,
            "the ladder reader found {} `downcast_arm!` rung(s); the tree carries over sixty, so the \
             reader stopped and every delegation row it answers for is unchecked",
            armed.len()
        );

        assert!(
            arm_count + relocated.len() >= rows.len(),
            "the relocated tree yields {arm_count} arms plus {} bridged whole-type classification(s) \
             against the baseline's {}; the walk missed a file",
            relocated.len(),
            rows.len()
        );

        // Coverage, which per-key uniqueness on [`EXTRACTED_MODULES`] does not
        // give: that check refuses two rows for one canonical name, and is
        // perfectly satisfied by a crate with *no* row. The floor above is what
        // makes this one non-vacuous — it fixes how much was read before this
        // asks what was read.
        assert!(
            uncanonicalised.is_empty(),
            "{} crate head(s) survived canonicalisation, so the arms naming them are compared \
             against a baseline that cannot spell them and every such row is a silent miss — \
             each needs an EXTRACTED_MODULES row: {uncanonicalised:#?}",
            uncanonicalised.len()
        );

        let mut drifted = Vec::new();
        // The "is gone" case alone, kept aside until the routes below have
        // spoken (DEC-106). A drift *in place* is still reported from inside
        // the loop: only a row whose declaring impl no longer exists can be
        // answered by another route, and only that case is deferred.
        let mut missing: Vec<(String, String)> = Vec::new();
        let mut unparsed = Vec::new();
        let mut baseline_keys: BTreeSet<(String, String, String, usize, String)> = BTreeSet::new();
        let mut baseline_raw: BTreeMap<String, (String, String)> = BTreeMap::new();
        // Incremented at the comparison itself, never above it: a counter
        // raised before the step that would have rejected the row counts
        // intent rather than work done.
        let mut compared = 0usize;
        for row in rows {
            let declared = row["type"].as_str().expect("every row names a type");
            let source = row["source"].as_str().expect("every row names its source");
            let file = source.rsplit_once(':').map_or(source, |(f, _)| f);
            let target = if declared == "Error" {
                (*aliases
                    .get(file)
                    .unwrap_or_else(|| panic!("no alias recorded for {file}")))
                .to_string()
            } else {
                declared.to_string()
            };
            let trait_name = row["trait"].as_str().expect("every row names its trait").to_string();
            let func = row["func"].as_str().expect("every row names its function").to_string();
            let match_id = row["match_id"].as_u64().expect("every row names its match") as usize;
            let written = row["pattern"].as_str().expect("every row has a pattern");
            let was = row["value"].as_str().expect("every row has a value");
            let Ok((pattern, want)) = canonical_row(written, was, &target) else {
                unparsed.push(format!("{source}: `{written}` => `{was}`"));
                continue;
            };
            // DEC-55's reverse direction reads both of these: the key set says
            // which tree arms the baseline already speaks for, and the raw
            // value keyed by `source` is what a `STANDS_IN_FOR` row names.
            baseline_keys.insert((
                target.clone(),
                trait_name.clone(),
                func.clone(),
                match_id,
                pattern.clone(),
            ));
            baseline_raw.insert(source.to_owned(), (written.to_owned(), was.to_owned()));

            // Asserted inside the loop, over every row: a table test whose
            // checking happens after the loop is one edit away from an empty
            // body, which is how this work package lost coverage once already.
            // Equality, not membership (DEC-46). `values.contains(&want)` asks
            // whether the baseline's value is *among* the candidates; the
            // question is whether it *is* the value. Under the finer key a
            // second value can only come from a fold the collapse check above
            // already refuses, so a set of any other shape is reported rather
            // than tolerated.
            compared += 1;
            match index.get(&(
                target.clone(),
                trait_name.clone(),
                func.clone(),
                match_id,
                pattern.clone(),
            )) {
                None => missing.push((
                    source.to_owned(),
                    format!("{target}/{trait_name}::{func} match #{match_id}: pattern `{pattern}` is gone ({source})"),
                )),
                Some(values) if values.len() != 1 || !values.contains(&want) => drifted.push(format!(
                    "{target}/{trait_name}::{func} match #{match_id}: `{pattern}` was `{want}`, is now \
                     {values:?} ({source})"
                )),
                Some(_) => {}
            }
        }
        // A row the parser cannot read is compared against nothing, so it would
        // otherwise be a silent hole in a 370-row table.
        assert!(
            unparsed.is_empty(),
            "{} baseline row(s) are not parseable Rust and so assert nothing:\n  {}",
            unparsed.len(),
            unparsed.join("\n  ")
        );

        // How many classifications were *compared*, floored twice. `rows`
        // bounds the table; this bounds what the loop did with it. A shortcut
        // that skipped rows — a filter, an early `continue`, a narrowed
        // selection — leaves the table its full size and the coverage
        // narrowed, and every assertion above is satisfied by whatever
        // survived. The literal floor is the row count the baseline was
        // frozen with: this table only ever grows by relocation, never by
        // shrinking.
        assert_eq!(
            compared,
            rows.len(),
            "{compared} of the baseline's {} classification(s) reached the comparison; the rest \
             assert nothing",
            rows.len()
        );
        assert!(
            compared >= 372,
            "{compared} classification(s) were compared; the baseline was frozen with 372 and a \
             row is never dropped to make a red go away"
        );

        // ------------------------------------------------------------------
        // DEC-55 — the other direction.
        //
        // Everything above walks the baseline and asks whether the tree still
        // agrees. That catches an arm that disappeared or changed, and is blind
        // to an arm that was **added**: `arm_count + relocated.len() >=
        // rows.len()` is satisfied by any tree with at least as many arms, so a
        // new arm carrying a wrong exit code passes while every baselined arm
        // stays intact (DEC-53).
        //
        // A genuinely new variant has no baseline row by construction — it did
        // not exist at `7adaea62` — so the rule cannot be "every tree arm has a
        // row". It is: every tree arm is accounted for by exactly one route,
        // and an arm accounted for by none of them fails.
        //
        //   1. it matches a baseline row on the full DEC-46 key (the loop above);
        //   2. it is a `Relocated` bridge (inserted into `index`, and its key is
        //      a baseline key, so route 1 already covers it);
        //   3. it is named in `STANDS_IN_FOR`, which says which baseline arm's
        //      code it copies — and the pin asserts the two values are equal.
        //
        // Route 3 is the per-arm proof an extraction would otherwise write by
        // hand in a commit body, moved somewhere that re-checks it on every run
        // — including on the commit three batches later that changes one of
        // them.
        // DEC-106. A baseline row whose declaring impl was deleted is answered
        // by naming, per row, what still produces its value — never by dropping
        // the row or loosening what equality means. The routes are the same
        // question ("is that value still produced?") with three answers, which
        // is why the checks differ. A `baseline_source` lands here only when
        // its row's value check *passed*: a citation that is not value-checked
        // is a rubber stamp with a table.
        let mut covered: BTreeSet<String> = BTreeSet::new();

        for row in &stands_in_for() {
            let failures_before = drifted.len();
            let (written, was) = baseline_raw.get(row.baseline_source).unwrap_or_else(|| {
                panic!(
                    "`{}`/`{}` stands in for {}, which is not a baseline row — the citation must name \
                     a row's `source` verbatim",
                    row.target, row.pattern, row.baseline_source
                )
            });
            let Ok((baseline_pattern, want)) = canonical_row(written, was, row.target) else {
                panic!("the baseline row at {} is not parseable Rust", row.baseline_source);
            };
            let Ok((pattern, _)) = canonical_row(row.pattern, was, row.target) else {
                panic!("`{}` in STANDS_IN_FOR is not a parseable match arm", row.pattern);
            };
            let key = (
                row.target.to_owned(),
                row.trait_name.to_owned(),
                row.func.to_owned(),
                row.match_id,
                pattern.clone(),
            );
            let Some(values) = index.get(&key) else {
                drifted.push(format!(
                    "{}/{}::{} match #{}: `{pattern}` is named in STANDS_IN_FOR but the tree has no such \
                     arm — a row outliving its arm asserts nothing",
                    row.target, row.trait_name, row.func, row.match_id
                ));
                continue;
            };
            let [is] = &values.iter().cloned().collect::<Vec<_>>()[..] else {
                drifted.push(format!(
                    "{}/{}::{} match #{}: `{pattern}` is {values:?} — one arm, one value",
                    row.target, row.trait_name, row.func, row.match_id
                ));
                continue;
            };
            match &row.claim {
                Claim::SameDelegation => {
                    // Each side against its own binder: the baseline spells it
                    // `e` where the tree spells it `error`, and normalising both
                    // with one pattern's binder leaves the other untouched —
                    // which reads as a drift that is not one.
                    let (baseline_shape, tree_shape) = (
                        delegation_shape(&baseline_pattern, &want),
                        delegation_shape(&pattern, is),
                    );
                    if baseline_shape != tree_shape {
                        drifted.push(format!(
                            "{}/{}::{} match #{}: `{pattern}` claims the delegation of {} (`{want}` \
                             -> `{baseline_shape}`) but is `{is}` -> `{tree_shape}`",
                            row.target, row.trait_name, row.func, row.match_id, row.baseline_source
                        ));
                    }
                }
                Claim::Resolves(code) => {
                    // The baseline side must be a literal for this claim to
                    // mean anything; a delegation there is `SameDelegation`'s
                    // case and mixing them would compare a code against text.
                    let rendered = match code {
                        Some(code) => format!("Some (ExitCode :: {code:?})"),
                        None => "None".to_owned(),
                    };
                    if want != rendered {
                        drifted.push(format!(
                            "{}/{}::{} match #{}: `{pattern}` resolves to `{rendered}` but {} is `{want}`",
                            row.target, row.trait_name, row.func, row.match_id, row.baseline_source
                        ));
                    }
                    if !is.contains("classify ()") {
                        drifted.push(format!(
                            "{}/{}::{} match #{}: `{pattern}` is `{is}`, which delegates to nothing — a \
                             `Resolves` row asserts a code the arm reaches by delegation, so a literal \
                             here belongs in the baseline comparison instead",
                            row.target, row.trait_name, row.func, row.match_id
                        ));
                    }
                }
            }
            // Accounted for, so the sweep below must not also report it.
            baseline_keys.insert(key);
            if drifted.len() == failures_before {
                covered.insert(row.baseline_source.to_owned());
            }
        }

        // DEC-106 route 2: the baseline rows left with no successor arm at all.
        for row in DISSOLVED {
            let (written, was) = baseline_raw.get(row.baseline_source).unwrap_or_else(|| {
                panic!(
                    "`{}` in DISSOLVED is not a baseline row — the citation must name a row's `source` \
                     verbatim",
                    row.baseline_source
                )
            });
            let Ok((pattern, want)) = canonical_row(written, was, "DissolvedRootError") else {
                panic!("the baseline row at {} is not parseable Rust", row.baseline_source);
            };
            match row.claim {
                DissolvedClaim::ArmedDelegation { ty, rung } => {
                    let shape = delegation_shape(&pattern, &want);
                    if shape != "BINDER . classify ()" {
                        drifted.push(format!(
                            "{}: `{pattern}` is claimed as a delegation onto `{ty}`, but its value is \
                             `{want}` -> `{shape}` — a row carrying a code of its own is not answered by \
                             a rung, which only says who is asked",
                            row.baseline_source
                        ));
                        continue;
                    }
                    let sites = armed.get(ty).cloned().unwrap_or_default();
                    if sites != BTreeSet::from([rung.to_owned()]) {
                        drifted.push(format!(
                            "{}: `{pattern}` delegated to `{ty}`, claimed armed by `{rung}`; the ladder \
                             arms that spelling in {sites:?} — a delegation reaching nothing, or reaching \
                             a different type of the same name, loses the row's code either way",
                            row.baseline_source
                        ));
                        continue;
                    }
                }
                DissolvedClaim::FallsThrough => {
                    if want != "None" {
                        drifted.push(format!(
                            "{}: `{pattern}` is retired as a no-op, which holds only for a `None` value; it \
                             is `{want}`",
                            row.baseline_source
                        ));
                        continue;
                    }
                }
            }
            covered.insert(row.baseline_source.to_owned());
        }

        drifted.extend(
            missing
                .into_iter()
                .filter(|(source, _)| !covered.contains(source))
                .map(|(_, message)| message),
        );

        // The floor this sweep needs is the `arm_count + relocated.len() >=
        // rows.len()` assertion already above: a walk that enumerated nothing
        // and a tree with no unaccounted arms produce the same green, and that
        // assertion refuses first. A second floor on `index.len()` was written
        // here and then deleted — the collapse check above refuses any key
        // carrying two arms, so `index.len() == arm_count` whenever the test
        // gets this far, and no tree state reds one without the other. Pointing
        // the walk at `src/api` reds the existing floor at `0 arms against 372`;
        // the duplicate never got to speak, which is a check whose red state is
        // unreachable.

        // DEC-55 route 4: an arm for a variant the tree grew **after** the
        // freeze.
        //
        // `STANDS_IN_FOR` answers "this code did not move", and a variant
        // minted afterwards has nothing to have moved from — so the claim it
        // can make is narrower, and stated rather than derived: *this arm
        // carries this value*. That still buys DEC-55's property. The sweep
        // below refuses an arm nothing speaks for, and the value a new arm
        // carries is written somewhere re-checked on every run, so a later
        // edit to it reds here instead of shipping.
        //
        // Deliberately **not** a `STANDS_IN_FOR` row with a nearby citation.
        // A citation marks its baseline row `covered`, which suppresses the
        // missing-arm report for that row — so a new arm citing a neighbour
        // would be able to answer for that neighbour's later disappearance,
        // which is the one thing route 3 exists to make impossible.
        for row in NEW_ARMS {
            let Ok((pattern, value)) = canonical_row(row.pattern, row.value, row.target) else {
                panic!(
                    "`{} => {}` in NEW_ARMS is not a parseable match arm",
                    row.pattern, row.value
                );
            };
            let key = (
                row.target.to_owned(),
                row.trait_name.to_owned(),
                row.func.to_owned(),
                row.match_id,
                pattern.clone(),
            );
            match index.get(&key) {
                None => drifted.push(format!(
                    "{}/{}::{} match #{}: `{pattern}` is named in NEW_ARMS but the tree has no such \
                     arm — a row outliving its arm asserts nothing",
                    row.target, row.trait_name, row.func, row.match_id
                )),
                Some(values) => {
                    let found: Vec<String> = values.iter().cloned().collect();
                    if found != vec![value.clone()] {
                        drifted.push(format!(
                            "{}/{}::{} match #{}: `{pattern}` is {found:?}, and NEW_ARMS records \
                             `{value}`",
                            row.target, row.trait_name, row.func, row.match_id
                        ));
                    }
                }
            }
            baseline_keys.insert(key);
        }

        let unaccounted: Vec<String> = index
            .keys()
            .filter(|key| !baseline_keys.contains(*key))
            .map(|(target, trait_name, func, match_id, pattern)| {
                format!("{target}/{trait_name}::{func} match #{match_id}: `{pattern}`")
            })
            .collect();
        assert!(
            unaccounted.is_empty(),
            "{} classification arm(s) exist in the tree and are spoken for by nothing — add a \
             `STANDS_IN_FOR` row naming the baseline arm whose code each copies, or the code moved \
             with no record of it:\n  {}",
            unaccounted.len(),
            unaccounted.join("\n  ")
        );

        assert!(
            drifted.is_empty(),
            "{} classification(s) drifted from {}:\n  {}",
            drifted.len(),
            baseline["recovered_from"].as_str().unwrap_or("the baseline"),
            drifted.join("\n  ")
        );
    }

    /// An arm that did not exist at `7adaea62` and so has no baseline row of
    /// its own, paired with the baseline arm whose exit code it copies.
    ///
    /// An extraction that gives a tier its own root error mints variants that
    /// stand in for variants of the crate-wide one — `ocx_index`'s `Store` for
    /// `Error::FileStructure`, its `File` for `Error::InternalFile`. Each such
    /// arm is a claim: *this classifies exactly as the arm it replaced did*.
    /// Written here, with the baseline row cited by its `source`, the claim is
    /// re-checked on every run instead of being asserted once in a commit body
    /// (DEC-55).
    ///
    /// Not a licence to add an arm: the cited row's value is what the new arm
    /// must equal, so a row cannot excuse a code that moved — it can only
    /// record that a code did *not*.
    struct StandsInFor {
        target: &'static str,
        trait_name: &'static str,
        func: &'static str,
        match_id: usize,
        pattern: &'static str,
        /// The baseline row this arm answers for, named by that row's `source`
        /// field verbatim (`<file>:<line>`).
        baseline_source: &'static str,
        claim: Claim,
    }

    /// How a `STANDS_IN_FOR` row is checked.
    ///
    /// Value **text** equality was the obvious rule and it is wrong, measured:
    /// of the five arms already in this tree with no baseline row, four are
    /// correct and none compares equal as text. Two shapes, two reasons.
    enum Claim {
        /// The new arm delegates exactly as the baseline arm delegated —
        /// `e.classify()` became `return error.classify()`. Same code by
        /// construction; only the binder and an early `return` differ, and both
        /// are normalised away before comparing.
        SameDelegation,
        /// The baseline arm carried a **literal** code and the new arm
        /// delegates to a type that did not exist then, so no text comparison
        /// can hold and text equality would reject a correct arm. The code is
        /// recovered by *calling* the classifier the arm delegates to — the
        /// same move `Relocated` above makes — and compared against the
        /// baseline's literal. Evaluated rather than declared: a row stating a
        /// code it does not compute would assert the author's belief instead of
        /// the tree's behaviour.
        Resolves(Option<ExitCode>),
    }

    /// An arm for a variant that **did not exist** at `7adaea62` and stands in
    /// for nothing — a refusal the tree grew afterwards.
    ///
    /// The freeze pins the codes the tool shipped with; it has nothing to say
    /// about a variant minted since, and `STANDS_IN_FOR` is the wrong shape for
    /// one (its claim is "the cited code did not move", and there is no cited
    /// code). What a row here buys is the rest of DEC-55: the arm is accounted
    /// for, and the value it carries is recorded where every later run
    /// re-checks it — so changing an exit code a release wrapper branches on
    /// reds here rather than shipping quietly.
    ///
    /// A row is owed by each arm, including the `kind_detail` slug arms: those
    /// strings ship in JSON envelopes, and the per-variant table in
    /// `exit/<crate>.rs` is the other pin on them.
    struct NewArm {
        target: &'static str,
        trait_name: &'static str,
        func: &'static str,
        match_id: usize,
        pattern: &'static str,
        /// The value the arm must carry, as written in the tree.
        value: &'static str,
    }

    /// Every classification arm minted since the freeze (DEC-55 route 4).
    const NEW_ARMS: &[NewArm] = &[
        // #477 — announce refuses a root whose `name` disagrees with the
        // identifier the run announces. 65, the `DescDisappeared` family: two
        // sides disagree and only a human decides.
        NewArm {
            target: "AnnounceError",
            trait_name: "ClassifyExitCode",
            func: "classify",
            match_id: 0,
            pattern: "Self::RootNameMismatch { .. }",
            value: "Some(ExitCode::DataError)",
        },
        // #477 / #481 — the re-claim path's two disagreement refusals.
        NewArm {
            target: "ClaimError",
            trait_name: "ClassifyExitCode",
            func: "classify",
            match_id: 0,
            pattern: "Self::RootNameMismatch { .. } | Self::RepositoryMismatch { .. }",
            value: "Some(ExitCode::DataError)",
        },
        // #482 — a claim's description failure classifies through the announce
        // taxonomy it reuses, so the arm delegates exactly as `Forge` does.
        NewArm {
            target: "ClaimError",
            trait_name: "ClassifyExitCode",
            func: "classify",
            match_id: 0,
            pattern: "Self::Description(inner)",
            value: "inner.classify()",
        },
        NewArm {
            target: "ClaimError",
            trait_name: "ClassifyErrorKind",
            func: "kind_detail",
            match_id: 0,
            pattern: "Self::RootNameMismatch { .. }",
            value: "\"root_name_mismatch\"",
        },
        NewArm {
            target: "ClaimError",
            trait_name: "ClassifyErrorKind",
            func: "kind_detail",
            match_id: 0,
            pattern: "Self::RepositoryMismatch { .. }",
            value: "\"repository_mismatch\"",
        },
        NewArm {
            target: "ClaimError",
            trait_name: "ClassifyErrorKind",
            func: "kind_detail",
            match_id: 0,
            pattern: "Self::Description(_)",
            value: "\"description\"",
        },
    ];

    /// A baseline arm whose **declaring type was deleted**, named with what
    /// still produces its value (DEC-106).
    ///
    /// `STANDS_IN_FOR` answers a baseline row whose successor is a real match
    /// arm. These are the rows left over when there is none — and a row here
    /// still carries a value check, because "something else handles it now" is
    /// the rubber stamp this table exists to avoid. Deleting the baseline row
    /// instead is never the move: frozen means the values never change, not
    /// that the reader may not learn how a row is satisfied.
    struct Dissolved {
        /// The baseline row this answers for, named by that row's `source`
        /// field verbatim (`<file>:<line>`).
        baseline_source: &'static str,
        claim: DissolvedClaim,
    }

    /// How a `DISSOLVED` row is checked.
    enum DissolvedClaim {
        /// The baseline arm delegated — `e.classify()` — and the delegation
        /// survives as a `downcast_arm!` rung on the named type, which
        /// `arms_in` cannot see because a rung is not a `match` arm.
        ///
        /// Both halves are checked. The baseline value must be a **pure**
        /// delegation: a row carrying a code of its own is not answered by a
        /// rung, since registration says who is asked and never what comes
        /// back. It is a value check only because the baseline's value is
        /// itself *"whatever this type answers"* — the same thing the rung
        /// says. A literal there would make the rung's existence say nothing
        /// about it, which is why that case reds rather than being tolerated.
        ArmedDelegation {
            /// The type as the rung spells it.
            ty: &'static str,
            /// The `exit/*.rs` file whose ladder arms it. Pinned, so a
            /// same-named type armed in another crate's ladder cannot satisfy
            /// this row — the join key is the rung, not the name.
            rung: &'static str,
        },
        /// The baseline arm's value was `None`, and `None` is exactly what an
        /// unarmed type produces by falling through the ladder — so deleting
        /// the arm is a no-op by construction, and the thing to show is that
        /// the default path yields what the arm yielded.
        ///
        /// Checked here only for the `None` value, because the tree-side half
        /// cannot be checked by a name: an armed type is spelled however its
        /// rung imports it, so `armed.contains("<path>::Error")` would be
        /// false whether or not the type is armed — a green that never had a
        /// red. The equivalence is proven concretely instead, by
        /// `shell_error_falls_through_to_the_same_failure`.
        FallsThrough,
    }

    /// WP-37's rows: `ocx_lib::Error` is deleted, so its 32 baseline arms have
    /// no declaring impl left. 26 are answered by `STANDS_IN_FOR` rows against
    /// the arms `ocx_package_manager::Error` and its siblings already carry;
    /// these six are the remainder.
    const DISSOLVED: &[Dissolved] = &[
        // `Self::Auth(e)`, inner `ocx_oci::auth::error::AuthError`.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:379",
            claim: DissolvedClaim::ArmedDelegation {
                ty: "AuthError",
                rung: "ocx_oci.rs",
            },
        },
        // `Self::PackageManager(e)`, inner `Box<ocx_package_manager::Error>` —
        // the tier's whole root error, whose own arms are baselined separately.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:383",
            claim: DissolvedClaim::ArmedDelegation {
                ty: "PackageManagerError",
                rung: "ocx_package_manager.rs",
            },
        },
        // `Self::Identifier(e)`, inner `ocx_oci::identifier::error::IdentifierError`.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:385",
            claim: DissolvedClaim::ArmedDelegation {
                ty: "IdentifierError",
                rung: "ocx_oci.rs",
            },
        },
        // `Self::Ci(e)`, inner `ocx_shell::ci::error::Error` — a *different*
        // type from the `Shell` row below, which wraps
        // `ocx_shell::shell::error::Error`. One crate, two types: the crate
        // name is not the join key, which is why every row here names the
        // inner path and the rung pins the import site.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:388",
            claim: DissolvedClaim::ArmedDelegation {
                ty: "CiError",
                rung: "ocx_shell.rs",
            },
        },
        // `Self::Records(e)`, inner `ocx_package_manager::record::RecordsError`.
        // `LaunchError::Records(e) => e.classify()` looks like a stand-in and
        // is not one: it is a sibling that happens to delegate the same way,
        // not this arm's successor. The successor is the rung.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:390",
            claim: DissolvedClaim::ArmedDelegation {
                ty: "RecordsError",
                rung: "ocx_package_manager.rs",
            },
        },
        // `Self::Shell(_) => None`. The one row with no successor, and it needs
        // none: the arm contributed nothing while it existed.
        // `shell_error_falls_through_to_the_same_failure` is the equivalence.
        Dissolved {
            baseline_source: "crates/ocx_lib/src/error.rs:393",
            claim: DissolvedClaim::FallsThrough,
        },
    ];

    /// The canonical value with the arm's own binder and a leading `return`
    /// normalised away, so two spellings of one delegation compare equal.
    fn delegation_shape(pattern: &str, value: &str) -> String {
        let mut body = value.strip_prefix("return ").unwrap_or(value).trim();
        // `classify` returns `Option<ExitCode>`, and an impl may spell that as
        // `Some(match self { … => ExitCode::X })` or as `match self { … =>
        // Some(ExitCode::X) }`. The arm text then differs by a `Some` the
        // enclosing expression supplies, and a block by the braces rustfmt
        // leaves around a wrapped arm — neither changes the code produced.
        loop {
            let stripped = body
                .strip_prefix("Some (")
                .and_then(|rest| rest.strip_suffix(')'))
                .or_else(|| body.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')));
            match stripped {
                Some(inner) => body = inner.trim(),
                None => break,
            }
        }
        // The FIRST parenthesised group is the variant's binding; the last one
        // may belong to a call inside a guard (`if error.path.is_empty()`),
        // which yields an empty binder and silently normalises nothing. Found
        // by the red/green below, not by reading.
        let binder = pattern
            .split_once('(')
            .and_then(|(_, tail)| tail.split(')').next())
            .map(str::trim)
            .filter(|binder| !binder.is_empty() && binder.chars().all(|c| c.is_alphanumeric() || c == '_'));
        match binder {
            Some(binder) => body
                .split(' ')
                .map(|token| if token == binder { "BINDER" } else { token })
                .collect::<Vec<_>>()
                .join(" "),
            None => body.to_owned(),
        }
    }

    /// The rows, built rather than declared, so a `Resolves` claim can be
    /// **computed** by calling the classifier it names instead of restating a
    /// code the author believes. Same move `Relocated` makes above, same
    /// reason: a declared code asserts a belief, a called one asserts the tree.
    fn stands_in_for() -> Vec<StandsInFor> {
        let file_error = ocx_util::error::FileError::new("x", std::io::Error::other("x")).classify();
        let serialization =
            ocx_util::error::SerializationError(serde_json::from_str::<u8>("x").expect_err("not a u8")).classify();
        vec![
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Store(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:395",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciClient(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:384",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Digest(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:396",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::PinnedIdentifier(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:399",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::File(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::Resolves(file_error),
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::PathInvalid(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:374",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "OciIndexError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::SerializationFailure(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::InternalFile`'s literal.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::File(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::SerializationFailure`'s literal.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::SerializationFailure(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::OciClient`'s delegation.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciClient(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:384",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::OciIndex`'s delegation.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Index(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:394",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::Digest`'s delegation.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Digest(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:396",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::Platform`'s delegation.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Platform(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:380",
                claim: Claim::SameDelegation,
            },
            // The package tier's E1 stand-in for `Error::Archive`'s delegation.
            StandsInFor {
                target: "PackageError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Archive(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:386",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "ArchiveError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Compression(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:387",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "ArchiveError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::File(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::Resolves(file_error),
            },
            StandsInFor {
                target: "ClientError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Digest(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:396",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "UtilError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::File(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::Resolves(file_error),
            },
            StandsInFor {
                target: "UtilError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Serialization(error)",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::Resolves(serialization),
            },
            // The project tier's E1 stand-ins. `ocx_project` stopped borrowing
            // `ocx_lib::Error` for the failures it raises itself, so each of
            // these four answers exactly what the borrowed variant answered —
            // three delegations and one literal, all cited below.
            // WP-34's E1: twenty-six arms the package-manager tier minted when it
            // stopped borrowing `ocx_lib::Error`. Rows and patterns are generated
            // from the arms in the tree and the baseline's own `source` fields, not
            // transcribed — 26 is the largest set this mechanism carries and an
            // approximate citation pins the wrong row while still passing (DEC-80).
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OfflineMode",
                baseline_source: "crates/ocx_lib/src/error.rs:365",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::InternalFile(_, _)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::LayerLayout(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:370",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::SymlinkWalk(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:373",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::InternalPathInvalid(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:374",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::SerializationFailure(_)",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::UnsupportedMediaType(_, _)",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::MetadataBlobTooLarge { .. }",
                baseline_source: "crates/ocx_lib/src/error.rs:375",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Platform(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:380",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Project(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:381",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::ProjectRegistry(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:382",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciClient(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:384",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Archive(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:386",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Package(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:391",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciIndex(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:394",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::FileStructure(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:395",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Digest(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:396",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Patch(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:397",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Dependency(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:398",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::PinnedIdentifier(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:399",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Singleflight(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:400",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::LauncherUnsafeCharacter { .. }",
                baseline_source: "crates/ocx_lib/src/error.rs:401",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::ToolchainHomeNotAbsolute { .. }",
                baseline_source: "crates/ocx_lib/src/error.rs:405",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::LauncherPathNotUtf8 { .. }",
                baseline_source: "crates/ocx_lib/src/error.rs:408",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Sign(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:409",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "PackageManagerError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Verify(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:410",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "ProjectError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciClient(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:384",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "ProjectError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::OciIndex(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:394",
                claim: Claim::SameDelegation,
            },
            StandsInFor {
                target: "ProjectError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::Config(e)",
                baseline_source: "crates/ocx_lib/src/error.rs:389",
                claim: Claim::SameDelegation,
            },
            // `Error::InternalFile`'s literal, copied rather than delegated —
            // `SameDelegation` compares the normalised value text, which is
            // what makes a literal-for-literal stand-in checkable at all.
            StandsInFor {
                target: "ProjectError",
                trait_name: "ClassifyExitCode",
                func: "classify",
                match_id: 0,
                pattern: "Self::InternalFile(_, _)",
                baseline_source: "crates/ocx_lib/src/error.rs:366",
                claim: Claim::SameDelegation,
            },
        ]
    }

    /// What the extractor must read out of the control source above:
    /// `(target, fn, match index, pattern, value)`, rendered the way every
    /// other arm in this test is rendered.
    const PROBE_ARMS: &[(&str, &str, usize, &str, &str)] = &[
        (
            "ocx_lib::deeply::nested::ProbeError",
            "classify",
            0,
            "ProbeError :: One",
            "Some (ExitCode :: DataError)",
        ),
        (
            "ocx_lib::deeply::nested::ProbeError",
            "classify",
            0,
            "Self :: Two (inner)",
            "match inner { Inner :: Deep => None , }",
        ),
        (
            "ocx_lib::deeply::nested::ProbeError",
            "classify",
            1,
            "Inner :: Deep",
            "None",
        ),
    ];
}
