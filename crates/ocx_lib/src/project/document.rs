// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Format-preserving rendering of a mutated [`ProjectConfig`] back to
//! `ocx.toml`.
//!
//! `ocx add` / `ocx remove` mutate a typed config, but the file on disk is a
//! document a person wrote: comments (the `#:schema` directive `ocx init` puts
//! on line 1 among them), declaration order, spacing. Serializing the struct
//! reproduces none of that — serde has no notion of a comment — so a mutation
//! used to hand the user back a normalised file with their content stripped
//! (issue #256).
//!
//! This module applies the mutation to the *document* instead: the edit touches
//! the keys that actually changed and nothing else. What the typed model does
//! not describe is never rewritten, and a key whose value is unchanged is not
//! even re-inserted, so its decor survives verbatim.
//!
//! Two things do not survive, both of them `toml_edit` round-trip
//! normalisations: CRLF line endings come back as LF, and a leading UTF-8 byte
//! order mark (PowerShell 5.1 writes one) is dropped. That is what the
//! whole-file serializer did too, so both are the status quo rather than a
//! regression, and re-encoding the output by hand would corrupt a multi-line
//! string that legitimately contains `\n`.

use std::collections::BTreeMap;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, TableLike, value};

use crate::oci::Identifier;
use crate::project::Error;
use crate::project::config::{ProjectConfig, parse_tool_value};
use crate::project::error::{ProjectError, ProjectErrorKind};

/// Render `candidate` as `ocx.toml` text, preserving everything in `original`
/// that the typed model does not own: comments, key order, spacing, and table
/// style.
///
/// `path` is used solely for error context; nothing is read from or written to
/// it.
///
/// # Errors
///
/// - [`ProjectErrorKind::ManifestEditParse`] — `original` does not parse as an
///   editable TOML document.
/// - [`ProjectErrorKind::ManifestEditDiverged`] — the edited document does not
///   describe `candidate`. Fail-closed: a mutation this module cannot express
///   (a surface it does not sync, an unexpected document shape) aborts the write
///   rather than falling back to a lossy whole-file rewrite.
pub fn render_preserving(original: &str, candidate: &ProjectConfig, path: &Path) -> Result<String, Error> {
    let mut document: DocumentMut = original
        .parse()
        .map_err(|source| ProjectError::new(path.to_path_buf(), ProjectErrorKind::ManifestEditParse(source)))?;
    let header = take_header(&mut document);

    if apply(&mut document, candidate).is_none() {
        crate::log::error!(
            "format-preserving ocx.toml edit hit a document shape it cannot express at '{}'",
            path.display()
        );
        return Err(diverged(path));
    }

    let rendered = format!("{header}{document}");
    match ProjectConfig::from_toml_str(&rendered) {
        Ok(reparsed) if reparsed == *candidate => Ok(rendered),
        Ok(_) => {
            crate::log::error!(
                "format-preserving ocx.toml edit produced text that no longer describes the mutation at '{}'",
                path.display()
            );
            Err(diverged(path))
        }
        Err(source) => {
            crate::log::error!("format-preserving ocx.toml edit produced text that no longer parses: {source}");
            Err(diverged(path))
        }
    }
}

fn diverged(path: &Path) -> Error {
    ProjectError::new(path.to_path_buf(), ProjectErrorKind::ManifestEditDiverged).into()
}

/// Detach the leading trivia of a document that declares no table, so the
/// caller can re-emit it above whatever the mutation creates.
///
/// `toml_edit` files every comment of a table-less `ocx.toml` — a hand-written
/// minimal file, or one whose `[tools]` line was deleted — under the
/// *document's* trailing trivia, there being no item to anchor it to. A
/// `[tools]` (or `[group.<name>.tools]`) table created by the mutation then
/// renders above it and pushes the `#:schema` directive `ocx init` puts on line
/// 1 down the file. Once the document does declare a table, its comments belong
/// to that table's decor and the trailing is a genuine end-of-file trailer —
/// left alone.
fn take_header(document: &mut DocumentMut) -> String {
    if !document.as_table().is_empty() {
        return String::new();
    }
    let mut header = document.trailing().as_str().unwrap_or_default().to_owned();
    document.set_trailing("");
    // A file whose last line carries no newline — a hand-written `ocx.toml`,
    // a heredoc — would otherwise have the created table glued onto that
    // line, commenting it out. The round-trip check catches the result, so
    // the cost of omitting this is a refused `ocx add`, not a corrupt file.
    if !header.is_empty() && !header.ends_with('\n') {
        header.push('\n');
    }
    header
}

