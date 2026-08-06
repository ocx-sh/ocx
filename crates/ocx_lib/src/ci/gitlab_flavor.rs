// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::Write;
use std::path::PathBuf;

use indexmap::IndexMap;
use serde::Serialize;

use crate::log::*;
use crate::package::metadata::env::conflict::ConstantTracker;
use crate::package::metadata::env::list::DEFAULT_SEPARATOR;
use crate::package::metadata::env::modifier::ModifierKind;

use super::{error, flavor::Flavor};

/// GitLab CI/CD flavor.
///
/// GitLab's step-runner persists later-step environment via an export file
/// (`${{ export_file }}`) of JSON-lines, each `{"name": "...", "value": "..."}`.
/// Unlike GitHub Actions there is **no path channel** and the scope is the
/// *later* steps, so every path-type entry — including `PATH` itself — is
/// flattened into a single computed value the same way GitHub treats its
/// non-`PATH` path case: accumulate the package values, read the existing
/// process value, prepend, join with [`PATH_SEPARATOR`](crate::env::PATH_SEPARATOR),
/// and emit one JSON-line. Constants emit one JSON-line each (last-writer-wins).
///
/// The destination defaults to stdout (redirect to `${{ export_file }}`); a
/// caller may instead pass an explicit output path. All entries are buffered in
/// memory and serialized on [`flush`](Flavor::flush), producing exactly one line
/// per key.
pub(super) struct GitLabFlavor {
    /// Destination sink — an append/create file or stdout.
    sink: Box<dyn Write + Send>,
    /// Buffered path entries (any key, `PATH` included): key → values in
    /// prepend order.
    buffered_paths: IndexMap<String, Vec<String>>,
    /// Buffered list entries: key → (separator, [values in append order]).
    /// The separator is captured from the first entry buffered for a key;
    /// every later entry for the same key already carries the same one,
    /// settled upstream by `reconcile_list_separators`.
    buffered_lists: IndexMap<String, (String, Vec<String>)>,
    /// Buffered constant entries: key → value (last-writer-wins).
    buffered_constants: IndexMap<String, String>,
    /// Tracks constant-type assignments to warn on conflicts.
    constants: ConstantTracker,
}

impl GitLabFlavor {
    /// Opens the destination sink.
    ///
    /// `Some(path)` opens the file for append/create; `None` writes the
    /// JSON-lines to stdout (the caller redirects to `${{ export_file }}`).
    pub fn new(export_file: Option<PathBuf>) -> Result<Self, error::Error> {
        let sink: Box<dyn Write + Send> = match export_file {
            Some(path) => {
                let file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| error::Error::File { path, source: e })?;
                Box::new(file)
            }
            None => Box::new(std::io::stdout()),
        };
        Ok(Self::new_with_writer(sink))
    }

    /// Constructs a flavor around an injected sink.
    ///
    /// Used by tests to assert against an in-memory buffer without touching
    /// stdout or the filesystem.
    fn new_with_writer(sink: Box<dyn Write + Send>) -> Self {
        Self {
            sink,
            buffered_paths: IndexMap::new(),
            buffered_lists: IndexMap::new(),
            buffered_constants: IndexMap::new(),
            constants: ConstantTracker::new(),
        }
    }

    /// Returns `true` if there are any buffered entries to flush.
    fn has_buffered(&self) -> bool {
        !self.buffered_paths.is_empty() || !self.buffered_lists.is_empty() || !self.buffered_constants.is_empty()
    }

    /// Computes the final key/value pairs and serializes them as JSON-lines.
    fn flush_inner(&mut self) -> crate::Result<()> {
        // Compute path-prepend values first (reads the existing process env),
        // then emit constants. Draining into a local list keeps the immutable
        // env reads separate from the mutable borrow of the sink.
        let mut lines: Vec<(String, String)> = Vec::new();
        for (key, values) in self.buffered_paths.drain(..) {
            let full = super::prepend_existing(&key, &values);
            lines.push((key, full));
        }
        for (key, (separator, values)) in self.buffered_lists.drain(..) {
            let full = super::append_existing(&key, &values, &separator);
            lines.push((key, full));
        }
        for (key, value) in self.buffered_constants.drain(..) {
            lines.push((key, value));
        }

        for (name, value) in &lines {
            write_export_line(&mut self.sink, name, value)?;
        }
        self.sink.flush().map_err(error::Error::Write)?;
        Ok(())
    }
}

