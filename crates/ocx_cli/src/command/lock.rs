// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::project::{DEFAULT_GROUP, ProjectConfig, ProjectLock, ResolveLockOptions, resolve_lock};

use crate::api::data::lock::{LockEntry, LockReport};

/// Resolve tool tags to digests and write `ocx.lock`.
///
/// Walks the nearest `ocx.toml`, resolves each tool's advisory tag to a
/// pinned OCI index-manifest digest, and writes a deterministic
/// `ocx.lock` next to it. Fully transactional — either every tool
/// resolves or nothing is written.
#[derive(Parser, Clone)]
pub struct Lock {
    /// Restrict resolution to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// When omitted, every `[tools]` and `[group.*]` entry is resolved.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,
}

impl Lock {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Pre-validate empty comma segments BEFORE any filesystem or network
        // work. `clap`'s `value_delimiter = ','` splits `-g ci,,lint` into
        // `["ci", "", "lint"]`; an empty string is a user-typing error and
        // the acceptance test (test 9) asserts the usage-error exit code
        // with no `ocx.lock` written.
        for raw in &self.groups {
            if raw.is_empty() {
                eprintln!("empty group segment in --group value; check for stray commas");
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        }

        // Resolve `ocx.toml` + sibling `ocx.lock` paths with the full
        // precedence chain: explicit flag > env > CWD walk > home fallback.
        let cwd = ocx_lib::env::current_dir()?;
        let home = context.file_structure().root().to_path_buf();
        let resolved = ProjectConfig::resolve(Some(&cwd), context.project_path(), Some(&home)).await?;

        let (config_path, lock_path) = match resolved {
            Some(pair) => pair,
            None => {
                // No `ocx.toml` anywhere in the precedence chain — this is a
                // usage error (test 6). The message mentions `ocx.toml` so
                // the Python assertion finds it in stderr.
                eprintln!(
                    "no ocx.toml found in {} or any parent; run `ocx lock` from a project directory",
                    cwd.display()
                );
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        };

        // Load the config so we can validate `--group` names against real
        // group keys before doing any resolution work.
        let config = ProjectConfig::from_path(&config_path).await?;

        // Validate requested groups against the loaded config. Unknown
        // groups produce exit 64 (test 8). `default` is always valid since
        // it names the top-level `[tools]` table.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP {
                continue;
            }
            if !config.groups.contains_key(raw) {
                eprintln!("unknown group '{raw}' in --group filter");
                return Ok(ocx_lib::cli::ExitCode::UsageError.into());
            }
        }

        // Acquire exclusive sidecar advisory lock and parse any existing
        // lock file. A corrupt existing lock surfaces as a TomlParse error
        // here and — critically — does NOT replace the file (test 10): the
        // error short-circuits before the `save` call below.
        let (previous, _guard) = ProjectLock::load_exclusive(&lock_path).await?;

        // Run the resolver. Fully transactional: on any error (tag 404,
        // auth, timeout, registry unreachable) nothing is written.
        let lock = resolve_lock(
            &config,
            context.default_index(),
            &self.groups,
            ResolveLockOptions::default(),
        )
        .await?;

        // Atomic save — preserves `generated_at` when resolved content is
        // unchanged (test 4 guards this invariant at the CLI boundary).
        lock.save(&lock_path, previous.as_ref()).await?;

        // Non-fatal advisory note when `.gitattributes` lacks
        // `ocx.lock merge=union` — helps prevent merge conflicts on team
        // projects (test 11). Emitted to stderr so it doesn't pollute
        // machine-readable stdout.
        let project_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        if !gitattributes_has_merge_union(project_dir).await {
            eprintln!("note: add `ocx.lock merge=union` to .gitattributes to avoid merge conflicts");
        }

        // Build the success report from the actual resolved lock (not from
        // CLI args) — the write-through via `previous.as_ref()` may have
        // preserved `generated_at`, but the tools themselves are always
        // the freshly resolved set.
        let entries: Vec<LockEntry> = lock
            .tools
            .iter()
            .map(|t| LockEntry {
                binding: t.name.clone(),
                group: t.group.clone(),
                digest: t.pinned.strip_advisory().to_string(),
            })
            .collect();
        let report = LockReport::new(entries);
        context.api().report(&report)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Probe whether `{project_dir}/.gitattributes` contains the
/// `ocx.lock merge=union` attribute line.
///
/// The check is intentionally lenient: any line matching `ocx.lock` +
/// whitespace + `merge=union` passes. Missing file, unreadable file,
/// and file-without-line all return `false` — callers then emit the
/// advisory note. Returns `true` only when the line is present.
async fn gitattributes_has_merge_union(project_dir: &Path) -> bool {
    let path = project_dir.join(".gitattributes");
    let Ok(contents) = tokio::fs::read_to_string(&path).await else {
        return false;
    };
    contents.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return false;
        }
        // Look for `ocx.lock` as a whitespace-separated token followed by
        // `merge=union` somewhere in the same line. Rough but sufficient
        // — .gitattributes syntax is `pattern attr[=value] ...`.
        let mut tokens = trimmed.split_whitespace();
        let Some(pattern) = tokens.next() else {
            return false;
        };
        if pattern != "ocx.lock" {
            return false;
        }
        tokens.any(|t| t == "merge=union")
    })
}