/// Apply `candidate`'s binding surfaces to `document`.
///
/// `None` signals a document shape the sync cannot express — the caller turns
/// that into [`ProjectErrorKind::ManifestEditDiverged`]. The `[env]` and
/// `[package]` surfaces are deliberately not synced: no mutator touches them,
/// and the round-trip check in [`render_preserving`] is what catches it if one
/// ever starts.
fn apply(document: &mut DocumentMut, candidate: &ProjectConfig) -> Option<()> {
    let root: &mut dyn TableLike = document.as_table_mut();
    sync_section(root, "tools", &candidate.tools)?;

    if candidate.groups.is_empty() {
        // Nothing to write. A bare `[group]` header already in the file is
        // left exactly as the user wrote it — it parses to no groups, so the
        // candidate still describes it. One that holds content does not: the
        // round-trip check in `render_preserving` sees the groups the
        // candidate dropped and fails the write.
        return Some(());
    }

    // `group` is an implicit super-table: `[group.ci.tools]` is the header the
    // file carries, never a bare `[group]`.
    let groups = ensure_table(root, "group", true)?;
    let stale: Vec<String> = groups
        .iter()
        .map(|(name, _)| name.to_owned())
        .filter(|name| !candidate.groups.contains_key(name))
        .collect();
    for name in stale {
        groups.remove(&name);
    }
    for (name, group) in &candidate.groups {
        let group_table = ensure_table(groups, name, true)?;
        sync_section(group_table, "tools", &group.tools)?;
    }
    Some(())
}

/// Sync one `tools` table under `parent`, creating it only when there is
/// something to put in it.
fn sync_section(parent: &mut dyn TableLike, key: &str, bindings: &BTreeMap<String, Identifier>) -> Option<()> {
    if bindings.is_empty() && !parent.contains_key(key) {
        return Some(());
    }
    sync_bindings(ensure_table(parent, key, false)?, bindings);
    Some(())
}

/// Bring `table` in line with `bindings`: drop what the candidate no longer
/// declares, append what it gained, and leave an unchanged binding untouched so
/// its spacing and trailing comment survive.
///
/// "Unchanged" is decided by parsing the cell, not by comparing its text
/// against the rendered identifier: [`parse_tool_value`] injects `:latest` into
/// a bare `registry/repo`, so `cmake = "ocx.sh/cmake"` renders as
/// `ocx.sh/cmake:latest` and a text comparison would rewrite a line the
/// mutation never targeted, taking its comments and key quoting with it.
fn sync_bindings(table: &mut dyn TableLike, bindings: &BTreeMap<String, Identifier>) {
    let stale: Vec<String> = table
        .iter()
        .map(|(key, _)| key.to_owned())
        .filter(|key| !bindings.contains_key(key))
        .collect();
    for key in stale {
        table.remove(&key);
    }

    for (key, identifier) in bindings {
        let declared = table
            .get(key)
            .and_then(Item::as_str)
            .and_then(|declared| parse_tool_value(declared).ok());
        if declared.as_ref() == Some(identifier) {
            continue;
        }
        write_binding(table, key, identifier.to_string());
    }
}

/// Set `key` to `rendered`, keeping the decor of a cell that already exists.
///
/// `Table::insert` reformats the key (dropping its leading comment block and
/// its quoting) and replaces the value wholesale (dropping its trailing
/// comment), so it is reserved for a key the file does not carry yet.
fn write_binding(table: &mut dyn TableLike, key: &str, rendered: String) {
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = rendered.into();
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(key, value(rendered));
        }
    }
}

