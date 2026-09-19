// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Structural guards over the whole workspace (plan C-009, C-010, C-046,
//! C-053, C-078) — properties Cargo accepts silently and no behavioural test
//! can observe: a dependency edge the crate map forbids, a `.rs` file no `mod`
//! names, a `__testing` seam the release build could still select, a
//! classification impl in a library crate.
//!
//! Every guard asserts a non-empty walk: a scope glob that stops matching
//! must red, not pass. A guard whose phase has not landed is written
//! `#[ignore = "lands with WP-nn"]` and un-ignored in that WP — never deleted,
//! never silently green (`cargo nextest run --run-ignored all` shows them red).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use ocx_test_support::boundary::{
    Reach, Source, Token, assert_no_imports, assert_no_needles, assert_scan, expand_use_tree, flatten, is_cfg_test,
    needles_in, reaches_in, rust_sources,
};
use ocx_test_support::syn::visit::Visit;
use ocx_test_support::{proc_macro2, syn};

fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    dunce::canonicalize(&root).unwrap_or_else(|error| panic!("canonicalize {}: {error}", root.display()))
}

fn crates_dir() -> PathBuf {
    workspace_root().join("crates")
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/boundaries")
        .join(name)
}

/// Every `crates/*/` directory holding a `Cargo.toml`, as `(package name, dir)`.
fn crate_dirs() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(crates_dir()).expect("crates/ is readable").flatten() {
        let dir = entry.path();
        let manifest = dir.join("Cargo.toml");
        if !manifest.is_file() {
            continue;
        }
        let parsed = read_manifest(&manifest);
        let name = parsed["package"]["name"].as_str().expect("[package].name").to_owned();
        out.push((name, dir));
    }
    out.sort();
    assert!(out.len() > 1, "no crate directories under {}", crates_dir().display());
    out
}

fn read_manifest(path: &Path) -> toml::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    toml::from_str(&text).unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

/// `cargo metadata --format-version 1 --locked <extra>` from the workspace
/// root, as JSON. Fails loudly — a checker fed an empty graph would report
/// "no edges" and pass.
fn cargo_metadata(extra: &[&str]) -> serde_json::Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked"])
        .args(extra)
        .current_dir(workspace_root())
        .output()
        .expect("spawn cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata {extra:?} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).expect("cargo metadata emits JSON");
    assert!(
        metadata["workspace_members"]
            .as_array()
            .is_some_and(|members| !members.is_empty()),
        "cargo metadata reported no workspace members"
    );
    metadata
}

/// Workspace member names, from `cargo metadata`.
fn member_names(metadata: &serde_json::Value) -> BTreeSet<String> {
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .filter(|p| members.contains(p["id"].as_str().unwrap_or("")))
        .map(|p| p["name"].as_str().expect("package name").to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// The crate map (C-046)
// ---------------------------------------------------------------------------

/// The parsed `scripts/crate_map.toml`.
struct CrateMap {
    allowed: BTreeMap<String, BTreeSet<String>>,
    dev_everywhere: BTreeSet<String>,
    testing_feature: String,
}

impl CrateMap {
    fn load() -> Self {
        let path = workspace_root().join("scripts/crate_map.toml");
        let value = read_manifest(&path);
        let allowed = value["allowed"]
            .as_table()
            .expect("[allowed] table")
            .iter()
            .map(|(from, targets)| {
                let set = targets
                    .as_array()
                    .unwrap_or_else(|| panic!("[allowed].{from} is not an array"))
                    .iter()
                    .map(|t| t.as_str().expect("crate name").to_owned())
                    .collect();
                (from.clone(), set)
            })
            .collect();
        let dev_everywhere = value["dev"]["allowed_everywhere"]
            .as_array()
            .expect("[dev].allowed_everywhere")
            .iter()
            .map(|t| t.as_str().expect("crate name").to_owned())
            .collect();
        let testing_feature = value["dev"]["testing_feature"]
            .as_str()
            .expect("[dev].testing_feature")
            .to_owned();
        Self {
            allowed,
            dev_everywhere,
            testing_feature,
        }
    }
}

/// The deliberate second copy of `scripts/crate_map.toml` `[allowed]` (C-046).
///
/// Its only job is to make a TOML edit require a Rust edit in the same commit:
/// `deps_direction` and `scripts/edge_inventory.py` both read the TOML, so a
/// loosened edge there would green both with no red anywhere — this table is
/// what reds. Source: ADR § "Architecture — the crate map" as corrected by
/// D-037 (the 17 rows); the transition rows sit in [`ADR_TRANSITION_ROWS`].
/// Order within a row is irrelevant (compared as sets); the row set and every
/// set are compared in both directions.
const ADR_MAP: &[(&str, &[&str])] = &[
    ("ocx_exit", &[]),
    ("ocx_util", &[]),
    ("ocx_console", &["ocx_exit", "ocx_util"]),
    ("ocx_oci", &["ocx_util", "ocx_console", "ocx_exit"]),
    ("ocx_trust", &["ocx_oci", "ocx_util"]),
    ("ocx_sign", &["ocx_trust", "ocx_oci", "ocx_util", "ocx_exit"]),
    ("ocx_config", &["ocx_trust", "ocx_oci", "ocx_util", "ocx_exit"]),
    ("ocx_store", &["ocx_config", "ocx_oci", "ocx_util", "ocx_exit"]),
    (
        "ocx_index",
        &["ocx_store", "ocx_config", "ocx_oci", "ocx_util", "ocx_exit"],
    ),
    (
        "ocx_package",
        &[
            "ocx_index",
            "ocx_store",
            "ocx_config",
            "ocx_oci",
            "ocx_util",
            "ocx_exit",
        ],
    ),
    (
        "ocx_shell",
        &[
            "ocx_package",
            "ocx_config",
            "ocx_store",
            "ocx_oci",
            "ocx_util",
            "ocx_console",
            "ocx_exit",
        ],
    ),
    (
        "ocx_project",
        &[
            "ocx_shell",
            "ocx_package",
            "ocx_index",
            "ocx_store",
            "ocx_config",
            "ocx_trust",
            "ocx_oci",
            "ocx_util",
            "ocx_exit",
        ],
    ),
    // D-037: += ocx_trust, += ocx_shell.
    (
        "ocx_package_manager",
        &[
            "ocx_project",
            "ocx_package",
            "ocx_index",
            "ocx_sign",
            "ocx_store",
            "ocx_config",
            "ocx_oci",
            "ocx_util",
            "ocx_console",
            "ocx_exit",
            "ocx_trust",
            "ocx_shell",
        ],
    ),
    (
        "ocx_announce",
        &[
            "ocx_index",
            "ocx_package",
            "ocx_config",
            "ocx_oci",
            "ocx_util",
            "ocx_exit",
        ],
    ),
    (
        "ocx_script",
        &["ocx_store", "ocx_config", "ocx_oci", "ocx_util", "ocx_console"],
    ),
    // D-037: += ocx_index, += ocx_package.
    (
        "ocx_setup",
        &[
            "ocx_package_manager",
            "ocx_shell",
            "ocx_config",
            "ocx_store",
            "ocx_oci",
            "ocx_util",
            "ocx_exit",
            "ocx_index",
            "ocx_package",
        ],
    ),
    ("ocx_test_support", &[]),
];

/// The rows of the transition, beside the 17: the three packages that are not
/// extraction targets. These have no README of the shell shape. `ocx_lib`'s
/// row left with the crate at WP-37.
const ADR_TRANSITION_ROWS: &[(&str, &[&str])] = &[
    (
        "ocx",
        &[
            "ocx_exit",
            "ocx_util",
            "ocx_console",
            "ocx_oci",
            "ocx_trust",
            "ocx_sign",
            "ocx_config",
            "ocx_store",
            "ocx_index",
            "ocx_package",
            "ocx_shell",
            "ocx_project",
            "ocx_package_manager",
            "ocx_announce",
            "ocx_script",
            "ocx_setup",
        ],
    ),
    (
        "ocx_schema",
        &[
            "ocx",
            "ocx_oci",
            "ocx_config",
            "ocx_package",
            "ocx_project",
            "ocx_package_manager",
        ],
    ),
    ("ocx_shim", &[]),
];

/// The `[dev]` half of the same copy.
const ADR_DEV_EVERYWHERE: &[&str] = &["ocx_test_support"];
const ADR_TESTING_FEATURE: &str = "__testing";

#[test]
fn crate_map_toml_matches_rust_table() {
    let map = CrateMap::load();
    let rust_rows = || ADR_MAP.iter().chain(ADR_TRANSITION_ROWS);
    let rows: BTreeSet<&str> = map.allowed.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = rust_rows().map(|(from, _)| *from).collect();
    assert_eq!(
        rows, expected,
        "scripts/crate_map.toml [allowed] rows differ from ADR_MAP + ADR_TRANSITION_ROWS"
    );
    // Each row's allowed set, both directions: an edge the TOML has and the
    // Rust table lacks reds here, and so does the reverse.
    for (from, rust_targets) in rust_rows() {
        let rust: BTreeSet<&str> = rust_targets.iter().copied().collect();
        let toml: BTreeSet<&str> = map.allowed[*from].iter().map(String::as_str).collect();
        let only_toml: Vec<&&str> = toml.difference(&rust).collect();
        let only_rust: Vec<&&str> = rust.difference(&toml).collect();
        assert!(
            only_toml.is_empty() && only_rust.is_empty(),
            "[allowed].{from}: scripts/crate_map.toml and ADR_MAP disagree — only in the TOML: {only_toml:?}, \
             only in the Rust table: {only_rust:?}. A crate-map edit is a Rust edit in the same commit (C-046)."
        );
    }
    for (from, targets) in &map.allowed {
        for to in targets {
            assert!(
                map.allowed.contains_key(to),
                "[allowed].{from} names `{to}`, which has no row of its own"
            );
            assert_ne!(from, to, "[allowed].{from} lists itself");
        }
    }
    let dev_expected: BTreeSet<String> = ADR_DEV_EVERYWHERE.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        map.dev_everywhere, dev_expected,
        "[dev].allowed_everywhere differs from ADR_DEV_EVERYWHERE"
    );
    assert_eq!(map.testing_feature, ADR_TESTING_FEATURE);

    // Every row is a workspace member and every `ocx*` member has a row.
    let members = member_names(&cargo_metadata(&[]));
    for row in &rows {
        assert!(
            members.contains(*row),
            "crate_map.toml row `{row}` is not a workspace member"
        );
    }
    for member in &members {
        assert!(
            rows.contains(member.as_str()),
            "workspace member `{member}` has no crate_map.toml row"
        );
    }
}

// ---------------------------------------------------------------------------
// Dependency direction over the resolve graph (C-009, S-009)
// ---------------------------------------------------------------------------

/// Every disallowed `ocx*` → `ocx*` edge in `metadata`'s resolve graph, as
/// `"<from> -> <to> (<kind>)"` lines.
///
/// Reads `resolve.nodes[].deps[]` (the resolved graph — a `package = "…"`
/// alias and a `[target.'cfg(…)'.dependencies]` entry both land there under
/// the real package id) and looks the per-edge features up in
/// `packages[].dependencies[]`. Runtime edges (kind `normal`/`build`) must be
/// in the map's row; a dev-only edge may also target `[dev].allowed_everywhere`
/// or enable the testing feature. A dev edge never legalises a runtime one.
fn direction_violations(metadata: &serde_json::Value, map: &CrateMap) -> Vec<String> {
    let packages: BTreeMap<&str, &serde_json::Value> = metadata["packages"]
        .as_array()
        .expect("packages")
        .iter()
        .map(|p| (p["id"].as_str().expect("id"), p))
        .collect();
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members")
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    let nodes = metadata["resolve"]["nodes"].as_array().expect("resolve.nodes");
    assert!(!nodes.is_empty(), "resolve graph has no nodes");
    let mut violations = Vec::new();
    let mut member_nodes = 0;
    for node in nodes {
        let id = node["id"].as_str().expect("node id");
        if !members.contains(id) {
            continue;
        }
        member_nodes += 1;
        let from_package = packages[id];
        let from = from_package["name"].as_str().expect("name");
        let allowed = map
            .allowed
            .get(from)
            .unwrap_or_else(|| panic!("`{from}` has no crate_map.toml row"));
        for dep in node["deps"].as_array().expect("deps") {
            let to = packages[dep["pkg"].as_str().expect("pkg")]["name"]
                .as_str()
                .expect("name");
            if !to.starts_with("ocx") || !members.contains(dep["pkg"].as_str().expect("pkg")) {
                continue;
            }
            let extern_name = dep["name"].as_str().expect("dep name");
            let kinds = dep["dep_kinds"].as_array().expect("dep_kinds");
            let runtime = kinds.iter().any(|k| matches!(k["kind"].as_str(), None | Some("build")));
            if runtime {
                if !allowed.contains(to) {
                    let target = kinds
                        .iter()
                        .find_map(|k| k["target"].as_str())
                        .map(|t| format!(", target {t}"))
                        .unwrap_or_default();
                    violations.push(format!("{from} -> {to} (runtime{target})"));
                }
                continue;
            }
            // Dev-only edge: the declaration's features decide the seam case.
            let enables_testing = from_package["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter(|d| d["kind"].as_str() == Some("dev"))
                .filter(|d| {
                    d["rename"]
                        .as_str()
                        .unwrap_or_else(|| d["name"].as_str().expect("name"))
                        == extern_name
                })
                .any(|d| {
                    d["features"]
                        .as_array()
                        .is_some_and(|f| f.iter().any(|x| x == map.testing_feature.as_str()))
                });
            if !(allowed.contains(to) || map.dev_everywhere.contains(to) || enables_testing) {
                violations.push(format!("{from} -> {to} (dev)"));
            }
        }
    }
    assert!(
        member_nodes > 1,
        "resolve graph carries {member_nodes} workspace member node(s) — scanned nothing"
    );
    violations.sort();
    violations
}

#[test]
fn deps_direction() {
    let violations = direction_violations(&cargo_metadata(&[]), &CrateMap::load());
    assert!(
        violations.is_empty(),
        "dependency edges outside scripts/crate_map.toml:\n  {}",
        violations.join("\n  ")
    );
}

/// One synthetic dependency declaration: `(from, to, rename, kind, target,
/// features)` — `kind` is `None` (normal), `"dev"` or `"build"`.
type SyntheticEdge<'a> = (
    &'a str,
    &'a str,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    &'a [&'a str],
);

/// Synthetic `cargo metadata` output: each edge is mirrored into the resolve
/// graph the way cargo does it (the resolve `deps[].name` is the extern name,
/// i.e. the rename when one exists).
fn synthetic_metadata(names: &[&str], edges: &[SyntheticEdge<'_>]) -> serde_json::Value {
    let id = |name: &str| format!("path+file:///w/crates/{name}#0.1.0");
    let packages: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let dependencies: Vec<serde_json::Value> = edges
                .iter()
                .filter(|e| e.0 == *name)
                .map(|(_, to, rename, kind, target, features)| {
                    serde_json::json!({"name": to, "rename": rename, "kind": kind, "target": target, "features": features})
                })
                .collect();
            serde_json::json!({"id": id(name), "name": name, "dependencies": dependencies})
        })
        .collect();
    let nodes: Vec<serde_json::Value> = names
        .iter()
        .map(|name| {
            let deps: Vec<serde_json::Value> = edges
                .iter()
                .filter(|e| e.0 == *name)
                .map(|(_, to, rename, kind, target, _)| {
                    serde_json::json!({"name": rename.unwrap_or(to), "pkg": id(to), "dep_kinds": [{"kind": kind, "target": target}]})
                })
                .collect();
            serde_json::json!({"id": id(name), "deps": deps, "features": []})
        })
        .collect();
    let members: Vec<String> = names.iter().map(|n| id(n)).collect();
    serde_json::json!({"packages": packages, "workspace_members": members, "resolve": {"nodes": nodes}})
}

fn synthetic_map() -> CrateMap {
    CrateMap {
        allowed: BTreeMap::from([
            ("ocx_util".to_owned(), BTreeSet::new()),
            ("ocx_config".to_owned(), BTreeSet::from(["ocx_util".to_owned()])),
            ("ocx_test_support".to_owned(), BTreeSet::new()),
        ]),
        dev_everywhere: BTreeSet::from(["ocx_test_support".to_owned()]),
        testing_feature: "__testing".to_owned(),
    }
}

const SYNTHETIC_NAMES: [&str; 3] = ["ocx_util", "ocx_config", "ocx_test_support"];

#[test]
fn deps_direction_reds_on_a_plain_disallowed_edge() {
    let metadata = synthetic_metadata(&SYNTHETIC_NAMES, &[("ocx_util", "ocx_config", None, None, None, &[])]);
    assert_eq!(
        direction_violations(&metadata, &synthetic_map()),
        ["ocx_util -> ocx_config (runtime)"]
    );
}

#[test]
fn deps_direction_reds_on_an_aliased_disallowed_edge() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[("ocx_util", "ocx_config", Some("cfg"), None, None, &[])],
    );
    assert_eq!(
        direction_violations(&metadata, &synthetic_map()),
        ["ocx_util -> ocx_config (runtime)"]
    );
}

#[test]
fn deps_direction_reds_on_a_target_specific_disallowed_edge() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[("ocx_util", "ocx_config", None, None, Some("cfg(unix)"), &[])],
    );
    assert_eq!(
        direction_violations(&metadata, &synthetic_map()),
        ["ocx_util -> ocx_config (runtime, target cfg(unix))"]
    );
}

#[test]
fn deps_direction_reds_on_a_dev_edge_outside_the_dev_set() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[("ocx_util", "ocx_config", None, Some("dev"), None, &[])],
    );
    assert_eq!(
        direction_violations(&metadata, &synthetic_map()),
        ["ocx_util -> ocx_config (dev)"]
    );
}

#[test]
fn deps_direction_reds_on_a_build_edge() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[("ocx_util", "ocx_config", None, Some("build"), None, &[])],
    );
    assert_eq!(
        direction_violations(&metadata, &synthetic_map()),
        ["ocx_util -> ocx_config (runtime)"]
    );
}

#[test]
fn deps_direction_greens_on_allowed_and_dev_edges() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[
            ("ocx_config", "ocx_util", None, None, None, &[]),
            ("ocx_util", "ocx_test_support", None, Some("dev"), None, &[]),
            ("ocx_util", "ocx_config", None, Some("dev"), None, &["__testing"]),
        ],
    );
    assert_eq!(direction_violations(&metadata, &synthetic_map()), Vec::<String>::new());
}

#[test]
#[should_panic(expected = "scanned nothing")]
fn deps_direction_refuses_an_empty_member_graph() {
    let metadata = synthetic_metadata(&["ocx_util"], &[]);
    direction_violations(&metadata, &synthetic_map());
}

// ---------------------------------------------------------------------------
// Release feature graph (C-078)
// ---------------------------------------------------------------------------

