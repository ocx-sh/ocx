// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Format-preserving rendering of a mutated [`ProjectConfig`] back to
//! `ocx.toml`: only changed keys are touched, so comments and `#:schema` survive.
//! CRLF comes back as LF and a BOM is dropped; re-encoding by hand would
//! corrupt a multi-line string containing `\n`.

use std::collections::BTreeMap;
use std::path::Path;

use toml_edit::{DocumentMut, Item, Table, TableLike, value};

use crate::Error;
use crate::config::{ProjectConfig, parse_tool_value};
use crate::error::{ProjectError, ProjectErrorKind};
use ocx_oci::PackageRef;

/// Render `candidate` as `ocx.toml` text, keeping `original`'s comments, key
/// order, spacing and table style; `path` is for error context only.
///
/// # Errors
///
/// - [`ProjectErrorKind::ManifestEditParse`] — `original` is not editable TOML.
/// - [`ProjectErrorKind::ManifestEditDiverged`] — the edit does not describe
///   `candidate`; fail-closed, never a lossy whole-file rewrite.
pub fn render_preserving(original: &str, candidate: &ProjectConfig, path: &Path) -> Result<String, Error> {
    let mut document: DocumentMut = original
        .parse()
        .map_err(|source| ProjectError::new(path.to_path_buf(), ProjectErrorKind::ManifestEditParse(source)))?;
    let header = take_header(&mut document);

    if apply(&mut document, candidate).is_none() {
        log::error!(
            "format-preserving ocx.toml edit hit a document shape it cannot express at '{}'",
            path.display()
        );
        return Err(diverged(path));
    }

    let rendered = format!("{header}{document}");
    match ProjectConfig::from_toml_str(&rendered) {
        Ok(reparsed) if reparsed == *candidate => Ok(rendered),
        Ok(_) => {
            log::error!(
                "format-preserving ocx.toml edit produced text that no longer describes the mutation at '{}'",
                path.display()
            );
            Err(diverged(path))
        }
        Err(source) => {
            log::error!("format-preserving ocx.toml edit produced text that no longer parses: {source}");
            Err(diverged(path))
        }
    }
}

fn diverged(path: &Path) -> Error {
    ProjectError::new(path.to_path_buf(), ProjectErrorKind::ManifestEditDiverged).into()
}

/// Detach a table-less document's trivia: `toml_edit` files it as trailing, so
/// a created `[tools]` would push `#:schema` off line 1.
fn take_header(document: &mut DocumentMut) -> String {
    if !document.as_table().is_empty() {
        return String::new();
    }
    let mut header = document.trailing().as_str().unwrap_or_default().to_owned();
    document.set_trailing("");
    // Without a final newline the created table glues onto the last line,
    // commenting it out.
    if !header.is_empty() && !header.ends_with('\n') {
        header.push('\n');
    }
    header
}