/// Borrow `key` from `parent` as a table, creating an empty one when absent.
///
/// `implicit` applies to a freshly created table only — an existing table keeps
/// whatever the user wrote. `None` when `key` holds something that is not
/// table-like.
fn ensure_table<'a>(parent: &'a mut dyn TableLike, key: &str, implicit: bool) -> Option<&'a mut dyn TableLike> {
    if !parent.contains_key(key) {
        let mut created = Table::new();
        created.set_implicit(implicit);
        parent.insert(key, Item::Table(created));
    }
    parent.get_mut(key)?.as_table_like_mut()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::project::mutate::{add_binding_in_memory, remove_binding_in_memory};

    fn path() -> PathBuf {
        PathBuf::from("ocx.toml")
    }

    fn identifier(repo: &str, tag: &str) -> Identifier {
        Identifier::new_registry(repo, "example.com").clone_with_tag(tag)
    }

    /// Parse `original`, apply an `add` of `repo:tag` into `group`, and render.
    fn render_after_add(original: &str, repo: &str, tag: &str, group: Option<&str>) -> String {
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        add_binding_in_memory(&mut candidate, &path(), &identifier(repo, tag), None, group).expect("add applies");
        render_preserving(original, &candidate, &path()).expect("render succeeds")
    }

    /// Parse `original`, apply a `remove` of `repo`, and render.
    fn render_after_remove(original: &str, repo: &str) -> String {
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        remove_binding_in_memory(&mut candidate, &path(), repo, None).expect("remove applies");
        render_preserving(original, &candidate, &path()).expect("render succeeds")
    }

    const SCHEMA: &str = "#:schema https://ocx.sh/schemas/project/v1.json";

    #[test]
    fn add_preserves_every_comment() {
        let original = format!(
            "{SCHEMA}\n\
             # toolchain notes\n\
             \n\
             [tools]\n\
             # pinned deliberately\n\
             cmake = \"example.com/cmake:3.28\"  # trailing note\n"
        );

        let rendered = render_after_add(&original, "shellcheck", "0.11", None);

        assert!(
            rendered.starts_with(SCHEMA),
            "schema directive must stay on line 1: {rendered}"
        );
        for fragment in ["# toolchain notes", "# pinned deliberately", "# trailing note"] {
            assert!(rendered.contains(fragment), "lost {fragment:?}: {rendered}");
        }
        assert!(rendered.contains("shellcheck = \"example.com/shellcheck:0.11\""));
    }

    #[test]
    fn add_appends_and_keeps_declaration_order() {
        let original = "[tools]\n\
                        zeta = \"example.com/zeta:1\"\n\
                        alpha = \"example.com/alpha:1\"\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        let zeta = rendered.find("zeta").expect("zeta present");
        let alpha = rendered.find("alpha").expect("alpha present");
        let added = rendered.find("shellcheck").expect("shellcheck present");
        assert!(zeta < alpha, "user order must survive: {rendered}");
        assert!(added > alpha, "a new binding is appended, not sorted in: {rendered}");
    }

    #[test]
    fn add_leaves_untouched_binding_byte_identical() {
        let original = "[tools]\ncmake    =    \"example.com/cmake:3.28\"\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            rendered.contains("cmake    =    \"example.com/cmake:3.28\""),
            "an unchanged binding keeps its own spacing: {rendered}"
        );
    }

    #[test]
    fn add_emits_no_empty_group_or_package_tables() {
        let rendered = render_after_add("[tools]\n", "cmake", "3.28", None);

        assert!(!rendered.contains("[group]"), "no bare [group]: {rendered}");
        assert!(!rendered.contains("[package]"), "no bare [package]: {rendered}");
    }

    #[test]
    fn add_to_new_group_emits_only_the_group_tools_header() {
        let original = format!("{SCHEMA}\n# keep me\n\n[tools]\n");

        let rendered = render_after_add(&original, "cmake", "3.28", Some("ci"));

        assert!(rendered.contains("[group.ci.tools]"), "group tools header: {rendered}");
        assert!(!rendered.contains("[group]\n"), "no bare [group] header: {rendered}");
        assert!(
            rendered.contains("# keep me"),
            "comments survive a group add: {rendered}"
        );
        assert!(rendered.starts_with(SCHEMA), "schema directive stays first: {rendered}");
    }

    #[test]
    fn remove_drops_one_key_and_keeps_the_rest_verbatim() {
        let original = format!(
            "{SCHEMA}\n\
             \n\
             [tools]\n\
             cmake = \"example.com/cmake:3.28\"  # keep this\n\
             shellcheck = \"example.com/shellcheck:0.11\"\n"
        );

        let rendered = render_after_remove(&original, "shellcheck");

        assert!(!rendered.contains("shellcheck"), "binding removed: {rendered}");
        assert!(rendered.contains("cmake = \"example.com/cmake:3.28\"  # keep this"));
        assert!(rendered.starts_with(SCHEMA));
    }

    #[test]
    fn remove_of_last_group_binding_keeps_the_group_table() {
        let original = "[tools]\n\n[group.ci.tools]\ncmake = \"example.com/cmake:3.28\"\n";

        let rendered = render_after_remove(original, "cmake");

        assert!(rendered.contains("[group.ci.tools]"), "group table stays: {rendered}");
        assert!(!rendered.contains("cmake"), "binding removed: {rendered}");
    }

    #[test]
    fn add_then_remove_round_trips_byte_identical() {
        let original = format!("{SCHEMA}\n# fixture\n\n[tools]\ncmake = \"example.com/cmake:3.28\"\n");

        let after_add = render_after_add(&original, "shellcheck", "0.11", None);
        let after_remove = render_after_remove(&after_add, "shellcheck");

        assert_eq!(
            after_remove, original,
            "add then remove must restore the original bytes"
        );
    }

    #[test]
    fn add_preserves_env_and_package_sections() {
        let tail = "[env]\n\
                    PROJECT_FLAG = \"1\"  # project-wide\n\
                    \n\
                    [group.ci.env]\n\
                    CI_FLAG = \"yes\"\n\
                    \n\
                    [package.\"example.com/cmake\"]\n\
                    no-patches = true\n";
        let original = format!("[tools]\ncmake = \"example.com/cmake:3.28\"\n\n{tail}");

        let rendered = render_after_add(&original, "shellcheck", "0.11", None);

        assert!(
            rendered.contains(tail),
            "sections the mutation does not target come back verbatim: {rendered}"
        );
    }

    #[test]
    fn indentation_survives_and_crlf_normalises() {
        let original = "[tools]\r\n\tcmake = \"example.com/cmake:3.28\"\r\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            rendered.contains("\tcmake = \"example.com/cmake:3.28\""),
            "indentation is the user's: {rendered:?}"
        );
        assert!(
            !rendered.contains('\r'),
            "toml_edit normalises line endings to LF — pinned so the day it stops is visible: {rendered:?}"
        );
    }

    #[test]
    fn a_bare_binding_is_not_rewritten_by_the_latest_default() {
        // `ocx.toml` may leave a binding unpinned; the schema boundary injects
        // `:latest` into the typed model, but the file said no such thing. A
        // sync that compares rendered text against the file text calls that a
        // change and rewrites the line, taking its comments with it.
        let original = "[tools]\n\
                        # bare repo, deliberately unpinned\n\
                        bun    =    \"example.com/bun\"  # no tag on purpose\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            rendered
                .contains("# bare repo, deliberately unpinned\nbun    =    \"example.com/bun\"  # no tag on purpose"),
            "an unpinned binding is unchanged, comments and spacing included: {rendered}"
        );
    }

    #[test]
    fn a_bare_binding_under_a_quoted_key_keeps_its_quoting() {
        let original = "[tools]\n\"go-task\" = \"example.com/go-task\"\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            rendered.contains("\"go-task\" = \"example.com/go-task\"\n"),
            "a quoted key over an unpinned value keeps both spellings: {rendered}"
        );
    }

    #[test]
    fn a_changed_binding_keeps_its_comments_and_spacing() {
        // No mutator rewrites a binding's value today — `add` refuses an
        // existing key — but [`apply`] promises to express one, so the
        // in-place path is exercised directly.
        let original = "[tools]\n\
                        # pinned deliberately\n\
                        cmake    =    \"example.com/cmake:3.28\"  # trailing note\n";
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        candidate.tools.insert("cmake".to_owned(), identifier("cmake", "3.29"));

        let rendered = render_preserving(original, &candidate, &path()).expect("render succeeds");

        assert!(
            rendered.contains("# pinned deliberately\ncmake    =    \"example.com/cmake:3.29\"  # trailing note"),
            "only the value changes; its decor is the user's: {rendered}"
        );
    }

    #[test]
    fn schema_directive_survives_a_file_with_no_tools_table() {
        // Comments in a table-less file are the document's *trailing* trivia,
        // so a created `[tools]` table renders above them unless the trivia is
        // re-emitted first.
        let original = format!("{SCHEMA}\n# my toolchain\n");

        let rendered = render_after_add(&original, "cmake", "3.28", None);

        assert!(
            rendered.starts_with(SCHEMA),
            "schema directive must stay on line 1: {rendered}"
        );
        assert!(rendered.contains("# my toolchain"), "comment survives: {rendered}");
        assert!(
            rendered.contains("[tools]"),
            "binding lands in a tools table: {rendered}"
        );
    }

    #[test]
    fn a_table_less_file_without_a_final_newline_still_gets_its_table() {
        // The header is re-emitted verbatim ahead of the body, so a last line
        // carrying no newline would glue `[tools]` onto the comment and
        // comment the table out — caught by the round-trip check, but only
        // after refusing an `ocx add` that has nothing wrong with it.
        let original = format!("{SCHEMA}\n# no newline at end of file");

        let rendered = render_after_add(&original, "cmake", "3.28", None);

        assert!(
            rendered.starts_with(SCHEMA),
            "schema directive must stay on line 1: {rendered}"
        );
        assert!(
            rendered.contains("\n[tools]\n"),
            "the created table must start its own line, not continue the comment: {rendered}"
        );
        assert!(
            rendered.contains("# no newline at end of file"),
            "the comment survives intact: {rendered}"
        );
    }

    #[test]
    fn schema_directive_survives_a_group_add_to_a_table_less_file() {
        let original = format!("{SCHEMA}\n# my toolchain\n");

        let rendered = render_after_add(&original, "cmake", "3.28", Some("ci"));

        assert!(
            rendered.starts_with(SCHEMA),
            "schema directive must stay on line 1: {rendered}"
        );
        assert!(rendered.contains("[group.ci.tools]"), "group tools header: {rendered}");
    }

    #[test]
    fn a_byte_order_mark_is_dropped() {
        // PowerShell 5.1 writes UTF-8 with a BOM. `toml_edit` accepts it and
        // drops it on the round trip, exactly as the whole-file serializer did
        // — pinned so the day it changes is visible.
        let original = "\u{feff}[tools]\ncmake = \"example.com/cmake:3.28\"\n";
        assert!(original.starts_with('\u{feff}'), "the fixture is the BOM");

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            !rendered.starts_with('\u{feff}'),
            "toml_edit strips a leading BOM: {rendered:?}"
        );
        assert!(rendered.starts_with("[tools]"), "the rest is untouched: {rendered:?}");
    }

    #[test]
    fn quoted_key_survives() {
        let original = "[tools]\n\"go-task\" = \"example.com/go-task:3\"\n";

        let rendered = render_after_add(original, "shellcheck", "0.11", None);

        assert!(
            rendered.contains("\"go-task\" = \"example.com/go-task:3\""),
            "a quoted key keeps its quoting: {rendered}"
        );
    }

    #[test]
    fn a_candidate_the_sync_cannot_express_fails_closed() {
        // `env` is not a synced surface. A mutator that changed it would
        // otherwise have its change silently dropped — the exact shape of the
        // bug this module exists to fix.
        let original = "[tools]\ncmake = \"example.com/cmake:3.28\"\n\n[env]\nFLAG = \"1\"\n";
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        candidate.env = ProjectConfig::from_toml_str("[env]\nFLAG = \"2\"\n")
            .expect("fixture parses")
            .env;

        let error = render_preserving(original, &candidate, &path()).expect_err("must fail closed");

        assert!(
            matches!(
                error,
                Error::Project(ProjectError {
                    kind: ProjectErrorKind::ManifestEditDiverged,
                    ..
                })
            ),
            "expected ManifestEditDiverged, got {error:?}"
        );
    }

    #[test]
    fn unparsable_original_surfaces_the_parse_error() {
        let candidate = ProjectConfig::from_toml_str("[tools]\n").expect("fixture parses");

        let error = render_preserving("[tools\n", &candidate, &path()).expect_err("must fail");

        assert!(
            matches!(
                error,
                Error::Project(ProjectError {
                    kind: ProjectErrorKind::ManifestEditParse(_),
                    ..
                })
            ),
            "expected ManifestEditParse, got {error:?}"
        );
    }
}