/// The release build is `dist build` of the `ocx` package alone, with its
/// default features and no dev-dependency edges — `cargo tree -p ocx --edges
/// normal,build` is that selection, feature-resolved per node. (`cargo
/// metadata`'s resolve graph cannot stand in for it: it always folds
/// dev-dependency edges in, so the `ocx_util = { features = ["__testing"] }`
/// dev edges design spec § E introduces would red it for the wrong reason.)
#[test]
fn release_feature_set_excludes_testing_seams() {
    let members = member_names(&cargo_metadata(&[]));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args([
            "tree",
            "-p",
            "ocx",
            "--edges",
            "normal,build",
            "--format",
            "{p}|{f}",
            "--prefix",
            "none",
            "--locked",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("spawn cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut seen = BTreeSet::new();
    let mut leaks = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((package, features)) = line.split_once('|') else {
            continue;
        };
        let name = package.split(' ').next().unwrap_or_default();
        if !members.contains(name) {
            continue;
        }
        seen.insert(name.to_owned());
        if features.split(',').any(|f| f == "__testing") {
            leaks.push(format!("{name} [{features}]"));
        }
    }
    assert!(
        seen.len() > 1,
        "cargo tree listed {} workspace member(s) — scanned nothing",
        seen.len()
    );
    leaks.sort();
    leaks.dedup();
    assert!(
        leaks.is_empty(),
        "`__testing` is active in the release feature selection:\n  {}",
        leaks.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// Manifest guards (LINT-01, E5, A4)
// ---------------------------------------------------------------------------

#[test]
fn manifests_inherit_lints_and_never_publish() {
    // C-002: the workspace `rust-version` is the toolchain channel — one
    // floor, declared twice, never drifting apart.
    let workspace = read_manifest(&workspace_root().join("Cargo.toml"));
    let rust_version = workspace["workspace"]["package"]["rust-version"]
        .as_str()
        .expect("[workspace.package].rust-version");
    let toolchain = read_manifest(&workspace_root().join("rust-toolchain.toml"));
    let channel = toolchain["toolchain"]["channel"].as_str().expect("[toolchain].channel");
    assert_eq!(
        rust_version, channel,
        "[workspace.package].rust-version `{rust_version}` differs from rust-toolchain.toml channel `{channel}` (C-002)"
    );
    for (name, dir) in crate_dirs() {
        let manifest = read_manifest(&dir.join("Cargo.toml"));
        let publish = manifest.get("package").and_then(|p| p.get("publish"));
        assert_eq!(
            publish,
            Some(&toml::Value::Boolean(false)),
            "{name}: `publish = false` missing"
        );
        let lints = manifest
            .get("lints")
            .and_then(toml::Value::as_table)
            .unwrap_or_else(|| panic!("{name}: no [lints] table"));
        assert_eq!(
            lints,
            &toml::Table::from_iter([("workspace".to_owned(), toml::Value::Boolean(true))]),
            "{name}: [lints] must be exactly `workspace = true` (LINT-01: lint policy lives in the workspace root)"
        );
    }
}

/// C-002's test half: the `# Internal crates` block of
/// `[workspace.dependencies]` names every member that has a library target,
/// each at its own directory, in sorted order — so every extraction commit of
/// the split lands on a distinct line (D-052) — and every member's edition
/// resolves to 2024. Read from the raw text: a parsed table has lost the
/// order the block is about.
#[test]
fn internal_crates_block_is_complete_and_sorted() {
    let text = std::fs::read_to_string(workspace_root().join("Cargo.toml")).expect("Cargo.toml");
    let mut lines = text.lines().skip_while(|line| !line.starts_with("# Internal crates"));
    assert!(
        lines.next().is_some(),
        "Cargo.toml has no `# Internal crates` block (C-002)"
    );
    // The block ends at the first blank line; comment lines inside it are prose.
    let entries: Vec<(&str, &str)> = lines
        .take_while(|line| !line.trim().is_empty())
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            line.split_once(" = { path = \"")
                .and_then(|(name, rest)| rest.strip_suffix("\" }").map(|path| (name, path)))
                .unwrap_or_else(|| panic!("`{line}` is not `<name> = {{ path = \"crates/<dir>\" }}`"))
        })
        .collect();
    let names: Vec<&str> = entries.iter().map(|(name, _)| *name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(names, sorted, "the `# Internal crates` block is not sorted (D-052)");
    let listed: BTreeMap<String, String> = entries
        .into_iter()
        .map(|(name, path)| (name.to_owned(), path.to_owned()))
        .collect();
    let expected: BTreeMap<String, String> = crate_dirs()
        .into_iter()
        .filter(|(_, dir)| dir.join("src/lib.rs").is_file())
        .map(|(name, dir)| {
            (
                name,
                format!("crates/{}", dir.file_name().expect("dir name").to_string_lossy()),
            )
        })
        .collect();
    let drift: Vec<String> = expected
        .iter()
        .filter(|(name, path)| listed.get(*name) != Some(path))
        .map(|(name, path)| format!("missing or misplaced: {name} = {{ path = \"{path}\" }}"))
        .chain(
            listed
                .keys()
                .filter(|name| !expected.contains_key(*name))
                .map(|name| format!("no such library crate: {name}")),
        )
        .collect();
    assert!(
        drift.is_empty(),
        "the `# Internal crates` block must list every member with a library target at its own directory (C-002):\n  {}",
        drift.join("\n  ")
    );

    let workspace = read_manifest(&workspace_root().join("Cargo.toml"));
    assert_eq!(
        workspace["workspace"]["package"]["edition"].as_str(),
        Some("2024"),
        "[workspace.package].edition (C-002)"
    );
    for (name, dir) in crate_dirs() {
        let manifest = read_manifest(&dir.join("Cargo.toml"));
        let edition = manifest
            .get("package")
            .and_then(|package| package.get("edition"))
            .unwrap_or_else(|| panic!("{name}: no [package].edition"));
        let inherited = edition.get("workspace") == Some(&toml::Value::Boolean(true));
        assert!(
            inherited || edition.as_str() == Some("2024"),
            "{name}: edition must resolve to 2024 — `edition.workspace = true` or the literal (C-002)"
        );
    }
}

/// Every crate README's `**May depend on:**` row is the prose copy of that
/// crate's `ADR_MAP` row — the copy a reader trusts without running anything.
/// Each of the 17 rows must have its README (a deleted one is red, not a
/// shorter walk); a README on a crate with no row at all is red too.
#[test]
fn readme_may_depend_on_rows_match_the_crate_map() {
    let rows: BTreeMap<&str, &[&str]> = ADR_MAP
        .iter()
        .chain(ADR_TRANSITION_ROWS)
        .map(|(from, targets)| (*from, *targets))
        .collect();
    for (name, _) in ADR_MAP {
        assert!(
            crates_dir().join(name).join("README.md").is_file(),
            "crates/{name}/README.md is missing — every ADR_MAP crate carries the shell README"
        );
    }
    let mut checked = 0;
    for (name, dir) in crate_dirs() {
        let readme = dir.join("README.md");
        if !readme.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&readme).expect("README readable");
        let row = text
            .lines()
            .find_map(|line| line.strip_prefix("**May depend on:**"))
            .unwrap_or_else(|| panic!("{}: no `**May depend on:**` row", readme.display()));
        // Backticked names only; `none (\`serde\` is its only dependency …)`
        // names no `ocx` crate and reads as the empty set.
        let listed: BTreeSet<&str> = row
            .split('`')
            .skip(1)
            .step_by(2)
            .filter(|item| *item == "ocx" || item.starts_with("ocx_"))
            .collect();
        let expected: BTreeSet<&str> = rows
            .get(name.as_str())
            .unwrap_or_else(|| panic!("{}: README for a crate with no ADR_MAP row", readme.display()))
            .iter()
            .copied()
            .collect();
        assert_eq!(
            listed,
            expected,
            "{}: the `May depend on` row differs from ADR_MAP",
            readme.display()
        );
        checked += 1;
    }
    assert!(checked > 1, "checked {checked} README(s) — scanned nothing");
}

/// Each workspace member's `anyhow` dependency kinds — `"normal"`, `"dev"`,
/// `"build"` — from `cargo metadata`'s per-package dependency list.
///
/// Read there rather than from the manifest's three flat tables, which are
/// not where a dependency has to live: cargo resolves
/// `[target.'cfg(unix)'.dependencies]` the same way, and a rename
/// (`err = { package = "anyhow" }`) keeps `anyhow` as the dependency's
/// `name` while the table key says otherwise.
fn anyhow_kinds(metadata: &serde_json::Value) -> BTreeMap<String, BTreeSet<String>> {
    let members = member_names(metadata);
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for package in metadata["packages"].as_array().expect("packages") {
        let name = package["name"].as_str().expect("package name");
        if !members.contains(name) {
            continue;
        }
        let kinds = out.entry(name.to_owned()).or_default();
        for dep in package["dependencies"].as_array().expect("dependencies") {
            if dep["name"].as_str() == Some("anyhow") {
                kinds.insert(dep["kind"].as_str().unwrap_or("normal").to_owned());
            }
        }
    }
    assert!(
        out.len() > 1,
        "cargo metadata carries {} workspace package(s) — read nothing",
        out.len()
    );
    out
}

#[test]
fn anyhow_only_in_cli_runtime() {
    let mut dev_carriers = BTreeSet::new();
    for (name, kinds) in anyhow_kinds(&cargo_metadata(&[])) {
        if name == "ocx" {
            assert!(
                kinds.contains("normal"),
                "the CLI is the anyhow boundary and must list it"
            );
            continue;
        }
        assert!(
            !kinds.contains("normal") && !kinds.contains("build"),
            "{name}: anyhow is a runtime dependency — libraries use thiserror (E5)"
        );
        if kinds.contains("dev") {
            dev_carriers.insert(name);
        }
    }
    // Four library dev uses, each asserting something about the *binary's*
    // render path from inside the library that owns the error: the starlark
    // engine's classify tests build `anyhow::Error`-typed `ErrorKind`s
    // (`ocx_script`), and
    // `ocx_sign`'s sign/verify error tests — joined at WP-34 by
    // `ocx_package_manager`'s — format an `anyhow::Error` with `{:#}` because
    // the sentence-duplication they guard is only observable through anyhow's
    // chain rendering.
    let permitted = BTreeSet::from([
        "ocx_package_manager".to_owned(),
        "ocx_script".to_owned(),
        "ocx_sign".to_owned(),
    ]);
    assert!(
        dev_carriers.is_subset(&permitted),
        "anyhow in [dev-dependencies] of {dev_carriers:?}; only {permitted:?} may"
    );
    assert!(
        !dev_carriers.is_empty(),
        "no crate carries anyhow as a dev-dependency — the engine::classify tests lost theirs"
    );
}

#[test]
fn anyhow_kinds_reads_a_target_specific_table() {
    // `[target.'cfg(unix)'.dependencies] anyhow` is a runtime edge, and the
    // shape a walk of the three flat tables passes green.
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[("ocx_util", "anyhow", None, None, Some("cfg(unix)"), &[])],
    );
    assert_eq!(
        anyhow_kinds(&metadata)["ocx_util"],
        BTreeSet::from(["normal".to_owned()])
    );
}

#[test]
fn anyhow_kinds_separates_dev_and_build_and_sees_through_a_rename() {
    let metadata = synthetic_metadata(
        &SYNTHETIC_NAMES,
        &[
            ("ocx_util", "anyhow", None, Some("dev"), None, &[]),
            ("ocx_config", "anyhow", Some("err"), Some("build"), None, &[]),
        ],
    );
    let kinds = anyhow_kinds(&metadata);
    assert_eq!(kinds["ocx_util"], BTreeSet::from(["dev".to_owned()]));
    assert_eq!(kinds["ocx_config"], BTreeSet::from(["build".to_owned()]));
    assert!(kinds["ocx_test_support"].is_empty(), "no anyhow edge is no kind");
}

#[test]
fn testing_feature_forward_list_matches_grep() {
    // Left side: what `ocx`'s `__testing` feature forwards.
    let cli = read_manifest(&crates_dir().join("ocx_cli/Cargo.toml"));
    let forwarded: BTreeSet<String> = cli["features"]["__testing"]
        .as_array()
        .expect("[features].__testing")
        .iter()
        .map(|entry| {
            let entry = entry.as_str().expect("feature entry");
            let (dep, feature) = entry
                .split_once('/')
                .unwrap_or_else(|| panic!("`{entry}` is not `<dep>/__testing`"));
            assert_eq!(feature, "__testing", "`{entry}` forwards a different feature");
            dep.to_owned()
        })
        .collect();

    // Right side: every library crate whose source names the feature.
    let mut seamed = BTreeSet::new();
    let mut walked = 0;
    for (name, dir) in crate_dirs() {
        if name == "ocx" {
            continue;
        }
        let sources = rust_sources(&dir.join("src"));
        assert!(
            !sources.is_empty(),
            "{}: no `.rs` file — a crate that falls out of the walk leaves `walked` above its floor on the rest",
            dir.join("src").display()
        );
        for file in sources {
            walked += 1;
            // Raw text on purpose: the contract is the crate set of
            // `grep -rl 'feature = "__testing"' crates/*/src`.
            if std::fs::read_to_string(&file)
                .expect("readable")
                .contains("feature = \"__testing\"")
            {
                seamed.insert(name.clone());
            }
        }
    }
    assert!(walked > 1, "walked {walked} source file(s) — scanned nothing");
    assert_eq!(
        forwarded, seamed,
        "ocx_cli's `__testing` forward list must equal the crates that gate on the feature (A4) — a missing \
         forward silently substitutes production behaviour into the acceptance run"
    );
}

/// `ocx_util` `pub` items with no consumer outside the crate, tolerated for now
/// and only ever shrinking. Named, never counted: a count reconciles one item
/// gaining a consumer against another losing its last, and reports the same
/// number either way.
const OCX_UTIL_WITHOUT_CONSUMER: &[&str] = &[
    "LockedTomlFile",
    "add_dir",
    "collect_and_descend",
    "embedded_roots",
    "lock_exclusive_with_timeout",
    "open_shared_with_timeout",
    "serde_ext",
    "with_level",
];

/// `ocx_util` is the closed tier: nothing is `pub` here because a caller
/// *inside* the crate found it convenient (DEC-40). `ocx_util/src/lib.rs`
/// stated in the present tense that this test held that open. It did not
/// exist — neither did the `OCX_UTIL_WITHOUT_CONSUMER` it named — and the only
/// occurrence of either name anywhere in the tree was the doc comment claiming
/// them (DEC-57). A named check that is absent self-corroborates: a reader
/// asking whether the contract is enforced finds the name, and the name is the
/// evidence.
///
/// **The baseline is a staging post, not a settlement.** Eight items are `pub`
/// today with no consumer outside this crate. Narrowing them to
/// `pub(crate)` is the right cleanup and is deliberately deferred past the
/// split: it is API surgery on files four remaining extractions still move,
/// and — the deciding reason — narrowing changes what `unreachable_pub` sees,
/// so `clippy-warn-baseline.json` would move in the same commit for a reason
/// adjacent to but not identical with its own subject, which is how a rekey
/// gets laundered. The guard's job is to stop the next one, and with a named
/// baseline it does that from the moment it lands.
///
/// **On the consumer floor, and read this before "fixing" a large finding.**
/// A scan that reads no consumers reports *every* item unconsumed. That is not
/// a silent green; it is the loudest possible false red, and it is worse in one
/// way — a vacuous green is eventually found by someone auditing, whereas a
/// deafening false red gets dismissed, and a guard people have learned to
/// dismiss is off.
///
/// The consumer scan cuts comments at `//`, and for this population that
/// decides nothing: none of the eight appears anywhere outside `ocx_util`, in
/// code or in a comment, across all 660 files. Measured, not assumed — cutting
/// can only shrink the consumer text, so a permissive reader's answer is a
/// subset, and here the subset is the same set.
///
/// So the consumer side floors on its population: every
/// workspace crate other than `ocx_util` must contribute at least one source
/// file, and a crate contributing none reds by name. If you are looking at a
/// hundred findings, check that assertion first.
#[test]
fn every_public_item_of_ocx_util_has_a_consumer() {
    /// Every `pub` item name a file declares, descending through `mod` blocks
    /// and into `impl` blocks — `with_level` and `file_mut` are `pub fn`s on an
    /// `impl`, and an item's reachability does not depend on where it is
    /// written.
    fn public_names(file: &syn::File) -> BTreeSet<String> {
        struct Names {
            out: BTreeSet<String>,
        }
        fn is_pub(vis: &syn::Visibility) -> bool {
            matches!(vis, syn::Visibility::Public(_))
        }
        impl<'ast> syn::visit::Visit<'ast> for Names {
            fn visit_item(&mut self, item: &'ast syn::Item) {
                syn::visit::visit_item(self, item);
                let named = match item {
                    syn::Item::Fn(i) => is_pub(&i.vis).then(|| i.sig.ident.to_string()),
                    syn::Item::Struct(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Enum(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Trait(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Const(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Static(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Type(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    syn::Item::Mod(i) => is_pub(&i.vis).then(|| i.ident.to_string()),
                    // `pub use` re-exports a name this crate does not own; the
                    // owning crate's own guard is where it belongs.
                    _ => None,
                };
                if let Some(name) = named {
                    self.out.insert(name);
                }
            }
            fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
                syn::visit::visit_impl_item_fn(self, item);
                if is_pub(&item.vis) {
                    self.out.insert(item.sig.ident.to_string());
                }
            }
        }
        let mut names = Names { out: BTreeSet::new() };
        syn::visit::Visit::visit_file(&mut names, file);
        names.out
    }

    let crates: Vec<(String, PathBuf)> = crate_dirs();
    let (_, util_dir) = crates
        .iter()
        .find(|(name, _)| name == "ocx_util")
        .expect("ocx_util is a workspace member");

    let mut declared: BTreeSet<String> = BTreeSet::new();
    let util_sources = rust_sources(&util_dir.join("src"));
    assert!(
        util_sources.len() > 1,
        "read {} source file(s) of ocx_util — the declaring side of this comparison is empty, and an \
         empty left side makes every item trivially consumed",
        util_sources.len()
    );
    for path in &util_sources {
        let text = std::fs::read_to_string(path).expect("readable");
        declared.extend(public_names(&syn::parse_file(&text).expect("ocx_util parses as Rust")));
    }

    // The consumer corpus, floored on its population rather than on a count of
    // files: a crate that contributes nothing is a crate the scan did not read.
    let mut consumer_text = String::new();
    let mut silent: Vec<String> = Vec::new();
    let mut files_read = 0usize;
    for (name, dir) in &crates {
        if name == "ocx_util" {
            continue;
        }
        let sources = rust_sources(dir);
        if sources.is_empty() {
            silent.push(name.clone());
            continue;
        }
        for path in sources {
            // This file is not a consumer of what it names. `OCX_UTIL_WITHOUT_CONSUMER`
            // spells every tolerated item as a string literal, so scanning it makes
            // each entry look consumed and the whole baseline read as departed — a
            // detector matching its own needles, which reports the same answer in
            // every state. Found on this guard's first run, by the departure
            // assertion below. `file!()` rather than a written-out path: a rename
            // must not silently re-open it.
            if path.ends_with(file!()) {
                continue;
            }
            files_read += 1;
            // Comments cut at `//` before matching: a name written in prose is
            // not a consumer, and this guard's sibling
            // (`every_repo_path_literal_in_a_test_names_something_live`) cuts
            // them for the same reason. Truncating a line carrying `//` inside
            // a string literal is acceptable here for the same reason it is
            // there — no use site is spelled that way.
            let source = std::fs::read_to_string(&path).expect("readable");
            for line in source.lines() {
                consumer_text.push_str(line.split("//").next().unwrap_or(""));
                consumer_text.push('\n');
            }
        }
    }
    assert!(
        silent.is_empty(),
        "{} workspace crate(s) contributed no source file to the consumer scan: {:?} — every item \
         would then read as unconsumed, which is a false red loud enough to be dismissed rather \
         than investigated",
        silent.len(),
        silent
    );

    let consumed: BTreeSet<&String> = declared
        .iter()
        .filter(|name| {
            consumer_text.match_indices(name.as_str()).any(|(at, _)| {
                let before = consumer_text[..at].chars().next_back();
                let after = consumer_text[at + name.len()..].chars().next();
                let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric() && c != '_');
                boundary(before) && boundary(after)
            })
        })
        .collect();
    let unconsumed: BTreeSet<String> = declared
        .difference(&consumed.into_iter().cloned().collect())
        .cloned()
        .collect();

    let tolerated: BTreeSet<String> = OCX_UTIL_WITHOUT_CONSUMER.iter().map(|s| (*s).to_owned()).collect();
    let arrived: Vec<&String> = unconsumed.difference(&tolerated).collect();
    let departed: Vec<&String> = tolerated.difference(&unconsumed).collect();
    assert!(
        arrived.is_empty(),
        "{} `pub` item(s) of ocx_util are named by no other crate and are not in \
         OCX_UTIL_WITHOUT_CONSUMER: {:?}. Make it `pub(crate)`, or give it the consumer it was \
         exported for. ({} of {} declared item(s) are consumed, over {} file(s) in {} crate(s).)",
        arrived.len(),
        arrived,
        declared.len() - unconsumed.len(),
        declared.len(),
        files_read,
        crates.len() - 1
    );
    assert!(
        departed.is_empty(),
        "OCX_UTIL_WITHOUT_CONSUMER names {} item(s) that now have a consumer or no longer exist: \
         {:?} — a row tolerating nothing is the same defect as an exemption whose target is gone. \
         Delete them, in the commit that made them consumed",
        departed.len(),
        departed
    );
}

/// Every `[features]` entry in the workspace that forwards `<dep>/<feature>`,
/// as `crate -> "<owning feature> -> <entry>"`.
///
/// Parameterised by feature name over ONE code path, which is what lets the
/// `__testing` control below vouch for the `__test_scaffolding` assertion: there
/// is no shape in which this recognises one and is blind to the other. If these
/// paths are ever split, that control stops being a control and the reasoning
/// here has to be redone.
fn feature_forwards(feature: &str) -> BTreeMap<String, Vec<String>> {
    let suffix = format!("/{feature}");
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, dir) in crate_dirs() {
        let manifest = read_manifest(&dir.join("Cargo.toml"));
        let Some(features) = manifest.get("features").and_then(|table| table.as_table()) else {
            continue;
        };
        for (owner, entries) in features {
            let Some(entries) = entries.as_array() else { continue };
            for entry in entries {
                let Some(entry) = entry.as_str() else { continue };
                if entry.ends_with(&suffix) {
                    found
                        .entry(name.clone())
                        .or_default()
                        .push(format!("{owner} -> {entry}"));
                }
            }
        }
    }
    found
}

/// `__test_scaffolding` is the seam feature nothing forwards, and the guard for
/// it never existed.
///
/// `testing_feature_forward_list_matches_grep` above has been the right shape
/// for `__testing` for batches — a set equality with a reader floor and a
/// per-crate source floor. This is not "add that pattern": the pattern was
/// already here, already correct, and simply was never applied to the sibling
/// feature. That is the more interesting defect, because nothing about the
/// existing guard looks incomplete.
///
/// The three clauses protect one property — the scaffolding stays out of both
/// the release binary and the acceptance binary — by different routes, so each
/// is asserted separately and each has its own discriminating mutation.
///
/// **Clause 2 is the one that cannot floor itself.** Its expected value is the
/// empty set, which is also what a broken scanner, an unresolvable root and a
/// regex that stopped matching all produce. Every other assertion here is
/// non-empty and therefore reds on its own collapse; this one does not, so it
/// leans on the `__testing` control below — the same extractor, the same code
/// path, a population that is never empty. The control asserts only
/// non-emptiness: `testing_feature_forward_list_matches_grep` already pins that
/// exact membership, and a second copy would be a second thing to rekey at every
/// extraction and a second chance for the two to disagree.
#[test]
fn test_scaffolding_is_declared_where_it_is_gated_and_forwarded_nowhere() {
    const FEATURE: &str = "__test_scaffolding";

    // ── Reader floor: the population, not a file count ──────────────────────
    // Every `crates/*/src` the workspace declares must have been read, and a
    // scan that stopped reds naming what it missed (DEC-63, and the shape
    // T-arch-G1 now uses).
    let mut declares = BTreeSet::new();
    let mut gates = BTreeSet::new();
    let mut unread = Vec::new();
    for (name, dir) in crate_dirs() {
        let manifest = read_manifest(&dir.join("Cargo.toml"));
        if manifest
            .get("features")
            .and_then(|table| table.as_table())
            .is_some_and(|features| features.contains_key(FEATURE))
        {
            declares.insert(name.clone());
        }
        // A crate with no `src/` counts as unread rather than skipped. Every
        // workspace member has one today, so the `continue` would be dead —
        // and that is the point: the day one does not, a skip makes it
        // invisible to all three clauses while the floor still reads clean.
        let sources = rust_sources(&dir.join("src"));
        if sources.is_empty() {
            unread.push(name.clone());
            continue;
        }
        let needle = format!("feature = \"{FEATURE}\"");
        for file in sources {
            if std::fs::read_to_string(&file).expect("readable").contains(&needle) {
                gates.insert(name.clone());
                break;
            }
        }
    }
    assert!(
        unread.is_empty(),
        "the scan read no source from {:?} — a crate that falls out of the walk is a crate whose \
         forwards and gates this test cannot see, and every assertion below then passes by absence",
        unread
    );

    // ── Clause 1: declared exactly where gated ──────────────────────────────
    // Non-empty on both sides today, so this clause floors itself.
    assert_eq!(
        declares, gates,
        "`{FEATURE}` is declared in {declares:?} and gated in {gates:?} — a declared-but-ungated \
         feature is dead weight that reads as a live seam, and a gated-but-undeclared one is a \
         compile error waiting for whoever enables it"
    );
    assert!(
        !declares.is_empty(),
        "no crate declares `{FEATURE}` — the seam this guard exists for is gone, and its emptiness \
         below would then be true for the wrong reason"
    );

    // ── Clause 2: forwarded by nothing, vouched for by a control ────────────
    let control = feature_forwards("__testing");
    assert!(
        !control.is_empty(),
        "the forward extractor found no `__testing` forward anywhere, and `ocx`, `ocx_config` and \
         `ocx_lib` all carry one — so it cannot see a forward at all, and the empty result below says \
         nothing about the tree. Keys here are PACKAGE names, so the CLI appears as `ocx`, not `ocx_cli`"
    );
    let forwards = feature_forwards(FEATURE);
    assert!(
        forwards.is_empty(),
        "`{FEATURE}` is forwarded by {forwards:?} — it is the NON-forwarded seam feature, and a \
         forward puts the scaffolding into whatever enables the forwarding feature: the release \
         binary via a normal edge, the acceptance binary via `ocx`'s `__testing`"
    );

    // ── Clause 3: every enabling edge is a dev edge, by identity ────────────
    // Named, not counted. A count of four is satisfied by swapping one of these
    // for a normal edge somewhere else — and the count is the shape that invites
    // the collapse in the first place: `ocx_lib` carries TWO of these, and
    // writing "three crates" instead of four triples is exactly the slip.
    const EXPECTED_EDGES: &[(&str, &str, &str)] = &[
        ("ocx_index", "dev-dependencies", "ocx_config"),
        // WP-34: `mutate.rs` calls `ToolchainRoot::from_validated` and
        // `tasks::pull_local`'s coalescing test reads
        // `BlobStore::write_call_count`. Both rows were `ocx_lib`'s until the
        // tests that read them moved, and `ocx_lib` keeps neither — nothing
        // left there reaches either item (DEC-83).
        ("ocx_package_manager", "dev-dependencies", "ocx_config"),
        ("ocx_package_manager", "dev-dependencies", "ocx_store"),
        // WP-33: `toolchain_home.rs`'s tests call `ToolchainRoot::from_validated`,
        // which is scaffolding-gated in `ocx_config`.
        ("ocx_project", "dev-dependencies", "ocx_config"),
        ("ocx_store", "dev-dependencies", "ocx_config"),
    ];
    let mut edges = BTreeSet::new();
    for (name, dir) in crate_dirs() {
        let manifest = read_manifest(&dir.join("Cargo.toml"));
        for table in ["dependencies", "dev-dependencies", "build-dependencies"] {
            let Some(deps) = manifest.get(table).and_then(|entry| entry.as_table()) else {
                continue;
            };
            for (dep, spec) in deps {
                let enables = spec
                    .get("features")
                    .and_then(|features| features.as_array())
                    .is_some_and(|features| features.iter().any(|f| f.as_str() == Some(FEATURE)));
                if enables {
                    edges.insert((name.clone(), table.to_owned(), dep.clone()));
                }
            }
        }
    }
    let expected: BTreeSet<(String, String, String)> = EXPECTED_EDGES
        .iter()
        .map(|(c, t, d)| ((*c).to_owned(), (*t).to_owned(), (*d).to_owned()))
        .collect();
    assert_eq!(
        edges, expected,
        "the edges enabling `{FEATURE}` are not the ones recorded. Every one must be a \
         `[dev-dependencies]` edge: resolver v3 keeps a dev-dependency feature out of the normal \
         build, so a normal edge here ships the scaffolding"
    );
}

// ---------------------------------------------------------------------------
// Source-text guards
// ---------------------------------------------------------------------------

/// Every `crates/*/src` (the CLI included when asked), walked as one corpus:
/// a one-file shell is scanned beside the rest instead of being skipped for
/// being alone (H4), so content that lands inside a shell's `lib.rs` is seen.
fn library_subtrees(include_cli: bool) -> Vec<PathBuf> {
    let dirs = crate_dirs();
    let expected = dirs.len() - usize::from(!include_cli);
    let subtrees: Vec<PathBuf> = dirs
        .into_iter()
        .filter(|(name, _)| include_cli || name != "ocx")
        .map(|(_, dir)| dir.join("src"))
        .collect();
    assert_eq!(
        subtrees.len(),
        expected,
        "library_subtrees dropped a crate — every crates/*/src is walked, one-file shells included (H4)"
    );
    subtrees
}

#[test]
fn no_classification_in_libraries() {
    assert_no_needles(
        &library_subtrees(false),
        &["ClassifyExitCode", "ClassifyErrorKind"],
        &fixture("classify_impl.rs.txt"),
    );
}

// ---------------------------------------------------------------------------
// The exit-code boundary (DEC-23)
// ---------------------------------------------------------------------------

/// The subtrees this guard judges, relative to `crates/` — DEC-35's contract
/// item as **data** (DEC-39).
///
/// Honouring DEC-35 is adding one entry. It used to be three judgement calls
/// per extraction, fourteen times over: a single-subtree constant, a
/// `declared > 20 && implementing > 8` floor read off `ocx_util`, and a
/// liveness assert written for one tree. A contract item that costs a code
/// change per extraction is one the next builder skips, and this run has three
/// findings — B5-22, `boundaries.rs:21`, DEC-37 — where a guard quietly stopped
/// covering its subject.
///
/// DEC-23 fired here: WP-17 changed a `utility` signature to hand `ocx_cli` a
/// `utility::error::Error` directly, the value reached the chain walker without
/// passing through `ocx_lib::Error`, the walker had no arm for it, and
/// `ocx package create` on a malformed sidecar exited **1 where it had exited
/// 65** with byte-identical stderr. `cargo nextest` was green at 8190/8190
/// across it.
///
/// Wider is measured, not assumed: across all of `crates/ocx_lib/src` the same
/// scan finds 22 error types with no arm today, most of them leaf `*Kind` enums
/// that are only ever reached through an armed wrapper. Each would need its own
/// decision — arming one moves an exit code — so widening the scope is a
/// package of its own rather than an allowlist of 22 entries nobody justified.
///
/// **Derived, because as a literal it silently stopped covering its subject.**
/// It was a seven-entry list whose doc comment still said "the three entries",
/// and B8 put three more crates on this boundary and added none of them. The
/// `!subtrees.is_empty()` floor below was satisfied by the survivors, so the
/// narrowing produced no signal at all — and widening it by hand afterwards
/// reds on ten reaches nobody had judged. DEC-39 made this *data* so that
/// honouring DEC-35 would cost one line per extraction; three extractions in a
/// row paid nothing, which is the answer to whether one line is cheap enough.
/// Its sibling `library_subtrees` five hundred lines above has derived from
/// `crate_dirs()` the whole time.
///
/// The exclusions are the only judgement left, and a stale one fails **safe**:
/// forgetting to remove an exclusion is impossible to do by adding a crate, and
/// a crate that stops deserving its exclusion simply starts being judged. That
/// is the opposite of the literal's failure mode, where forgetting to add an
/// entry silently shrank the scope.
fn boundary_subtrees() -> Vec<PathBuf> {
    /// Each excluded for a reason, never for convenience:
    /// - `ocx` is the consumer side of this boundary, not a crossing.
    /// - `ocx_schema` is build-only and links into no command.
    /// - `ocx_test_support` is this harness.
    ///
    /// `ocx_shim` needs no entry: it is a Windows launcher binary with no
    /// `lib.rs`, so the filter below drops it without a judgement call.
    const NOT_ON_THE_BOUNDARY: &[&str] = &["ocx", "ocx_schema", "ocx_test_support"];

    let dirs = crate_dirs();
    for excluded in NOT_ON_THE_BOUNDARY {
        assert!(
            dirs.iter().any(|(name, _)| name == excluded),
            "NOT_ON_THE_BOUNDARY names {excluded:?}, which is not a crate — an exclusion that \
             resolves to nothing is a judgement about a tree that no longer exists"
        );
    }
    // The population this guard owes coverage to, read off the same `dirs` but
    // independently of the walk below: every crate not excluded that has a
    // `src/lib.rs`. Named, and compared for equality — the floor this replaces
    // was `subtrees.len() >= NOT_ON_THE_BOUNDARY.len()`, which keyed the
    // guard's strength to the *exclusion list's* length and so got weaker every
    // time an exclusion was correctly removed. Three exclusions floored a
    // sixteen-crate boundary at three.
    let owed: Vec<String> = dirs
        .iter()
        .filter(|(name, dir)| !NOT_ON_THE_BOUNDARY.contains(&name.as_str()) && dir.join("src/lib.rs").is_file())
        .map(|(_, dir)| directory_name(dir))
        .collect();
    let subtrees: Vec<PathBuf> = dirs
        .iter()
        .filter(|(name, _)| !NOT_ON_THE_BOUNDARY.contains(&name.as_str()))
        .map(|(_, dir)| dir.join("src"))
        .filter(|src| src.join("lib.rs").is_file())
        .collect();
    let walked: Vec<String> = subtrees
        .iter()
        .map(|src| directory_name(src.parent().expect("<crate>/src has a parent")))
        .collect();
    assert_eq!(
        walked, owed,
        "boundary_subtrees walked a different set than the workspace offers — a crate on this \
         boundary is not being judged (left: walked, right: owed)"
    );
    subtrees
}

/// The `crates/<dir>` component of a crate directory, for naming a crate in a
/// refusal. Directory, not package: the `ocx` package lives in `crates/ocx_cli`.
fn directory_name(dir: &Path) -> String {
    dir.file_name()
        .unwrap_or_else(|| panic!("{} has no final component", dir.display()))
        .to_string_lossy()
        .into_owned()
}

/// Error types a `pub fn` under [`BOUNDARY_SUBTREES`] returns that the `ocx_cli`
/// ladder carries no `downcast_arm!` for.
///
/// Arming one **moves an exit code** — from the walker's fall-through
/// `Failure` (1) to whatever the arm says — and the crate split must not change
/// what `ocx` does, so a type that already escaped at both revisions is recorded
/// here instead of fixed. The subject of the guard is a *new* type joining them.
///
/// Every entry must still be produced by the scan. An exclusion whose target is
/// gone — armed since, renamed, deleted — forbids nothing while reading as
/// coverage, which is the unmatched-glob class; the test asserts each one is
/// still live before it filters anything out.
/// B3-14 names a second escapee, `tempfile::PersistError`, which is **not** a
/// row here: `attempt_noclobber` and `persist_with_retry` are private and both
/// `pub` wrappers around them return `std::io::Result`, so it crosses no `pub`
/// signature and the existence assertion below rejects the row outright.
/// Error types a `pub fn` hands the CLI that no `downcast_arm!` registers, each
/// keyed on **the boundary site as well as the type** (B6-B3).
///
/// Keyed on the rendered type alone, an exemption was a wildcard: every later
/// `pub fn` anywhere in the scanned subtrees returning a `serde_json::Error`
/// inherited this file's exemption, and so did any crate that happened to
/// declare its own `TrustPolicyError`. The name of a type is not the identity
/// of a boundary — a second site is a second decision, and it has to be taken
/// rather than inherited.
const UNARMED_AT_THE_BOUNDARY: &[(&str, &str, &str)] = &[
    // WP-32 put `ocx_shell` on the derived boundary. Two of its three reaches
    // were spelling, not substance: `GitHubFlavor::from_env` and
    // `GitLabFlavor::new` returned `super::error::Error`, which the arm
    // registry cannot resolve, while the type itself is armed by
    // `downcast_arm!(cause, CiError)` in `ocx_cli/src/exit/ocx_shell.rs`. They
    // are spelled `crate::ci::error::Error` now rather than recorded here —
    // a row claiming an armed type is unarmed is a false record, and the next
    // reader would act on it. Only the third is a real decision.
    (
        "ocx_shell/src/shell.rs",
        // Keyed as the scanner renders it: the type comes off
        // `to_token_stream().to_string()` with its spaces collapsed, so
        // the readable `&'static str` matches nothing and the staleness
        // check rejects the row.
        "&'staticstr",
        "WP-32: `is_emittable` returns a REASON, not an error. Its two callers consume the string \
         as text — `reconcile::plan` logs it in the A-10 warn line and `conventions::emit_lines` \
         formats it into a `# ocx:` note — and neither propagates a value that could carry an exit \
         code. There is no type here to arm: `&'static str` implements no `std::error::Error`, so a \
         `downcast_arm!` rung could never match it",
    ),
    (
        "ocx_util/src/fs/bounded_read.rs",
        "BoundedReadError",
        "B3-14: unarmed at 7adaea62 and at HEAD. `options/tags.rs` handles it by hand; every other \
         path exits 1 today, and arming it is a behaviour decision, not this guard's job",
    ),
    // The `ocx_oci` and `ocx_trust` rows are REVELATION: each signature is
    // byte-identical to the one at the merge base, and the widening is what
    // made it visible. Each type reaches the CLI only inside an armed enum, so
    // arming it would add a second, closer classification for a value that
    // already has one — which moves an exit code, and DEC-23 forbids that.
    (
        "ocx_oci/src/manifest.rs",
        "InvalidImageIndex",
        "WP-24: `manifest::validate_image_index` raises it; every caller either `?`s it into \
         `ClientError` or maps it onto `oci::index::error::Error`, both of which a `downcast_arm!` \
         registers",
    ),
    (
        "ocx_oci/src/media_type.rs",
        "UnsupportedMediaType",
        "WP-24: `ocx_lib::Error` carries a `From` for it (error.rs), so the value that reaches the \
         CLI is the armed `ocx_lib::Error`, never this type",
    ),
    (
        "ocx_oci/src/referrer/manifest.rs",
        "serde_json::Error",
        "WP-24: `ReferrerManifest::to_canonical_json` raises the serializer's error verbatim \
         (ADR 1.18); the two pipelines that push a referrer wrap it as `SignErrorKind::Internal` at \
         their own boundary, and every other caller is a test that expects success",
    ),
    (
        "ocx_oci/src/resolve_target.rs",
        "ResolveTargetError",
        "WP-24: `resolve_sign_target` is consumed by exactly two pipelines, each of which matches \
         all three variants onto its own kind — `VerifyErrorKind::TargetNotAnIndex` and \
         `SignErrorKind::TargetNotAnIndex` — with no wildcard",
    ),
    (
        "ocx_oci/src/ssrf.rs",
        "PhysicalDialRefused",
        "WP-24: it is only ever a `#[source]` field of an `oci::index::error::Error` variant, which \
         is armed; its own `ClassifyExitCode` impl is reached through that cause chain, not by a \
         downcast",
    ),
    (
        "ocx_trust/src/key_ref.rs",
        "KeyEnvError",
        "WP-25: `read_key_env`'s error. `oci/sign/key_backend.rs` maps each variant onto \
         `KeyBackendError`, and `trust`'s own reader maps it onto `TrustPolicyError::KeyMalformed`; \
         both destinations are armed",
    ),
    (
        "ocx_trust/src/key_ref.rs",
        "KeyRefError",
        "WP-25: `oci/verify/error.rs` is the single `From` site, choosing between \
         `VerifyErrorKind::UnsupportedKeyBackend` and `KeyReferenceInvalid`; `ocx_sign.rs` \
         classifies both",
    ),
    (
        "ocx_trust/src/lib.rs",
        "TrustPolicyError",
        "WP-25: it crosses as `VerifyErrorKind::TrustPolicyInvalid(..)`, which `ocx_sign.rs` \
         classifies variant by variant — arming the inner type would put a second, closer \
         classifier on a value that already has one",
    ),
    // The `ocx_config` rows are REVELATION too: every signature is
    // byte-identical to the one at the merge base, and WP-26's widening is
    // what made it visible. None is armed, and none may be.
    (
        "ocx_config/src/lib.rs",
        "ToolchainRootError",
        "WP-26: `ToolchainRoot::resolve` raises it, and it reaches the CLI only inside \
         `config::error::Error::Toolchain`, whose `classify` already delegates to this type's own \
         (`exit/ocx_config.rs`). Arming it would put a second, closer classifier on a value that \
         already has one",
    ),
    (
        "ocx_config/src/mirror.rs",
        "D::Error",
        "WP-26: `deserialize_mirrors_table`'s error is the deserializer's own associated type, not a \
         nameable one — `#[serde(deserialize_with)]` on `Config::mirrors` makes it `toml::de::Error`, \
         which the loader raises as `ConfigError::Parse`. There is no concrete type to register",
    ),
    (
        "ocx_config/src/shell.rs",
        "ConsentPatternError",
        "WP-26: `validate_consent_pattern` and `normalize_consent_pattern` have no caller outside this \
         crate; inside it the `[shell.consent]` deserializer hands every refusal to \
         `serde::de::Error::custom`, which erases the type into a `toml::de::Error` message before it \
         leaves the loader (`loader.rs`'s `consent_table_shape_is_readable` states the same erasure)",
    ),
    (
        "ocx_trust/src/lib.rs",
        "regex::Error",
        "WP-25: `IdentityRule::compile_regex` raises the compiler's own error; its one production \
         caller maps it onto `TrustPolicyError::InvalidRegex`, whose `#[source]` field it becomes, \
         and the other caller is a test",
    ),
    // The `ocx_store` rows are the three narrow unions WP-27's E1 minted so the
    // store tier could stop naming the crate-wide `ocx_lib::Error`. Each is
    // rebuilt into the exact variant it replaced by an `impl From<..> for
    // ocx_lib::Error` in `crates/ocx_lib/src/error.rs`, so the value that
    // reaches the CLI is the armed `ocx_lib::Error`, never the union. Arming
    // one would add a second, closer classification for a value that already
    // has one — which moves an exit code, and DEC-23 forbids that.
    (
        "ocx_store/src/file_structure/assemble.rs",
        "AssembleError",
        "WP-27: `File` -> `InternalFile` (74), `SymlinkWalk` -> `SymlinkWalk` (delegates, 64/74), \
         `Archive` -> `Archive`; every `assemble_from_layer*` caller `?`s it into `ocx_lib::Error`, \
         which a `downcast_arm!` registers",
    ),
    (
        "ocx_store/src/file_structure/cas_path.rs",
        "DigestFileError",
        "WP-27: `read_digest_file`'s error. `File` -> `InternalFile` (74) and `Digest` -> `Digest` \
         (delegates, 65); both callers are inside `ocx_lib`, which converts before the CLI sees it",
    ),
    (
        "ocx_store/src/file_structure/package_store.rs",
        "PackageDirError",
        "WP-27: `package_dir_for_content` and its seven accessors. `File` -> `InternalFile` (74), \
         `PathInvalid` -> `InternalPathInvalid` (1) — the two variants the wide enum produced here",
    ),
    (
        "ocx_store/src/reference_manager.rs",
        "PackageDirError",
        "WP-27: the same type, a second site, so a second decision (this table's own rule). \
         `link`/`create_forward_dep_ref`/`link_blobs` propagate the store's error unchanged; \
         `ocx_lib`'s callers convert at the same `From` impl",
    ),
    // The `ocx_package` and `ocx_script` rows are what DEC-70's derivation
    // revealed: B8 put three crates on this boundary and the literal it
    // replaced named none of them. Each signature is byte-identical to the one
    // at the merge base — nothing changed but who was looking.
    (
        "ocx_package/src/cascade/graph.rs",
        "ScopeError",
        "B8: `scope_request` and `scope_filter`. Both production callers are in `ocx_cli` itself \
         (`command/package_cascade.rs`) and convert on the spot with \
         `UsageError::with_source(..)`, which boxes the cause; `UsageError` is armed and classifies \
         unconditionally (64), so nothing downcasts to this type and arming it could only \
         contradict a decision already taken. No `From<ScopeError>` exists anywhere",
    ),
    (
        "ocx_package/src/metadata/authoring.rs",
        "D::Error",
        "B8: `reject_retired_platform`, a `#[serde(deserialize_with)]` helper. `D::Error` is \
         `<D as Deserializer<'de>>::Error` — a generic associated type under a bare \
         `D: serde::Deserializer<'de>` bound, so there is no concrete type a `downcast_arm!` could \
         name; serde's generated code is the only caller",
    ),
    (
        "ocx_package/src/metadata/visibility.rs",
        "D::Error",
        "B8: `deserialize_entry_visibility`, wired onto `Var::visibility`. The same generic \
         associated type as the row above, at a second site and therefore a second decision — and \
         the same answer: universally quantified over `D`, so it names no type to arm",
    ),
    (
        "ocx_package/src/version.rs",
        "BuildMetaError",
        "B8: `Version::with_build`. Its one production caller, `publisher::apply_build_meta`, \
         converts through `impl From<BuildMetaError> for ocx_package::error::Error` \
         (`#[error(transparent)] BuildMeta(#[from] ..)`), and `exit/ocx_package.rs` classifies \
         `Self::BuildMeta(_)` by name onto 65 — an explicit arm, not a wildcard. Arming the inner \
         type would add a second, closer classification for a value that already has one (DEC-23)",
    ),
    (
        "ocx_script/src/engine.rs",
        "ScriptError",
        "B8: `engine::evaluate`, `pub(super)` and reached only through `run_script` below, which \
         forwards its `Result` unchanged. Not a boundary in its own right — the row exists because \
         any non-private visibility counts as one",
    ),
    (
        "ocx_script/src/lib.rs",
        "ScriptError",
        "B8: `run_script`. `command/script_runner.rs` matches its `Err` arm by hand and returns \
         `Ok(ExitCode::Failure)` — 1, deliberately, with the comment saying `never code 2` — so the \
         value never enters the classifier chain at all. Arming it would register a type the \
         classification path cannot reach; the exit code here is a command decision",
    ),
    (
        "ocx_script/src/guard.rs",
        "GuardError",
        "B8: the Starlark path sandbox's three resolvers. `pub(super)` inside a private `mod guard`, \
         and every one of the five callers in `ocx_module.rs` does `.map_err(|e| e.to_string())` at \
         the call; no value of this type leaves the crate, let alone reaches the CLI",
    ),
    // The `ocx_sign` rows, WP-31. `SignError` and `VerifyError` are armed;
    // `SignErrorKind` and `VerifyErrorKind` are what they carry, and
    // `exit/ocx_sign.rs` implements `ClassifyErrorKind` on the Kind itself —
    // so a Kind's exit code is already decided there and is reached *through*
    // the armed wrapper. A `downcast_arm!` on the Kind would register a second
    // entry for a value that already has one, which is the DEC-23 hazard.
    //
    // Three routes carry a bare Kind out of this crate, and none of them is an
    // unclassified one:
    //
    // - Seven signatures hand it to `ocx_lib` or `ocx_cli` directly, through a
    //   type alias or a closure typed by one (`SubjectResolver`,
    //   `VerifySubjectResolver`, auto-verify's `get_or_try_init`). Every one
    //   wraps into the armed `SignError`/`VerifyError` at its own site, except
    //   `managed_config/publish.rs`, which renders it with `Display` into
    //   `ManagedConfigPublishError::TrustedRootInvalid` — armed in turn.
    // - `RefusedCandidate { reason: VerifyErrorKind }` carries a Kind on the
    //   **success** path as report data, and `command/package_sbom.rs` renders
    //   it with `Display` and `kind_detail()`. That is not an exit-code path at
    //   all, so arming the Kind would change nothing about it.
    // - `command/package_sign_common.rs::leg_exit_code` classifies a bare Kind
    //   by hand, for the one case the error path cannot express: a
    //   `--signature-format both` run where one leg failed and another did not,
    //   so the run is `Ok`. It calls `ClassifyErrorKind::exit_code` — the same
    //   impl the armed wrapper delegates to — so the code is identical either
    //   way, by construction rather than by coincidence.
    (
        "ocx_sign/src/attest/dsse.rs",
        "VerifyErrorKind",
        "WP-31: `DsseEnvelope::parse`. Reached only from `verify::dsse::verify_envelope` and \
         `attestation_sidecar::verify_layer`, both of which return the same Kind; the outermost \
         caller either wraps into `VerifyError` or records the value as a `RefusedCandidate` reason",
    ),
    (
        "ocx_sign/src/attest/predicate.rs",
        "serde_json::Error",
        "WP-31: `PredicateType::wrap` hands the serializer's error through verbatim (ADR 1.18 shape). \
         Its one production caller, `statement::build`, maps it to `SignErrorKind::Internal` \
         immediately, and `Internal` is the arm `exit/ocx_sign.rs` deliberately classifies as `None` \
         so the chain walker decides — exit 1 for a serializer that cannot fail on our own types",
    ),
    (
        "ocx_sign/src/attest/statement.rs",
        "SignErrorKind",
        "WP-31: `Statement::build`. One production caller, `attest::pipeline`, `?`s it into a \
         function whose `run` wraps with `SignError::new(identifier, kind)`",
    ),
    (
        "ocx_sign/src/attest/statement.rs",
        "VerifyErrorKind",
        "WP-31: `Statement::parse` and `binds_subject`, the same file and a different type, so a \
         separate decision. Both are reached from `verify::dsse::verify_envelope`, whose Err becomes \
         a `RefusedCandidate` reason on a scan and a `VerifyError` on a single-candidate verify",
    ),
    (
        "ocx_sign/src/sbom/cyclonedx.rs",
        "SbomError",
        "WP-31: `summarize_cyclonedx` is called from `command/package_sbom.rs` and rendered with \
         `Display` into `RefusedEntry { reason_kind: SUMMARY_FAILED }` one line later; the listing \
         still succeeded, so the command exits 0 and this value never reaches the classifier. \
         Arming it would be arming a type the exit path cannot see. That exit 0 is the contract for \
         a best-effort `--summary`, not an oversight — recorded here because the row is the only \
         place that says so",
    ),
    (
        "ocx_sign/src/sign/bundle.rs",
        "SignErrorKind",
        "WP-31: `SignedEnvelope::new` and `build_dsse_bundle`. Both are called only by \
         `sign::signer` and `sign::key_signer`, which return the same Kind into the pipeline that \
         wraps it",
    ),
    (
        "ocx_sign/src/sign/fulcio.rs",
        "SignErrorKind",
        "WP-31: `FulcioClient::request_certificate`. One production caller, `sign::signer`, in the \
         same Kind-returning chain the pipeline wraps",
    ),
    (
        "ocx_sign/src/sign/key_backend.rs",
        "KeyBackendError",
        "WP-31: `PemKeyBackend::{open, open_env, from_encrypted_pem}`. \
         `#[error(transparent)] KeyBackend(#[from] KeyBackendError)` on `SignErrorKind` is the sole \
         conversion, and `exit/ocx_sign.rs` matches inside that arm to pick the code — so the value \
         is classified by variant already, one level in",
    ),
    (
        "ocx_sign/src/sign/pipeline.rs",
        "SignErrorKind",
        "WP-31: `guard_dialed_endpoints` and `sign_target_from_resolution`. The second is the \
         `SubjectResolver` alias `ocx_lib`'s `tasks/resolve_subject.rs` implements, so a closure \
         written there has this Kind as its own Err — and `tasks/sign.rs` wraps it with \
         `SignError::new` before it goes further",
    ),
    (
        "ocx_sign/src/sign/referrers.rs",
        "SignErrorKind",
        "WP-31: `referrers_capability` and `attach_referrer`, called only by the sign and attest \
         pipelines in this crate",
    ),
    (
        "ocx_sign/src/sign/rekor.rs",
        "SignErrorKind",
        "WP-31: `RekorClient::{upload_entry, upload_dsse_entry}`, called only by `sign::signer` and \
         `sign::key_signer`",
    ),
    (
        "ocx_sign/src/sign/simplesigning_write.rs",
        "SignErrorKind",
        "WP-31: `append_layer` and `offline_bundle`. Both feed the two pipelines; `append_layer` is \
         also a leg whose failure can be the `first_failure` that `leg_exit_code` classifies by \
         hand, through the same `ClassifyErrorKind` impl",
    ),
    (
        "ocx_sign/src/simplesigning.rs",
        "serde_json::Error",
        "WP-31: `SimpleSigningClaim::to_signing_bytes`, the serializer for our own struct. \
         `sign::pipeline` maps it to `SignErrorKind::Internal` at the only production call; on a \
         `--signature-format both` run that leg's failure reaches `leg_exit_code`, whose `Internal` \
         arm hands the boxed cause to `classify_library_error` — no arm matches a `serde_json::Error` \
         and the walk ends at exit 1, which is what `Internal` means",
    ),
    (
        "ocx_sign/src/verify/attestation_sidecar.rs",
        "VerifyErrorKind",
        "WP-31: `read_attestation_sidecar_tag`. Its `AttestationSidecarScan` carries \
         `refused: Vec<RefusedCandidate>`, the success-path route above; the Err arm is matched by \
         `verify::pipeline`",
    ),
    (
        "ocx_sign/src/verify/dsse.rs",
        "VerifyErrorKind",
        "WP-31: `verify_envelope`, `verify_tlog_binding` and `enforce_builder_pin` — the DSSE \
         candidate checks. Every caller either records the Err as a `RefusedCandidate` reason or \
         `?`s it into a function the pipeline wraps",
    ),
    (
        "ocx_sign/src/verify/identity.rs",
        "VerifyErrorKind",
        "WP-31: `parse_certificate`, `matching_policies` and `matching_key_policies`. Same two \
         destinations; `candidates.rs` additionally discards the Err with a `let .. else`, which \
         classifies nothing",
    ),
    (
        "ocx_sign/src/verify/pipeline.rs",
        "VerifyErrorKind",
        "WP-31: `RekorKeyMemo::resolve`, `fetch_rekor_public_key_pem`, `pull_blob_capped` and \
         `verify_target_from_resolution`. Two cross the crate boundary bare — auto-verify's \
         `get_or_try_init` closure and the `VerifySubjectResolver` alias — and `ocx_lib` wraps both \
         with `VerifyError::new` at `tasks/{auto_verify,verify}.rs`",
    ),
    (
        "ocx_sign/src/verify/simplesigning_read.rs",
        "VerifyErrorKind",
        "WP-31: the sidecar reader — `read_sidecar_tag`, `read_sidecar_manifest`, `verify_layer`, \
         `logged_entry`, `layer_certificate`, `layer_chain`. `verify_layer` is the direct producer \
         of `SidecarScan.refused`, so this file is where the success-path route starts",
    ),
    (
        "ocx_sign/src/verify/tlog.rs",
        "VerifyErrorKind",
        "WP-31: `rekor_key`, `verify_set`, `verify_inclusion` and \
         `verify_integrated_time_within_certificate`. All four are per-candidate checks reached \
         from the sidecar reader and the pipeline, and land in the same two destinations",
    ),
    (
        "ocx_sign/src/verify/trust_resolve.rs",
        "VerifyErrorKind",
        "WP-31: `resolve_trust_root`. Two callers take the bare Kind outside this crate — \
         `ocx_lib`'s auto-verify closure, and `command/package_sign_common.rs:587`, which names it \
         `kind` and wraps it with `VerifyError::new` on the same line",
    ),
    (
        "ocx_sign/src/verify/trust_root.rs",
        "VerifyErrorKind",
        "WP-31: `TrustRoot::{load_embedded, load_trusted_root_json, with_rekor_key_pem}`. \
         `managed_config/publish.rs` is the one caller that does not wrap: it renders the Kind with \
         `Display` into `ManagedConfigPublishError::TrustedRootInvalid`, which is itself armed, so \
         the classification is that type's, deliberately",
    ),
    // WP-33 put `ocx_project` on the derived boundary, and all three reaches are
    // REVELATION: every signature below is byte-identical to the one at
    // `9d34070d`, verified line-for-line against
    // `crates/ocx_lib/src/project/{env,lock,registry}.rs`. They were invisible
    // only because `ocx_lib/src` is not a boundary subtree and the extracted
    // crate is. Arming any of them moves an exit code off the walker's
    // fall-through `Failure` (1), which is the change this split may not make.
    (
        "ocx_project/src/env.rs",
        "ProjectErrorKind",
        "WP-33 revelation: `from_table` returns a bare kind, which every caller inside the crate \
         wraps into a `ProjectError` before it can travel. Arming the kind would classify a value \
         that reaches the CLI only already wrapped, and would move the code for anything that \
         somehow did not",
    ),
    (
        "ocx_project/src/lock.rs",
        "ProjectErrorKind",
        "WP-33 revelation: `validate_canonical_platform_keys` is `pub(super)` and has no caller \
         outside `ocx_project/src`. Unarmed at `9d34070d` and unarmed now, same signature",
    ),
    (
        "ocx_project/src/registry.rs",
        "ocx_project/src/registry/error.rs::Error",
        "WP-33 revelation: `ProjectRegistry::register` and `live_projects` are genuinely crate- \
         external — `package_manager/tasks/render_toolchain.rs` calls `.register(..)`. The type \
         HAS a `ClassifyExitCode` impl; what it has no rung for is the downcast ladder, so a bare \
         one arriving as a `dyn Error` source exits 1 today and did at `9d34070d`. Adding the rung \
         is a behaviour decision for after the split, not a move",
    ),
    (
        "ocx_package_manager/src/activation.rs",
        "IdentityError",
        "WP-34 revelation: `ProjectIdentity::resolve` is genuinely crate-external — \
         `ocx_cli/src/command/shell_allow.rs:60` and three siblings `?` it. No arm registers the \
         type, so both its variants exit 1 through the walker fall-through; they did the same at \
         `9d34070d` as `ocx_lib::activation::IdentityError`, where `ocx_lib`\'s exclusion from \
         this scan is the only reason nothing said so. Arming it moves an exit code",
    ),
    (
        "ocx_package_manager/src/tasks/attest.rs",
        "PackageError",
        "WP-34 revelation: the batch tier classifies on the *kind*, never on the wrapper. \
         `PackageError` is `{identifier, kind}` and its own doc says so; the ladder arms \
         `PackageErrorKind` (`ocx_cli/src/exit/ocx_package_manager.rs:203`), which is what every \
         value of this type carries. Arming the wrapper too would give the pair two rungs and a \
         resolution order nothing states. Unarmed at `9d34070d`, same signature",
    ),
    (
        "ocx_package_manager/src/tasks/sbom.rs",
        "PackageError",
        "WP-34 revelation: `sbom_one`, the same wrapper-versus-kind split as `attest.rs` above. A \
         second site is a second decision (B6-B3), not an inherited one",
    ),
    (
        "ocx_package_manager/src/tasks/sign.rs",
        "PackageError",
        "WP-34 revelation: `sign_one` and `resolve_swept_index`, the same wrapper-versus-kind \
         split as `attest.rs` above",
    ),
    (
        "ocx_package_manager/src/tasks/sign.rs",
        "SignErrorKind",
        "WP-34 revelation: `build_signer` is `pub(super)` and has no caller outside \
         `ocx_package_manager/src/tasks`. Unarmed at `9d34070d` and unarmed now, same signature",
    ),
    (
        "ocx_package_manager/src/tasks/verify.rs",
        "PackageError",
        "WP-34 revelation: `verify_one`, the same wrapper-versus-kind split as `attest.rs` above",
    ),
    (
        "ocx_setup/src/session_path.rs",
        "SessionPathError",
        "WP-36 revelation: the session-PATH writers hand this up to `setup::run`, which is the only caller, and `SetupError::SessionPath` carries a `#[from]` for it whose `classify` arm delegates straight back to this type's own impl. So the value reaching the CLI is always the armed `SetupError`, and a rung of its own would be a second, closer classification for a value that already has one — DEC-23. Unarmed at `9d34070d`, byte-identical signature: the scan sees it only because WP-36 moved the tree out of the excluded `ocx_lib`",
    ),
    (
        "ocx_setup/src/session_path/linux.rs",
        "SessionPathError",
        "WP-36 revelation: the session-PATH writers hand this up to `setup::run`, which is the only caller, and `SetupError::SessionPath` carries a `#[from]` for it whose `classify` arm delegates straight back to this type's own impl. So the value reaching the CLI is always the armed `SetupError`, and a rung of its own would be a second, closer classification for a value that already has one — DEC-23. Unarmed at `9d34070d`, byte-identical signature: the scan sees it only because WP-36 moved the tree out of the excluded `ocx_lib`",
    ),
    (
        "ocx_setup/src/session_path/macos.rs",
        "SessionPathError",
        "WP-36 revelation: the session-PATH writers hand this up to `setup::run`, which is the only caller, and `SetupError::SessionPath` carries a `#[from]` for it whose `classify` arm delegates straight back to this type's own impl. So the value reaching the CLI is always the armed `SetupError`, and a rung of its own would be a second, closer classification for a value that already has one — DEC-23. Unarmed at `9d34070d`, byte-identical signature: the scan sees it only because WP-36 moved the tree out of the excluded `ocx_lib`",
    ),
    (
        "ocx_setup/src/session_path/windows.rs",
        "SessionPathError",
        "WP-36 revelation: the session-PATH writers hand this up to `setup::run`, which is the only caller, and `SetupError::SessionPath` carries a `#[from]` for it whose `classify` arm delegates straight back to this type's own impl. So the value reaching the CLI is always the armed `SetupError`, and a rung of its own would be a second, closer classification for a value that already has one — DEC-23. Unarmed at `9d34070d`, byte-identical signature: the scan sees it only because WP-36 moved the tree out of the excluded `ocx_lib`",
    ),
];

/// The one type name this workspace cannot be keyed on (B5R-1, DEC-27).
///
/// DEC-27 has every crate mint its own root `Error`: `ocx_lib` already declares
/// ten, `ocx_util` three, and fourteen more arrive with the remaining
/// extractions. The `ocx_cli` ladder imports each under a rename — `UtilError`,
/// `ArchiveError`, `CompressionError` — that no library signature ever writes,
/// and a one-argument `Result<T>` names no type at its use site at all. Keyed
/// on the bare last segment, all of them collapse onto the single string
/// `Error`, which `downcast_arm!(cause, ocx_lib::Error)` arms — so every
/// `pub fn` under `crates/ocx_util/src` returning the alias discharged against
/// an arm for a *different crate's* type, and deleting the two arms that really
/// serve them left this guard green.
///
/// So a reach or an arm whose declared name is `Error` is keyed on the file
/// that declares it, resolved through `use` re-exports, and an `Error` whose
/// declaration cannot be found never discharges anything. A distinctly named
/// type stays its own key: `SymlinkWalkError` names one type in this workspace,
/// and resolving it would buy nothing while risking a one-sided failure.
const AMBIGUOUS_ERROR_NAME: &str = "Error";

/// A type as a `pub` signature or a `downcast_arm!` names it: the name at its
/// declaration — not at the use site, which may be a rename — and the file that
/// declares it when the workspace does.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Resolved {
    /// The name at the declaration — or, unresolved, the one written.
    name: String,
    /// The file declaring it, when a crate in this workspace does.
    site: Option<PathBuf>,
    /// The path as written at the use site, which is all a type the workspace
    /// does not declare has: `std::io::Error` is the same string from the
    /// signature and from the arm, and keys the two together.
    written: String,
}

impl Resolved {
    /// Unresolved, under the path written at the use site.
    fn bare(segments: &[String]) -> Self {
        Self {
            name: segments.last().cloned().unwrap_or_default(),
            site: None,
            written: segments.join("::"),
        }
    }

    /// The string the two sides are compared on — see [`AMBIGUOUS_ERROR_NAME`].
    fn key(&self) -> String {
        if self.name != AMBIGUOUS_ERROR_NAME {
            return self.name.clone();
        }
        match &self.site {
            Some(site) => format!("{}::{}", under_crates(site), self.name),
            None if self.written.contains("::") => self.written.clone(),
            // Deliberately unmatchable: an arm keys on a declaration it found
            // or on a path it wrote, so an `Error` with neither is reported
            // rather than discharged.
            None => format!("{} <declaration unresolved>", self.name),
        }
    }
}

/// `path` written relative to `crates/`, with `/` separators on every platform.
fn under_crates(path: &Path) -> String {
    path.strip_prefix(crates_dir())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The `src/` directory of the crate `file` belongs to — the nearest ancestor
/// holding a `lib.rs` or `main.rs`. `None` for a loose fixture, which then
/// resolves nothing and keys on bare names.
fn crate_src_root_of(file: &Path) -> Option<PathBuf> {
    let mut dir = file.parent()?;
    loop {
        if dir.join("lib.rs").is_file() || dir.join("main.rs").is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// The module path of `file` under `root`: `src/a/b.rs` → `[a, b]`,
/// `src/a/mod.rs` → `[a]`, `src/lib.rs` → `[]`.
fn module_path_of(file: &Path, root: &Path) -> Vec<String> {
    let Ok(relative) = file.strip_prefix(root) else {
        return Vec::new();
    };
    let mut segments: Vec<String> = relative
        .with_extension("")
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    if matches!(segments.last().map(String::as_str), Some("lib" | "main" | "mod")) {
        segments.pop();
    }
    segments
}

/// The file holding module `modules` of the crate rooted at `root`.
fn module_file(root: &Path, modules: &[String]) -> Option<PathBuf> {
    if modules.is_empty() {
        let lib = root.join("lib.rs");
        return lib.is_file().then_some(lib);
    }
    let base = modules
        .iter()
        .fold(root.to_path_buf(), |path, segment| path.join(segment));
    let flat = base.with_extension("rs");
    if flat.is_file() {
        return Some(flat);
    }
    let nested = base.join("mod.rs");
    nested.is_file().then_some(nested)
}

/// The file the module path `prefix` names, read from inside `from`.
///
/// `crate` and `super` resolve against `from`'s own module path; a leading
/// segment naming a directory under `crates/` is that crate; anything else is a
/// child module of `from`. A path that leaves the workspace — `std`, a registry
/// dependency — resolves to nothing, which is the honest answer.
fn module_file_from(from: &Path, prefix: &[String]) -> Option<PathBuf> {
    let root = crate_src_root_of(from)?;
    let here = module_path_of(from, &root);
    let first = prefix.first()?;
    let (root, modules) = match first.as_str() {
        "crate" => (root, prefix[1..].to_vec()),
        // `expand_use_tree` keeps a leading `self` since B5R-8; it names this
        // file's own module, the resolution `super` gets with no ups.
        "self" => (root, here.iter().chain(prefix[1..].iter()).cloned().collect::<Vec<_>>()),
        "super" => {
            let ups = prefix.iter().take_while(|segment| *segment == "super").count();
            let base = here.len().checked_sub(ups)?;
            let modules = here[..base]
                .iter()
                .chain(prefix[ups..].iter())
                .cloned()
                .collect::<Vec<_>>();
            (root, modules)
        }
        other => {
            let sibling = crates_dir().join(other).join("src");
            if sibling.is_dir() {
                (sibling, prefix[1..].to_vec())
            } else {
                (root, here.iter().chain(prefix.iter()).cloned().collect::<Vec<_>>())
            }
        }
    };
    module_file(&root, &modules)
}

/// The declaration `segments` names, as written in `from`.
///
/// Both sides of the armed-error guard resolve through this one function, so
/// `use ocx_util::error::Error as UtilError` in the ladder and a bare
/// `Result<T>` in `ocx_util` land on the same `(file, name)` pair. Resolution
/// that runs out — a glob, a type from outside the workspace, a cycle — falls
/// back to the name written at the use site.
fn resolve_type(from: &Path, segments: &[String]) -> Resolved {
    let mut seen = BTreeSet::new();
    resolve_from(from, segments, &mut seen).unwrap_or_else(|| Resolved::bare(&canonical_path(from, segments)))
}

/// `segments` with its first segment expanded through `from`'s own `use`
/// statements.
///
/// A type the workspace does not declare keys on the path written, and the
/// same type is written differently in different files: `use std::io;` then
/// `io::Result<T>` in `ocx_console` against `downcast_ref::<std::io::Error>()`
/// in the ladder are one type and were two keys, so widening the subtree set
/// to `ocx_console` reported two reaches that are armed. Expanding the head
/// puts both on `std::io::Error`. A path whose head names no import — `std`
/// itself, a module of this crate — is already canonical.
fn canonical_path(from: &Path, segments: &[String]) -> Vec<String> {
    let Some(first) = segments.first() else {
        return segments.to_vec();
    };
    if !from.is_file() {
        return segments.to_vec();
    }
    for item in Source::parse(from).file.items {
        let syn::Item::Use(import) = item else { continue };
        if is_cfg_test(&import.attrs) {
            continue;
        }
        for (path, alias) in expand_use_tree(&import.tree) {
            let Some(last) = path.last() else { continue };
            if last == "*" || path.len() < 2 {
                continue;
            }
            if alias.as_deref().unwrap_or(last) == first {
                return path.iter().chain(segments[1..].iter()).cloned().collect();
            }
        }
    }
    segments.to_vec()
}

fn resolve_from(from: &Path, segments: &[String], seen: &mut BTreeSet<(PathBuf, String)>) -> Option<Resolved> {
    let (name, prefix) = segments.split_last()?;
    if name == "*" {
        return None;
    }
    let start = if prefix.is_empty() {
        from.to_path_buf()
    } else {
        module_file_from(from, prefix)?
    };
    resolve_in_file(&start, name, seen)
}

/// Whether `file` declares `name` itself, and otherwise the `use` it re-exports
/// it through. `#[cfg(test)]` items are skipped for the reason the boundary
/// scan skips them: they reach no binary.
fn resolve_in_file(file: &Path, name: &str, seen: &mut BTreeSet<(PathBuf, String)>) -> Option<Resolved> {
    if !file.is_file() || !seen.insert((file.to_path_buf(), name.to_owned())) {
        return None;
    }
    let parsed = Source::parse(file).file;
    for item in &parsed.items {
        let declared = match item {
            syn::Item::Struct(declaration) if !is_cfg_test(&declaration.attrs) => Some(&declaration.ident),
            syn::Item::Enum(declaration) if !is_cfg_test(&declaration.attrs) => Some(&declaration.ident),
            syn::Item::Type(declaration) if !is_cfg_test(&declaration.attrs) => Some(&declaration.ident),
            syn::Item::Union(declaration) if !is_cfg_test(&declaration.attrs) => Some(&declaration.ident),
            _ => None,
        };
        if declared.is_some_and(|ident| ident == name) {
            return Some(Resolved {
                name: name.to_owned(),
                site: Some(file.to_path_buf()),
                written: name.to_owned(),
            });
        }
    }
    for item in &parsed.items {
        let syn::Item::Use(import) = item else { continue };
        if is_cfg_test(&import.attrs) {
            continue;
        }
        for (segments, alias) in expand_use_tree(&import.tree) {
            let Some(last) = segments.last() else { continue };
            let bound = alias.as_deref().unwrap_or(last);
            if bound != name || segments.len() < 2 {
                continue;
            }
            if let Some(resolved) = resolve_from(file, &segments, seen) {
                return Some(resolved);
            }
        }
    }
    None
}

/// The error type a one-argument `Result<T>` written in `file` names.
///
/// The alias is resolved rather than assumed: `ocx_util` declares three
/// `type Result<T>` pointing at three different types all named `Error`, and
/// `fs/dir_walker.rs` declares a fourth pointing at `FileError`. Reading the
/// declaration is what tells them apart — the list of alias files this replaced
/// could not, and was one edit from being short besides (DEC-22).
fn resolve_result_alias(file: &Path, prefix: &[String]) -> Resolved {
    // A `Result` alias the workspace does not declare — `std::io::Result` is
    // the one this tree writes — carries the sibling `Error` of its own module
    // by the convention every such alias in std and the registry follows. Keyed
    // on the written path, so the longhand `downcast_ref::<std::io::Error>` arm
    // keys identically; an alias that broke the convention would key on a path
    // no arm carries, which reds.
    let unresolved = || {
        if prefix.is_empty() {
            Resolved::bare(&[format!("Result<T> in {} <alias unresolved>", under_crates(file))])
        } else {
            let mut path = prefix.to_vec();
            path.push(AMBIGUOUS_ERROR_NAME.to_owned());
            Resolved::bare(&canonical_path(file, &path))
        }
    };
    let mut segments = prefix.to_vec();
    segments.push("Result".to_owned());
    alias_error_type(file, &segments).unwrap_or_else(unresolved)
}

/// The error type of the one-argument alias `segments` names, read off its
/// declaration — `type Result<T> = std::result::Result<T, ClientError>` is
/// `ClientError` whatever the alias is spelled at the use site.
///
/// `None` when the path resolves to no declaration in the workspace, or to one
/// that is not a two-argument `Result` — which is how a `pub fn` returning
/// `Option<T>` or `Vec<T>` is not mistaken for a boundary crossing.
fn alias_error_type(file: &Path, segments: &[String]) -> Option<Resolved> {
    let resolved = resolve_type(file, segments);
    let site = resolved.site?;
    for item in Source::parse(&site).file.items {
        let syn::Item::Type(alias) = item else { continue };
        if alias.ident != resolved.name.as_str() || is_cfg_test(&alias.attrs) {
            continue;
        }
        let syn::Type::Path(typed) = alias.ty.as_ref() else {
            continue;
        };
        let last = typed.path.segments.last()?;
        if last.ident != "Result" {
            continue;
        }
        let syn::PathArguments::AngleBracketed(arguments) = &last.arguments else {
            continue;
        };
        let named: Vec<&syn::Type> = arguments
            .args
            .iter()
            .filter_map(|argument| match argument {
                syn::GenericArgument::Type(inner) => Some(inner),
                _ => None,
            })
            .collect();
        let [_, error] = named.as_slice() else { continue };
        return Some(resolve_named_type(&site, error));
    }
    None
}

/// `ty` resolved to its declaration, or rendered whole when it is not a path —
/// a reference or a boxed trait object has no segments to follow, and a name no
/// arm can write is exactly the shape this guard exists to surface.
fn resolve_named_type(from: &Path, ty: &syn::Type) -> Resolved {
    match ty {
        syn::Type::Path(typed) => {
            let segments: Vec<String> = typed
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect();
            resolve_type(from, &segments)
        }
        _ => Resolved::bare(&[quote_type(ty)]),
    }
}

/// Every type named by a `downcast_arm!` under `crates/ocx_cli/src/`, keyed the
/// way [`Resolved::key`] keys the boundary scan's reaches.
///
/// Read off the invocations rather than a maintained list, for the reason
/// DEC-22 gives: a list is one edit away from being short, and the guard that
/// reads it cannot notice the entry that was never added. The `macro_rules!`
/// definition in `exit.rs` is not an invocation and does not match — its own
/// path is `macro_rules`.
///
/// The whole path is kept, not its last segment: the ladder writes renamed
/// imports (`UtilError`) and crate-qualified paths (`ocx_lib::Error`), and
/// which declaration each one names is the question B5R-1 turned on.
///
/// `#[cfg(test)]` code is skipped, matching [`boundary_error_types`] and
/// [`DeclaredTypes::absorb`]: an arm that reaches no binary arms nothing, and
/// counting it would let a test-only `downcast_arm!` discharge a real reach
/// (B5R-6).
fn armed_error_types() -> BTreeSet<String> {
    struct Arms {
        paths: Vec<Vec<String>>,
        /// Whether a bare `downcast_ref::<T>()` counts as an arm in this file.
        longhand: bool,
    }
    impl Visit<'_> for Arms {
        /// The one arm the sugar cannot express. `std::io::Error` is not
        /// OCX-owned, so `ClassifyExitCode` cannot be implemented for it
        /// (orphan rule) and `exit/classify.rs` downcasts it by hand — a form a
        /// scanner that reads only `downcast_arm!` sees as *absent*, which is
        /// B5R-1's defect in miniature. Counted only under `exit/`, where every
        /// downcast is a classification: `error_envelope.rs` downcasts to
        /// render, not to pick an exit code.
        ///
        /// The question this answers is whether the ladder names the type at
        /// all, not whether it classifies every value of it — the io arm codes
        /// only `PermissionDenied` and lets the rest fall through, which is the
        /// behaviour at `7adaea62` and not this guard's to change.
        fn visit_expr_method_call(&mut self, call: &syn::ExprMethodCall) {
            if self.longhand
                && call.method == "downcast_ref"
                && let Some(turbofish) = &call.turbofish
                && let Some(syn::GenericArgument::Type(syn::Type::Path(typed))) = turbofish.args.first()
            {
                self.paths.push(
                    typed
                        .path
                        .segments
                        .iter()
                        .map(|segment| segment.ident.to_string())
                        .collect(),
                );
            }
            syn::visit::visit_expr_method_call(self, call);
        }

        fn visit_item_mod(&mut self, module: &syn::ItemMod) {
            if !is_cfg_test(&module.attrs) {
                syn::visit::visit_item_mod(self, module);
            }
        }
        fn visit_item_fn(&mut self, function: &syn::ItemFn) {
            if !is_cfg_test(&function.attrs) {
                syn::visit::visit_item_fn(self, function);
            }
        }
        fn visit_impl_item_fn(&mut self, function: &syn::ImplItemFn) {
            if !is_cfg_test(&function.attrs) {
                syn::visit::visit_impl_item_fn(self, function);
            }
        }
        fn visit_item_impl(&mut self, block: &syn::ItemImpl) {
            if !is_cfg_test(&block.attrs) {
                syn::visit::visit_item_impl(self, block);
            }
        }
        fn visit_macro(&mut self, invocation: &syn::Macro) {
            if invocation
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "downcast_arm")
            {
                let tokens: Vec<Token> = flatten(invocation.tokens.clone());
                // `cause , <path :: to :: Type>` — the type is everything after
                // the one separating comma, and its identifiers are its path.
                if let Some(comma) = tokens.iter().position(|token| token.text == ",") {
                    let path: Vec<String> = tokens[comma + 1..]
                        .iter()
                        .filter(|token| is_identifier(&token.text))
                        .map(|token| token.text.clone())
                        .collect();
                    if !path.is_empty() {
                        self.paths.push(path);
                    }
                }
            }
            syn::visit::visit_macro(self, invocation);
        }
    }

    let mut armed = BTreeSet::new();
    let ladder = crates_dir().join("ocx_cli/src/exit");
    let sources = rust_sources(&crates_dir().join("ocx_cli/src"));
    assert!(
        sources.len() > 1,
        "crates/ocx_cli/src walked {} file(s) — every type would then read as unarmed",
        sources.len()
    );
    for file in sources {
        let mut arms = Arms {
            paths: Vec::new(),
            longhand: file.starts_with(&ladder),
        };
        arms.visit_file(&Source::parse(&file).file);
        armed.extend(arms.paths.iter().map(|path| resolve_type(&file, path).key()));
    }
    // A floor, not just non-emptiness: an extractor that stopped recognising
    // *some* invocations would leave the rest of the ladder registered and the
    // scan below would only red on types it happened to drop.
    assert!(
        armed.len() > 40,
        "the downcast ladder yielded only {} armed type(s) — the extractor stopped reading \
         `downcast_arm!` invocations",
        armed.len()
    );
    armed
}

fn is_identifier(text: &str) -> bool {
    text.starts_with(|c: char| c.is_alphabetic() || c == '_')
}

/// The error type of a `Result<..>` return, with the line its `Result` sits on.
///
/// A one-argument `Result<T>` is an alias and is followed to the declaration it
/// names; a two-argument one is resolved the same way from the file that writes
/// it, so a rename at either end lands on the same key as the ladder's arm.
fn result_error_type(from: &Path, ty: &syn::Type) -> Option<(Resolved, usize)> {
    let syn::Type::Path(typed) = ty else { return None };
    let last = typed.path.segments.last()?;
    let syn::PathArguments::AngleBracketed(arguments) = &last.arguments else {
        return None;
    };
    let prefix: Vec<String> = typed
        .path
        .segments
        .iter()
        .take(typed.path.segments.len() - 1)
        .map(|segment| segment.ident.to_string())
        .collect();
    let types: Vec<&syn::Type> = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            syn::GenericArgument::Type(inner) => Some(inner),
            _ => None,
        })
        .collect();
    let line = last.ident.span().start().line;
    match (last.ident == "Result", types.as_slice()) {
        (true, [_]) => Some((resolve_result_alias(from, &prefix), line)),
        (true, [_, error]) => Some((resolve_named_type(from, error), line)),
        // A one-argument `Result` alias imported under another name — `use
        // client::Result as ClientResult`, or a crate that declares
        // `type ClientResult<T>` outright. A `pub fn` returning it crosses
        // exactly as one returning `Result<T>` does, and a scan matching the
        // literal spelling sees none of them. Recognised by the declaration:
        // a one-argument generic that resolves to no `type _<T> = Result<T, E>`
        // is not a boundary crossing, which is how `Option<T>` stays out.
        (false, [_]) => {
            let mut path = prefix.clone();
            path.push(last.ident.to_string());
            alias_error_type(from, &path).map(|resolved| (resolved, line))
        }
        _ => None,
    }
}

/// The last path segment of `ty`, or the whole type rendered when it has none.
fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(typed) => typed
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_else(|| quote_type(ty)),
        _ => quote_type(ty),
    }
}

