// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::package::metadata::env::modifier::ModifierKind;

use super::{error, flavor::Flavor};

/// GitHub Actions CI flavor.
///
/// Writes environment variable exports to the runtime files specified by
/// `$GITHUB_PATH` (for `PATH` entries) and `$GITHUB_ENV` (for everything else).
pub(super) struct GitHubFlavor {
    path_file: PathBuf,
    env_file: PathBuf,
}

impl GitHubFlavor {
    /// Reads `$GITHUB_PATH` and `$GITHUB_ENV` from the environment.
    pub fn from_env() -> Result<Self, error::Error> {
        let path_file = required_env_path("GITHUB_PATH")?;
        let env_file = required_env_path("GITHUB_ENV")?;
        Ok(Self { path_file, env_file })
    }
}

impl Flavor for GitHubFlavor {
    fn write_entry(&self, key: &str, value: &str, kind: &ModifierKind) -> crate::Result<()> {
        match kind {
            ModifierKind::Path if key == "PATH" => {
                append_line(&self.path_file, value)?;
            }
            ModifierKind::Path => {
                // Prepend to existing value. GITHUB_ENV doesn't do shell expansion,
                // so we read the current env value and concatenate.
                let existing = crate::env::var(key).unwrap_or_default();
                let full = if existing.is_empty() {
                    value.to_string()
                } else {
                    format!("{value}{}{existing}", crate::env::PATH_SEPARATOR)
                };
                append_env_var(&self.env_file, key, &full)?;
            }
            ModifierKind::Constant => {
                append_env_var(&self.env_file, key, value)?;
            }
        }
        Ok(())
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
        .map_err(|e| error::Error::File(file.to_path_buf(), e))?;
    f.write_all(content.as_bytes())
        .map_err(|e| error::Error::File(file.to_path_buf(), e))
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
        let targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("PATH", "/home/user/.ocx/objects/foo/bin", &ModifierKind::Path)
            .unwrap();

        assert_eq!(read(&targets.path_file), "/home/user/.ocx/objects/foo/bin\n");
    }

    #[test]
    fn non_path_prepend_empty() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let targets = setup_github_env(&env, &tmp);
        env.remove("LD_LIBRARY_PATH");

        targets
            .write_entry(
                "LD_LIBRARY_PATH",
                "/home/user/.ocx/objects/foo/lib",
                &ModifierKind::Path,
            )
            .unwrap();

        assert_eq!(
            read(&targets.env_file),
            "LD_LIBRARY_PATH=/home/user/.ocx/objects/foo/lib\n"
        );
    }

    #[test]
    fn non_path_prepend_existing() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let targets = setup_github_env(&env, &tmp);
        env.set("LD_LIBRARY_PATH", "/existing/lib");

        targets
            .write_entry(
                "LD_LIBRARY_PATH",
                "/home/user/.ocx/objects/foo/lib",
                &ModifierKind::Path,
            )
            .unwrap();

        assert_eq!(
            read(&targets.env_file),
            "LD_LIBRARY_PATH=/home/user/.ocx/objects/foo/lib:/existing/lib\n"
        );
    }

    #[test]
    fn constant_export() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("JAVA_HOME", "/home/user/.ocx/objects/foo", &ModifierKind::Constant)
            .unwrap();

        assert_eq!(read(&targets.env_file), "JAVA_HOME=/home/user/.ocx/objects/foo\n");
    }

    #[test]
    fn multiline_constant() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("MY_VAR", "line1\nline2", &ModifierKind::Constant)
            .unwrap();

        assert_eq!(read(&targets.env_file), "MY_VAR<<EOF\nline1\nline2\nEOF\n");
    }

    #[test]
    fn multiline_with_eof_in_value() {
        let env = crate::test::env::lock();
        let tmp = tempfile::tempdir().unwrap();
        let targets = setup_github_env(&env, &tmp);

        targets
            .write_entry("MY_VAR", "contains\nEOF\ninside", &ModifierKind::Constant)
            .unwrap();

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
}
