// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The boundary-test harness (ADR phase 0.1, plan C-006).
//!
//! A boundary test asserts over source that one module tree never reaches
//! another. Such a guard fails in ways a behavioural test cannot: a needle
//! that quietly stops matching reads as green, and a denylist can match its
//! own comment. So every entry point here bakes in the five properties a
//! boundary test needs instead of leaving them to each caller:
//!
//! 1. no scanned file reaches a forbidden module — via `use crate::x…`
//!    (nested groups expanded), a `crate::x::` path anywhere in an item,
//!    expression, type, attribute or macro invocation, a `super::` chain
//!    resolved against the file's module path *and* every `mod` it sits
//!    inside — including the ones written in macro tokens, which `syn` never
//!    parses into items (`module_stack_per_token` names the shapes it
//!    covers, each with a committed fixture) — or a lib-root re-export
//!    (`use crate::Config` resolved through `lib.rs`'s `pub use`, a glob
//!    through the module file it names — one that names no file is a loud
//!    failure, never a shorter table) — or, for a guard whose scope has grown
//!    onto an already-extracted crate, an `ocx_*::` path, the only spelling a
//!    reach into an extracted subject has (DEC-30 item 1, B5-7);
//! 2. the walk over the subtrees found more than one file, so it did not
//!    scan nothing — a one-file crate is walked beside the rest, never
//!    skipped for being alone — and no single subtree came back empty,
//!    which the union's count alone absorbs;
//! 3. the scanner still finds a forbidden reach in `witness` — **one per
//!    entry**, whether the guard is a token-needle list or a forbidden-module
//!    list — so 1 is not vacuous, for any of them. The one carve-out is
//!    [`assert_no_imports_derived`], whose forbidden set the caller read off
//!    the compiler's own `mod` items, where a needle that names nothing is
//!    not expressible in the first place;
//! 4. comments and the contents of string, raw-string and char literals are
//!    never scanned — of any file *and* of the witness, so a witness whose
//!    only offending line is commented out is refused (C-007);
//! 5. every scanned file parses as Rust, and every `use` slice the walk finds
//!    in macro tokens re-lexes as a `use` statement; one that does not is a
//!    loud failure naming the file, the line and the text, never a shorter
//!    walk — the two shapes that are not misses (`use<'a>` precise capturing,
//!    the hygienic `$crate` root) are carved out at the refusal, each with
//!    its reason.
//!
//! Source is read through `syn` (`syn::parse_file` + `syn::visit::Visit`)
//! and `proc_macro2`'s lexer rather than a tokenizer of our own: a Rust
//! parser is not this product's domain (quality-core "Don't Own Non-Domain
//! Code"), and the hand-rolled one this replaced once lost 94 % of a file
//! to a raw-string desync and reported green. `syn` leaves macro arguments
//! and attribute lists as token streams; those are walked token by token so
//! a `crate::x` or `module_path!()` inside `format!(…)` is still found, and
//! a `use` statement written there is re-lexed and expanded rather than left
//! to a path scan that stops at its group's brace.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use proc_macro2::{Delimiter, LineColumn, TokenStream, TokenTree};
use quote::ToTokens as _;
use syn::visit::Visit;

/// A path rendered with `/` separators on every platform.
///
/// Every message this harness panics with names a fixture path, and
/// `tests/boundary.rs` matches those messages with
/// `#[should_panic(expected = "macro_arguments/a.rs:7: ...")]`. `Path::display`
/// emits `\` on Windows, so the expectation misses there and thirty-five
/// red-proof tests fail for the separator rather than for what they assert.
/// Rendering one way keeps the diagnostic — and the expectation — platform-flat.
pub fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// One forbidden reach: the file, the 1-based line and the `crate::`-relative
/// path (or needle) that was reached.
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

/// One source file, parsed once: its syntax tree for the structural walks
/// and its flat token list for needle and macro-argument scans.
pub struct Source {
    pub file: syn::File,
    /// Filled on the first [`Source::tokens`] call, never by `parse`. Only the
    /// needle scans and the `thiserror` sweep read tokens; every structural
    /// walk reads `file` alone, and re-emitting the tree as a token stream
    /// costs about as much again as parsing it did.
    tokens: OnceCell<Vec<Token>>,
}