fn quote_type(ty: &syn::Type) -> String {
    use ocx_test_support::quote::ToTokens as _;
    ty.to_token_stream().to_string().replace(' ', "")
}

/// Whether `attributes` carry a `derive` naming `Error`.
///
/// Matched on the derive's last path segment, so `thiserror::Error` — the one
/// spelling this workspace writes — and a bare imported `Error` both count.
fn derives_error(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("derive")
            && matches!(&attribute.meta, syn::Meta::List(list)
                if flatten(list.tokens.clone()).iter().any(|token| token.text == "Error"))
    })
}

/// Which types a subtree declares, and which of them implement
/// `std::error::Error` — the two sets [`DeclaredTypes::admits`] decides on.
#[derive(Default)]
struct DeclaredTypes {
    declared: BTreeSet<String>,
    implementing: BTreeSet<String>,
}

impl DeclaredTypes {
    /// Whether an error type named in a `pub` signature can reach the `ocx_cli`
    /// chain walker at all.
    ///
    /// The walker only ever sees `&dyn std::error::Error` — that is what
    /// `anyhow`'s `chain()` yields — so a type off the trait cannot be
    /// classified, cannot be *mis*classified, and is not this guard's subject
    /// however a signature returns it. Three rules:
    ///
    /// - declared here and implementing `Error`: in scope.
    /// - declared here and not implementing it: out of scope. Worked example —
    ///   `ocx_util::tls::PemBundleError` is `#[derive(Debug, Clone, PartialEq,
    ///   Eq)]`, its own doc says it stays off the trait deliberately so it
    ///   cannot enter a chain the CLI walks, and `config::tls::parse_pem` maps
    ///   each variant onto an armed `TlsError` arm. Filing it in
    ///   [`UNARMED_AT_THE_BOUNDARY`] instead would record a value type as an
    ///   unarmed hazard and make the exemption list the thing nobody can trust.
    /// - declared elsewhere: in scope, and it reds unless armed. A type this
    ///   subtree does not own cannot be judged from its source, and for a guard
    ///   whose subject is the type nobody remembered, loud is the safe
    ///   direction.
    fn admits(&self, name: &str) -> bool {
        self.implementing.contains(name) || !self.declared.contains(name)
    }

