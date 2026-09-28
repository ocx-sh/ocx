// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The boundary-test harness: assert over source that one module tree never reaches another.
//!
//! Every entry point holds five properties, so a guard cannot pass silently:
//! 1. no scanned file reaches a forbidden module (`use`, `crate::`, `super::`, `self::`, `ocx_*::`
//!    paths anywhere, macro tokens and lib-root re-exports included);
//! 2. the walk found more than one file and no subtree came back empty;
//! 3. `witness` yields a reach for every required entry, so 1 is not vacuous;
//! 4. comments and literal contents are never scanned;
//! 5. every file parses and every `use` slice in macro tokens re-lexes, else it panics.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, LineColumn, TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::visit::Visit;

/// A path rendered with `/` separators on every platform.
///
/// `tests/boundary.rs` matches panic paths with `#[should_panic(expected = "macro_arguments/a.rs:7: ...")]`,
/// which `Path::display`'s `\` fails on Windows.
pub fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// One forbidden reach: the file, the 1-based line and the path (or needle) reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reach {
    pub file: PathBuf,
    pub line: usize,
    pub path: String,
}

impl std::fmt::Display for Reach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", slashed(&self.file), self.line, self.path)
    }
}

/// One source file parsed once: its syntax tree and its flat token list.
///
/// Read through `syn`, never a hand-rolled lexer: one lost 94 % of a file to a raw-string desync and read green.
pub struct Source {
    pub file: syn::File,
    /// Filled on the first [`Source::tokens`] call, never by `parse`: emitting it costs as much again as parsing.
    tokens: OnceCell<Vec<Token>>,
}

impl Source {
    /// Read and parse `path`, panicking with file and position on any failure (property 5).
    ///
    /// Not memoised: holding the 19 MB corpus's trees took `no_classification_in_libraries` from 0.92 s to 1.82 s.
    pub fn parse(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("boundary harness: read {}: {error}", slashed(path)));
        let file = syn::parse_file(&text).unwrap_or_else(|error| {
            let at = error.span().start();
            panic!(
                "boundary harness: parse {}:{}:{}: {error}",
                slashed(path),
                at.line,
                at.column + 1
            )
        });
        Self {
            file,
            tokens: OnceCell::new(),
        }
    }

    /// The flat token list, re-emitted from the tree (original spans intact) on first use.
    pub fn tokens(&self) -> &[Token] {
        self.tokens.get_or_init(|| flatten(self.file.to_token_stream()))
    }
}

/// One token, with comments and literals dropped (property 4): an identifier or keyword, a single
/// punctuation character, or a group delimiter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub at: LineColumn,
}

