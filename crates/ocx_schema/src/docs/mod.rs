// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generator for the reference pages under `reference/`.
//!
//! [`read_model`] gathers every documented fact from its source of truth, [`render`] turns the model into
//! pages, and [`run`] is the `ocx_schema docs --out <dir> [--check]` command over both. Output is
//! deterministic bytes, so `--check` can compare against the committed tree.

mod read;
mod render;

use std::path::{Path, PathBuf};

/// A page lists at most this many entries; one more is a [`DocsError::PageTooLarge`].
pub const MAX_ENTRIES_PER_PAGE: usize = 20;

/// `run` exit code: nothing drifted, or the pages were written.
pub const EXIT_CLEAN: i32 = 0;
/// `run` exit code: `--check` found a page that differs from what the sources render.
pub const EXIT_DRIFT: i32 = 1;
/// `run` exit code: bad invocation (missing `--out`, unknown flag).
pub const EXIT_USAGE: i32 = 2;

/// Everything the generated pages state, read from the sources and not yet laid out.
#[derive(Debug, Clone, Default)]
pub struct DocsModel {
    /// Every command, nested ones included, in the order the CLI declares them.
    pub commands: Vec<CommandEntry>,
    /// Flags accepted by every command.
    pub global_flags: Vec<FlagEntry>,
    /// Every environment variable, of any visibility.
    pub env_vars: Vec<EnvVarEntry>,
    /// Keys of the config file.
    pub config_keys: Vec<KeyEntry>,
    /// Keys of the project file (`ocx.toml`).
    pub project_keys: Vec<KeyEntry>,
    /// Every exit code, in declaration order.
    pub exit_codes: Vec<ExitCodeEntry>,
}

/// One command; its page is the one for `path[0]`.
#[derive(Debug, Clone, Default)]
pub struct CommandEntry {
    /// Words after `ocx`, for example `["package", "push"]`.
    pub path: Vec<String>,
    /// The first line of the command's help.
    pub summary: String,
    /// The rest of the help text, empty when the command has none.
    pub description: String,
    /// The usage lines clap renders, one per line, without the `Usage:` label.
    pub usage: String,
    /// The visible aliases, for example `x` for `exec`.
    pub aliases: Vec<String>,
    /// The command's own flags, not the global ones.
    pub flags: Vec<FlagEntry>,
}

/// One flag of a command or of the root.
#[derive(Debug, Clone, Default)]
pub struct FlagEntry {
    /// The `--long` name without dashes.
    pub long: String,
    /// The `-s` short name.
    pub short: Option<char>,
    /// The value placeholder, `None` for a switch.
    pub value: Option<String>,
    /// The environment variable that stands in for the flag.
    pub env: Option<String>,
    /// The flag's one-line help.
    pub help: String,
    /// The rest of the flag's long help, empty when it has none.
    pub description: String,
}

/// One environment variable.
#[derive(Debug, Clone, Default)]
pub struct EnvVarEntry {
    /// The variable name, for example `OCX_HOME`.
    pub name: String,
    /// `public`, `foreign` or `plumbing`.
    pub visibility: String,
    /// The value grammar, for example `bool` or `path`.
    pub value: String,
    /// What the variable does, the first paragraph of its declaration doc.
    pub summary: String,
    /// The remaining paragraphs of the declaration doc.
    pub description: String,
    /// The `--long` flag that stands in for the variable.
    pub flag: Option<String>,
}

/// One key of the config or project file.
#[derive(Debug, Clone, Default)]
pub struct KeyEntry {
    /// The dotted key path, for example `registry.default`.
    pub key: String,
    /// The value type as the schema names it.
    pub type_name: String,
    /// The default, `None` when the key has none.
    pub default: Option<String>,
    /// The environment variable that overrides the key.
    pub env: Option<String>,
    /// What the key does.
    pub summary: String,
    /// The closed set of values the key accepts, each with the first sentence of its meaning (possibly empty).
    pub values: Vec<(String, String)>,
}

/// One process exit code.
#[derive(Debug, Clone, Default)]
pub struct ExitCodeEntry {
    /// The numeric code.
    pub code: u8,
    /// The variant name, for example `UsageError`.
    pub name: String,
    /// The published meaning.
    pub summary: String,
    /// The `error.kind` the code is reported under; empty for `Success`, which reports no error.
    pub kind: String,
    /// The `error.detail` slugs reported under this code, with their one-line meaning.
    pub slugs: Vec<SlugEntry>,
}

/// One `error.detail` slug.
#[derive(Debug, Clone, Default)]
pub struct SlugEntry {
    /// The frozen snake_case value of `error.detail`.
    pub slug: String,
    /// What the slug means.
    pub summary: String,
}

/// One generated page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Path relative to the `--out` directory, `/`-separated, for example `reference/environment.md`.
    pub path: PathBuf,
    /// The page bytes: front matter, the `doc_type` declaration, then entries.
    pub body: String,
}