    /// Fold one parsed file's type declarations and `impl std::error::Error`
    /// blocks in.
    ///
    /// `#[cfg(test)]` code is skipped for the reason the boundary scan skips
    /// it: it reaches no binary, and a test-only type sharing a name with a
    /// shipping one would otherwise decide the rule for both.
    fn absorb(&mut self, file: &syn::File) {
        struct Scan<'a>(&'a mut DeclaredTypes);
        impl Scan<'_> {
            fn record(&mut self, name: &syn::Ident, attributes: &[syn::Attribute]) {
                let name = name.to_string();
                if derives_error(attributes) {
                    self.0.implementing.insert(name.clone());
                }
                self.0.declared.insert(name);
            }
        }
        impl<'ast> Visit<'ast> for Scan<'_> {
            fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
                if !is_cfg_test(&module.attrs) {
                    syn::visit::visit_item_mod(self, module);
                }
            }
            fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
                if !is_cfg_test(&item.attrs) {
                    self.record(&item.ident, &item.attrs);
                }
            }
            fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
                if !is_cfg_test(&item.attrs) {
                    self.record(&item.ident, &item.attrs);
                }
            }
            fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
                if is_cfg_test(&block.attrs) {
                    return;
                }
                if let Some((_, path, _)) = &block.trait_
                    && path.segments.last().is_some_and(|segment| segment.ident == "Error")
                {
                    self.0.implementing.insert(type_name(&block.self_ty));
                }
                syn::visit::visit_item_impl(self, block);
            }
        }
        Scan(self).visit_file(file);
    }
}