impl Flavor for GitLabFlavor {
    fn write_entry(
        &mut self,
        key: &str,
        value: &str,
        kind: &ModifierKind,
        separator: Option<&str>,
    ) -> crate::Result<()> {
        // Gate the key slot before buffering: a non-identifier charset (spaces,
        // `=`, newlines) corrupts the JSON-lines `name` field. Values are
        // serde-framed so they need no separate newline guard here.
        if !crate::env::is_valid_env_key(key) {
            warn!("skipping invalid env-var key {key:?} for CI export");
            return Ok(());
        }
        match kind {
            // GitLab has no path channel, so PATH is flattened like any other
            // path-type variable (accumulate, prepend to existing on flush).
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
            ModifierKind::List => {
                let separator = separator.unwrap_or(DEFAULT_SEPARATOR);
                self.buffered_lists
                    .entry(key.to_string())
                    .or_insert_with(|| (separator.to_string(), Vec::new()))
                    .1
                    .push(value.to_string());
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> crate::Result<()> {
        self.flush_inner()
    }
}

impl Drop for GitLabFlavor {
    fn drop(&mut self) {
        if self.has_buffered()
            && let Err(e) = self.flush_inner()
        {
            error!("failed to flush CI env vars on drop: {e}");
        }
    }
}

/// Detects whether we're running inside GitLab CI/CD.
pub(super) fn detect() -> bool {
    crate::env::var("GITLAB_CI").as_deref() == Some("true")
}

/// A single GitLab export entry, serialized as one JSON object per line.
#[derive(Serialize)]
struct ExportLine<'a> {
    name: &'a str,
    value: &'a str,
}

/// The single serializer for GitLab's JSON-lines export format.
///
/// Produces `{"name":"<key>","value":"<value>"}` followed by a newline. This is
/// the one place the wire shape is pinned, so a step-runner format change is a
/// one-line fix.
fn write_export_line(writer: &mut dyn Write, name: &str, value: &str) -> Result<(), error::Error> {
    let line = ExportLine { name, value };
    let mut encoded = serde_json::to_string(&line).map_err(|e| error::Error::Write(std::io::Error::other(e)))?;
    encoded.push('\n');
    writer.write_all(encoded.as_bytes()).map_err(error::Error::Write)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use crate::ci::CiFlavor;
    use crate::ci::flavor::Flavor;
    use crate::package::metadata::env::modifier::ModifierKind;

    /// A `Write` sink that records bytes into a shared buffer so tests can
    /// assert the exact JSON-lines without touching stdout or disk.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn flavor(buf: &SharedBuf) -> super::GitLabFlavor {
        super::GitLabFlavor::new_with_writer(Box::new(buf.clone()))
    }

    #[test]
    fn single_path() {
        let env = crate::test::env::lock();
        env.remove("LD_LIBRARY_PATH");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("LD_LIBRARY_PATH", "/pkg/lib", &ModifierKind::Path, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            "{\"name\":\"LD_LIBRARY_PATH\",\"value\":\"/pkg/lib\"}\n"
        );
    }

    #[test]
    fn path_accumulates_across_entries() {
        let env = crate::test::env::lock();
        env.remove("LD_LIBRARY_PATH");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("LD_LIBRARY_PATH", "/pkg1/lib", &ModifierKind::Path, None)
            .unwrap();
        target
            .write_entry("LD_LIBRARY_PATH", "/pkg2/lib", &ModifierKind::Path, None)
            .unwrap();
        target.flush().unwrap();

        // C1a: last applied wins and lands at the front, so `/pkg2/lib`
        // (written second) precedes `/pkg1/lib`. Both CI flavors share
        // `ci::prepend_existing`, so this direction is pinned identically
        // here and in the GitHub flavor.
        assert_eq!(
            buf.contents(),
            format!(
                "{{\"name\":\"LD_LIBRARY_PATH\",\"value\":\"/pkg2/lib{0}/pkg1/lib\"}}\n",
                crate::env::PATH_SEPARATOR
            )
        );
    }

    #[test]
    fn path_prepends_existing_env() {
        let env = crate::test::env::lock();
        env.set("LD_LIBRARY_PATH", "/existing/lib");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("LD_LIBRARY_PATH", "/pkg/lib", &ModifierKind::Path, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            format!(
                "{{\"name\":\"LD_LIBRARY_PATH\",\"value\":\"/pkg/lib{0}/existing/lib\"}}\n",
                crate::env::PATH_SEPARATOR
            )
        );
    }

    #[test]
    fn path_key_flattened_with_existing() {
        // GitLab has no path channel: PATH is treated like any other path var,
        // prepended onto the existing process PATH.
        let env = crate::test::env::lock();
        env.set("PATH", "/usr/bin");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("PATH", "/pkg/bin", &ModifierKind::Path, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            format!(
                "{{\"name\":\"PATH\",\"value\":\"/pkg/bin{0}/usr/bin\"}}\n",
                crate::env::PATH_SEPARATOR
            )
        );
    }

    #[test]
    fn constant() {
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("JAVA_HOME", "/pkg/java", &ModifierKind::Constant, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(buf.contents(), "{\"name\":\"JAVA_HOME\",\"value\":\"/pkg/java\"}\n");
    }

    #[test]
    fn constant_conflict_warns_last_wins() {
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("JAVA_HOME", "/pkg1/java", &ModifierKind::Constant, None)
            .unwrap();
        target
            .write_entry("JAVA_HOME", "/pkg2/java", &ModifierKind::Constant, None)
            .unwrap();
        target.flush().unwrap();

        // Only the last value survives (constants are deduplicated on flush).
        assert_eq!(buf.contents(), "{\"name\":\"JAVA_HOME\",\"value\":\"/pkg2/java\"}\n");
    }

    #[test]
    fn json_escapes_special_chars() {
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        // A value with a double-quote, backslash, and newline must be escaped
        // so each entry stays exactly one JSON line.
        target
            .write_entry("WEIRD", "a\"b\\c\nd", &ModifierKind::Constant, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(buf.contents(), "{\"name\":\"WEIRD\",\"value\":\"a\\\"b\\\\c\\nd\"}\n");
    }

    #[test]
    fn file_sink_writes_json_lines() {
        let env = crate::test::env::lock();
        env.remove("LD_LIBRARY_PATH");
        let tmp = tempfile::tempdir().unwrap();
        let export = tmp.path().join("export.env");

        let mut target = super::GitLabFlavor::new(Some(export.clone())).unwrap();
        target
            .write_entry("LD_LIBRARY_PATH", "/pkg/lib", &ModifierKind::Path, None)
            .unwrap();
        target.flush().unwrap();
        drop(target);

        let content = std::fs::read_to_string(&export).unwrap();
        assert_eq!(content, "{\"name\":\"LD_LIBRARY_PATH\",\"value\":\"/pkg/lib\"}\n");
    }

    #[test]
    fn key_with_newline_skipped() {
        // A newline-bearing key is rejected: nothing is buffered and nothing
        // is emitted (Finding 3 — GitLab key charset / CWE-77 parity).
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("FOO\nINJECTED", "value", &ModifierKind::Constant, None)
            .unwrap();
        target
            .write_entry("PATH\nINJECTED", "/pkg/bin", &ModifierKind::Path, None)
            .unwrap();
        assert!(!target.has_buffered(), "invalid keys must not be buffered");
        target.flush().unwrap();

        assert_eq!(buf.contents(), "", "no line must be emitted for invalid keys");
    }

    #[test]
    fn key_with_invalid_charset_skipped() {
        // Keys with `=` or spaces are not valid identifiers and must be
        // dropped before they corrupt the JSON-lines `name` field.
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("FOO=BAR", "value", &ModifierKind::Constant, None)
            .unwrap();
        target
            .write_entry("FOO BAR", "value", &ModifierKind::Constant, None)
            .unwrap();
        assert!(!target.has_buffered(), "invalid-charset keys must not be buffered");
        target.flush().unwrap();

        assert_eq!(buf.contents(), "", "no line must be emitted for invalid-charset keys");
    }

    #[test]
    fn drop_flushes_buffered_entries() {
        // The Drop impl flushes any buffered entries that were never explicitly
        // flushed. SharedBuf uses Arc<Mutex<Vec<u8>>>, so the clone held by
        // the test outlives the flavor and can observe the flushed output.
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let target = flavor(&buf);

        // Construct a separate scope so `write_entry` can borrow `target`
        // mutably and the borrow ends before the drop assertion below.
        {
            let mut target = target;
            target
                .write_entry("DROP_TEST", "drop-value", &ModifierKind::Constant, None)
                .unwrap();
            // Intentionally NO flush() call — drop must flush instead.
            drop(target);
        }

        // The Drop impl should have serialized the buffered constant.
        assert_eq!(
            buf.contents(),
            "{\"name\":\"DROP_TEST\",\"value\":\"drop-value\"}\n",
            "Drop must flush buffered entries without an explicit flush() call"
        );
    }

    // ── W-9: `ModifierKind::List` buffering ─────────────────────────────────

    #[test]
    fn list_export_on_an_empty_ambient() {
        let env = crate::test::env::lock();
        env.remove("JDK_JAVA_OPTIONS");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("JDK_JAVA_OPTIONS", "-ea", &ModifierKind::List, Some(" "))
            .unwrap();
        target.flush().unwrap();

        assert_eq!(buf.contents(), "{\"name\":\"JDK_JAVA_OPTIONS\",\"value\":\"-ea\"}\n");
    }

    #[test]
    fn list_duplicate_contribution_moves_to_the_back() {
        // A value already present in the ambient process env is removed from
        // its old position and re-appended at the back — re-exporting an
        // already-exported list variable must not grow it.
        let env = crate::test::env::lock();
        env.set("JDK_JAVA_OPTIONS", "-ea -Xmx1g");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("JDK_JAVA_OPTIONS", "-ea", &ModifierKind::List, Some(" "))
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            "{\"name\":\"JDK_JAVA_OPTIONS\",\"value\":\"-Xmx1g -ea\"}\n"
        );
    }

    #[test]
    fn list_entry_with_an_explicit_separator() {
        let env = crate::test::env::lock();
        env.remove("GODEBUG");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("GODEBUG", "gctrace=1", &ModifierKind::List, Some(","))
            .unwrap();
        target
            .write_entry("GODEBUG", "madvdontneed=1", &ModifierKind::List, Some(","))
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            "{\"name\":\"GODEBUG\",\"value\":\"gctrace=1,madvdontneed=1\"}\n"
        );
    }

    #[test]
    fn list_entry_with_no_separator_defaults_to_space() {
        // By the time an entry reaches `write_entry`, `reconcile_list_separators`
        // has already settled every contributor to a key — a bare `None` here
        // legitimately means "nobody declared one", which folds with `" "`.
        let env = crate::test::env::lock();
        env.remove("JDK_JAVA_OPTIONS");
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("JDK_JAVA_OPTIONS", "-ea", &ModifierKind::List, None)
            .unwrap();
        target
            .write_entry("JDK_JAVA_OPTIONS", "-Xmx1g", &ModifierKind::List, None)
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            "{\"name\":\"JDK_JAVA_OPTIONS\",\"value\":\"-ea -Xmx1g\"}\n"
        );
    }