impl Source {
    /// Read and parse `path`. Panics naming the file and position on any
    /// failure — a file the scanner cannot read is red, not skipped
    /// (property 5).
    ///
    /// Deliberately **not** memoised. A guard that re-asks for the same file
    /// caches what it needs itself, over the small set it re-enters; a cache
    /// here would have to retain every file of a 19 MB corpus for the whole
    /// test, and the allocator pressure of holding six hundred syntax trees
    /// cost the single-pass guards more than the re-parse it saved them
    /// (`no_classification_in_libraries` went 0.92 s to 1.82 s when this was
    /// global).
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

    /// The flat token list, built on first use.
    ///
    /// Re-emitted from the tree rather than lexed a second time: `syn`
    /// carries the original spans, so a line number is the same either
    /// way, and the file is read once.
    pub fn tokens(&self) -> &[Token] {
        self.tokens.get_or_init(|| flatten(self.file.to_token_stream()))
    }
}

/// One lexed token, comments gone and literal contents dropped: an
/// identifier or keyword, a single punctuation character, or a group
/// delimiter. Literals are not emitted — their contents are what property 4
/// keeps out of every scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub at: LineColumn,
}

/// Flatten a token stream depth-first, emitting each group's delimiters as
/// tokens around its contents.
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

/// Assert that no `.rs` file under any of `subtrees` reaches any module in
/// `forbidden`.
///
/// `forbidden` entries are `crate::`-relative module paths (`"project"`,
/// `"cli::classify"`) **or** `ocx_*`-rooted crate paths (`"ocx_package"`,
/// `"ocx_setup::session_path"`); a reach matches when its leading segments equal an
/// entry. The second spelling is what a guard scoped onto an extracted `src/`
/// needs: inside `crates/ocx_util/src`, a reach at a forbidden tier can only be
/// written `ocx_package::…`, which has no `crate::` form at all. `witness` is a
/// file that reaches **every** forbidden module — the committed negative
/// fixture of C-007. Panics on every violated property listed in the module
/// doc, naming the file and line.
///
/// One witness reach per entry, not one across the list: a needle that names
/// no module — a typo, or a module renamed out from under it — forbids nothing
/// and hides behind its neighbours' hits, and its share of the scan is then
/// vacuous. Where the forbidden set is machine-derived rather than written by
/// hand, use [`assert_no_imports_derived`].
pub fn assert_no_imports(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path) {
    // `Reach::path` is the resolved path in the spelling `record` reports it
    // under — `crate::…` for a module entry, the crate path verbatim for an
    // `ocx_*` one — and the required atoms have to match that to witness it.
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

/// [`assert_no_imports`] for a forbidden set the caller **derived** — read off
/// the crate's own `mod` items rather than typed out — where one witness reach
/// across the list is all the harness can ask for.
///
/// A derived set cannot carry a needle that names nothing: every entry is a
/// module the compiler resolved, so the defect per-entry witnessing exists to
/// catch is not expressible. Demanding a witness per entry would instead demand
/// a fixture that reaches every module of the crate and reds the moment one is
/// added — which is the opposite of what deriving the set buys, namely that a
/// new module joins the guard by itself.
pub fn assert_no_imports_derived(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path) {
    scan_imports(subtrees, forbidden, witness, &[]);
}

fn scan_imports(subtrees: &[PathBuf], forbidden: &[&str], witness: &Path, required: &[&str]) {
    // One re-export table per crate root among the subtrees; a file resolves
    // against the root it sits under, the witness (outside every crate)
    // against none.
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

/// The forbidden reaches one file carries, without the five properties around
/// them — the static half of DEC-33.
///
/// [`assert_scan`] already demands that a guard's witness carry a hit for every
/// atom, but only for a guard that *runs*: a `#[ignore]`d one, or one nobody
/// calls, takes its needle list out of every check at once. This is the same
/// scan, answerable about a fixture from outside the guard that owns it.
///
/// `file` is read as a witness is — outside any crate root, so no re-export
/// table and no module path, which is exactly how [`assert_no_imports`] scans
/// the witness it is handed.
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

/// Assert that no `.rs` file under any of `subtrees` contains any of
/// `needles` as a token sequence — the same five properties as
/// [`assert_no_imports`] for guards whose forbidden shape is not a module
/// path (`module_path!`, `type_name::<`). A needle is Rust tokens: comments
/// and literal contents can never match it.
pub fn assert_no_needles(subtrees: &[PathBuf], needles: &[&str], witness: &Path) {
    let scan = |file: &Path, source: &Source| needle_hits(source.tokens(), file, needles);
    assert_scan(subtrees, witness, &format!("one of {needles:?}"), needles, &scan);
}

/// The engine behind [`assert_no_imports`] and [`assert_no_needles`], and the
/// entry point for a guard whose forbidden shape is neither a module path nor a
/// token needle: `scan` answers "what does this one file violate", and the five
/// module-doc properties are held around it here rather than by the caller.
///
/// `required` are the atoms the witness must each produce a hit for. An atom is
/// witnessed by a hit whose `Reach::path` equals it, or extends it at a `::`
/// boundary — so a token needle matches its own hit text, and a forbidden module
/// matches the resolved path that reached into it. Empty means "at least one hit
/// anywhere", which is what [`assert_no_imports_derived`] asks for and what a
/// caller assembling its own `scan` may choose.
pub fn assert_scan(
    subtrees: &[PathBuf],
    witness: &Path,
    what: &str,
    required: &[&str],
    scan: &(dyn Fn(&Path, &Source) -> Vec<Reach> + Sync),
) {
    let files = walked_corpus(subtrees);
    // Property 5 rides in `Source::parse`: a file that does not parse panics
    // there, before any scan could report a shorter walk as clean.
    //
    // The result is one entry per walked file, so the count is checked before
    // it is flattened: a parallel walk that lost a chunk would otherwise be
    // indistinguishable from a corpus with no violation in it.
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

/// [`assert_scan`] for a caller that has **already** walked and scanned these
/// same subtrees, and must not pay for a second pass over them.
///
/// Every property [`assert_scan`] holds is held here, and two of them are
/// checked rather than trusted. The subtrees are re-walked (a directory walk,
/// no parsing) and `scanned` must name exactly that walk, so a caller that read
/// a narrower set than this guard's scope reds naming the difference — property
/// 2, now stated about the caller's own reading. Property 5 was established by
/// the caller's parse of those very files, which is the set this re-walk just
/// confirmed. Properties 3 and 4 run here exactly as always: the witness goes
/// through `scan`, the caller's own scan function, on this side of the call.
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
    // Property 2: a scope glob that stopped matching must red, not pass.
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
    // And per subtree: the corpus stays far above the one-file floor while
    // all but one of its members empty out, so a dropped subtree has to be
    // named rather than absorbed by the union (`rust_sources` answers a
    // missing or unreadable directory with an empty walk).
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
    // Property 3 continued: EVERY atom of `required` must be one the scanner
    // still recognises. "At least one hit" lets the list erode — a needle
    // that quietly stopped matching (a lexer change, a typo) hides behind
    // its neighbours' hits, and its whole scan is then vacuous.
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

/// Parse every file of `files` and apply `scan` to it, across the machine's
/// cores, answering in `files` order.
///
/// **Reads exactly `files`, and every one of them.** Threading decides only
/// *when* a file is parsed, never *whether* — every path handed in is parsed
/// and scanned, a scan that panics (an unreadable or unparseable file,
/// property 5) is re-raised here with its original message rather than
/// swallowed, and the answer is one entry per input file in input order, so a
/// caller can check the count it got against the walk it handed in. A walk
/// that came back short is the one failure this cannot be allowed to hide.
///
/// A `Source` never leaves the thread that parsed it — `proc_macro2` spans are
/// thread-local state — so only `T` crosses, and each thread reads its share
/// of the corpus start to finish by itself.
///
/// The thread count is capped well under the core count because the caller is
/// itself one of `nextest`'s parallel test processes. It is capped at all
/// because the workspace is ~19 MB of Rust that `syn` parses in ~0.9 s on one
/// core, which is the whole reason a guard reading all of it cannot fit in a
/// second.
pub fn map_sources<T: Send>(files: &[PathBuf], scan: &(dyn Fn(&Path, &Source) -> T + Sync)) -> Vec<T> {
    /// Enough to put a full-corpus walk under a second, few enough that
    /// thirty-odd concurrent test processes do not each claim the machine.
    const MAX_THREADS: usize = 8;

    let run = |chunk: &[PathBuf]| -> Vec<T> { chunk.iter().map(|file| scan(file, &Source::parse(file))).collect() };
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_THREADS);
    if threads <= 1 || files.len() <= 1 {
        return run(files);
    }
    let per_thread = files.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(per_thread)
            .map(|chunk| scope.spawn(move || run(chunk)))
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(found) => found,
                // The guard's own refusal, raised on a worker: re-raised
                // verbatim, so the message a `#[should_panic(expected = …)]`
                // test matches is the one it always was.
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    })
}