/// Every type `subtree` declares, and which of them are errors.
///
/// The floor is **derived, not tuned** (DEC-39), and it targets one failure.
///
/// A *dead* collector is not the hazard: with `declared` empty too,
/// [`DeclaredTypes::admits`] admits every name and the scan reds loudly. The
/// quiet one is a collector that keeps reading declarations and stops
/// recognising the `Error` impl — every declared type then falls out of scope
/// unjudged and the scan goes permissively green, the state a check that never
/// ran is in.
///
/// So the parsed set is checked against an independent reading of the same
/// source: the token `thiserror`, the one spelling this workspace derives with,
/// found by the flat token list rather than by any `syn` visitor. A subtree
/// that derives none is a fact about that crate — `ocx_exit` declares two types
/// and no error at all — not a failure, which is what lets DEC-35's widening
/// stay one entry. The `declared > 20 && implementing > 8` this replaces was
/// read off `ocx_util`, and would have refused `ocx_exit` and `ocx_console`
/// alike: a threshold calibrated to the first crate the guard met rejects
/// exactly the entries DEC-35 exists to add.
///
/// The walk is asserted for the reason every walk here is: a subtree that has
/// moved leaves an empty directory, and an empty directory judges nothing.
fn declared_types(subtree: &Path) -> DeclaredTypes {
    let files = rust_sources(subtree);
    assert!(
        !files.is_empty(),
        "{} walked no .rs file — the subtree has moved or was never there, and every type it \
         should have judged falls out of scope",
        subtree.display()
    );
    let mut types = DeclaredTypes::default();
    let mut derives_anywhere = false;
    for file in &files {
        let source = Source::parse(file);
        derives_anywhere |= source.tokens.iter().any(|token| token.text == "thiserror");
        types.absorb(&source.file);
    }
    assert!(
        !derives_anywhere || !types.implementing.is_empty(),
        "{} derives `thiserror::Error` somewhere across {} file(s), and the collector found none of \
         the {} type(s) it declares implementing `std::error::Error` — every one of them then falls \
         out of scope unjudged and this guard passes over a subtree it stopped reading",
        subtree.display(),
        files.len(),
        types.declared.len()
    );
    types
}

/// The error type every `pub` function in `file` returns, as `Reach`es whose
/// `path` is that type's name — the atom the witness has to keep producing.
///
/// `pub` is the conservative reading of "can cross into `ocx_cli`": a `pub fn`
/// inside a private module is unreachable from outside the crate, but deciding
/// that needs the module graph, and over-reporting costs a scan that names one
/// extra type while under-reporting costs a silently moved exit code. Test code
/// is skipped — a `#[cfg(test)]` signature reaches no binary. `declared` then
/// drops what cannot reach the chain walker at all, per
/// [`DeclaredTypes::admits`].
fn boundary_error_types(path: &Path, file: &syn::File, declared: &DeclaredTypes) -> Vec<Reach> {
    struct Scan<'a> {
        file: &'a Path,
        declared: &'a DeclaredTypes,
        out: Vec<Reach>,
    }
    impl Scan<'_> {
        fn record(&mut self, visibility: &syn::Visibility, signature: &syn::Signature) {
            if matches!(visibility, syn::Visibility::Inherited) {
                return;
            }
            let syn::ReturnType::Type(_, returned) = &signature.output else {
                return;
            };
            if let Some((resolved, line)) = result_error_type(self.file, returned)
                && self.declared.admits(&resolved.name)
            {
                self.out.push(Reach {
                    file: self.file.to_path_buf(),
                    line,
                    path: resolved.key(),
                });
            }
        }
    }
    impl<'ast> Visit<'ast> for Scan<'_> {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if !is_cfg_test(&module.attrs) {
                syn::visit::visit_item_mod(self, module);
            }
        }
        fn visit_item_impl(&mut self, block: &'ast syn::ItemImpl) {
            if !is_cfg_test(&block.attrs) {
                syn::visit::visit_item_impl(self, block);
            }
        }
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            if is_cfg_test(&function.attrs) {
                return;
            }
            self.record(&function.vis, &function.sig);
            syn::visit::visit_item_fn(self, function);
        }
        fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
            if is_cfg_test(&function.attrs) {
                return;
            }
            self.record(&function.vis, &function.sig);
            syn::visit::visit_impl_item_fn(self, function);
        }
    }
    let mut scan = Scan {
        file: path,
        declared,
        out: Vec::new(),
    };
    scan.visit_file(file);
    scan.out
}

/// Every error type the boundary scan finds under `subtree` that `armed` does
/// not carry — the exclusion list's own subject, before it is applied.
fn unarmed_boundary_types(
    subtree: &Path,
    armed: &BTreeSet<String>,
    declared: &DeclaredTypes,
) -> BTreeSet<(String, String)> {
    rust_sources(subtree)
        .into_iter()
        .flat_map(|file| boundary_error_types(&file, &Source::parse(&file).file, declared))
        .filter(|reach| !armed.contains(&reach.path))
        .map(|reach| (under_crates(&reach.file), reach.path))
        .collect()
}

/// Every error type a `pub fn` under `crates/ocx_lib/src/utility/` hands the
/// CLI is registered in the `ocx_cli` downcast ladder (DEC-23, B3-13).
///
/// **What the existing tests cannot do.** `exit/classify.rs:456` says the
/// mechanism outright: `try_classify` is a hand-written downcast ladder with no
/// compile-time guard, and a type with no arm falls through to `Failure` (1).
/// Every classification test in the tree is written against a type *someone
/// remembered to arm* — they pin the arms that exist and are structurally
/// incapable of failing for a type that has none, which is precisely the state
/// `cargo nextest` was in, 8190/8190 green, while DEC-23 was shipped and only
/// the acceptance suite saw it. `every_classification_matches_the_pre_split_baseline`
/// is the same direction one step wider: it pins the arms the tree *has*
/// against the arms it had at `7adaea62`.
///
/// This is the other direction. Neither side is a list: the obligation is
/// derived from `ocx_lib`'s own signatures and the discharge is derived from the
/// ladder's own invocations, so a package that re-points a signature at a new
/// error type grows the left side by itself and reds until the arm exists.
///
/// **The two alternatives, and why not.** An *exhaustive match* cannot be
/// written — `downcast_ref` over `dyn Error` has no exhaustiveness, so the
/// compiler would need the list of types enumerated somewhere, which is the
/// thing that can be short. A *shared registry* the ladder is generated from and
/// the test reads is worse: both sides then read the same list, so a type
/// missing from it is missing from both and the check is green in exactly the
/// broken state — the defect DEC-22 already named in the pin's old file list.
#[test]
fn every_utility_error_reaching_the_cli_is_armed() {
    let armed = armed_error_types();
    let subtrees: Vec<PathBuf> = boundary_subtrees();
    assert!(
        !subtrees.is_empty(),
        "boundary_subtrees is empty — the guard would judge nothing and pass"
    );

    // Per subtree, never a union: `DeclaredTypes::admits` rules on whether the
    // *owning* crate keeps a type off `std::error::Error`, and a union lets one
    // crate's value type decide another crate's error out of scope. `scoped`
    // picks the set by the longest matching subtree; a file under none — the
    // witness fixture — is judged by the empty set, which admits everything and
    // is the loud direction.
    let declared: Vec<(PathBuf, DeclaredTypes)> = subtrees
        .iter()
        .map(|subtree| (subtree.clone(), declared_types(subtree)))
        .collect();
    let empty = DeclaredTypes::default();
    let scoped = |path: &Path| -> &DeclaredTypes {
        declared
            .iter()
            .filter(|(subtree, _)| path.starts_with(subtree))
            .max_by_key(|(subtree, _)| subtree.as_os_str().len())
            .map_or(&empty, |(_, types)| types)
    };

    // The resolver must still be resolving, stated as a property of each reach
    // rather than as a count (DEC-39): a threshold read off `ocx_util` is the
    // same fossil the floor above was, and it would have to be re-tuned by the
    // very commit DEC-35 wants to be one line. A resolver that stopped
    // resolving keys every `Error` on a string no arm carries, so the reach is
    // reported by name here instead of quietly rejoining the bare `Error` that
    // `downcast_arm!(cause, ocx_lib::Error)` discharges (B5R-1).
    let reaches: Vec<Reach> = subtrees
        .iter()
        .flat_map(|subtree| rust_sources(subtree))
        .flat_map(|file| boundary_error_types(&file, &Source::parse(&file).file, scoped(&file)))
        .collect();
    let unresolved: Vec<&Reach> = reaches.iter().filter(|reach| reach.path.contains('<')).collect();
    assert!(
        unresolved.is_empty(),
        "the `Result<T>` alias chain resolved nothing for {} of {} reach(es): {unresolved:?}",
        unresolved.len(),
        reaches.len()
    );

    let unarmed: BTreeSet<(String, String)> = subtrees
        .iter()
        .flat_map(|subtree| unarmed_boundary_types(subtree, &armed, scoped(subtree)))
        .collect();
    let stale: Vec<String> = UNARMED_AT_THE_BOUNDARY
        .iter()
        .filter(|(file, name, _)| !unarmed.contains(&((*file).to_owned(), (*name).to_owned())))
        .map(|(file, name, _)| format!("{file}::{name}"))
        .collect();
    assert!(
        stale.is_empty(),
        "UNARMED_AT_THE_BOUNDARY still excludes {stale:?}, which the scan over {subtrees:?} \
         no longer finds — armed since, renamed, or gone. An exclusion whose target does not exist \
         forbids nothing and reads as coverage; delete the row"
    );

    let exempt: BTreeSet<(&str, &str)> = UNARMED_AT_THE_BOUNDARY
        .iter()
        .map(|(file, name, _)| (*file, *name))
        .collect();
    assert_scan(
        &subtrees,
        &fixture("unarmed_boundary_error.rs.txt"),
        "a `pub fn` returning an error type no `downcast_arm!` in crates/ocx_cli/src registers",
        &["NeverArmedError"],
        &|path, source| {
            boundary_error_types(path, &source.file, scoped(path))
                .into_iter()
                .filter(|reach| {
                    !armed.contains(&reach.path)
                        && !exempt.contains(&(under_crates(&reach.file).as_str(), reach.path.as_str()))
                })
                .collect()
        },
    );
}

/// Red and green for the boundary predicate, on snippets this test owns.
///
/// Every clause of `boundary_error_types` has one case that isolates it, so
/// deleting a clause reds here — where the cause is named — rather than turning
/// the workspace scan silently permissive. The red cases are the ways an error
/// type crosses; the green ones are the ways it does not.
#[test]
fn boundary_error_predicate_reds_and_greens() {
    let parse = |source: &str| -> syn::File {
        syn::parse_file(source).unwrap_or_else(|error| panic!("snippet parses: {error}"))
    };
    let judge = |declared: &DeclaredTypes, source: &str| -> Vec<String> {
        boundary_error_types(Path::new("snippet.rs"), &parse(source), declared)
            .into_iter()
            .map(|reach| reach.path)
            .collect()
    };
    // The signature cases below declare nothing, so every type they name is
    // foreign and admitted; the admission rule gets its own cases at the end.
    let names = |source: &str| judge(&DeclaredTypes::default(), source);

    // Red — each way a `pub` signature names an error type.
    assert_eq!(names("pub fn f() -> Result<(), Boom> { todo!() }"), ["Boom"]);
    assert_eq!(names("pub async fn f() -> Result<(), Boom> { todo!() }"), ["Boom"]);
    assert_eq!(
        names("pub fn f() -> std::result::Result<(), path::to::Boom> { todo!() }"),
        ["Boom"]
    );
    assert_eq!(
        names("impl T { pub fn f(&self) -> Result<(), Boom> { todo!() } }"),
        ["Boom"]
    );
    assert_eq!(names("pub(crate) fn f() -> Result<(), Boom> { todo!() }"), ["Boom"]);
    // The alias: a bare `Result<T>` names no error type at the call site, and a
    // snippet belongs to no crate, so the chain resolves nothing — which reds
    // under a key no arm can carry rather than reading as the armed bare
    // `Error`. `resolver_resolves_each_error_declaration_apart` is the same
    // clause against the real tree, where it does resolve.
    assert_eq!(
        names("pub fn f() -> Result<u8> { todo!() }"),
        ["Result<T> in snippet.rs <alias unresolved>"]
    );
    // An error type with no last segment is rendered whole, so it can never
    // match an armed name and always reds.
    assert_eq!(
        names("pub fn f() -> Result<(), &'static str> { todo!() }"),
        ["&'staticstr"]
    );

    // Green — each body carries exactly one reason the signature does not cross.
    for (clause, source) in [
        ("private fn", "fn f() -> Result<(), Boom> { todo!() }"),
        (
            "private method",
            "impl T { fn f(&self) -> Result<(), Boom> { todo!() } }",
        ),
        ("no Result", "pub fn f() -> Option<Boom> { todo!() }"),
        ("no return type", "pub fn f() { todo!() }"),
        (
            "cfg(test) module",
            "#[cfg(test)] mod t { pub fn f() -> Result<(), Boom> { todo!() } }",
        ),
        (
            "cfg(test) impl",
            "#[cfg(test)] impl T { pub fn f(&self) -> Result<(), Boom> { todo!() } }",
        ),
        (
            "cfg(test) fn",
            "#[cfg(test)] pub fn f() -> Result<(), Boom> { todo!() }",
        ),
    ] {
        assert!(
            names(source).is_empty(),
            "the {clause} case must stay green — `{source}` was flagged, so a clause of \
             boundary_error_types has gone missing or over-wide"
        );
    }

    // The admission rule, one case per branch of `DeclaredTypes::admits`.
    let mut owned = DeclaredTypes::default();
    owned.absorb(&parse(
        "#[derive(Debug, thiserror::Error)] pub enum Boom {} pub struct Plain;",
    ));
    assert_eq!(judge(&owned, "pub fn f() -> Result<(), Boom> { todo!() }"), ["Boom"]);
    assert!(
        judge(&owned, "pub fn f() -> Result<(), Plain> { todo!() }").is_empty(),
        "a type the subtree declares and keeps off `std::error::Error` cannot reach the chain \
         walker, so it is not an unarmed hazard — `PemBundleError` is the shipping case"
    );
    assert_eq!(
        judge(&owned, "pub fn f() -> Result<(), Foreign> { todo!() }"),
        ["Foreign"],
        "a type declared outside the subtree cannot be judged from here and must red"
    );

    // A hand-written `impl std::error::Error` admits the same as the derive —
    // four types under `ocx_util` implement the trait that way.
    let mut manual = DeclaredTypes::default();
    manual.absorb(&parse("pub struct Boom; impl std::error::Error for Boom {}"));
    assert_eq!(judge(&manual, "pub fn f() -> Result<(), Boom> { todo!() }"), ["Boom"]);
    let mut unimplemented = DeclaredTypes::default();
    unimplemented.absorb(&parse("pub struct Boom; impl std::fmt::Display for Boom {}"));
    assert!(
        judge(&unimplemented, "pub fn f() -> Result<(), Boom> { todo!() }").is_empty(),
        "only an `Error` impl admits — matching any trait impl would re-admit every value type"
    );
}

/// Red and green for the resolver B5R-1 added, on the three declarations that
/// defeated the old key.
///
/// `ocx_util` declares three types named `Error` and the ladder imports each
/// under a rename; the then-undissolved `ocx_lib` declared ten more. Keyed on
/// the last path segment all thirteen were the string `Error`, and one arm —
/// `ocx_lib::Error` — armed the lot. The assertions below are the
/// discriminator itself: each rename lands on its own declaring file, and no
/// two of the four keys are equal.
#[test]
fn resolver_resolves_each_error_declaration_apart() {
    let ladder = crates_dir().join("ocx_cli/src/exit/ocx_util.rs");
    let classify = crates_dir().join("ocx_cli/src/exit/classify.rs");

    // Green: the rename at the use site resolves to the name and file of the
    // declaration, which is what a library signature writing `Result<T>` also
    // produces.
    for (from, written, site) in [
        (&ladder, "UtilError", "ocx_util/src/error.rs"),
        (&ladder, "ArchiveError", "ocx_util/src/archive/error.rs"),
        (&ladder, "CompressionError", "ocx_util/src/compression/error.rs"),
    ] {
        let resolved = resolve_type(from, &[written.to_owned()]);
        assert_eq!(resolved.name, "Error", "{written} names a type declared as `Error`");
        assert_eq!(
            resolved.site.as_deref().map(under_crates),
            Some(site.to_owned()),
            "{written} must resolve to {site}"
        );
        assert_eq!(resolved.key(), format!("{site}::Error"));
    }

    // A crate-qualified path resolves through the lib root's re-export. Was
    // `ocx_lib::Error` until WP-37 deleted that crate; the property under test
    // is the re-export hop, not the crate, so the example moved to a live one
    // with the same shape (`pub use error::Error` in its `lib.rs`).
    let pm_error = resolve_type(&classify, &["ocx_package_manager".to_owned(), "Error".to_owned()]);
    assert_eq!(pm_error.key(), "ocx_package_manager/src/error.rs::Error");

    // Red, had the keys not been separated: all four are `Error`, and the old
    // key made them one string.
    let keys: BTreeSet<String> = ["UtilError", "ArchiveError", "CompressionError"]
        .iter()
        .map(|written| resolve_type(&ladder, &[(*written).to_owned()]).key())
        .chain([pm_error.key()])
        .collect();
    assert_eq!(
        keys.len(),
        4,
        "four distinct declarations named `Error` collapsed onto {} key(s): {keys:?}",
        keys.len()
    );

    // And a distinctly named type stays its own key — resolution must not
    // rewrite what the library signature writes.
    assert_eq!(
        resolve_type(&ladder, &["SymlinkWalkError".to_owned()]).key(),
        "SymlinkWalkError"
    );

    // A type from outside the workspace resolves nothing and keys on the name
    // written, which is the behaviour every non-`Error` arm relies on.
    let foreign = ["nowhere".to_owned(), "AtAll".to_owned()];
    assert_eq!(resolve_type(&ladder, &foreign), Resolved::bare(&foreign));
    assert_eq!(resolve_type(&ladder, &foreign).key(), "AtAll");

    // A foreign type *named* `Error` keys on the path written, which is how the
    // `std::io::Result` signatures and the longhand io arm meet.
    let io = ["std".to_owned(), "io".to_owned(), "Error".to_owned()];
    assert_eq!(resolve_type(&ladder, &io).key(), "std::io::Error");
}