    #[test]
    fn mixed_kind_same_key_bucket_order_not_call_order() {
        // Accepted gap (`adr_env_modifier_types.md` Consequences, extending the
        // pre-existing path/constant precedent to `list`): each kind buffers
        // into its own bucket, and `flush_inner` drains buckets in a FIXED
        // order (paths, then lists, then constants) — so when two entries for
        // the SAME key carry DIFFERENT kinds, which line lands last (and
        // therefore wins when the file is sourced) depends on bucket order,
        // not on the entries' actual call order. Here the constant is written
        // FIRST and the list SECOND, but the constant bucket flushes last, so
        // its line is emitted last anyway — the inverse of call order.
        // Documented, not fixed.
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target
            .write_entry("OPTS", "constant-value", &ModifierKind::Constant, None)
            .unwrap();
        target
            .write_entry("OPTS", "-ea", &ModifierKind::List, Some(" "))
            .unwrap();
        target.flush().unwrap();

        assert_eq!(
            buf.contents(),
            "{\"name\":\"OPTS\",\"value\":\"-ea\"}\n{\"name\":\"OPTS\",\"value\":\"constant-value\"}\n",
            "bucket order decides which line is last, regardless of call order"
        );
    }

    #[test]
    fn empty_entries_flush_writes_nothing() {
        // Calling flush() when no entries have been written must produce no
        // output and must not panic (closes coverage gap A2).
        let _env = crate::test::env::lock();
        let buf = SharedBuf::new();
        let mut target = flavor(&buf);

        target.flush().unwrap();

        assert_eq!(buf.contents(), "", "flush with no entries must produce no output");
    }

    #[test]
    fn detect_gitlab_ci() {
        let env = crate::test::env::lock();
        env.set("GITLAB_CI", "true");
        env.remove("GITHUB_ACTIONS");
        assert_eq!(CiFlavor::detect(), Some(CiFlavor::GitLab));
    }

    #[test]
    fn detect_no_gitlab() {
        let env = crate::test::env::lock();
        env.remove("GITLAB_CI");
        assert!(!super::detect());
    }
}
