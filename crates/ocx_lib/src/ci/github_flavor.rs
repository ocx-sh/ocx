// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::log::*;
use crate::package::metadata::env::conflict::ConstantTracker;
use crate::package::metadata::env::modifier::ModifierKind;

use super::{error, flavor::Flavor};

/// GitHub Actions CI flavor.
///
/// Writes environment variable exports to the runtime files specified by
/// `$GITHUB_PATH` (for `PATH` entries) and `$GITHUB_ENV` (for everything else).
///
/// All entries are buffered in memory and written on [`flush`](Flavor::flush),
/// producing exactly one line per key in each output file. This avoids
/// last-writer-wins issues when multiple packages contribute to the same var.
pub(super) struct GitHubFlavor {
    path_file: PathBuf,
    env_file: PathBuf,
    /// Buffered PATH entries for `$GITHUB_PATH` (one per line, in order).
    path_entries: Vec<String>,
    /// Buffered non-PATH path entries: key → [values in prepend order].
    buffered_paths: IndexMap<String, Vec<String>>,
    /// Buffered constant entries: key → value (last-writer-wins).
    buffered_constants: IndexMap<String, String>,
    /// Tracks constant-type assignments to warn on conflicts.
    constants: ConstantTracker,
}

impl GitHubFlavor {
    /// Reads `$GITHUB_PATH` and `$GITHUB_ENV` from the environment.
    pub fn from_env() -> Result<Self, error::Error> {
        let path_file = required_env_path("GITHUB_PATH")?;
        let env_file = required_env_path("GITHUB_ENV")?;
        Ok(Self {
            path_file,
            env_file,
            path_entries: Vec::new(),
            buffered_paths: IndexMap::new(),
            buffered_constants: IndexMap::new(),
            constants: ConstantTracker::new(),
        })
    }

    /// Returns `true` if there are any buffered entries to flush.
    fn has_buffered(&self) -> bool {
        !self.path_entries.is_empty() || !self.buffered_paths.is_empty() || !self.buffered_constants.is_empty()
    }

    /// Flushes all buffered entries to their respective files.
    fn flush_inner(&mut self) -> crate::Result<()> {
        // Dedup `$GITHUB_PATH` lines keeping the LAST occurrence, preserving
        // order. The runner prepends each line to `PATH` as it reads the file,
        // so the last line written wins precedence (LIFO); keeping the last
        // occurrence matches the in-process move-to-front semantics and avoids
        // emitting duplicate directory lines on re-export. See
        // <https://docs.github.com/actions/reference/workflow-commands-for-github-actions#adding-a-system-path>.
        use crate::utility::vec_ext::VecExt;
        self.path_entries.unique_last();
        for entry in self.path_entries.drain(..) {
            append_line(&self.path_file, &entry)?;
        }
        for (key, values) in self.buffered_paths.drain(..) {
            let full = super::prepend_existing(&key, &values);
            append_env_var(&self.env_file, &key, &full)?;
        }
        for (key, value) in self.buffered_constants.drain(..) {
            append_env_var(&self.env_file, &key, &value)?;
        }
        Ok(())
    }
}