/// The resolver on aliases that share nothing textually with their targets.
///
/// `ocx_util`'s four `Result` aliases land on types named `Error` or
/// `FileError`, so a resolver still influenced by the name written at the use
/// site passes `resolver_resolves_each_error_declaration_apart` and resolves
/// nothing on the next crate. `ocx_oci` — being extracted as this lands —
/// carries four aliases over three error types, **none named `Error`**, and
/// the fixture is built to that measured shape rather than to an invented one:
///
/// ```text
/// ocx_oci/src/auth.rs             -> std::result::Result<T, AuthError>
/// ocx_oci/src/client.rs           -> std::result::Result<T, error::ClientError>
/// ocx_oci/src/client/transport.rs -> std::result::Result<T, ClientError>
/// ocx_oci/src/platform.rs         -> std::result::Result<T, PlatformError>
/// ```
///
/// Three properties `ocx_util` never exercises, each a way to be subtly wrong
/// and still green:
///
/// 1. no alias shares a name with its target;
/// 2. two aliases in one crate name **one** declaration by different paths —
///    the qualified `error::ClientError` and the bare `ClientError` — so the
///    right answer here is *merge*, where for `Error` it was *split*;
/// 3. every right-hand side is a fully qualified `std::result::Result`, which a
///    scan matching the unqualified spelling reads as unresolvable — and
///    "resolved nothing" is indistinguishable from "found no reaches".
///
/// The fixture is a loose tree under `tests/fixtures/`, not a crate, and not
/// `ocx_oci`'s live files: the resolver must not assume a declaration lives
/// inside a subtree the guard currently registers, or DEC-35's widening
/// resolves nothing on the first crate that uses it — and a fixture pinned to
/// another branch's files is a test of that branch.
#[test]
fn resolver_resolves_aliases_that_name_unlike_error_types() {
    let tiers = fixture("alias_tiers");
    let reaches = |file: &str| -> Vec<String> {
        let path = tiers.join(file);
        boundary_error_types(&path, &Source::parse(&path).file, &DeclaredTypes::default())
            .into_iter()
            .map(|reach| reach.path)
            .collect()
    };

    // Property 1: each `pub fn` returning the bare alias resolves to the type
    // that alias actually names — read off its declaration, never off its
    // spelling. Property 3 rides along: `auth`, `client` and `platform` write
    // `std::result::Result` and `manifest` writes the bare `Result`, and both
    // spellings resolve, so the match is on the path's last segment.
    assert_eq!(reaches("auth.rs"), ["AuthError"]);
    assert_eq!(reaches("client.rs"), ["ClientError"]);
    assert_eq!(reaches("client/transport.rs"), ["ClientError"]);
    assert_eq!(reaches("manifest.rs"), ["ManifestError"]);
    // Two aliases in one file: the local one and a `super::`-rooted import of
    // another module's, which must not collapse onto each other.
    assert_eq!(reaches("platform.rs"), ["PlatformError", "ClientError"]);

    // Every `pub fn` of the tier crosses, and each is attributed: a scan that
    // stopped resolving reports the same empty set as a tier with no
    // signatures, so the total is asserted beside the per-file lists.
    let all: Vec<String> = rust_sources(&tiers)
        .into_iter()
        .flat_map(|file| boundary_error_types(&file, &Source::parse(&file).file, &DeclaredTypes::default()))
        .map(|reach| reach.path)
        .collect();
    assert_eq!(all.len(), 6, "every `pub fn` return of the tier must resolve: {all:?}");

    // Property 2: the two client aliases name ONE declaration by two paths.
    let by_path = resolve_result_alias(&tiers.join("client.rs"), &[]);
    let by_name = resolve_result_alias(&tiers.join("client/transport.rs"), &[]);
    assert_eq!(by_path.name, "ClientError");
    assert_eq!(
        by_path, by_name,
        "`error::ClientError` and `ClientError` must be one declaration"
    );
    assert!(
        by_path
            .site
            .as_deref()
            .is_some_and(|site| site.ends_with("client/error.rs")),
        "the client aliases resolved to {:?}, not client/error.rs",
        by_path.site
    );

    // Four aliases, three declarations — asserted as the count, so a resolver
    // that split the client pair or merged an unrelated pair reds here.
    let declarations: BTreeSet<Option<PathBuf>> = ["auth.rs", "client.rs", "client/transport.rs", "platform.rs"]
        .iter()
        .map(|file| resolve_result_alias(&tiers.join(file), &[]).site)
        .collect();
    assert_eq!(
        declarations.len(),
        3,
        "four aliases over three error types resolved to {} declaration(s): {declarations:?}",
        declarations.len()
    );

    // And the declarations themselves, by site: `resolve_type` walks the
    // fixture's own `lib.rs` root, so a tree outside `crates/` resolves.
    for (from, written, site) in [
        ("auth.rs", "AuthError", "auth/error.rs"),
        ("client.rs", "ClientError", "client/error.rs"),
        ("platform.rs", "PlatformError", "platform.rs"),
    ] {
        let resolved = resolve_type(&tiers.join(from), &[written.to_owned()]);
        assert_eq!(resolved.name, written);
        let landed = resolved
            .site
            .as_deref()
            .map(|path| path.to_string_lossy().replace('\\', "/"));
        assert!(
            landed
                .as_deref()
                .is_some_and(|path| path.ends_with(&format!("alias_tiers/{site}"))),
            "{written} resolved to {landed:?}, not alias_tiers/{site}"
        );
    }

    // The real tree, in the same shape. The anchor left `ocx_lib` at WP-24
    // exactly as the previous spelling of this comment predicted, so it now
    // sits *inside* a registered subtree; the fixture above is what carries the
    // outside-every-subtree property, and this block is the real-tree shape.
    // `client/transport.rs` declares `type Result<T> = Result<T, ClientError>`.
    let transport = crates_dir().join("ocx_oci/src/client/transport.rs");
    let alias = resolve_result_alias(&transport, &[]);
    assert_eq!(
        alias.name, "ClientError",
        "the real oci alias must resolve to its own type"
    );
    assert!(
        alias
            .site
            .is_some_and(|site| site.ends_with("client/error.rs") || site.ends_with("client.rs")),
        "the real oci alias resolved to no declaring file"
    );
}

// ---------------------------------------------------------------------------
// Every boundary needle names something live (DEC-33)
// ---------------------------------------------------------------------------

/// One `assert_no_imports` / `assert_no_needles` call site, read off the source.
struct GuardCall {
    /// The file the call sits in, and the test that holds it.
    file: PathBuf,
    test: String,
    line: usize,
    /// `true` for `assert_no_needles` — token needles, which name no module.
    tokens: bool,
    /// `true` when the subtrees scanned are themselves `fixture(..)` trees: the
    /// harness proving itself on trees it owns, whose "modules" are fixture
    /// modules and resolve against no crate.
    owned_trees: bool,
    /// `true` when the test proves, itself, that its subject is gone — a
    /// `!<path>.exists()` assertion. Such a guard forbids a name *because* it
    /// resolves to nothing, so liveness is the wrong question for it.
    resurrection: bool,
    needles: Vec<String>,
    witness: String,
    /// The crates this call's subtrees actually cover — `SELF` for the guard
    /// file's own crate, a crate name for each `extracted_src("..")` the test
    /// or the helpers it calls reach for.
    ///
    /// Not derived from `file`, which is where the *test* lives rather than
    /// what it *scans*. Since B5 a guard file scans crates it does not live in,
    /// and resolving a bare needle against the guard file's crate passed
    /// needles that forbade nothing in the tree under test (B6-B1).
    scanned: BTreeSet<String>,
}

/// Every such call site under `crates/*/tests/`, with its needle list and its
/// witness resolved.
///
/// Derived, not listed: a guard file added later joins by itself, which is the
/// property `top_level_modules_except` had and B5 removed. A call whose needle
/// list or witness this reader cannot resolve is a **loud failure** naming the
/// site, never a skipped one — the same rule [`Source::parse`] applies to a
/// file it cannot read. A shape nobody can resolve is a guard nobody is
/// checking.
fn boundary_guard_calls() -> Vec<GuardCall> {
    struct Calls<'a> {
        file: &'a Path,
        consts: &'a BTreeMap<String, Vec<String>>,
        test: String,
        locals: BTreeMap<String, Vec<String>>,
        skip: bool,
        resurrection: bool,
        helpers: BTreeMap<String, BTreeSet<String>>,
        scanned: BTreeSet<String>,
        out: Vec<GuardCall>,
    }
    impl Calls<'_> {
        /// The string array `expression` names: a literal, a `let` in this
        /// test, or a `const` at the file root.
        fn array(&self, expression: &syn::Expr) -> Option<Vec<String>> {
            match strip_reference(expression) {
                syn::Expr::Array(array) => array.elems.iter().map(literal).collect(),
                syn::Expr::Path(path) => {
                    let name = path.path.segments.last()?.ident.to_string();
                    self.locals.get(&name).or_else(|| self.consts.get(&name)).cloned()
                }
                _ => None,
            }
        }
    }
    impl<'ast> Visit<'ast> for Calls<'_> {
        fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
            // A `#[should_panic]` test asserts a refusal, so its call site is
            // *meant* to carry an unwitnessed needle — the harness's own
            // red/green proof is built out of them. Derived from the attribute
            // rather than from a list of files to skip.
            let panics = function.attrs.iter().any(|attr| attr.path().is_ident("should_panic"));
            let reached = crates_named(&function.block, &self.helpers);
            let (test, locals, skip, resurrection, scanned) = (
                std::mem::replace(&mut self.test, function.sig.ident.to_string()),
                std::mem::take(&mut self.locals),
                std::mem::replace(&mut self.skip, panics),
                std::mem::replace(&mut self.resurrection, proves_absence(function)),
                std::mem::replace(&mut self.scanned, reached),
            );
            syn::visit::visit_item_fn(self, function);
            self.test = test;
            self.locals = locals;
            self.skip = skip;
            self.resurrection = resurrection;
            self.scanned = scanned;
        }
        fn visit_local(&mut self, local: &'ast syn::Local) {
            if let syn::Pat::Ident(name) = &local.pat
                && let Some(init) = &local.init
                && let syn::Expr::Array(array) = strip_reference(&init.expr)
                && let Some(values) = array.elems.iter().map(literal).collect::<Option<Vec<_>>>()
            {
                self.locals.insert(name.ident.to_string(), values);
            }
            syn::visit::visit_local(self, local);
        }
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            syn::visit::visit_expr_call(self, call);
            let syn::Expr::Path(function) = call.func.as_ref() else {
                return;
            };
            let Some(name) = function.path.segments.last().map(|s| s.ident.to_string()) else {
                return;
            };
            // `assert_no_imports_derived` is exempt by its own contract: its
            // forbidden set is read off the compiler's own `mod` items, so an
            // entry naming nothing is not expressible and a witness per entry
            // is deliberately not asked for.
            let tokens = match name.as_str() {
                "assert_no_imports" => false,
                "assert_no_needles" => true,
                _ => return,
            };
            if self.skip {
                return;
            }
            let line = function.path.segments[0].ident.span().start().line;
            let at = format!("{}:{line} in `{}`", under_crates(self.file), self.test);
            assert_eq!(call.args.len(), 3, "{at}: `{name}` takes three arguments");
            let needles = self
                .array(&call.args[1])
                .unwrap_or_else(|| panic!("{at}: the needle list is a shape this reader cannot resolve"));
            let witness = fixture_name(&call.args[2])
                .unwrap_or_else(|| panic!("{at}: the witness is not a `fixture(\"..\")` call"));
            let owned_trees = mentions_fixture(&call.args[0]);
            self.out.push(GuardCall {
                file: self.file.to_path_buf(),
                test: self.test.clone(),
                line,
                tokens,
                owned_trees,
                resurrection: self.resurrection,
                needles,
                witness,
                // A call that reaches for no crate at all scans the file's own,
                // which is every guard written before a crate left `ocx_lib`.
                scanned: if self.scanned.is_empty() {
                    BTreeSet::from(["SELF".to_owned()])
                } else {
                    self.scanned.clone()
                },
            });
        }
    }

    let mut out = Vec::new();
    let mut scanned = 0;
    for (_, dir) in crate_dirs() {
        for file in test_targets(&dir.join("tests")) {
            let parsed = Source::parse(&file).file;
            let mut consts: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for item in &parsed.items {
                if let syn::Item::Const(declaration) = item
                    && let syn::Expr::Array(array) = strip_reference(&declaration.expr)
                    && let Some(values) = array.elems.iter().map(literal).collect::<Option<Vec<_>>>()
                {
                    consts.insert(declaration.ident.to_string(), values);
                }
            }
            // Every free function in the file, mapped to the crates its body
            // reaches for, so a test calling `generic_oci_tree()` resolves to
            // the crate that helper walks rather than to the file's own.
            let mut helpers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for item in &parsed.items {
                if let syn::Item::Fn(function) = item {
                    helpers.insert(
                        function.sig.ident.to_string(),
                        crates_named(&function.block, &BTreeMap::new()),
                    );
                }
            }
            let mut calls = Calls {
                file: &file,
                consts: &consts,
                test: "<file scope>".to_owned(),
                locals: BTreeMap::new(),
                skip: false,
                resurrection: false,
                helpers,
                scanned: BTreeSet::new(),
                out: Vec::new(),
            };
            calls.visit_file(&parsed);
            scanned += 1;
            out.extend(calls.out);
        }
    }
    assert!(
        scanned > 5 && out.len() > 10,
        "the call-site scan read {scanned} test file(s) and found {} guard call(s) — it stopped \
         recognising the shape, and every needle list would then be checked by nothing",
        out.len()
    );
    out
}

/// Every repo path a test file names as a literal still names something
/// (DEC-44).
///
/// **The defect this closes.** `every_boundary_needle_names_something_live`
/// derives *needles*. A positive control, a fixture anchor, or any other path
/// literal has the identical failure mode — the extraction moves the file, the
/// literal goes on naming the old place, and nothing checks it — and is derived
/// by nothing. WP-24 produced two at once: `oci::copy` as a live-needle control
/// and `ocx_lib/src/oci/client/transport.rs` as a resolver anchor. Both
/// reddened, but only because the assertions around them happened to be sharp.
/// With eleven extractions ahead the next one lands in a test whose green
/// survives the path going dead.
///
/// **What counts as a path, stated rather than guessed.** A literal is a repo
/// path when it names `crates/` or `/src/` *and* carries no whitespace, no `{`
/// and no `::`. The three exclusions are what the other forms in these files
/// are: an assertion message is prose and has spaces, `crates/{}` and
/// `path+file:///w/crates/{name}#0.1.0` are format templates, and
/// `ocx_lib/src/error.rs::Error` is a file joined to a type — a composite key,
/// not a path, and a literal with an empty segment — `crates/`, `/src/` — is a
/// marker rather than a target, which is also what keeps this guard from
/// matching the two needles it tests with. That is a closed syntactic rule over
/// a closed subject, not a
/// heuristic, and deliberately not an allowlist: a named exemption is the thing
/// that rots silently.
///
/// Both halves carry a floor, so neither can pass by finding nothing.
#[test]
fn every_repo_path_literal_in_a_test_names_something_live() {
    /// Every string literal in a token stream, `#[doc]` attributes excluded.
    ///
    /// A token walk rather than `syn::visit`, because `syn` does not descend
    /// into a macro's arguments and nearly every path literal in these files is
    /// an argument to `assert!`. The AST walk this replaces saw 7 of them and
    /// reported a clean scan — the same shape of green this guard exists to
    /// refuse.
    fn string_literals(tokens: proc_macro2::TokenStream, out: &mut Vec<(String, usize)>) {
        let trees: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
        let mut index = 0;
        while index < trees.len() {
            if let proc_macro2::TokenTree::Punct(punct) = &trees[index]
                && punct.as_char() == '#'
                && let Some(proc_macro2::TokenTree::Group(group)) = trees.get(index + 1)
                && matches!(group.stream().into_iter().next(),
                    Some(proc_macro2::TokenTree::Ident(ident)) if ident == "doc")
            {
                index += 2;
                continue;
            }
            match &trees[index] {
                proc_macro2::TokenTree::Group(group) => string_literals(group.stream(), out),
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(text) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                        out.push((text.value(), literal.span().start().line));
                    }
                }
                _ => {}
            }
            index += 1;
        }
    }

    /// The literal-bearing items of one file, paired with whether the item is a
    /// burial — `no_log_shim` names `ocx_lib/src/log.rs` precisely to assert it
    /// is gone (D-001), and demanding that one exist would invert the guard it
    /// lives in. Same exemption, same helper, as the needle half (DEC-33).
    fn literal_items(file: &syn::File) -> Vec<(proc_macro2::TokenStream, bool)> {
        struct Items {
            out: Vec<(proc_macro2::TokenStream, bool)>,
        }
        impl<'ast> syn::visit::Visit<'ast> for Items {
            fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
                syn::visit::visit_item_fn(self, node);
                self.out
                    .push((quote::ToTokens::to_token_stream(node), proves_absence(node)));
            }
            // An `impl` method is a literal-bearing item like any other, and
            // the three other visitors in this file that must see method
            // bodies all declare this arm. Omitting it here made every path
            // literal inside an `impl` invisible to the liveness check below —
            // latent rather than active while only this file holds a top-level
            // `impl`, and about to stop being latent as the extractions land.
            fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
                syn::visit::visit_impl_item_fn(self, node);
                self.out
                    .push((quote::ToTokens::to_token_stream(node), proves_absence_in(&node.block)));
            }
            fn visit_item_const(&mut self, node: &'ast syn::ItemConst) {
                self.out.push((quote::ToTokens::to_token_stream(node), false));
            }
            fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
                self.out.push((quote::ToTokens::to_token_stream(node), false));
            }
            // A `macro_rules!` body and an item-position invocation are both
            // `syn::ItemMacro`, and neither is reached by any arm above: the
            // parser keeps their contents as an opaque `TokenStream` hanging
            // off an item the harvest list did not name. `string_literals`
            // below recurses through token groups, so a literal inside an
            // `assert!` *argument* is already covered — the hole is one level
            // up, at which items get their tokens taken at all.
            //
            // Latent when written, exactly as the `visit_impl_item_fn` arm
            // above was: of the 15 `crates/*/tests/*.rs` targets, one carries
            // such a construct (`ocx_schema`'s `golden_tests!`) and it holds no
            // path literal. Closed before it opens, because a guard that reads
            // as covering these files while one construct is structurally
            // invisible is how the next unwired instrument gets written.
            fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
                self.out.push((quote::ToTokens::to_token_stream(node), false));
            }
        }
        let mut items = Items { out: Vec::new() };
        syn::visit::Visit::visit_file(&mut items, file);
        items.out
    }
    struct ExtractedSrc {
        out: Vec<(String, usize)>,
    }
    impl<'ast> syn::visit::Visit<'ast> for ExtractedSrc {
        fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
            syn::visit::visit_expr_call(self, node);
            let syn::Expr::Path(path) = &*node.func else {
                return;
            };
            if path
                .path
                .segments
                .last()
                .is_none_or(|last| last.ident != "extracted_src")
            {
                return;
            }
            if let Some(syn::Expr::Lit(lit)) = node.args.first()
                && let syn::Lit::Str(name) = &lit.lit
            {
                self.out
                    .push((name.value(), syn::spanned::Spanned::span(name).start().line));
            }
        }
    }

    /// The call the cross-read counts, spelled once so no comment repeats it.
    const NEEDLE: &str = concat!("extracted_src", "(\"");

    let root = workspace_root();
    let mut targets: Vec<PathBuf> = Vec::new();
    for (_, dir) in crate_dirs() {
        targets.extend(test_targets(&dir.join("tests")));
    }
    assert!(
        targets.len() >= 10,
        "found {} `crates/*/tests/*.rs` target(s); the scan lost its subject",
        targets.len()
    );

    let mut read = 0usize;
    let mut checked_paths = 0usize;
    let mut checked_crates = 0usize;
    let mut textual_crate_calls = 0usize;
    let mut dead: Vec<String> = Vec::new();
    for target in &targets {
        let source = std::fs::read_to_string(target).expect("a test target is readable");
        let file = syn::parse_file(&source).expect("a test target parses as Rust");
        let where_ = target.strip_prefix(&root).unwrap_or(target).display().to_string();

        let mut literals: Vec<(String, usize)> = Vec::new();
        for (tokens, buries) in literal_items(&file) {
            if buries {
                continue;
            }
            string_literals(tokens, &mut literals);
        }
        read += literals.len();
        for (value, line) in literals {
            let names_repo = value.contains("crates/") || value.contains("/src/");
            // A path names segments, so it has no empty one — which is also
            // what keeps this guard off its own markers. `"crates/"` and
            // `"/src/"` are the two literals it tests *with*, and a detector
            // that matches its own needles reports itself in every state.
            let well_formed = !value.starts_with('/')
                && !value.ends_with('/')
                && !value.contains("//")
                && value.split('/').count() >= 2;
            let is_path =
                well_formed && !value.contains(char::is_whitespace) && !value.contains('{') && !value.contains("::");
            if !(names_repo && is_path) {
                continue;
            }
            checked_paths += 1;
            let relative = value.strip_prefix("crates/").unwrap_or(&value);
            if !root.join(&value).exists() && !crates_dir().join(relative).exists() {
                dead.push(format!(
                    "{where_}:{line}: `{value}` names nothing under the workspace root"
                ));
            }
        }

        // The cross-read that replaces a hand-set floor (DEC-39). Counting the
        // call textually and through `syn` are two independent readers of the
        // same thing, so a reader that stops recognising the call shape reds
        // here instead of quietly passing with a smaller subject — and unlike a
        // number, it survives a guard being deleted.
        //
        // Comments are cut first, and that is not tidiness: the first version
        // of this line spelled the needle in the comment above it and counted
        // itself, which is the same self-matching detector the path rule's
        // empty-segment clause exists to prevent. A needle written in prose is
        // not a call site. Cutting at `//` would also truncate a code line
        // carrying `//` inside a string literal; no call site does.
        let code: String = source
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect();
        textual_crate_calls += code.matches(NEEDLE).count();

        let mut calls = ExtractedSrc { out: Vec::new() };
        syn::visit::Visit::visit_file(&mut calls, &file);
        for (name, line) in calls.out {
            checked_crates += 1;
            if !crates_dir().join(&name).join("src").is_dir() {
                dead.push(format!(
                    "{where_}:{line}: `extracted_src({name:?})` names no `crates/{name}/src`"
                ));
            }
        }
    }

    // DEC-110: the production population is empty — every `extracted_src` call
    // lived in the boundaries test that died with `ocx_lib` at WP-37 — and an
    // empty expected set floors nothing. `0 == 0` is also what a visitor that
    // stopped matching `ExprCall`, and a `NEEDLE` that stopped matching, both
    // return: two readers gone blind agree perfectly. So both are run over a
    // controlled input here, and the floor rides on that until a caller
    // returns.
    //
    // The control is assembled from `NEEDLE` at runtime on purpose. This test
    // scans `crates/*/tests/*.rs` and this file is one of them, so a literal
    // `extracted_src("` in these bytes would be counted by the textual scan of
    // this very file while `syn` sees a string rather than a call — reding the
    // cross-read at `0 == 1`. (It is safe in this comment only because the
    // textual reader cuts at `//` first, for the reason stated above it.)
    let control = format!("fn control() {{ let _live = {NEEDLE}ocx_util\"); let _dead = {NEEDLE}ocx_lib\"); }}");
    let control_textual = control.matches(NEEDLE).count();
    let mut control_calls = ExtractedSrc { out: Vec::new() };
    syn::visit::Visit::visit_file(
        &mut control_calls,
        &syn::parse_file(&control).expect("the control parses as Rust"),
    );
    let control_names: Vec<&str> = control_calls.out.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        (control_textual, control_names.as_slice()),
        (2, ["ocx_util", "ocx_lib"].as_slice()),
        "both readers must find both calls in the control; a reader that has gone blind reports the \
         same zero here that the empty production population does"
    );
    let control_dead: Vec<&str> = control_names
        .iter()
        .copied()
        .filter(|name| !crates_dir().join(name).join("src").is_dir())
        .collect();
    assert_eq!(
        control_dead,
        ["ocx_lib"],
        "the dead half of the control must land where a dead call lands, and the live half must not; \
         otherwise the `dead` rule above cannot fire for a call either"
    );

    // Two floors, because the two ways this goes quiet are different. The
    // reader stops seeing literals at all — which is what the `syn::visit`
    // version did, reading 7 where the token walk reads 1685 — or it still
    // reads them and stops recognising a path among them.
    assert!(
        read >= 800,
        "the literal reader saw {read} string literal(s) across {} test target(s); it has stopped \
         descending, and every check below it is then vacuous",
        targets.len()
    );
    assert!(
        checked_paths >= 5,
        "only {checked_paths} repo path literal(s) were checked out of {read} literal(s) read; the \
         path rule stopped recognising them"
    );
    assert_eq!(
        checked_crates, textual_crate_calls,
        "the `syn` walk read {checked_crates} `extracted_src` call(s) and a textual scan of the same \
         files found {textual_crate_calls}; the two readers disagree, so one of them has stopped \
         seeing the call shape"
    );
    // The `checked_crates > 0` floor that stood here is gone with its subject.
    // Every `extracted_src("..")` call in the repo was in
    // `crates/ocx_lib/tests/boundaries.rs`, which died with the crate at WP-37
    // (DEC-7), so the floor could no longer be satisfied by any correct tree —
    // and a floor that cannot go green is not a floor, it is a standing red.
    // The cross-read above is what survives: it is `0 == 0` while no call
    // exists, and it reds the moment one returns and only one reader sees it.
    // Restore the floor in the same commit as the first new caller.
    assert!(
        dead.is_empty(),
        "{} path literal(s) name nothing on disk — a literal that resolves to nothing documents a tree \
         that no longer exists, and the assertion around it proves whatever it likes:\n  {}",
        dead.len(),
        dead.join("\n  ")
    );
}

/// Cargo's integration-test targets for one crate: `tests/*.rs`, top level only.
///
/// Not [`rust_sources`], which recurses. A guard call can only live in a file
/// the compiler builds as a test, and everything below `tests/` is a helper
/// module or a fixture tree — including trees that are deliberately unparsable,
/// because proving [`Source::parse`] panics on bad input is itself a fixture.
fn test_targets(tests: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(tests)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    out.sort();
    out
}

