// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The selector a rendered trampoline bakes must reach the
//! project it names.
//!
//! These two cases are end-to-end across a seam: [`toolchain_record`](super::toolchain_record)
//! bakes a `project_root` and `ConfigLoader::project_path` resolves it,
//! and each half is self-consistently green on its own — the record has
//! byte-exact goldens, the resolver has its own directory cases. The defect
//! the directory-selector fix addressed lived only in the join, so the test has to render the real
//! record rather than a second spelling of it. That makes the launcher the
//! subject, which is why they sit here and not with the resolver.

#![cfg(test)]

use tempfile::TempDir;

/// Helper: write a file at `path` with the given content.
fn write_file(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).expect("write test fixture");
}

/// The `project_root` out of a rendered toolchain record, read from its encoded bytes.
///
/// Parsed out of the emitted record rather than re-derived, so this reads the
/// **shipped** selector and not a second spelling of it.
fn baked_project_selector(project_root: &std::path::Path) -> String {
    let record = super::toolchain_record(
        &super::TrampolineTarget::Project(project_root.to_path_buf()),
        Some(std::path::Path::new("/opt/ocx/bin/ocx")),
        cfg!(windows),
        project_root,
    )
    .expect("an ordinary absolute root carries no launcher-unsafe character");
    let value: serde_json::Value = serde_json::from_slice(&record.encode()).expect("the record is JSON");
    value["project_root"]
        .as_str()
        .expect("a project record bakes `project_root`")
        .to_owned()
}

/// **The load-bearing one.** A rendered toolchain trampoline
/// re-enters as `ocx --project '<abs project root>' exec`, and that root is
/// a **directory**. Before the fix the explicit-project resolver refused
/// anything that was not a regular file, so *every rendered trampoline*
/// exited 74 before it ran anything.
///
/// This is the join between the two halves — the baked selector and this
/// module's resolver — and it is deliberately end-to-end across them: each
/// half is self-consistently green on its own (the record has byte-exact
/// goldens, the resolver has the directory cases above), and the defect
/// lived only in the seam.
///
/// Mutation that reds it: restoring the regular-file-only arm, or changing
/// `toolchain_record` to bake `<root>/ocx.toml` — the workaround the design
/// rejects, because it would contradict the home selector, the
/// sidecar grammar, the trampoline goldens and the committed shim blobs.
#[tokio::test]
async fn the_selector_a_trampoline_bakes_resolves_to_the_project_it_names() {
    let env = ocx_env::overrides::lock();
    env.remove(&ocx_env::OCX_PROJECT);
    env.remove(&ocx_env::OCX_NO_PROJECT);
    env.remove(&ocx_env::OCX_CEILING_PATH);

    let dir = TempDir::new().unwrap();
    // The canonical spelling: `tempfile` hands back a path under `/tmp`,
    // itself a symlink on macOS, and the renderer bakes the canonical
    // project directory.
    let project_root = dunce::canonicalize(dir.path()).expect("the scratch project canonicalises");
    write_file(&project_root.join("ocx.toml"), "");

    let baked = baked_project_selector(&project_root);
    assert_eq!(
        std::path::Path::new(&baked),
        project_root.as_path(),
        "C-028 — the record bakes the project *root*, a directory, not the manifest inside it"
    );

    let resolved = ocx_config::loader::ConfigLoader::project_path(None, Some(std::path::Path::new(&baked)))
        .await
        .expect("RUL-55 — the baked selector must reach the project, not exit 74");
    assert_eq!(
        resolved,
        Some(project_root.join("ocx.toml")),
        "RUL-55 — `--project '<abs project root>'` resolves to the `ocx.toml` inside it"
    );
}

/// The missing-home precondition, stated from the trampoline's side: a baked home
/// whose `ocx.toml` is gone — the project was deleted or moved — resolves to
/// **no project** rather than to an error.
///
/// That is what lets the caller answer `NoProjectIn` → exit 64 instead of
/// `FileNotFound` → 79. Only the directory branch can tell *this directory
/// governs no project* from *this file is missing*, which is why the directory fix and
/// the missing-home rule are one change.
#[tokio::test]
async fn a_baked_home_whose_project_moved_away_resolves_to_no_project() {
    let env = ocx_env::overrides::lock();
    env.remove(&ocx_env::OCX_PROJECT);
    env.remove(&ocx_env::OCX_NO_PROJECT);
    env.remove(&ocx_env::OCX_CEILING_PATH);

    let dir = TempDir::new().unwrap();
    let project_root = dunce::canonicalize(dir.path()).expect("the scratch project canonicalises");
    let manifest = project_root.join("ocx.toml");
    write_file(&manifest, "");

    let baked = baked_project_selector(&project_root);

    // The project moves away; the trampoline keeps its baked selector.
    std::fs::remove_file(&manifest).expect("the manifest is removable");

    let resolved = ocx_config::loader::ConfigLoader::project_path(None, Some(std::path::Path::new(&baked)))
        .await
        .expect("a directory holding no ocx.toml is not an error");
    assert_eq!(
        resolved, None,
        "C-068 — a baked home with no manifest is `no project` (exit 64), never `file not found` (79)"
    );
}