/// Every `*.rs` file under `subtree`, recursively, sorted. A file path is
/// returned as a single-element walk.
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

/// Whether `attrs` carries exactly `#[cfg(test)]` — the one gate a guard that
/// judges production code strips.
pub fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| match &attr.meta {
        syn::Meta::List(list) if list.path.is_ident("cfg") => {
            let tokens: Vec<Token> = flatten(list.tokens.clone());
            tokens.len() == 1 && tokens[0].text == "test"
        }
        _ => false,
    })
}

/// Expand a `use` tree into `(path segments, alias)` pairs — `crate::a::{b,
/// c::{d as e}, *}` → `[crate,a,b]`, `[crate,a,c,d]` (alias `e`),
/// `[crate,a,*]`. A glob keeps its `*` as the last segment.
///
/// `self` is dropped where it names the module already in the prefix — `use
/// a::{self, b}` is `a` — and **kept where it leads**, because `use
/// self::project::lock` roots the path at the current module and dropping it
/// left `[project, lock]`, a relative path `reaches` could not tell from an
/// extern crate's and therefore ignored (B5R-8).
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

/// The `src/` directory of the crate `subtree` belongs to: the nearest
/// ancestor (inclusive) holding a `lib.rs` or `main.rs`. `None` for a loose
/// fixture tree, which then has no module path and no re-export table.
fn crate_src_root(subtree: &Path) -> Option<PathBuf> {
    let mut dir = if subtree.is_file() { subtree.parent()? } else { subtree };
    loop {
        if dir.join("lib.rs").is_file() || dir.join("main.rs").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Lib-root re-exports: item name → `crate::`-relative module path, read from
/// `lib.rs`'s `use <module>::…` statements whose first segment is a module
/// declared in that file (a private `use` at the root is reachable from every
/// descendant, so it counts). A glob (`pub use media_type::*;`) is resolved
/// by reading that module's file for its `pub` items.
fn reexport_table(src_root: &Path) -> BTreeMap<String, String> {
    let mut table = BTreeMap::new();
    // A `main.rs`-only crate has no root to re-export from; a `lib.rs` that
    // exists but does not parse is red, never an empty table.
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

/// Names of the top-level `pub` (any visibility but inherited) functions,
/// types, traits, constants and statics of a module.
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

/// A path segment with its raw-identifier prefix stripped: `r#project` and
/// `project` name the same module, so a forbidden entry written plainly has to
/// match both — otherwise `use crate::r#project::api;` is a reach nothing
/// reports. (A *root* is never raw: `r#crate`, `r#self`, `r#super` and
/// `r#Self` are the four spellings Rust does not allow.)
fn plain_segment(segment: &str) -> String {
    segment.strip_prefix("r#").unwrap_or(segment).to_owned()
}

fn is_forbidden(segs: &[String], forbidden: &[&str]) -> bool {
    forbidden.iter().any(|entry| {
        let want: Vec<&str> = entry.split("::").collect();
        segs.len() >= want.len() && want.iter().zip(segs).all(|(w, s)| w == s)
    })
}

/// The inline `mod` chain each token sits inside, one entry per token of
/// `tokens`.
///
/// `syn` parses no macro body, so a `mod x { … }` written inside one never
/// becomes an `ItemMod` and never reaches the visitor that tracks them: the
/// module stack a `super::` chain resolves against would be short by however
/// many modules the macro opens, and the chain then pops past the crate root
/// into a path that names nothing forbidden — a silent miss, not a false
/// positive.
///
/// Covered, each pinned by a fixture under `boundary_fixtures/`: a literal
/// `mod x {`, a substituted `mod $x {`, a macro written inside another macro,
/// an attribute (`#[cfg(…)]`, `#[path = "…"]`) between the macro's brace and
/// the `mod`, a `use super::` inside the module, and a module handed to a
/// macro as an *argument* rather than written in its body. Out of scope:
/// `#[path = "…"] mod x;` pointing the module at another file — which file
/// backs a module is [`file_module_path`]'s business, it is read off the
/// file's own location, and the attribute misleads it inside a macro and
/// outside one alike.
fn module_stack_per_token(tokens: &[Token]) -> Vec<Vec<String>> {
    /// The module a `{` at `index` opens, if it opens one. A name written
    /// `$x` is a macro substitution: the name is not knowable before
    /// expansion but the *depth* is, so it is kept verbatim as `$x` — an
    /// opaque segment, since a forbidden entry is always a bare identifier.
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
    // (brace depth the module opened at, its name) — a `}` pops only the
    // module whose own depth it closes.
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

/// Every `crate::`-, `super::`- or `ocx_*::`-rooted path in a flat token list,
/// as `(segments, token index)` — the shape a macro argument or attribute list
/// leaves unparsed. The index, not the line, so the caller can read both the
/// line and the module chain the token sits inside. A root preceded by a `::`
/// separator is the tail of another path; a single `:` before it is a field or
/// type ascription (`Foo { field: crate::x }`, `|a: crate::x::T|`) and the
/// root counts.
///
/// `ocx_*` is here for the same reason it is in `record`: a reach into an
/// extracted crate has no `crate::` spelling, so leaving it out of the
/// macro-token arm would cover the defect in parsed items and miss it in
/// `format!`/`stringify!`/attribute arguments — the asymmetry that makes one
/// arm of a guard quietly do less than the other.
fn rooted_paths_in_tokens(tokens: &[Token]) -> Vec<(Vec<String>, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let root = &tokens[i].text;
        // A `::` before the root ends the question only when something a path
        // can continue from precedes it: `foo::ocx_util` is a segment of
        // someone else's path, but `::ocx_util::x` is the absolute form and is
        // a reach. Treating the pair alone as disqualifying let every
        // `::ocx_*::` written inside macro tokens through both arms of
        // `reaches` (B5R-7) — a shape new with the B5 fix, since `::crate::`
        // is not valid Rust and `ocx_*` is the only root a leading `::` takes.
        let after_separator =
            i > 2 && tokens[i - 1].text == ":" && tokens[i - 2].text == ":" && continues_a_path(&tokens[i - 3].text);
        // `self` is a root like `crate` and `super` (B5R-8). `self.field` and
        // `Self::…` are different tokens, and the `::`-and-identifier loop
        // below is what decides a path anyway, so no receiver can match.
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
            // `#` so a raw identifier (`crate::r#project::api`) is one segment
            // rather than the end of the path — truncating there drops every
            // segment after it, forbidden ones included.
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

/// Whether a `::` following `text` continues a path — an identifier, a raw
/// identifier, or the `>` closing a turbofish. A delimiter, an operator or the
/// start of the stream leaves the `::` path-leading instead.
fn continues_a_path(text: &str) -> bool {
    text == ">" || text.starts_with(|c: char| c.is_alphanumeric() || c == '_' || c == '#')
}

/// Every `use` statement in a flat token list, expanded, as `(segments, token
/// index)` — the shape a macro body leaves unparsed.
///
/// `rooted_paths_in_tokens` reads a path segment by segment and stops at the
/// brace of a group, so a `use crate::{project::lock, shell::export}` written
/// inside a macro invocation yields the bare root `crate` and every name in
/// the group drops out. The tokens are re-lexed as an `ItemUse` and expanded
/// by [`expand_use_tree`] — the same expansion a `use` item outside a macro
/// gets, rather than a second one to drift from it.
///
/// A slice beginning `use` that does not re-lex is a **loud failure**, the
/// same way [`Source::parse`] refuses a file it cannot read (property 5): the
/// silent `continue` this replaced dropped `$crate::{a, b}` — the spelling the
/// crate split forces on every macro that becomes cross-module — with no trace,
/// and the token path scan misses the group independently, so both arms read
/// clean. Panicking converts the whole class, including shapes nobody has
/// enumerated yet. Two shapes are not misses and are handled before the panic,
/// each commented where it is skipped: `use<'a>` precise capturing, and the
/// hygienic `$crate` root, normalised to `crate`.
fn use_trees_in_tokens(file: &Path, tokens: &[Token]) -> Vec<(Vec<String>, usize)> {
    let mut out = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token.text != "use" {
            continue;
        }
        // Not a miss: `impl Iterator<…> + use<'_>` is edition-2024 precise
        // capturing, not a statement. A `use` statement can never be followed
        // by `<`, so the one token decides it. (5 in `crates/**` today, none
        // yet inside macro tokens — this keeps the first one that lands there
        // from reading as a dropped `use`.)
        if tokens.get(index + 1).is_some_and(|next| next.text == "<") {
            continue;
        }
        let slice = match tokens[index..].iter().position(|t| t.text == ";") {
            Some(end) => &tokens[index..=index + end],
            None => &tokens[index..],
        };
        // `flatten` splits every punctuation character into its own token, so
        // the texts are re-joined tight — `: :` has to read back as the path
        // separator `::`. Only two adjacent word tokens need the space that
        // keeps `use crate` from lexing as one identifier.
        let mut text = String::new();
        let mut prev_word = false;
        for (at, part) in slice.iter().enumerate() {
            // Not a miss: `$crate` is the hygienic spelling of `crate`, and
            // dropping the `$` is what makes the statement re-lex at all. This
            // is the fix — without it `$crate::{a, b}` reaches neither arm.
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
        /// `nested` is the `mod` chain opened inside macro tokens around this
        /// path — empty for anything `syn` parsed into items, where
        /// `visit_item_mod` already tracked the nesting in `inline`.
        fn record(&mut self, path: &[String], line: usize, nested: &[String]) {
            // `ocx_*` is the split's crate namespace. Once a subject is
            // extracted, the only way to reach it from another crate is
            // `ocx_<crate>::…` — and a scan that understands `crate::` and
            // `super::` alone returns here without looking, so a guard whose
            // scope has grown onto an extracted `src/` (DEC-30 item 1) walks
            // the new address while being blind to the one defect shape that
            // address admits (B5-7). Matched and reported under the
            // crate-rooted spelling, never folded back onto a module path:
            // crate and module names are not in bijection (`file_structure`
            // is `ocx_store`, `oci::index` is `ocx_index`), so a derived
            // mapping would cover half the needles and say nothing about the
            // other half. A guard that wants the cross-crate half lists
            // `ocx_<crate>` as its own forbidden entry, and property 3 makes
            // its fixture witness it.
            if path.first().is_some_and(|root| root.starts_with("ocx_")) {
                let segs: Vec<String> = path.iter().map(|seg| plain_segment(seg)).collect();
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
                // `self::x` is the current module's `x` — `super` with no ups.
                // It resolved to nothing at all until B5R-8, in a scanner
                // carrying explicit fixtures for `crate::`, `super::`, raw
                // identifiers, `$crate` and macro-nested `mod`.
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
            // Raw spellings are the same module as the plain ones; normalising
            // here puts the re-export lookup, the forbidden test and the
            // reported path all on the canonical form at once.
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
            // The attributes and visibility sit outside the module they
            // decorate: resolved against the enclosing module, before the
            // name is pushed.
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
            // `pub(crate)` / `pub(super)` scope an item; only `pub(in
            // crate::x)` names a module.
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

/// Every occurrence of one of `needles` as a contiguous token sequence in
/// `tokens`, as reaches whose `path` is the needle. A needle that is not a
/// balanced token sequence (`foo(`) is refused.
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
