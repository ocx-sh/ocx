// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! RUL-55 / C-068 — the selector a rendered trampoline bakes must reach the
//! project it names.
//!
//! These two cases are end-to-end across a seam: [`unix_trampoline_body`](super::unix_trampoline_body)
//! bakes a `--project` selector and `ConfigLoader::project_path` resolves it,
//! and each half is self-consistently green on its own — the body has
//! byte-exact goldens, the resolver has its own directory cases. The defect
//! RUL-55 fixed lived only in the join, so the test has to render the real
//! body rather than a second spelling of it. That makes the launcher the
//! subject, which is why they sit here and not with the resolver.

#![cfg(test)]

use tempfile::TempDir;

/// Helper: write a file at `path` with the given content.
fn write_file(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).expect("write test fixture");
}

/// The `--project` value out of a POSIX trampoline body, between the single
/// quotes `unix_trampoline_body` puts it in.
///
/// Parsed out of the emitted text rather than re-derived, so this reads the
/// **shipped** selector and not a second spelling of it.
fn baked_project_selector(body: &str) -> String {
    let after = body
        .split_once("--project '")
        .expect("a project trampoline bakes `--project '<root>'`")
        .1;
    after
        .split_once('\'')
        .expect("the baked selector is single-quoted")
        .0
        .to_owned()
}

/// **RUL-55, the load-bearing one.** A rendered toolchain trampoline
/// re-enters as `ocx --project '<abs project root>' exec`, and that root is
/// a **directory**. Before RUL-55 the explicit-project resolver refused
/// anything that was not a regular file, so *every rendered trampoline*
/// exited 74 before it ran anything.
///
/// This is the join between the two halves — WP-6's baked selector and this
/// module's resolver — and it is deliberately end-to-end across them: each
/// half is self-consistently green on its own (the body has byte-exact
/// goldens, the resolver has the directory cases above), and the defect
/// lived only in the seam.
///
/// Mutation that reds it: restoring the regular-file-only arm, or changing
/// `unix_trampoline_body` to bake `<root>/ocx.toml` — the workaround D-V33
/// rejects, because it would contradict C-028's home selector, C-030/C-031's
/// sidecar grammar, WP-6's goldens and the committed shim blobs.
#[tokio::test]
async fn the_selector_a_trampoline_bakes_resolves_to_the_project_it_names() {
    let env = ocx_util::env::overrides::lock();
    env.remove("OCX_PROJECT");
    env.remove("OCX_NO_PROJECT");
    env.remove("OCX_CEILING_PATH");

    let dir = TempDir::new().unwrap();
    // The canonical spelling: `tempfile` hands back a path under `/tmp`,
    // itself a symlink on macOS, and the renderer bakes the canonical
    // project directory.
    let project_root = dunce::canonicalize(dir.path()).expect("the scratch project canonicalises");
    write_file(&project_root.join("ocx.toml"), "");

    let body = super::unix_trampoline_body(
        &super::TrampolineTarget::Project(project_root.clone()),
        Some(std::path::Path::new("/opt/ocx/bin/ocx")),
    )
    .expect("an ordinary absolute root carries no launcher-unsafe character");

    let baked = baked_project_selector(&body);
    assert_eq!(
        std::path::Path::new(&baked),
        project_root.as_path(),
        "C-028 — the body bakes the project *root*, a directory, not the manifest inside it"
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

/// C-068's precondition, stated from the trampoline's side: a baked home
/// whose `ocx.toml` is gone — the project was deleted or moved — resolves to
/// **no project** rather than to an error.
///
/// That is what lets the caller answer `NoProjectIn` → exit 64 instead of
/// `FileNotFound` → 79. Only the directory branch can tell *this directory
/// governs no project* from *this file is missing*, which is why RUL-55 and
/// C-068 are one change.
#[tokio::test]
async fn a_baked_home_whose_project_moved_away_resolves_to_no_project() {
    let env = ocx_util::env::overrides::lock();
    env.remove("OCX_PROJECT");
    env.remove("OCX_NO_PROJECT");
    env.remove("OCX_CEILING_PATH");

    let dir = TempDir::new().unwrap();
    let project_root = dunce::canonicalize(dir.path()).expect("the scratch project canonicalises");
    let manifest = project_root.join("ocx.toml");
    write_file(&manifest, "");

    let body = super::unix_trampoline_body(
        &super::TrampolineTarget::Project(project_root.clone()),
        Some(std::path::Path::new("/opt/ocx/bin/ocx")),
    )
    .expect("an ordinary absolute root carries no launcher-unsafe character");
    let baked = baked_project_selector(&body);

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