impl Flavor for GitHubFlavor {
    fn write_entry(&mut self, key: &str, value: &str, kind: &ModifierKind) -> crate::Result<()> {
        // Gate the key slot before any branch buffers it: a key bearing a
        // newline would inject a second `KEY=value` line into `$GITHUB_ENV`
        // (CWE-77), and a non-identifier charset corrupts the var name.
        if !crate::env::is_valid_env_key(key) {
            warn!("skipping invalid env-var key {key:?} for CI export");
            return Ok(());
        }
        match kind {
            ModifierKind::Path if key == "PATH" => {
                // `$GITHUB_PATH` entries are written one raw value per line,
                // unquoted. A value containing `\n`/`\r` would inject extra
                // PATH directories (CWE-426/77); a real PATH dir never has a
                // newline, so reject it.
                if value.contains('\n') || value.contains('\r') {
                    warn!("skipping PATH value with embedded newline for CI export: {value:?}");
                    return Ok(());
                }
                self.path_entries.push(value.to_string());
            }
            ModifierKind::Path => {
                self.buffered_paths
                    .entry(key.to_string())
                    .or_default()
                    .push(value.to_string());
            }
            ModifierKind::Constant => {
                if let Some(conflict) = self.constants.track("", key, value) {
                    warn!("{}", conflict);
                }
                self.buffered_constants.insert(key.to_string(), value.to_string());
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> crate::Result<()> {
        self.flush_inner()
    }
}

impl Drop for GitHubFlavor {
    fn drop(&mut self) {
        if self.has_buffered()
            && let Err(e) = self.flush_inner()
        {
            error!("failed to flush CI env vars on drop: {e}");
        }
    }
}

/// Detects whether we're running inside GitHub Actions.
pub(super) fn detect() -> bool {
    crate::env::var("GITHUB_ACTIONS").as_deref() == Some("true")
}

/// Appends a `KEY=value` entry to a GitHub Actions environment file,
/// using heredoc delimiters for values containing newlines or double quotes.
fn append_env_var(file: &Path, key: &str, value: &str) -> Result<(), error::Error> {
    let needs_delimiter = value.contains('\n') || value.contains('"');
    if needs_delimiter {
        let delim = unique_delimiter(value);
        let content = format!("{key}<<{delim}\n{value}\n{delim}\n");
        append_line_raw(file, &content)
    } else {
        append_line(file, &format!("{key}={value}"))
    }
}

/// Appends a single line (with trailing newline) to a file.
fn append_line(file: &Path, line: &str) -> Result<(), error::Error> {
    append_line_raw(file, &format!("{line}\n"))
}

/// Appends raw content to a file.
fn append_line_raw(file: &Path, content: &str) -> Result<(), error::Error> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| error::Error::File {
            path: file.to_path_buf(),
            source: e,
        })?;
    f.write_all(content.as_bytes()).map_err(|e| error::Error::File {
        path: file.to_path_buf(),
        source: e,
    })
}

/// Reads a required environment variable that should contain a file path.
fn required_env_path(name: &str) -> Result<PathBuf, error::Error> {
    crate::env::var(name)
        .map(PathBuf::from)
        .ok_or_else(|| error::Error::MissingEnv(name.to_string()))
}

/// Generates a delimiter that does not appear in the value.
fn unique_delimiter(value: &str) -> String {
    let base = "EOF";
    if !value.contains(base) {
        return base.to_string();
    }
    for i in 0u64.. {
        let candidate = format!("{base}_{i}");
        if !value.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::ci::CiFlavor;
    use crate::ci::flavor::Flavor;
    use crate::package::metadata::env::modifier::ModifierKind;

    fn setup_github_env(env: &crate::test::env::EnvLock, tmp: &tempfile::TempDir) -> super::GitHubFlavor {
        let path_file = tmp.path().join("github_path");
        let env_file = tmp.path().join("github_env");
        std::fs::write(&path_file, "").unwrap();
        std::fs::write(&env_file, "").unwrap();
        env.set("GITHUB_ACTIONS", "true");
        env.set("GITHUB_PATH", path_file.to_str().unwrap());
        env.set("GITHUB_ENV", env_file.to_str().unwrap());
        super::GitHubFlavor::from_env().unwrap()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn path_export() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("PATH", "/home/user/.ocx/objects/foo/bin", &ModifierKind::Path)
            .unwrap();
        targets.flush().unwrap();

        assert_eq!(read(&targets.path_file), "/home/user/.ocx/objects/foo/bin\n");
    }

    #[test]
    fn non_path_prepend_empty() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);
        env.remove("LD_LIBRARY_PATH");

        targets
            .write_entry(
                "LD_LIBRARY_PATH",
                "/home/user/.ocx/objects/foo/lib",
                &ModifierKind::Path,
            )
            .unwrap();
        targets.flush().unwrap();

        assert_eq!(
            read(&targets.env_file),
            "LD_LIBRARY_PATH=/home/user/.ocx/objects/foo/lib\n"
        );
    }