/// Why [`render`] refused a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocsError {
    /// A page would hold more than [`MAX_ENTRIES_PER_PAGE`] entries.
    PageTooLarge {
        /// The page that overflowed.
        page: PathBuf,
        /// How many entries it would hold.
        entries: usize,
    },
    /// A subcommand of the group that spans pages names no page.
    UngroupedCommand {
        /// The command, for example `package frobnicate`.
        command: String,
    },
}

impl std::fmt::Display for DocsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PageTooLarge { page, entries } => write!(
                f,
                "{} would hold {entries} entries, over the {MAX_ENTRIES_PER_PAGE} a page may carry",
                page.display()
            ),
            Self::UngroupedCommand { command } => write!(f, "`ocx {command}` is on no command page"),
        }
    }
}

impl std::error::Error for DocsError {}

/// The directory the generator owns outright: a `.md` file in it that no page explains is drift.
///
/// Its siblings under `reference/` are authored pages and are never touched.
const OWNED_DIR: &str = "reference/command-line";

/// Read the model from the CLI tree, the env registry, the config and project schemas, and `ExitCode::ALL`.
pub fn read_model() -> DocsModel {
    read::model()
}

/// Lay the model out as pages, sorted by path.
///
/// Pages: `reference/command-line/index.md` plus one page per command group, `reference/environment.md`,
/// `reference/configuration.md`, `reference/project-file.md` and `reference/exit-codes.md`.
pub fn render(model: &DocsModel) -> Result<Vec<Page>, DocsError> {
    render::pages(model)
}

/// Run `docs --out <dir> [--check]` over `model`; `args` excludes the `docs` word. Returns an `EXIT_*` code.
///
/// Without `--check` it writes the pages under `<dir>` and removes stray pages from the generator-owned
/// directory. With `--check` it writes nothing and prints each drifted path.
pub fn run(args: &[String], model: &DocsModel) -> i32 {
    let Some((out, check)) = parse(args) else {
        eprintln!("usage: ocx_schema docs --out <dir> [--check]");
        return EXIT_USAGE;
    };
    let pages = match render(model) {
        Ok(pages) => pages,
        Err(error) => {
            eprintln!("ocx_schema docs: {error}");
            return EXIT_USAGE;
        }
    };
    let outcome = if check {
        drifted(&out, &pages).map(|paths| {
            for path in &paths {
                println!("{}", path.display());
            }
            if paths.is_empty() { EXIT_CLEAN } else { EXIT_DRIFT }
        })
    } else {
        write(&out, &pages).map(|()| EXIT_CLEAN)
    };
    outcome.unwrap_or_else(|error| {
        eprintln!("ocx_schema docs: {error}");
        EXIT_USAGE
    })
}

/// The command entry point: [`run`] over [`read_model`].
pub fn run_cli(args: &[String]) -> i32 {
    run(args, &read_model())
}

/// Paths relative to `out` whose bytes differ from `pages`, missing pages and stray ones included.
///
/// A page is stray when it sits in the generator-owned `reference/command-line/` and no page explains it.
pub fn drifted(out: &Path, pages: &[Page]) -> std::io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for page in pages {
        match std::fs::read(out.join(&page.path)) {
            Ok(bytes) if bytes == page.body.as_bytes() => {}
            Ok(_) => paths.push(page.path.clone()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => paths.push(page.path.clone()),
            Err(error) => return Err(error),
        }
    }
    paths.extend(strays(out, pages)?);
    paths.sort();
    Ok(paths)
}

/// `(out, check)` from `--out <dir>` / `--out=<dir>` and `--check`; `None` for anything else.
fn parse(args: &[String]) -> Option<(PathBuf, bool)> {
    let (mut out, mut check) = (None, false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check" => check = true,
            "--out" => out = Some(PathBuf::from(args.next()?)),
            other => out = Some(PathBuf::from(other.strip_prefix("--out=")?)),
        }
    }
    Some((out?, check))
}

fn write(out: &Path, pages: &[Page]) -> std::io::Result<()> {
    for page in pages {
        let dest = out.join(&page.path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, &page.body)?;
    }
    for stray in strays(out, pages)? {
        std::fs::remove_file(out.join(stray))?;
    }
    Ok(())
}

fn strays(out: &Path, pages: &[Page]) -> std::io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    collect_markdown(out, Path::new(OWNED_DIR), &mut found)?;
    found.retain(|path| !pages.iter().any(|page| page.path == *path));
    Ok(found)
}

fn collect_markdown(root: &Path, rel: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(root.join(rel)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let child = rel.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            collect_markdown(root, &child, found)?;
        } else if child.extension().is_some_and(|ext| ext == "md") {
            found.push(child);
        }
    }
    Ok(())
}