/// Apply `candidate`'s binding surfaces; `None` is a shape the sync cannot express.
/// `[env]`, `[package]` and `pinned` are not synced; `activate` is, or the
/// round-trip gate refuses every `set_activate` write.
fn apply(document: &mut DocumentMut, candidate: &ProjectConfig) -> Option<()> {
    let root: &mut dyn TableLike = document.as_table_mut();
    sync_activate(root, candidate.activate);
    sync_section(root, "tools", &candidate.tools)?;

    if candidate.groups.is_empty() {
        // A bare `[group]` header parses to no groups and stays; one with
        // content fails the round-trip check.
        return Some(());
    }

    // Implicit: the file carries `[group.ci.tools]`, never a bare `[group]`.
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

/// Sync the root `activate` scalar; `None` removes it, or the round-trip gate
/// refuses a mutation that unset it.
fn sync_activate(root: &mut dyn TableLike, activate: Option<crate::activate::ActivateMode>) {
    match activate {
        Some(mode) => write_binding(root, "activate", mode.to_string()),
        None => {
            root.remove("activate");
        }
    }
}

/// Sync one `tools` table, creating it only when there is something to put in it.
fn sync_section(parent: &mut dyn TableLike, key: &str, bindings: &BTreeMap<String, PackageRef>) -> Option<()> {
    if bindings.is_empty() && !parent.contains_key(key) {
        return Some(());
    }
    sync_bindings(ensure_table(parent, key, false)?, bindings);
    Some(())
}

/// Bring `table` in line with `bindings`, leaving an unchanged binding's line
/// untouched. Compared parsed, not as text: `parse_tool_value` injects
/// `:latest`, so a text compare rewrites untargeted lines and drops their comments.
fn sync_bindings(table: &mut dyn TableLike, bindings: &BTreeMap<String, PackageRef>) {
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

/// Set `key`, keeping an existing cell's decor; `Table::insert` drops comments
/// and quoting, so only a new key goes through it.
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

/// Borrow `key` as a table, creating it when absent (`implicit` applies only
/// then); `None` when `key` is not table-like.
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
    use crate::mutate::{add_binding_in_memory, remove_binding_in_memory};

    fn path() -> PathBuf {
        PathBuf::from("ocx.toml")
    }

    fn identifier(repo: &str, tag: &str) -> PackageRef {
        PackageRef::new_registry(repo, "example.com").clone_with_tag(tag)
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

    /// Parse `original`, set `activate`, and render — the `set_activate` path.
    fn render_after_activate(original: &str, mode: crate::activate::ActivateMode) -> String {
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        candidate.activate = Some(mode);
        render_preserving(original, &candidate, &path()).expect("render succeeds")
    }

    const SCHEMA: &str = "#:schema https://ocx.sh/schemas/project/v1.json";

    /// C-042: `activate` is a synced surface. Without the sync the round-trip
    /// gate refuses every `--toolchain-activate` invocation with
    /// `ManifestEditDiverged` — the guard firing on a mutation this module had
    /// no way to express.
    #[test]
    fn the_activate_key_round_trips_through_the_gate() {
        use crate::activate::ActivateMode;

        let rendered = render_after_activate("[tools]\ncmake = \"example.com/cmake:3.28\"\n", ActivateMode::Bin);
        assert!(rendered.contains("activate = \"bin\""), "rendered: {rendered}");
        assert_eq!(
            ProjectConfig::from_toml_str(&rendered).expect("re-parses").activate,
            Some(ActivateMode::Bin)
        );
    }

    /// A top-level scalar must lead the file: TOML renders a table's own
    /// key/value pairs before its sub-tables, so no hoisting is needed — but a
    /// regression here would produce a file that parses and reads wrongly, so
    /// the ordering is pinned rather than assumed.
    #[test]
    fn the_activate_key_renders_above_the_first_table() {
        use crate::activate::ActivateMode;

        let rendered = render_after_activate("[tools]\ncmake = \"example.com/cmake:3.28\"\n", ActivateMode::Env);
        let activate = rendered.find("activate").expect("the key is present");
        let table = rendered.find("[tools]").expect("the table is present");
        assert!(activate < table, "activate must precede [tools]: {rendered}");
    }

    /// C-042's create-when-absent case, at the render layer: an empty document
    /// plus one scalar is a file carrying only that key — no `[tools]` table
    /// and no template.
    #[test]
    fn an_empty_document_gains_only_the_activate_key() {
        use crate::activate::ActivateMode;

        let rendered = render_after_activate("", ActivateMode::None);
        assert_eq!(rendered, "activate = \"none\"\n");
    }

    /// A re-set changes the value and nothing else — the comment above the key
    /// and the one trailing it both survive, because the sync goes through
    /// `write_binding` rather than `Table::insert`.
    #[test]
    fn re_setting_activate_keeps_the_decor_around_it() {
        use crate::activate::ActivateMode;

        let original = "# how the toolchain reaches PATH\nactivate = \"env\"  # for now\n\n[tools]\n";
        let rendered = render_after_activate(original, ActivateMode::Bin);
        assert!(
            rendered.contains("# how the toolchain reaches PATH\nactivate = \"bin\"  # for now"),
            "rendered: {rendered}"
        );
    }

    /// The typed model's *absence* must be expressible too, or the gate would
    /// refuse a mutation that unset the key.
    #[test]
    fn clearing_activate_removes_the_key() {
        let original = "activate = \"bin\"\n\n[tools]\n";
        let mut candidate = ProjectConfig::from_toml_str(original).expect("fixture parses");
        candidate.activate = None;
        let rendered = render_preserving(original, &candidate, &path()).expect("render succeeds");
        assert!(!rendered.contains("activate"), "rendered: {rendered}");
    }

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