/// Flatten a token stream depth-first, emitting each group's delimiters as tokens around its contents.
pub fn flatten(stream: TokenStream) -> Vec<Token> {
    fn walk(stream: TokenStream, out: &mut Vec<Token>) {
        for tree in stream {
            match tree {
                TokenTree::Ident(ident) => out.push(Token {
                    text: ident.to_string(),
                    at: ident.span().start(),
                }),
                TokenTree::Punct(punct) => out.push(Token {
                    text: punct.as_char().to_string(),
                    at: punct.span().start(),
                }),
                TokenTree::Literal(_) => {}
                TokenTree::Group(group) => {
                    let (open, close) = match group.delimiter() {
                        Delimiter::Parenthesis => ("(", ")"),
                        Delimiter::Bracket => ("[", "]"),
                        Delimiter::Brace => ("{", "}"),
                        Delimiter::None => ("", ""),
                    };
                    if !open.is_empty() {
                        out.push(Token {
                            text: open.to_owned(),
                            at: group.span_open().start(),
                        });
                    }
                    walk(group.stream(), out);
                    if !close.is_empty() {
                        out.push(Token {
                            text: close.to_owned(),
                            at: group.span_close().start(),
                        });
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(stream, &mut out);
    out
}

/// Assert that no `.rs` file under `subtrees` reaches any module in `forbidden`; panics naming file and line.
///
/// Entries are `crate::`-relative module paths (`"cli::classify"`) or `ocx_*` crate paths (`"ocx_package"`),
/// matched on leading segments. `witness` is a committed fixture reaching **every** entry, so an entry naming no
/// module reds instead of hiding behind its neighbours. For a derived set use [`assert_no_imports_derived`].
pub fn assert_no_imports(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path) {
    // Atoms in the spelling `record` reports (`crate::…`, or `ocx_*` verbatim), or no witness hit matches them.
    let rooted: Vec<String> = forbidden
        .iter()
        .map(|entry| {
            if entry.starts_with("ocx_") {
                (*entry).to_owned()
            } else {
                format!("crate::{entry}")
            }
        })
        .collect();
    let required: Vec<&str> = rooted.iter().map(String::as_str).collect();
    scan_imports(subtrees, forbidden, witness, &required);
}

/// [`assert_no_imports`] for a set derived from the crate's own `mod` items, needing one witness reach overall.
///
/// A per-entry witness would red on every new module, which a derived set exists to admit by itself.
pub fn assert_no_imports_derived(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path) {
    scan_imports(subtrees, forbidden, witness, &[]);
}

fn scan_imports(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path, required: &[&str]) {
    let reexports: BTreeMap<PathBuf, BTreeMap<String, String>> = subtrees
        .iter()
        .filter_map(|subtree| crate_src_root(subtree))
        .map(|root| {
            let table = reexport_table(&root);
            (root, table)
        })
        .collect();
    let none = BTreeMap::new();
    let scan = |file: &Path, source: &Source| {
        let root = crate_src_root(file);
        let table = root.as_deref().and_then(|root| reexports.get(root)).unwrap_or(&none);
        reaches(source, file, root.as_deref(), forbidden, table)
    };
    assert_scan(
        subtrees,
        witness,
        &format!("a `crate::`- or `ocx_*`-rooted path into {forbidden:?}"),
        required,
        &scan,
    );
}

/// The forbidden reaches one file carries, read as a witness is and without the five properties, so a
/// fixture can be checked from outside the guard that owns it.
pub fn reaches_in(file: &Path, forbidden: &[&str]) -> Vec<Reach> {
    let source = Source::parse(file);
    let root = crate_src_root(file);
    let table = BTreeMap::new();
    reaches(&source, file, root.as_deref(), forbidden, &table)
}

/// [`reaches_in`] for a token-needle guard — the [`assert_no_needles`] half.
pub fn needles_in(file: &Path, needles: &[&str]) -> Vec<Reach> {
    let source = Source::parse(file);
    needle_hits(source.tokens(), file, needles)
}

/// Assert that no `.rs` file under `subtrees` contains any of `needles` as a token sequence, holding the
/// same five properties for a forbidden shape that is not a module path (`module_path!`, `type_name::<`).
pub fn assert_no_needles(subtrees: &[PathBuf], needles: &[&str], witness: &Path) {
    let scan = |file: &Path, source: &Source| needle_hits(source.tokens(), file, needles);
    assert_scan(subtrees, witness, &format!("one of {needles:?}"), needles, &scan);
}

/// The engine behind [`assert_no_imports`] and [`assert_no_needles`]: `scan` answers what one file
/// violates and the five properties are held around it.
///
/// Each `required` atom needs a witness hit whose path equals it or extends it at `::`; empty means one hit anywhere.
pub fn assert_scan(
    subtrees: &[PathBuf],
    witness: &Path,
    what: &str,
    required: &[&str],
    scan: &(dyn Fn(&Path, &Source) -> Vec<Reach> + Sync),
) {
    let files = walked_corpus(subtrees);
    // Counted before flattening, or a parallel walk that lost a chunk reads as a clean corpus.
    let per_file = map_sources(&files, scan);
    assert_eq!(
        per_file.len(),
        files.len(),
        "boundary harness: the scan answered for {} of {} walked file(s) — it read less than it \
         walked, and a short read reports as a clean tree",
        per_file.len(),
        files.len()
    );
    conclude_scan(
        subtrees,
        per_file.into_iter().flatten().collect(),
        witness,
        what,
        required,
        scan,
    );
}

/// [`assert_scan`] for a caller that already scanned these subtrees: `scanned` must equal a re-walk of them,
/// so a narrower read reds (property 2).
pub fn assert_scan_of(
    subtrees: &[PathBuf],
    scanned: &[PathBuf],
    violations: Vec<Reach>,
    witness: &Path,
    what: &str,
    required: &[&str],
    scan: &(dyn Fn(&Path, &Source) -> Vec<Reach> + Sync),
) {
    let walked = walked_corpus(subtrees);
    let files: BTreeSet<&Path> = walked.iter().map(PathBuf::as_path).collect();
    let given: BTreeSet<&Path> = scanned.iter().map(PathBuf::as_path).collect();
    let unread: Vec<&&Path> = files.difference(&given).collect();
    let extra: Vec<&&Path> = given.difference(&files).collect();
    assert!(
        unread.is_empty() && extra.is_empty(),
        "boundary harness: the caller scanned a different set than this guard's {} subtree(s) hold \
         — {} walked file(s) it never read {unread:?}, {} file(s) it read from outside the scope \
         {extra:?}; its findings cannot stand in for a scan of this scope",
        subtrees.len(),
        unread.len(),
        extra.len()
    );
    conclude_scan(subtrees, violations, witness, what, required, scan);
}

/// The walk behind both entry points, with property 2 on it.
fn walked_corpus(subtrees: &[PathBuf]) -> Vec<PathBuf> {
    let walked: Vec<(&Path, Vec<PathBuf>)> = subtrees
        .iter()
        .map(|subtree| (subtree.as_path(), rust_sources(subtree)))
        .collect();
    let mut files: Vec<PathBuf> = walked.iter().flat_map(|(_, found)| found.iter().cloned()).collect();
    files.sort();
    files.dedup();
    assert!(
        files.len() > 1,
        "boundary harness: walked {} file(s) across {} subtree(s) — it scanned nothing",
        files.len(),
        subtrees.len()
    );
    // Per subtree too: `rust_sources` answers a missing directory with an empty walk the union would absorb.
    let empty: Vec<&Path> = walked
        .iter()
        .filter(|(_, found)| found.is_empty())
        .map(|(subtree, _)| *subtree)
        .collect();
    assert!(
        empty.is_empty(),
        "boundary harness: {} of {} subtree(s) hold no `.rs` file — {empty:?}; the rest carry the \
         corpus past the one-file floor, so the drop would otherwise read as a clean scan",
        empty.len(),
        subtrees.len()
    );
    files
}

/// Properties 1, 3 and 4, over findings the caller's scan already produced.
fn conclude_scan(
    subtrees: &[PathBuf],
    violations: Vec<Reach>,
    witness: &Path,
    what: &str,
    required: &[&str],
    scan: &(dyn Fn(&Path, &Source) -> Vec<Reach> + Sync),
) {
    // Properties 3 and 4: the witness goes through the very same scan.
    let witness_hits = scan(witness, &Source::parse(witness));
    assert!(
        !witness_hits.is_empty(),
        "boundary harness: witness {} reaches nothing the scanner recognises as {what} — the assertion \
         over {} subtree(s) proves nothing (a commented-out reach does not count)",
        slashed(witness),
        subtrees.len()
    );
    // Every atom, not one hit overall: a needle that stopped matching hides behind its neighbours' hits.
    let witnessed = |atom: &str| {
        witness_hits
            .iter()
            .any(|hit| hit.path == atom || hit.path.strip_prefix(atom).is_some_and(|rest| rest.starts_with("::")))
    };
    let unwitnessed: Vec<&str> = required.iter().copied().filter(|atom| !witnessed(atom)).collect();
    assert!(
        unwitnessed.is_empty(),
        "boundary harness: witness {} carries no {unwitnessed:?} — the scan over {} subtree(s) proves \
         nothing for those",
        slashed(witness),
        subtrees.len()
    );

    // Property 1.
    let mut report = String::new();
    for reach in &violations {
        let _ = writeln!(report, "  {reach}");
    }
    assert!(
        violations.is_empty(),
        "boundary harness: {} reach(es) of {what} across {} subtree(s):\n{report}",
        violations.len(),
        subtrees.len()
    );
}

/// Parse and scan every one of `files` across the machine's cores, one answer per file in input order.
///
/// A scan's panic (property 5) is re-raised with its original message.
pub fn map_sources<T: Send>(files: &[PathBuf], scan: &(dyn Fn(&Path, &Source) -> T + Sync)) -> Vec<T> {
    /// A full-corpus walk under a second, without ~30 concurrent `nextest` processes each claiming the machine.
    const MAX_THREADS: usize = 8;

    let run = |chunk: &[PathBuf]| -> Vec<T> { chunk.iter().map(|file| scan(file, &Source::parse(file))).collect() };
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_THREADS);
    if threads <= 1 || files.len() <= 1 {
        return run(files);
    }
    let per_thread = files.len().div_ceil(threads);
    // A `Source` never leaves its thread (`proc_macro2` spans are thread-local), so only `T` crosses.
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(per_thread)
            .map(|chunk| scope.spawn(move || run(chunk)))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(found) => found,
                // Re-raised verbatim, or `#[should_panic(expected = …)]` tests stop matching.
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

/// Every `*.rs` file under `subtree`, recursively, sorted; a file path is a single-element walk.
pub fn rust_sources(subtree: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    if subtree.is_file() {
        out.push(subtree.to_path_buf());
    } else {
        walk(subtree, &mut out);
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Attributes and `use` trees
// ---------------------------------------------------------------------------

/// Whether `attrs` carries exactly `#[cfg(test)]`.
pub fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| match &attr.meta {
        syn::Meta::List(list) if list.path.is_ident("cfg") => {
            let tokens: Vec<Token> = flatten(list.tokens.clone());
            tokens.len() == 1 && tokens[0].text == "test"
        }
        _ => false,
    })
}

/// Expand a `use` tree into `(path segments, alias)` pairs: `crate::a::{b, c::{d as e}, *}` →
/// `[crate,a,b]`, `[crate,a,c,d]` (alias `e`), `[crate,a,*]`.
///
/// A non-leading `self` is dropped (`use a::{self, b}` is `a`); a leading one is kept, or `use self::x`
/// reads as an extern crate path that `reaches` ignores.
pub fn expand_use_tree(tree: &syn::UseTree) -> Vec<(Vec<String>, Option<String>)> {
    fn expand(prefix: &[String], tree: &syn::UseTree, out: &mut Vec<(Vec<String>, Option<String>)>) {
        let mut push = |ident: &syn::Ident, alias: Option<String>| {
            let mut full = prefix.to_vec();
            if ident != "self" || prefix.is_empty() {
                full.push(ident.to_string());
            }
            out.push((full, alias));
        };
        match tree {
            syn::UseTree::Path(path) => {
                let mut full = prefix.to_vec();
                if path.ident != "self" || prefix.is_empty() {
                    full.push(path.ident.to_string());
                }
                expand(&full, &path.tree, out);
            }
            syn::UseTree::Name(name) => push(&name.ident, None),
            syn::UseTree::Rename(rename) => push(&rename.ident, Some(rename.rename.to_string())),
            syn::UseTree::Glob(_) => {
                let mut full = prefix.to_vec();
                full.push("*".to_owned());
                out.push((full, None));
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    expand(prefix, item, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    expand(&[], tree, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Reach detection
// ---------------------------------------------------------------------------

/// The nearest ancestor (inclusive) of `subtree` holding a `lib.rs` or `main.rs`; `None` for a loose fixture tree.
fn crate_src_root(subtree: &Path) -> Option<PathBuf> {
    let mut dir = if subtree.is_file() { subtree.parent()? } else { subtree };
    loop {
        if dir.join("lib.rs").is_file() || dir.join("main.rs").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Lib-root re-exports, item name → `crate::`-relative path, from every `lib.rs` `use` (private ones too) whose
/// first segment is a module declared there; a glob is resolved from that module's `pub` items.
fn reexport_table(src_root: &Path) -> BTreeMap<String, String> {
    let mut table = BTreeMap::new();
    // A `lib.rs` that exists but does not parse is red, never an empty table.
    let lib = src_root.join("lib.rs");
    if !lib.is_file() {
        return table;
    }
    let source = Source::parse(&lib);
    let file = &source.file;
    struct Uses(Vec<syn::UseTree>);
    impl Visit<'_> for Uses {
        fn visit_item_use(&mut self, item: &syn::ItemUse) {
            self.0.push(item.tree.clone());
        }
    }
    let declared: BTreeSet<String> = file
        .items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Mod(module) if module.content.is_none() => Some(module.ident.to_string()),
            _ => None,
        })
        .collect();
    let mut uses = Uses(Vec::new());
    uses.visit_file(file);
    for tree in &uses.0 {
        for (path, alias) in expand_use_tree(tree) {
            let Some(first) = path.first() else { continue };
            if !declared.contains(first) || path.len() < 2 {
                continue;
            }
            let last = &path[path.len() - 1];
            if last == "*" {
                let module = &path[..path.len() - 1];
                let file = src_root.join(format!("{}.rs", module.join("/")));
                let alt = src_root.join(module.join("/")).join("mod.rs");
                let Some(module_file) = [file, alt].into_iter().find(|candidate| candidate.is_file()) else {
                    panic!(
                        "boundary harness: {}: `pub use {}::*;` names no module file — every name it \
                         re-exports drops out of the table, and each `crate::<Name>` that resolves \
                         through it then reads clean",
                        slashed(&lib),
                        module.join("::")
                    )
                };
                for item in pub_item_names(&Source::parse(&module_file).file) {
                    table.insert(item.clone(), format!("{}::{item}", module.join("::")));
                }
            } else {
                table.insert(alias.unwrap_or_else(|| last.clone()), path.join("::"));
            }
        }
    }
    table
}

/// Names of a module's top-level non-private functions, types, traits, constants and statics.
fn pub_item_names(file: &syn::File) -> Vec<String> {
    file.items
        .iter()
        .filter_map(|item| {
            let (visibility, ident) = match item {
                syn::Item::Fn(i) => (&i.vis, &i.sig.ident),
                syn::Item::Struct(i) => (&i.vis, &i.ident),
                syn::Item::Enum(i) => (&i.vis, &i.ident),
                syn::Item::Type(i) => (&i.vis, &i.ident),
                syn::Item::Trait(i) => (&i.vis, &i.ident),
                syn::Item::Const(i) => (&i.vis, &i.ident),
                syn::Item::Static(i) => (&i.vis, &i.ident),
                _ => return None,
            };
            (!matches!(visibility, syn::Visibility::Inherited)).then(|| ident.to_string())
        })
        .collect()
}

/// The module path of `file` under `src_root`: `src/a/b.rs` → `[a, b]`,
/// `src/a/mod.rs` → `[a]`, `src/lib.rs` → `[]`.
fn file_module_path(file: &Path, src_root: Option<&Path>) -> Vec<String> {
    let Some(root) = src_root else { return Vec::new() };
    let Ok(rel) = file.strip_prefix(root) else {
        return Vec::new();
    };
    let mut segs: Vec<String> = rel
        .with_extension("")
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    if matches!(segs.last().map(String::as_str), Some("lib" | "main" | "mod")) {
        segs.pop();
    }
    segs
}

fn resolve_super(effective: &[String], segs: &[String]) -> Vec<String> {
    let ups = segs.iter().take_while(|s| *s == "super").count();
    let base = &effective[..effective.len().saturating_sub(ups)];
    base.iter().cloned().chain(segs[ups..].iter().cloned()).collect()
}

/// A segment without its `r#` prefix, or `use crate::r#project::api;` escapes a plain `project` entry.
fn plain_segment(segment: &str) -> String {
    segment.strip_prefix("r#").unwrap_or(segment).to_owned()
}

fn is_forbidden(segs: &[String], forbidden: &[&str]) -> bool {
    forbidden.iter().any(|entry| {
        let want: Vec<&str> = entry.split("::").collect();
        segs.len() >= want.len() && want.iter().zip(segs).all(|(w, s)| w == s)
    })
}

/// The inline `mod` chain each token sits inside, one entry per token of `tokens`.
///
/// `syn` parses no macro body, so without this a `mod x { … }` inside a macro leaves `super::` resolving
/// past the crate root, a silent miss; each covered shape has a fixture under `boundary_fixtures/`.
fn module_stack_per_token(tokens: &[Token]) -> Vec<Vec<String>> {
    /// The module a `{` at `index` opens, if any; a `$x` name is kept verbatim as an opaque segment.
    fn opened_at(tokens: &[Token], index: usize) -> Option<String> {
        let name = |at: usize| {
            tokens
                .get(at)
                .filter(|token| token.text.starts_with(|c: char| c.is_alphabetic() || c == '_'))
                .map(|token| token.text.clone())
        };
        if index >= 2 && tokens[index - 2].text == "mod" {
            return name(index - 1);
        }
        if index >= 3 && tokens[index - 3].text == "mod" && tokens[index - 2].text == "$" {
            return name(index - 1).map(|ident| format!("${ident}"));
        }
        None
    }

    let mut out = Vec::with_capacity(tokens.len());
    // (depth opened at, name): a `}` pops only the module whose own depth it closes.
    let mut open: Vec<(usize, String)> = Vec::new();
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        match token.text.as_str() {
            "{" => {
                depth += 1;
                if let Some(name) = opened_at(tokens, index) {
                    open.push((depth, name));
                }
            }
            "}" => {
                if open.last().is_some_and(|(at, _)| *at == depth) {
                    open.pop();
                }
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
        out.push(open.iter().map(|(_, name)| name.clone()).collect());
    }
    out
}

/// Every `crate::`-, `super::`-, `self::`- or `ocx_*::`-rooted path in a flat token list, as `(segments,
/// token index)`; a root after a single `:` (`Foo { field: crate::x }`) counts.
fn rooted_paths_in_tokens(tokens: &[Token]) -> Vec<(Vec<String>, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let root = &tokens[i].text;
        // A `::` disqualifies the root only after something a path continues from: `::ocx_util::x` is a reach.
        let after_separator =
            i > 2 && tokens[i - 1].text == ":" && tokens[i - 2].text == ":" && continues_a_path(&tokens[i - 3].text);
        // `self.field` and `Self::…` are different tokens, so no receiver matches the `self` root.
        let rooted =
            (root == "crate" || root == "super" || root == "self" || root.starts_with("ocx_")) && !after_separator;
        if !rooted {
            i += 1;
            continue;
        }
        let mut segs = vec![root.clone()];
        let mut j = i + 1;
        while j + 2 < tokens.len()
            && tokens[j].text == ":"
            && tokens[j + 1].text == ":"
            // `#` keeps `r#project` one segment, or the path truncates before its forbidden segments.
            && tokens[j + 2].text.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '#')
        {
            segs.push(tokens[j + 2].text.clone());
            j += 3;
        }
        out.push((segs, i));
        i = j.max(i + 1);
    }
    out
}

/// Whether a `::` following `text` continues a path: an identifier, a raw identifier, or a turbofish `>`.
fn continues_a_path(text: &str) -> bool {
    text == ">" || text.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '#')
}

/// Every `use` statement in a flat token list, re-lexed and expanded by [`expand_use_tree`], as `(segments,
/// token index)`: `rooted_paths_in_tokens` stops at a brace and would drop grouped names.
///
/// A slice beginning `use` that does not re-lex panics (property 5); skipping it hides the reach.
fn use_trees_in_tokens(file: &Path, tokens: &[Token]) -> Vec<(Vec<String>, usize)> {
    let mut out = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.text != "use" {
            continue;
        }
        // `use<'_>` is precise capturing; a `use` statement is never followed by `<`.
        if tokens.get(index + 1).is_some_and(|next| next.text == "<") {
            continue;
        }
        let slice = match tokens[index..].iter().position(|t| t.text == ";") {
            Some(end) => &tokens[index..=index + end],
            None => &tokens[index..],
        };
        // Rejoined tight so `: :` reads back as `::`; only adjacent words need a space.
        let mut text = String::new();
        let mut prev_word = false;
        for (at, part) in slice.iter().enumerate() {
            // Dropping the `$` of hygienic `$crate` is what lets `$crate::{a, b}` re-lex at all.
            if part.text == "$" && slice.get(at + 1).is_some_and(|next| next.text == "crate") {
                continue;
            }
            let word = part.text.starts_with(|c: char| c.is_alphanumeric() || c == '_');
            if word && prev_word {
                text.push(' ');
            }
            text.push_str(&part.text);
            prev_word = word;
        }
        let item = syn::parse_str::<syn::ItemUse>(&text).unwrap_or_else(|error| {
            panic!(
                "boundary harness: {}:{}: `{text}` begins `use` but does not re-lex as a `use` statement \
                 ({error}) — a slice dropped here is a reach the scan never reports, so it is red rather \
                 than a shorter walk; if this shape is genuinely not a `use` statement, carve it out \
                 above the panic with the reason",
                slashed(file),
                slice[0].at.line
            )
        });
        out.extend(
            expand_use_tree(&item.tree)
                .into_iter()
                .map(|(path, _alias)| (path, index)),
        );
    }
    out
}

/// Every forbidden reach in `source` (see [`assert_no_imports`]).
fn reaches(
    source: &Source,
    file: &Path,
    src_root: Option<&Path>,
    forbidden: &[&str],
    reexports: &BTreeMap<String, String>,
) -> Vec<Reach> {
    struct Reacher<'a> {
        file: &'a Path,
        file_mod: Vec<String>,
        inline: Vec<String>,
        forbidden: &'a [&'a str],
        reexports: &'a BTreeMap<String, String>,
        out: Vec<Reach>,
    }
    impl Reacher<'_> {
        /// `nested` is the `mod` chain opened inside macro tokens around this path; empty for parsed items.
        fn record(&mut self, path: &[String], line: usize, nested: &[String]) {
            // A reach into an extracted crate has no `crate::` spelling, only `ocx_*::`.
            if path.first().is_some_and(|root| root.starts_with("ocx_")) {
                let segs: Vec<String> = path.iter().map(|seg| plain_segment(seg)).collect();
                // Never folded onto a module path: crate and module names are not in bijection
                // (`file_structure` is `ocx_store`), so a guard lists `ocx_<crate>` as its own entry.
                if is_forbidden(&segs, self.forbidden) {
                    self.out.push(Reach {
                        file: self.file.to_path_buf(),
                        line,
                        path: segs.join("::"),
                    });
                }
                return;
            }
            let segs: Vec<String> = match path.first().map(String::as_str) {
                Some("crate") => path[1..].to_vec(),
                // `self::x` is the current module's `x`.
                Some("self") => {
                    let effective: Vec<String> = self
                        .file_mod
                        .iter()
                        .chain(&self.inline)
                        .chain(nested)
                        .cloned()
                        .collect();
                    effective.into_iter().chain(path[1..].iter().cloned()).collect()
                }
                Some("super") => {
                    let effective: Vec<String> = self
                        .file_mod
                        .iter()
                        .chain(&self.inline)
                        .chain(nested)
                        .cloned()
                        .collect();
                    resolve_super(&effective, path)
                }
                _ => return,
            };
            let segs: Vec<String> = segs.iter().map(|seg| plain_segment(seg)).collect();
            if segs.is_empty() {
                return;
            }
            // A lib-root re-export: `crate::Config` → `config::Config`.
            let resolved: Vec<String> = match (segs.len(), segs.first().and_then(|name| self.reexports.get(name))) {
                (1, Some(target)) => target.split("::").map(str::to_owned).collect(),
                _ => segs,
            };
            if is_forbidden(&resolved, self.forbidden) {
                self.out.push(Reach {
                    file: self.file.to_path_buf(),
                    line,
                    path: format!("crate::{}", resolved.join("::")),
                });
            }
        }
        fn scan_tokens(&mut self, stream: &TokenStream) {
            let tokens = flatten(stream.clone());
            let nesting = module_stack_per_token(&tokens);
            for (segs, index) in rooted_paths_in_tokens(&tokens)
                .into_iter()
                .chain(use_trees_in_tokens(self.file, &tokens))
            {
                self.record(&segs, tokens[index].at.line, &nesting[index]);
            }
        }
    }
    impl<'ast> Visit<'ast> for Reacher<'_> {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            let Some((_, items)) = &module.content else {
                return syn::visit::visit_item_mod(self, module);
            };
            // Attributes and visibility resolve against the enclosing module, so before the push.
            for attr in &module.attrs {
                self.visit_attribute(attr);
            }
            self.visit_visibility(&module.vis);
            self.inline.push(module.ident.to_string());
            for item in items {
                self.visit_item(item);
            }
            self.inline.pop();
        }
        fn visit_vis_restricted(&mut self, restricted: &'ast syn::VisRestricted) {
            // Only `pub(in crate::x)` names a module.
            if restricted.in_token.is_some() {
                self.visit_path(&restricted.path);
            }
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let line = item.use_token.span.start().line;
            for (path, _alias) in expand_use_tree(&item.tree) {
                self.record(&path, line, &[]);
            }
            syn::visit::visit_item_use(self, item);
        }
        fn visit_path(&mut self, path: &'ast syn::Path) {
            let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
            if let Some(first) = path.segments.first() {
                self.record(&segs, first.ident.span().start().line, &[]);
            }
            syn::visit::visit_path(self, path);
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.visit_path(&mac.path);
            self.scan_tokens(&mac.tokens);
        }
        fn visit_meta_list(&mut self, list: &'ast syn::MetaList) {
            self.visit_path(&list.path);
            self.scan_tokens(&list.tokens);
        }
    }
    let mut reacher = Reacher {
        file,
        file_mod: file_module_path(file, src_root),
        inline: Vec::new(),
        forbidden,
        reexports,
        out: Vec::new(),
    };
    reacher.visit_file(&source.file);
    reacher.out
}

/// Every occurrence of a needle as a contiguous token sequence, as reaches whose `path` is the needle; a
/// needle that is not a balanced token sequence (`foo(`) is refused.
fn needle_hits(tokens: &[Token], file: &Path, needles: &[&str]) -> Vec<Reach> {
    let mut out = Vec::new();
    for needle in needles {
        let stream: TokenStream = needle
            .parse()
            .unwrap_or_else(|error| panic!("boundary harness: needle `{needle}` is not a token sequence: {error}"));
        let want: Vec<String> = flatten(stream).into_iter().map(|t| t.text).collect();
        assert!(
            !want.is_empty(),
            "boundary harness: needle `{needle}` lexes to no token"
        );
        for (index, window) in tokens.windows(want.len()).enumerate() {
            if window.iter().zip(&want).all(|(t, w)| t.text == *w) {
                out.push(Reach {
                    file: file.to_path_buf(),
                    line: tokens[index].at.line,
                    path: (*needle).to_owned(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Reach, slashed};
    use std::path::{Path, PathBuf};

    /// The seam `tests/boundary.rs` matches against.
    ///
    /// Its thirty-five red-proof tests spell the expectation with `/`
    /// (`#[should_panic(expected = "macro_arguments/a.rs:7: ...")]`), so the
    /// rendering has to produce `/` whatever the host separator is. Asserting
    /// on a Windows-shaped path makes the case reachable on this host too:
    /// restore `self.file.display()` in `Reach`'s `Display` and this reds
    /// here, exactly as it reds on the Windows runner.
    #[test]
    fn a_reach_renders_its_path_with_forward_slashes() {
        let reach = Reach {
            file: PathBuf::from(r"D:\a\ocx\ocx\crates\x\tests/fixtures\macro_arguments\a.rs"),
            line: 7,
            path: "crate::project::lock::is_stale".to_owned(),
        };
        let rendered = reach.to_string();
        assert!(
            rendered.contains("macro_arguments/a.rs:7: crate::project::lock::is_stale"),
            "a should_panic expectation spelled with `/` must match this: {rendered}"
        );
        assert!(
            !rendered.contains('\\'),
            "no separator may survive as a backslash: {rendered}"
        );
    }

    /// A POSIX path is already flat, so the normalizer is a no-op on it — the
    /// control that keeps the assertion above from passing for a renderer that
    /// simply deleted every separator.
    #[test]
    fn a_posix_path_is_left_alone() {
        assert_eq!(slashed(Path::new("/tmp/fixtures/a.rs")), "/tmp/fixtures/a.rs");
    }
}