    #[test]
    fn non_path_prepend_existing() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);
        env.set("LD_LIBRARY_PATH", "/existing/lib");

        targets
            .write_entry(
                "LD_LIBRARY_PATH",
                "/home/user/.ocx/objects/foo/lib",
                &ModifierKind::Path,
            )
            .unwrap();
        targets.flush().unwrap();

        assert_eq!(
            read(&targets.env_file),
            format!(
                "LD_LIBRARY_PATH=/home/user/.ocx/objects/foo/lib{0}/existing/lib\n",
                crate::env::PATH_SEPARATOR
            )
        );
    }

    #[test]
    fn constant_export() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("JAVA_HOME", "/home/user/.ocx/objects/foo", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        assert_eq!(read(&targets.env_file), "JAVA_HOME=/home/user/.ocx/objects/foo\n");
    }

    #[test]
    fn multiline_constant() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("MY_VAR", "line1\nline2", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        assert_eq!(read(&targets.env_file), "MY_VAR<<EOF\nline1\nline2\nEOF\n");
    }

    #[test]
    fn multiline_with_eof_in_value() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("MY_VAR", "contains\nEOF\ninside", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.env_file);
        assert!(content.contains("EOF_0"));
        assert!(content.starts_with("MY_VAR<<EOF_0\n"));
    }

    #[test]
    fn detect_github_actions() {
        let env = crate::test::env::lock();
        env.set("GITHUB_ACTIONS", "true");
        assert_eq!(CiFlavor::detect(), Some(CiFlavor::GitHubActions));
    }

    #[test]
    fn detect_no_ci() {
        let env = crate::test::env::lock();
        env.remove("GITHUB_ACTIONS");
        assert_eq!(CiFlavor::detect(), None);
    }

    #[test]
    fn missing_github_path_env_errors() {
        let env = crate::test::env::lock();
        env.set("GITHUB_ACTIONS", "true");
        env.remove("GITHUB_PATH");
        env.set("GITHUB_ENV", "/tmp/fake");

        let result = super::GitHubFlavor::from_env();
        assert!(result.is_err());
    }

    #[test]
    fn non_path_accumulates_across_entries() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);
        env.remove("LD_LIBRARY_PATH");

        targets
            .write_entry("LD_LIBRARY_PATH", "/pkg1/lib", &ModifierKind::Path)
            .unwrap();
        targets
            .write_entry("LD_LIBRARY_PATH", "/pkg2/lib", &ModifierKind::Path)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.env_file);
        assert_eq!(
            content,
            format!("LD_LIBRARY_PATH=/pkg1/lib{0}/pkg2/lib\n", crate::env::PATH_SEPARATOR)
        );
    }

    #[test]
    fn non_path_accumulates_with_existing_env() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);
        env.set("LD_LIBRARY_PATH", "/existing/lib");

        targets
            .write_entry("LD_LIBRARY_PATH", "/pkg1/lib", &ModifierKind::Path)
            .unwrap();
        targets
            .write_entry("LD_LIBRARY_PATH", "/pkg2/lib", &ModifierKind::Path)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.env_file);
        assert_eq!(
            content,
            format!(
                "LD_LIBRARY_PATH=/pkg1/lib{0}/pkg2/lib{0}/existing/lib\n",
                crate::env::PATH_SEPARATOR
            )
        );
    }

    #[test]
    fn key_with_newline_skipped() {
        // A constant key bearing a newline would inject a second `KEY=value`
        // line into `$GITHUB_ENV` (Finding 2, CWE-77). The entry is dropped,
        // so the env file stays empty — no injected second variable.
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("FOO\nINJECTED=evil", "value", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.env_file);
        assert_eq!(content, "", "invalid key must not write any line to $GITHUB_ENV");
        assert!(
            !content.contains("INJECTED"),
            "no injected second variable must appear: got {content:?}"
        );
    }

    #[test]
    fn path_value_with_newline_rejected() {
        // A `$GITHUB_PATH` value with an embedded newline would inject extra
        // PATH directories (Finding 1, CWE-426/77). The value is rejected, so
        // the path file does not contain the injected dir.
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("PATH", "/pkg/bin\n/injected/evil", &ModifierKind::Path)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.path_file);
        assert_eq!(content, "", "newline-bearing PATH value must not be written");
        assert!(
            !content.contains("/injected/evil"),
            "injected PATH dir must not appear: got {content:?}"
        );
    }

    #[test]
    fn key_with_invalid_charset_skipped() {
        // Keys containing `=` or spaces are not valid env-var identifiers.
        // They must be rejected before reaching `$GITHUB_ENV` to prevent
        // corrupting the KEY=value line format (parity with GitLab charset test).
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("FOO=BAR", "value", &ModifierKind::Constant)
            .unwrap();
        targets
            .write_entry("FOO BAR", "value", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        let content = read(&targets.env_file);
        assert_eq!(
            content, "",
            "invalid-charset keys must not write any line to $GITHUB_ENV"
        );
    }

    #[test]
    fn drop_flushes_buffered_entries() {
        // The Drop impl flushes any buffered entries that were never explicitly
        // flushed. GitHubFlavor writes to files, so we observe the output by
        // reading the env file after the flavor is dropped.
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let env_file = tmp.path().join("github_env");
        let path_file = tmp.path().join("github_path");
        std::fs::write(&path_file, "").unwrap();
        std::fs::write(&env_file, "").unwrap();
        env.set("GITHUB_ACTIONS", "true");
        env.set("GITHUB_PATH", path_file.to_str().unwrap());
        env.set("GITHUB_ENV", env_file.to_str().unwrap());

        {
            let mut targets = super::GitHubFlavor::from_env().unwrap();
            targets
                .write_entry("DROP_TEST", "drop-value", &ModifierKind::Constant)
                .unwrap();
            // Intentionally NO flush() call — drop must flush instead.
            drop(targets);
        }

        // The Drop impl should have written the buffered constant to disk.
        let content = read(&env_file);
        assert_eq!(
            content, "DROP_TEST=drop-value\n",
            "Drop must flush buffered entries without an explicit flush() call"
        );
    }

    #[test]
    fn duplicate_path_dir_dedups_keeping_last() {
        // Two packages contributing the same PATH dir must collapse to a single
        // `$GITHUB_PATH` line (keep-last order), so re-export does not grow it.
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets.write_entry("PATH", "/shared/bin", &ModifierKind::Path).unwrap();
        targets.write_entry("PATH", "/pkg/bin", &ModifierKind::Path).unwrap();
        targets.write_entry("PATH", "/shared/bin", &ModifierKind::Path).unwrap();
        targets.flush().unwrap();

        // keep-last: the first `/shared/bin` is dropped, leaving `/pkg/bin`
        // then `/shared/bin` — each directory appears exactly once.
        assert_eq!(read(&targets.path_file), "/pkg/bin\n/shared/bin\n");
    }

    #[test]
    fn constant_conflict_warns() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let mut targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("JAVA_HOME", "/pkg1/java", &ModifierKind::Constant)
            .unwrap();
        targets
            .write_entry("JAVA_HOME", "/pkg2/java", &ModifierKind::Constant)
            .unwrap();
        targets.flush().unwrap();

        // Only the last value should appear (constants are deduplicated on flush)
        let content = read(&targets.env_file);
        assert_eq!(content, "JAVA_HOME=/pkg2/java\n");
    }
}