/// Every fixture name any test target mentions, whatever it does with it.
///
/// Deliberately wider than [`boundary_guard_calls`]: the orphan half asks
/// whether a fixture is *read by anything*, and a witness is legitimately named
/// by an `assert_scan` site, a `let` binding, or a `#[should_panic]` test the
/// needle checks exclude. Narrowing this to the two assert functions would
/// report seven live fixtures as dead.
fn named_fixtures() -> BTreeSet<String> {
    struct Names(BTreeSet<String>);
    impl<'ast> Visit<'ast> for Names {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(function) = call.func.as_ref()
                && function.path.is_ident("fixture")
                && let Some(name) = call.args.first().and_then(literal)
            {
                self.0.insert(name);
            }
            syn::visit::visit_expr_call(self, call);
        }
    }
    let mut names = Names(BTreeSet::new());
    for (_, dir) in crate_dirs() {
        for file in test_targets(&dir.join("tests")) {
            names.visit_file(&Source::parse(&file).file);
        }
    }
    names.0
}

/// Whether `function` asserts that a path does **not** exist — `!x.exists()`.
///
/// The one honest exemption from liveness. `no_log_shim` forbids `log` because
/// D-001 deleted `crates/ocx_lib/src/log.rs`, so the needle names nothing *by
/// design*: the guard exists to stop the module coming back. Demanding it
/// resolve would invert it. Rather than list that test, the exemption is earned
/// — a guard forbidding a dead name proves the name is dead, in its own body,
/// and a needle is then either live or provably buried.
fn proves_absence(function: &syn::ItemFn) -> bool {
    proves_absence_in(&function.block)
}

/// [`proves_absence`] over a bare body, so an `impl` method can earn the same
/// exemption. Kept as a second name rather than a widened signature because
/// `proves_absence`'s own unit tests below pass an `ItemFn`, and changing the
/// signature breaks their *compile* — a red that names the file rather than
/// the guard, and so reads as a mistake in the edit instead of in the reader.
fn proves_absence_in(block: &syn::Block) -> bool {
    struct Negated(bool);
    impl<'ast> Visit<'ast> for Negated {
        fn visit_expr_unary(&mut self, unary: &'ast syn::ExprUnary) {
            if matches!(unary.op, syn::UnOp::Not(_)) {
                let mut inner = Exists(false);
                inner.visit_expr(&unary.expr);
                self.0 |= inner.0;
            }
            syn::visit::visit_expr_unary(self, unary);
        }
        // The assertion lives inside `assert!`, and `syn` does not descend into
        // a macro's tokens — a visitor that only walks parsed expressions sees
        // an empty body here and reports every guard live. So parse each
        // macro's arguments as expressions first.
        fn visit_macro(&mut self, item: &'ast syn::Macro) {
            type Args = syn::punctuated::Punctuated<syn::Expr, syn::Token![,]>;
            if let Ok(args) = item.parse_body_with(Args::parse_terminated) {
                for argument in &args {
                    self.visit_expr(argument);
                }
            }
            syn::visit::visit_macro(self, item);
        }
    }
    struct Exists(bool);
    impl<'ast> Visit<'ast> for Exists {
        fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
            self.0 |= call.method == "exists";
            syn::visit::visit_expr_method_call(self, call);
        }
    }
    let mut negated = Negated(false);
    negated.visit_block(block);
    negated.0
}

fn strip_reference(expression: &syn::Expr) -> &syn::Expr {
    match expression {
        syn::Expr::Reference(reference) => strip_reference(&reference.expr),
        other => other,
    }
}

fn literal(expression: &syn::Expr) -> Option<String> {
    match expression {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(text),
            ..
        }) => Some(text.value()),
        _ => None,
    }
}

/// The name inside a `fixture("..")` call, wherever it sits in `expression`.
fn fixture_name(expression: &syn::Expr) -> Option<String> {
    struct Find(Option<String>);
    impl<'ast> Visit<'ast> for Find {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(function) = call.func.as_ref()
                && function.path.is_ident("fixture")
                && let Some(name) = call.args.first().and_then(literal)
                && self.0.is_none()
            {
                self.0 = Some(name);
            }
            syn::visit::visit_expr_call(self, call);
        }
    }
    let mut find = Find(None);
    find.visit_expr(expression);
    find.0
}

/// The crates a function body reaches for, one level deep through file-local
/// helpers.
///
/// `extracted_src("x")` names crate `x`; `src()` names the crate the file lives
/// in, recorded as `SELF` because the caller knows which that is. Every subtree
/// helper in these files is built out of exactly those two, so one level of
/// indirection is the whole graph rather than a bet on it — and a helper this
/// cannot resolve contributes nothing, which makes the needle look *less* live,
/// never more.
fn crates_named(block: &syn::Block, helpers: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
    struct Find<'a> {
        out: BTreeSet<String>,
        helpers: &'a BTreeMap<String, BTreeSet<String>>,
    }
    impl<'ast> Visit<'ast> for Find<'_> {
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            syn::visit::visit_expr_call(self, call);
            let syn::Expr::Path(function) = call.func.as_ref() else {
                return;
            };
            let Some(name) = function.path.segments.last().map(|s| s.ident.to_string()) else {
                return;
            };
            match name.as_str() {
                "extracted_src" => {
                    if let Some(crate_name) = call.args.first().and_then(literal) {
                        self.out.insert(crate_name);
                    }
                }
                "src" => {
                    self.out.insert("SELF".to_owned());
                }
                other => {
                    if let Some(reached) = self.helpers.get(other) {
                        self.out.extend(reached.iter().cloned());
                    }
                }
            }
        }
    }
    let mut find = Find {
        out: BTreeSet::new(),
        helpers,
    };
    find.visit_block(block);
    find.out
}

fn mentions_fixture(expression: &syn::Expr) -> bool {
    fixture_name(expression).is_some()
}

/// The file `witness` names, found by name under `crate_dir/tests/`.
///
/// Resolved by search rather than by re-implementing each guard file's own
/// `fixture()` helper — three of them exist and they point at two different
/// directories. A name that matches no file, or more than one, is a loud
/// failure: both mean the call site's witness is not the file anyone thinks.
fn resolve_witness(guard: &GuardCall) -> PathBuf {
    let tests = guard
        .file
        .parent()
        .unwrap_or_else(|| panic!("{}: no tests directory", under_crates(&guard.file)));
    let mut found: Vec<PathBuf> = walk_any(tests)
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == guard.witness.as_str()))
        .collect();
    found.sort();
    assert_eq!(
        found.len(),
        1,
        "{}:{} in `{}`: the witness `{}` matches {} file(s) under {} — {found:?}",
        under_crates(&guard.file),
        guard.line,
        guard.test,
        guard.witness,
        found.len(),
        under_crates(tests)
    );
    found.remove(0)
}

/// Every file under `dir`, recursively — `rust_sources` only sees `.rs`, and a
/// witness is `.rs.txt` so the guards' own walks never pick one up as source.
fn walk_any(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_any(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Whether `needle` names something that exists, from the crate under test.
///
/// **What "live" means, stated where it is enforced.** A needle is live when it
/// names a module that can be walked to from a crate root:
/// `ocx_lib::oci::simplesigning` from `crates/ocx_lib/src`,
/// `oci/simplesigning.rs` or `oci/simplesigning/`. A bare needle —
/// `package`, `publisher` — is a *sibling module of the crate under test*, so it
/// resolves against `crate_under_test` alone, and a `ocx_*`-rooted one resolves
/// against the crate it names.
///
/// **Why not "any crate".** Resolving a bare needle workspace-wide is the false
/// green this check exists to kill: `crates/ocx_store/src/package.rs` would keep
/// a needle alive for years after `ocx_lib::package` left, and the guard would
/// forbid a reach nothing can spell.
///
/// **Why not `ocx_lib` either — the DEC-37 answer.** The scope is the crate that
/// *owns the guard file*, never a hard-coded `ocx_lib`. Phase 2 empties
/// `crates/ocx_lib/src`, and a check anchored there would empty with it. Here the
/// emptying is what *reds*: when `announce` moves to `ocx_announce`, the bare
/// needle stops naming a module of `ocx_lib` and the call site must be re-pointed
/// to `ocx_announce::announce` before it is green again. That is DEC-30 item 1's
/// obligation — the guard follows its subject — enforced rather than remembered.
///
/// **The half nothing enforced.** [`assert_scan`] already demands a witness reach
/// per needle, but a witness is hand-written too:
/// `oci_client_does_not_import_workflow` says so in its own comment — two of its
/// seven needles are forward-looking, "their witness lines are constructed, and a
/// constructed line witnesses a module that was deleted just as happily as one
/// that exists". Both sides of a hand-written guard can go stale together, and
/// forbidding nothing is green.
fn needle_resolves(needle: &str, crate_under_test: &Path) -> bool {
    let segments: Vec<&str> = needle.split("::").collect();
    let Some((first, rest)) = segments.split_first() else {
        return false;
    };
    let (root, tail) = if first.starts_with("ocx_") {
        (crates_dir().join(first).join("src"), rest.to_vec())
    } else {
        (crate_under_test.to_path_buf(), segments.clone())
    };
    if !root.is_dir() {
        return false;
    }
    let mut here = root;
    for segment in tail {
        // `a.rs` beside `a/b.rs` is the layout every crate here uses, so a
        // module file's children live in its same-named sibling directory.
        let base = if here.extension().is_some_and(|extension| extension == "rs") {
            here.with_extension("")
        } else {
            here.clone()
        };
        if base.join(format!("{segment}.rs")).is_file() {
            here = base.join(format!("{segment}.rs"));
        } else if base.join(segment).is_dir() {
            here = base.join(segment);
        } else {
            return false;
        }
    }
    true
}

/// Every boundary guard's needle list is witnessed by its own fixture, and
/// every module needle names a module that exists (DEC-33).
///
/// **What this replaces.** `top_level_modules_except` was the only *derived*
/// forbidden-set mechanism in `boundaries.rs`, and B5 deleted it with the four
/// guards it served — correctly, one at a time: once a crate extracts, its
/// allowed set turns the forbidden reach into a Cargo error `deps_direction`
/// reds on first, and a compiler error beats a test. The aggregate is the
/// problem. Every remaining needle list is hand-written, phase 2 performs
/// seventeen renames against them, and eleven are still ahead.
///
/// **Two halves, and they catch different things.** The literal ruling is the
/// first: every needle has a live reach in the fixture its call site names.
/// [`assert_scan`] already demands that — but only of a guard that *runs*, so a
/// `#[ignore]`d one takes its whole needle list out of every check at once, and
/// this repository ignores guards by convention until their WP lands.
///
/// The second is [`needle_resolves`], and it is the half nothing enforced: a
/// needle naming no live module is witnessed by a hand-written fixture exactly
/// as happily as one naming a real module. One call site —
/// `oci_client_does_not_import_workflow` — already resolves its own seven
/// needles against `lib.rs` by hand, and says in its comment why. This lifts
/// that from one site of thirteen to every site, derived.
///
/// **The inverse, decided and pinned.** A fixture carrying reaches no needle
/// names is **not** a failure: a witness is allowed to document a neighbouring
/// reach for its reader, the extra reach forbids nothing, and requiring
/// fixture ⊆ needles would make every fixture edit a guard edit. A fixture file
/// no call site names at all **is** a failure — it reads as coverage that does
/// not exist, and it is precisely what a deleted guard leaves behind. B5
/// deleted `theme_reach.rs.txt` with its guard, which is the right way and the
/// one this check makes mandatory.
#[test]
fn every_boundary_needle_names_something_live() {
    let calls = boundary_guard_calls();

    let mut unwitnessed = Vec::new();
    let mut dead = Vec::new();
    for guard in &calls {
        let witness = resolve_witness(guard);
        // `crates/<name>/tests/<file>.rs` → `crates/<name>/src`.
        let own_crate = guard
            .file
            .parent()
            .and_then(Path::parent)
            .map(|dir| dir.join("src"))
            .unwrap_or_else(|| panic!("{}: no crate root", under_crates(&guard.file)));
        // The crates the call actually scans, not the one the test lives in
        // (B6-B1). A bare needle is live when it names a module of any of them;
        // resolving against the guard file's crate alone passed needles that
        // forbade nothing in the tree under test.
        let scanned: Vec<PathBuf> = guard
            .scanned
            .iter()
            .map(|name| {
                if name == "SELF" {
                    own_crate.clone()
                } else {
                    crates_dir().join(name).join("src")
                }
            })
            .collect();
        let at = format!("{}:{} in `{}`", under_crates(&guard.file), guard.line, guard.test);
        for needle in guard.needles.iter().map(String::as_str) {
            let hits = if guard.tokens {
                needles_in(&witness, &[needle])
            } else {
                reaches_in(&witness, &[needle])
            };
            if hits.is_empty() {
                unwitnessed.push(format!("{at}: `{needle}` has no reach in {}", guard.witness));
            }
            // Three shapes are exempt, each by what the call site *is*, never
            // by a list: a token needle names a capability, not a module
            // (`registry()`, `MakeWriter`); a guard proving the harness on
            // trees it owns names fixture modules; and a resurrection guard
            // forbids a name precisely because nothing answers to it.
            if !guard.tokens
                && !guard.owned_trees
                && !guard.resurrection
                && !scanned.iter().any(|root| needle_resolves(needle, root))
            {
                dead.push(format!(
                    "{at}: `{needle}` names no module of any crate this call scans ({})",
                    scanned
                        .iter()
                        .map(|root| under_crates(root))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
    }
    assert!(
        dead.is_empty(),
        "{} boundary needle(s) name nothing the crate under test declares, so they forbid nothing \
         — a needle that resolves to nothing is a hole in the guard, not a stricter guard. If the \
         module moved to another crate, re-point the needle at it (DEC-30 item 1):\n  {}",
        dead.len(),
        dead.join("\n  ")
    );
    assert!(
        unwitnessed.is_empty(),
        "{} boundary needle(s) are not witnessed by the fixture their call site names, so that \
         share of the scan proves nothing:\n  {}",
        unwitnessed.len(),
        unwitnessed.join("\n  ")
    );

    // The inverse that IS a failure: a witness no test target names.
    let named = named_fixtures();
    let mut orphans: Vec<String> = Vec::new();
    for (_, dir) in crate_dirs() {
        for path in walk_any(&dir.join("tests")) {
            let named_here = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| named.contains(name));
            if path.to_string_lossy().ends_with(".rs.txt") && !named_here {
                orphans.push(under_crates(&path));
            }
        }
    }
    orphans.sort();
    assert!(
        orphans.is_empty(),
        "{} witness fixture(s) that no guard names — a guard was deleted and its fixture was not, \
         and a fixture nothing reads is coverage that does not exist:\n  {}",
        orphans.len(),
        orphans.join("\n  ")
    );
}

/// The two judgement calls in [`every_boundary_needle_names_something_live`]
/// discriminate — shown in both states, over literal inputs.
///
/// Both encode a decision rather than a measurement, so both can be widened by
/// an edit that looks like a simplification: `needle_resolves` accepting any
/// crate would keep a dead needle alive off a namesake module elsewhere, and
/// `proves_absence` accepting a bare `.exists()` would exempt any guard that
/// happens to touch the filesystem. Neither widening reds the guard on today's
/// tree, which is exactly why they are pinned here instead.
#[test]
fn needle_liveness_and_the_absence_exemption_discriminate() {
    let store = crates_dir().join("ocx_store/src");
    let util = crates_dir().join("ocx_util/src");

    // A path needle resolves through the `a.rs` + `a/b.rs` layout, and one
    // letter off resolves to nothing. `oci::simplesigning` stood here until
    // WP-31 took it into `ocx_sign`, and `package_manager::tasks` until WP-34
    // took it into `ocx_package_manager` — a self-test naming a module that has
    // moved fails loudly, which is the behaviour wanted, but it has to be
    // re-pointed at a live pair to keep saying anything. Anchored in
    // `ocx_store` this time rather than in `ocx_lib`, which WP-37 deletes: a
    // needle whose crate is scheduled for removal buys one work package.
    assert!(
        needle_resolves("file_structure::blob_store", &store),
        "file_structure/blob_store.rs is a module of ocx_store"
    );
    assert!(
        !needle_resolves("file_structure::blob_stores", &store),
        "a typo must not resolve"
    );

    // A crate-qualified needle resolves against the crate it names, from any
    // crate under test — that is how a re-pointed needle goes green again.
    assert!(needle_resolves("ocx_util::fs", &store));
    assert!(!needle_resolves("ocx_util::fsx", &store));

    // The scope decision: `file_structure` is a module of `ocx_store` and of no
    // other crate here, so the same bare needle is dead for a different crate
    // under test. Accepting "any crate" would report this live.
    assert!(needle_resolves("file_structure", &store));
    assert!(
        !needle_resolves("file_structure", &util),
        "a namesake elsewhere must not keep a needle alive"
    );

    // The exemption is earned by a negated `.exists()`, inside the macro that
    // `syn` does not descend into by itself.
    let parse =
        |body: &str| -> syn::ItemFn { syn::parse_str(&format!("fn probe() {{ {body} }}")).expect("probe parses") };
    assert!(proves_absence(&parse(
        r#"assert!(!p.join("log.rs").exists(), "gone");"#
    )));
    assert!(
        !proves_absence(&parse(r#"assert!(p.join("log.rs").exists(), "there");"#)),
        "an assertion that the path DOES exist is not a burial"
    );
    assert!(
        !proves_absence(&parse(r#"assert!(!v.is_empty(), "nonempty");"#)),
        "any other negated assertion must not exempt a guard from liveness"
    );
    assert!(!proves_absence(&parse("let _ = p.exists();")));
}

#[test]
fn no_log_shim() {
    // D-001 deleted `ocx_lib/src/log.rs`. Naming that one path kept the
    // property alive only while the crate did: WP-37 deleted the whole tree, so
    // the assertion became true for a second reason and its red was no longer
    // reachable. The property D-001 actually states is that *no* library crate
    // grows a `log` shim module, which is a live subject in all 20 — and it is
    // still the `!..exists()` shape `proves_absence` reads to exempt this
    // guard's `"log"` needle from the liveness rule.
    let shims: Vec<String> = crate_dirs()
        .into_iter()
        .filter(|(_, dir)| dir.join("src").join("lib.rs").is_file())
        .filter(|(_, dir)| dir.join("src").join("log.rs").exists())
        .map(|(name, _)| name)
        .collect();
    assert!(
        !crate_dirs()
            .iter()
            .any(|(_, dir)| dir.join("src").join("log.rs").exists()),
        "a log shim module is back in {shims:?} (D-001)"
    );
    assert_no_imports(&library_subtrees(false), &["log"], &fixture("log_shim.rs.txt"));
}

#[test]
fn no_module_path_in_output() {
    assert_no_needles(
        &library_subtrees(true),
        &["module_path!", "type_name::<", "any::type_name", "type_name_of_val"],
        &fixture("no_module_path_in_output.rs.txt"),
    );
}

/// Every `.rs` under a crate's `src/` is named by a `mod` item or `#[path]`
/// in its parent, walking from `lib.rs` / `main.rs` / `bin/*.rs`.
#[test]
fn every_source_file_is_reachable() {
    let mut orphans = Vec::new();
    let mut walked = 0;
    for (name, dir) in crate_dirs() {
        let src = dir.join("src");
        let (files, unreached) = orphans_under(&src);
        assert!(
            files > 0,
            "{}: no `.rs` file — a crate that falls out of the walk leaves `walked` above its floor on the rest",
            src.display()
        );
        walked += files;
        orphans.extend(unreached.iter().map(|f| format!("{name}: src/{}", f.display())));
    }
    assert!(walked > 1, "walked {walked} source file(s) — scanned nothing");
    assert!(
        orphans.is_empty(),
        "source files no `mod` item reaches (a green build hides them):\n  {}",
        orphans.join("\n  ")
    );
}

/// `(files walked, orphans relative to src)` for one `src/` tree.
fn orphans_under(src: &Path) -> (usize, Vec<PathBuf>) {
    let all: BTreeSet<PathBuf> = rust_sources(src).into_iter().collect();
    let mut reached = BTreeSet::new();
    let mut pending: Vec<PathBuf> = ["lib.rs", "main.rs"]
        .iter()
        .map(|f| src.join(f))
        .filter(|f| f.is_file())
        .collect();
    let bin_dir = src.join("bin");
    pending.extend(all.iter().filter(|f| f.parent() == Some(bin_dir.as_path())).cloned());
    while let Some(file) = pending.pop() {
        if !reached.insert(file.clone()) {
            continue;
        }
        for child in declared_submodules(&file, src) {
            if child.is_file() {
                pending.push(child);
            }
        }
    }
    let orphans = all
        .difference(&reached)
        .map(|f| f.strip_prefix(src).expect("under src").to_path_buf())
        .collect();
    (all.len(), orphans)
}

/// The orphan detector on a tree with every module spelling it must follow:
/// `#[path = "…"]`, `#[cfg_attr(…, path = "…")]`, `#[cfg(test)] mod`, a plain
/// `mod x;` resolved through `x/mod.rs`, and one file nothing names — exactly
/// that one is reported. The decoys — a `path = "decoy.rs"` in a trailing
/// comment, a `mod decoy;` inside a string, a `mod nothing;` in the crate
/// docs — must name nothing.
#[test]
fn orphan_detector_follows_every_mod_spelling() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let write = |rel: &str, text: &str| {
        let path = src.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
    };
    write(
        "lib.rs",
        "//! crate docs mentioning mod nothing;\n\
         #[path = \"impls/first.rs\"]\n\
         mod first;\n\
         #[cfg_attr(unix, path = \"plat/unix.rs\")] // path = \"decoy.rs\"\n\
         #[cfg_attr(windows, path = \"plat/windows.rs\")]\n\
         mod plat;\n\
         #[cfg(test)]\n\
         mod tests;\n\
         pub mod nested;\n\
         const DECOY: &str = \"mod decoy;\";\n",
    );
    write("impls/first.rs", "pub fn first() {}\n");
    write("plat/unix.rs", "pub fn plat() {}\n");
    write("plat/windows.rs", "pub fn plat() {}\n");
    write("tests.rs", "#[test]\nfn t() {}\n");
    write("nested/mod.rs", "pub(crate) mod leaf;\n");
    write("nested/leaf.rs", "pub fn leaf() {}\n");
    write("orphan.rs", "pub fn nobody_names_me() {}\n");
    write("decoy.rs", "pub fn only_a_comment_names_me() {}\n");
    let (walked, orphans) = orphans_under(&src);
    assert_eq!(walked, 9, "the fixture tree has nine files");
    assert_eq!(
        orphans,
        [PathBuf::from("decoy.rs"), PathBuf::from("orphan.rs")],
        "exactly the two files no live `mod` item names are orphans"
    );
}

/// The files the `mod x;` declarations in `file` resolve to. `#[path = "…"]`
/// (also inside `#[cfg_attr(…, path = "…")]`) is relative to the declaring
/// file's directory — inside an inline `mod a { … }` to that module's
/// directory; otherwise `mod x;` in `lib.rs`/`main.rs`/`mod.rs` looks beside
/// the file and any other file looks in its own-named subdirectory.
fn declared_submodules(file: &Path, src: &Path) -> Vec<PathBuf> {
    let dir = file.parent().expect("has parent").to_path_buf();
    let stem = file.file_stem().expect("stem").to_string_lossy();
    let module_dir =
        if file == src.join("lib.rs") || file == src.join("main.rs") || stem == "mod" || dir.ends_with("bin") {
            dir.clone()
        } else {
            dir.join(&*stem)
        };
    struct Mods {
        dir: PathBuf,
        module_dir: PathBuf,
        inline: Vec<String>,
        out: Vec<PathBuf>,
    }
    impl<'ast> Visit<'ast> for Mods {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            if let Some((_, items)) = &module.content {
                self.inline.push(module.ident.to_string());
                for item in items {
                    self.visit_item(item);
                }
                self.inline.pop();
                return;
            }
            let inline_dir = self
                .inline
                .iter()
                .fold(self.module_dir.clone(), |dir, name| dir.join(name));
            let paths = path_attributes(&module.attrs);
            if paths.is_empty() {
                let name = module.ident.to_string();
                self.out.push(inline_dir.join(format!("{name}.rs")));
                self.out.push(inline_dir.join(name).join("mod.rs"));
            } else {
                let base = if self.inline.is_empty() { &self.dir } else { &inline_dir };
                self.out.extend(paths.into_iter().map(|path| base.join(path)));
            }
        }
    }
    let mut mods = Mods {
        dir,
        module_dir,
        inline: Vec::new(),
        out: Vec::new(),
    };
    mods.visit_file(&Source::parse(file).file);
    mods.out
}

/// Every `path = "…"` an item's attributes name: `#[path = "…"]` and each
/// `#[cfg_attr(…, path = "…")]` — a `unix`/`windows` pair names two files
/// for one item, and both must count.
fn path_attributes(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        match &attr.meta {
            syn::Meta::NameValue(pair) if pair.path.is_ident("path") => {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(value),
                    ..
                }) = &pair.value
                {
                    out.push(value.value());
                }
            }
            syn::Meta::List(list) if list.path.is_ident("cfg_attr") => {
                let trees: Vec<proc_macro2::TokenTree> = list.tokens.clone().into_iter().collect();
                for window in trees.windows(3) {
                    if let [
                        proc_macro2::TokenTree::Ident(key),
                        proc_macro2::TokenTree::Punct(eq),
                        proc_macro2::TokenTree::Literal(literal),
                    ] = window
                        && key == "path"
                        && eq.as_char() == '='
                        && let syn::Lit::Str(value) = syn::Lit::new(literal.clone())
                    {
                        out.push(value.value());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The SSRF construction ratchet (C-053, ratchet half): every
/// `reqwest::Client` / `reqwest::ClientBuilder` construction in production
/// code either seeds a `GuardedResolver`, follows a `guard_destination` call
/// in the same function, or is on the committed allowlist — which only
/// shrinks. Each allowlist entry carries the number of constructions in that
/// function, so a second one added inside an allow-listed function reds
/// instead of hiding behind the entry.
#[test]
fn ssrf_guard_ratchet() {
    let allowlist_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssrf_unguarded_baseline.txt");
    let mut allowlist: BTreeMap<String, usize> = BTreeMap::new();
    for line in std::fs::read_to_string(&allowlist_path)
        .expect("allowlist readable")
        .lines()
    {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, count) = line
            .rsplit_once(' ')
            .and_then(|(key, count)| count.parse::<usize>().ok().map(|count| (key, count)))
            .unwrap_or_else(|| panic!("{}: `{line}` is not `<path> <fn> <count>`", allowlist_path.display()));
        assert!(
            count > 0 && allowlist.insert(key.to_owned(), count).is_none(),
            "{}: `{line}` — zero count or duplicate key",
            allowlist_path.display()
        );
    }

    let mut unguarded: BTreeMap<String, usize> = BTreeMap::new();
    let mut walked = 0;
    for (_, dir) in crate_dirs() {
        let sources = rust_sources(&dir.join("src"));
        assert!(
            !sources.is_empty(),
            "{}: no `.rs` file — a crate that falls out of the walk leaves `walked` above its floor on the rest",
            dir.join("src").display()
        );
        for file in sources {
            walked += 1;
            let rel = file
                .strip_prefix(crates_dir())
                .expect("under crates/")
                .to_string_lossy()
                .replace('\\', "/");
            for (fn_name, _) in unguarded_constructions(&Source::parse(&file).file) {
                *unguarded.entry(format!("{rel} {fn_name}")).or_default() += 1;
            }
        }
    }
    assert!(walked > 1, "walked {walked} source file(s) — scanned nothing");

    // The witness: one unguarded construction per spelling family the scanner
    // must flag — `builder()` and the `impl Default` form, each in its own fn,
    // so dropping either needle reds here instead of silently narrowing the scan.
    let witness = fixture("ssrf_guard_ratchet.rs.txt");
    let witness_hits: BTreeSet<String> = unguarded_constructions(&Source::parse(&witness).file)
        .into_iter()
        .map(|(fn_name, _)| fn_name)
        .collect();
    let expected: BTreeSet<String> = ["fetch", "fetch_default"].iter().map(|s| s.to_string()).collect();
    assert_eq!(
        witness_hits,
        expected,
        "witness {} — every construction spelling it carries must be recognised as unguarded",
        witness.display()
    );

    // Grown: a construction with no entry, or more in a function than its
    // entry admits. Stale: an entry with no construction left, or with more
    // than the function still holds — the list only shrinks, counts included.
    let mut grown = Vec::new();
    let mut stale = Vec::new();
    for (key, live) in &unguarded {
        match allowlist.get(key) {
            None => grown.push(format!("{key} ({live}, no entry)")),
            Some(allowed) if live > allowed => grown.push(format!("{key} ({live} live, {allowed} allowed)")),
            Some(allowed) if live < allowed => stale.push(format!("{key} ({allowed} allowed, {live} live)")),
            Some(_) => {}
        }
    }
    stale.extend(
        allowlist
            .iter()
            .filter(|(key, _)| !unguarded.contains_key(*key))
            .map(|(key, allowed)| format!("{key} ({allowed} allowed, 0 live)")),
    );
    assert!(
        grown.is_empty(),
        "new unguarded reqwest client construction(s) — seed a GuardedResolver or call guard_destination first:\n  {}",
        grown.join("\n  ")
    );
    assert!(
        stale.is_empty(),
        "stale allowlist entries in {} — the list only shrinks, delete them or lower the count:\n  {}",
        allowlist_path.display(),
        stale.join("\n  ")
    );
    assert!(
        !unguarded.is_empty() || allowlist.is_empty(),
        "no construction found at all while the allowlist is non-empty"
    );
}

/// A qualified construction inside macro arguments, in a file that also
/// imports the bare name, is one site — the bare spelling must not count
/// the tail of the qualified one again.
#[test]
fn ssrf_ratchet_counts_a_qualified_construction_in_macro_arguments_once() {
    let file =
        syn::parse_file("use reqwest::Client;\nfn q11() {\n    vec![reqwest::Client::new()];\n}\n").expect("parses");
    assert_eq!(unguarded_constructions(&file), [("q11".to_owned(), 3)]);
}

/// `(enclosing fn, line)` of every reqwest client construction in `file`
/// outside `#[cfg(test)]` items that neither seeds a `GuardedResolver` in its
/// function nor follows `guard_destination`. A construction inside a macro
/// invocation's arguments is found by walking those tokens; one outside any
/// function is reported under `-`.
fn unguarded_constructions(file: &syn::File) -> Vec<(String, usize)> {
    // `::default(` is the `impl Default` on both types — the same constructor
    // under a third name (reqwest `async_impl/client.rs`).
    const CONSTRUCTORS: [(&str, &str); 5] = [
        ("Client", "builder"),
        ("Client", "new"),
        ("Client", "default"),
        ("ClientBuilder", "new"),
        ("ClientBuilder", "default"),
    ];

    // Bare spellings count only where the file imports them from reqwest.
    struct ReqwestImports(bool);
    impl Visit<'_> for ReqwestImports {
        fn visit_item_use(&mut self, item: &syn::ItemUse) {
            for (path, alias) in expand_use_tree(&item.tree) {
                let names = path.iter().chain(alias.as_ref());
                if path.first().is_some_and(|first| first == "reqwest")
                    && names.into_iter().any(|n| n.contains("Client"))
                {
                    self.0 = true;
                }
            }
        }
    }
    let mut imports = ReqwestImports(false);
    imports.visit_file(file);
    let bare = imports.0;

    /// The tokens of one construction spelling, `reqwest::Client::builder(`
    /// or its bare form, for the macro-argument walk.
    fn spelling(qualified: bool, type_name: &str, constructor: &str) -> Vec<String> {
        let mut out = Vec::new();
        if qualified {
            out.extend(["reqwest", ":", ":"].map(str::to_owned));
        }
        out.extend([type_name, ":", ":", constructor, "("].map(str::to_owned));
        out
    }
    let spellings: Vec<Vec<String>> = CONSTRUCTORS
        .iter()
        .flat_map(|(type_name, constructor)| {
            let mut forms = vec![spelling(true, type_name, constructor)];
            if bare {
                forms.push(spelling(false, type_name, constructor));
            }
            forms
        })
        .collect();

    /// What one function's body has shown so far. On leaving a nested `fn`
    /// its guards fold into the parent: the parent's body text holds the
    /// nested function too, so a guard there guards the parent's sites.
    #[derive(Default)]
    struct Frame {
        name: String,
        constructions: Vec<proc_macro2::LineColumn>,
        guards: Vec<proc_macro2::LineColumn>,
        resolver: bool,
    }
    struct Ratchet<'a> {
        spellings: &'a [Vec<String>],
        frames: Vec<Frame>,
        out: Vec<(String, usize)>,
    }
    impl Ratchet<'_> {
        fn construction(&mut self, at: proc_macro2::LineColumn) {
            match self.frames.last_mut() {
                Some(frame) => frame.constructions.push(at),
                None => self.out.push(("-".to_owned(), at.line)),
            }
        }
        fn ident(&mut self, name: &str, at: proc_macro2::LineColumn) {
            let Some(frame) = self.frames.last_mut() else { return };
            match name {
                "GuardedResolver" => frame.resolver = true,
                "guard_destination" => frame.guards.push(at),
                _ => {}
            }
        }
        fn enter(&mut self, name: &syn::Ident) {
            self.frames.push(Frame {
                name: name.to_string(),
                ..Frame::default()
            });
        }
        fn leave(&mut self) {
            let frame = self.frames.pop().expect("a frame was entered");
            for at in &frame.constructions {
                let guarded = frame.resolver || frame.guards.iter().any(|guard| guard < at);
                if !guarded {
                    self.out.push((frame.name.clone(), at.line));
                }
            }
            if let Some(parent) = self.frames.last_mut() {
                parent.resolver |= frame.resolver;
                parent.guards.extend(frame.guards);
            }
        }
        fn scan_tokens(&mut self, tokens: &[Token]) {
            for (index, token) in tokens.iter().enumerate() {
                self.ident(&token.text, token.at);
                // A bare `Client::new(` right after `::` is the tail of the
                // qualified spelling, already counted at `reqwest`.
                let after_separator = index > 1 && tokens[index - 1].text == ":" && tokens[index - 2].text == ":";
                let hit = self.spellings.iter().any(|want| {
                    !(after_separator && want[0] != "reqwest")
                        && tokens[index..].iter().zip(want).filter(|(t, w)| t.text == **w).count() == want.len()
                });
                if hit {
                    self.construction(token.at);
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Ratchet<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let attrs = match item {
                syn::Item::Const(i) => &i.attrs,
                syn::Item::Enum(i) => &i.attrs,
                syn::Item::ExternCrate(i) => &i.attrs,
                syn::Item::Fn(i) => &i.attrs,
                syn::Item::ForeignMod(i) => &i.attrs,
                syn::Item::Impl(i) => &i.attrs,
                syn::Item::Macro(i) => &i.attrs,
                syn::Item::Mod(i) => &i.attrs,
                syn::Item::Static(i) => &i.attrs,
                syn::Item::Struct(i) => &i.attrs,
                syn::Item::Trait(i) => &i.attrs,
                syn::Item::TraitAlias(i) => &i.attrs,
                syn::Item::Type(i) => &i.attrs,
                syn::Item::Union(i) => &i.attrs,
                syn::Item::Use(i) => &i.attrs,
                _ => &Vec::new(),
            };
            if !is_cfg_test(attrs) {
                syn::visit::visit_item(self, item);
            }
        }
        fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
            let attrs = match item {
                syn::ImplItem::Const(i) => &i.attrs,
                syn::ImplItem::Fn(i) => &i.attrs,
                syn::ImplItem::Type(i) => &i.attrs,
                syn::ImplItem::Macro(i) => &i.attrs,
                _ => &Vec::new(),
            };
            if !is_cfg_test(attrs) {
                syn::visit::visit_impl_item(self, item);
            }
        }
        fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
            self.enter(&item.sig.ident);
            syn::visit::visit_item_fn(self, item);
            self.leave();
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            self.enter(&item.sig.ident);
            syn::visit::visit_impl_item_fn(self, item);
            self.leave();
        }
        fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
            self.enter(&item.sig.ident);
            syn::visit::visit_trait_item_fn(self, item);
            self.leave();
        }
        fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = &*call.func {
                let mut spelled: Vec<String> = Vec::new();
                for segment in &path.path.segments {
                    if !spelled.is_empty() {
                        spelled.extend([":".to_owned(), ":".to_owned()]);
                    }
                    spelled.push(segment.ident.to_string());
                }
                spelled.push("(".to_owned());
                if self.spellings.contains(&spelled) {
                    self.construction(path.path.segments[0].ident.span().start());
                }
            }
            syn::visit::visit_expr_call(self, call);
        }
        fn visit_ident(&mut self, ident: &'ast syn::Ident) {
            self.ident(&ident.to_string(), ident.span().start());
        }
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            self.visit_path(&mac.path);
            self.scan_tokens(&flatten(mac.tokens.clone()));
        }
    }
    let mut ratchet = Ratchet {
        spellings: &spellings,
        frames: Vec::new(),
        out: Vec::new(),
    };
    ratchet.visit_file(file);
    ratchet.out
}

// ---------------------------------------------------------------------------
// Inert test loops
// ---------------------------------------------------------------------------

/// No loop inside test code has a body that can neither fail nor be observed.
///
/// WP-10 carried a classification ladder from the libraries into the binary and
/// left thirteen table-driven tests as `for (_expected, _kind) in pairs {}` —
/// the match arms went with the types, the loop stayed, and a length assertion
/// above it kept each test green. They compiled, `cargo nextest` passed them,
/// and they pinned nothing. More batches of the same kind of move are queued,
/// each stripping arms and bodies out of tests, so this is the gate rather than
/// whoever happens to re-read the diff.
///
/// **Violation.** A `for`, `while` or `loop` lexically inside test code whose
/// body holds no call, no method call, no macro invocation, no assignment or
/// compound assignment, no `.await`, no `?` and no wildcard-free `match`. Such
/// a body can do nothing at all: it cannot assert, cannot panic, cannot
/// propagate an error, cannot make the compiler refuse the file, and cannot
/// leave a mark outside itself. It is the shape an in-place strip leaves.
///
/// **Exempt, and the ceiling that implies.** A body that calls *anything*
/// passes, because the call may be a helper that asserts (`check_case(row)`)
/// and nothing short of resolving the callee tells that apart from
/// `parse(row)`. So `for row in rows { parse(row); }` is NOT caught — reaching
/// that class means resolving callees, not loosening this predicate, and
/// loosening it instead would put noise in front of the next reader and earn
/// the guard an allowlist. A `while` whose *condition* is itself active is
/// exempt too: in `while let Some(task) = set.join_next().await {}` the
/// condition is the loop's work and the empty body is correct (the task drain
/// in `package_manager/tasks/clean.rs` is the production twin). A `for`'s
/// iterator expression gets no such carve-out — it is evaluated once and says
/// nothing about what the body does per item. And a body whose only content is
/// a wildcard-free `match` with `{}` arms is exempt because that loop's whole
/// purpose is the compile error a new variant would cause; the three sites in
/// `crates/ocx_cli` that first reddened this guard are all that idiom.
///
/// **Scope.** Every `crates/*/src`: where `#[cfg(test)]` modules live and where
/// the batches move code from. A crate's `tests/` directory is not walked,
/// because `crates/ocx_test_support/tests/boundary_fixtures/` deliberately
/// holds a file that is not valid Rust and `Source::parse` is loud by design —
/// reaching integration tests needs a path exclusion this guard has no defect
/// to justify yet.
///
/// **The positive control is not noise — do not delete it.** The `required`
/// list names every loop keyword the witness must still be recognised through.
/// A scanner that stopped descending into test functions, or stopped matching
/// one of the three shapes, would report zero violations across the workspace —
/// which is exactly what a clean tree reports. The witness is the only thing
/// that tells those two states apart, and each atom is one more way for the
/// difference to surface as red rather than as a green that never ran.
#[test]
fn tests_hold_no_inert_loop() {
    assert_scan(
        &library_subtrees(true),
        &fixture("inert_test_loop.rs.txt"),
        "a loop in a test whose body can neither fail nor be observed",
        &["for", "while", "loop", "fn"],
        &|path, source| inert_test_loops(path, &source.file),
    );
}

/// Red and green for the predicate, on snippets this test owns. Every clause of
/// `inert_test_loops` has one case that isolates it, so deleting a clause reds
/// here — where the cause is named — before the workspace scan turns a whole
/// crate's worth of honest loops into false positives.
#[test]
fn inert_loop_predicate_reds_and_greens() {
    let hits = |source: &str| -> Vec<String> {
        let parsed = syn::parse_file(source).unwrap_or_else(|error| panic!("snippet parses: {error}"));
        inert_test_loops(Path::new("snippet.rs"), &parsed)
            .into_iter()
            .map(|reach| reach.path)
            .collect()
    };

    // Red — one per keyword, and one per way of entering test code.
    assert_eq!(hits("#[test] fn t() { for (_a, _b) in pairs {} }"), ["for"]);
    assert_eq!(hits("#[test] fn t() { while flag {} }"), ["while"]);
    assert_eq!(hits("#[cfg(test)] mod t { fn helper() { loop {} } }"), ["loop"]);
    assert_eq!(hits("#[cfg(test)] impl F { fn helper() { for _x in xs {} } }"), ["for"]);
    assert_eq!(hits("#[rstest] fn t() { for _x in xs { let _y = _x; } }"), ["for"]);
    // A nested inert loop is reported once, at the outermost one.
    assert_eq!(hits("#[test] fn t() { for _a in xs { for _b in ys {} } }"), ["for"]);
    // A `match` with a catch-all arm is not an exhaustiveness guard.
    assert_eq!(
        hits("#[test] fn t() { for s in xs { match s { A::X => {}, _ => {} } } }"),
        ["for"]
    );
    // An emptied body has no loop to be inert — the clause that would have
    // caught B2-1's four husks.
    assert_eq!(hits("#[test] fn t() {}"), ["fn"]);
    assert_eq!(hits("#[tokio::test] async fn t() {}"), ["fn"]);

    // Green — each body carries exactly one of the things that make it active.
    for (clause, source) in [
        ("macro", "#[test] fn t() { for x in xs { assert_eq!(x, 1); } }"),
        ("call", "#[test] fn t() { for x in xs { parse(x); } }"),
        ("method call", "#[test] fn t() { for x in xs { out.push(x); } }"),
        ("compound assign", "#[test] fn t() { for x in xs { sum += x; } }"),
        ("assign", "#[test] fn t() { for x in xs { seen = x; } }"),
        ("try", "#[test] fn t() -> R { for x in xs { x?; } Ok(()) }"),
        ("await", "#[tokio::test] async fn t() { for x in xs { x.await; } }"),
        (
            "active while condition",
            "#[test] fn t() { while let Some(_x) = it.next() {} }",
        ),
        (
            "exhaustiveness guard",
            "#[test] fn t() { for s in xs { match s { A::X => {}, A::Y => {} } } }",
        ),
        ("outside test code", "fn helper() { for _x in xs {} }"),
        // Only a *marked* test's empty body is a husk. An empty helper under
        // `#[cfg(test)]` is a legitimate no-op stub, and an unmarked free
        // function is not a test at all.
        ("empty cfg(test) helper", "#[cfg(test)] mod t { fn noop() {} }"),
        ("empty unmarked fn", "fn noop() {}"),
    ] {
        assert!(
            hits(source).is_empty(),
            "the {clause} case must stay green — `{source}` was flagged, so a clause of inert_test_loops \
             has gone missing or over-wide"
        );
    }
}

/// Every loop in `file`'s test code whose body is inert, as `Reach`es whose
/// `path` is the loop keyword — the atoms `tests_hold_no_inert_loop` requires
/// its witness to keep producing. Test code is a `#[cfg(test)]` `mod` or
/// `impl`, or a function carrying a test marker (`#[test]`, `#[tokio::test]`,
/// `#[rstest]` — any attribute whose last path segment ends in `test`).
fn inert_test_loops(path: &Path, file: &syn::File) -> Vec<Reach> {
    fn is_test_marker(attrs: &[syn::Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident.to_string().ends_with("test"))
        })
    }

    /// Whether a node holds anything that could fail or be observed from
    /// outside it. One clause per shape; the doc comment on
    /// `tests_hold_no_inert_loop` says why the list stops where it does.
    #[derive(Default)]
    struct Active(bool);
    impl Visit<'_> for Active {
        fn visit_expr_call(&mut self, node: &syn::ExprCall) {
            self.0 = true;
            syn::visit::visit_expr_call(self, node);
        }
        fn visit_expr_method_call(&mut self, node: &syn::ExprMethodCall) {
            self.0 = true;
            syn::visit::visit_expr_method_call(self, node);
        }
        // Not descended into: a macro's presence is already the answer, and its
        // tokens are not a syntax tree `Visit` can walk.
        fn visit_macro(&mut self, _: &syn::Macro) {
            self.0 = true;
        }
        fn visit_expr_try(&mut self, node: &syn::ExprTry) {
            self.0 = true;
            syn::visit::visit_expr_try(self, node);
        }
        fn visit_expr_await(&mut self, node: &syn::ExprAwait) {
            self.0 = true;
            syn::visit::visit_expr_await(self, node);
        }
        fn visit_expr_assign(&mut self, node: &syn::ExprAssign) {
            self.0 = true;
            syn::visit::visit_expr_assign(self, node);
        }
        // A wildcard-free `match` is an assertion the *compiler* evaluates:
        // adding a variant breaks the build. That is the entire body of the
        // exhaustiveness-guard loops in `app.rs` and `api/data/shell_state.rs`,
        // whose arms are all `{}`. One catch-all arm and it asserts nothing.
        fn visit_expr_match(&mut self, node: &syn::ExprMatch) {
            fn is_catch_all(pat: &syn::Pat) -> bool {
                match pat {
                    syn::Pat::Wild(_) => true,
                    syn::Pat::Ident(pat) => pat.subpat.is_none(),
                    syn::Pat::Or(pat) => pat.cases.iter().any(is_catch_all),
                    syn::Pat::Paren(pat) => is_catch_all(&pat.pat),
                    _ => false,
                }
            }
            self.0 |= node.arms.iter().all(|arm| !is_catch_all(&arm.pat));
            syn::visit::visit_expr_match(self, node);
        }
        fn visit_expr_binary(&mut self, node: &syn::ExprBinary) {
            self.0 |= matches!(
                node.op,
                syn::BinOp::AddAssign(_)
                    | syn::BinOp::SubAssign(_)
                    | syn::BinOp::MulAssign(_)
                    | syn::BinOp::DivAssign(_)
                    | syn::BinOp::RemAssign(_)
                    | syn::BinOp::BitXorAssign(_)
                    | syn::BinOp::BitAndAssign(_)
                    | syn::BinOp::BitOrAssign(_)
                    | syn::BinOp::ShlAssign(_)
                    | syn::BinOp::ShrAssign(_)
            );
            syn::visit::visit_expr_binary(self, node);
        }
    }
    fn is_active(visit: impl FnOnce(&mut Active)) -> bool {
        let mut active = Active::default();
        visit(&mut active);
        active.0
    }

    struct Scan<'a> {
        path: &'a Path,
        /// How many enclosing items put this position inside test code.
        depth: usize,
        out: Vec<Reach>,
    }
    impl Scan<'_> {
        fn inert(&mut self, keyword: &str, at: proc_macro2::LineColumn) {
            if self.depth > 0 {
                self.out.push(Reach {
                    file: self.path.to_path_buf(),
                    line: at.line,
                    path: keyword.to_owned(),
                });
            }
        }
    }
    impl<'ast> Visit<'ast> for Scan<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let test = match item {
                syn::Item::Fn(item) => is_test_marker(&item.attrs),
                syn::Item::Mod(item) => is_cfg_test(&item.attrs),
                syn::Item::Impl(item) => is_cfg_test(&item.attrs),
                _ => false,
            };
            // A test whose whole body is `{}` is the same strip one step
            // further along — there is no loop left to find inert, and the
            // name still promises whatever it was named after. Only a
            // *marked* test counts: an empty helper under `#[cfg(test)]` is a
            // legitimate no-op stub, and libtest collects free functions
            // only, so an empty `#[test]` inside an `impl` is not a test at
            // all and is left to the compiler's own dead-code complaint.
            if let syn::Item::Fn(function) = item
                && is_test_marker(&function.attrs)
                && function.block.stmts.is_empty()
            {
                self.out.push(Reach {
                    file: self.path.to_path_buf(),
                    line: function.sig.fn_token.span.start().line,
                    path: "fn".to_owned(),
                });
            }
            self.depth += usize::from(test);
            syn::visit::visit_item(self, item);
            self.depth -= usize::from(test);
        }
        // An inert body holds no call, so it holds no active nested loop
        // either: reporting the outermost one and stopping keeps one strip
        // from being counted twice.
        fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
            if is_active(|active| active.visit_block(&node.body)) {
                syn::visit::visit_expr_for_loop(self, node);
            } else {
                self.inert("for", node.for_token.span.start());
            }
        }
        fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
            let condition_works = is_active(|active| active.visit_expr(&node.cond));
            if condition_works || is_active(|active| active.visit_block(&node.body)) {
                syn::visit::visit_expr_while(self, node);
            } else {
                self.inert("while", node.while_token.span.start());
            }
        }
        fn visit_expr_loop(&mut self, node: &'ast syn::ExprLoop) {
            if is_active(|active| active.visit_block(&node.body)) {
                syn::visit::visit_expr_loop(self, node);
            } else {
                self.inert("loop", node.loop_token.span.start());
            }
        }
    }

    let mut scan = Scan {
        path,
        depth: 0,
        out: Vec::new(),
    };
    scan.visit_file(file);
    scan.out
}
