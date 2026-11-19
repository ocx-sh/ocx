// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OpenTelemetry exemption's mechanical conditions: exact pins, no extra features, one
//! dependent crate that inherits them verbatim, a pointer comment on every line, and a lock where
//! only the registry's OpenTelemetry crates exist and only that crate depends on them.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

const POINTER: &str = "# OBS-12 exemption and its conditions: .claude/artifacts/adr_otel_telemetry.md";
const VERSION: &str = "=0.33.1";
const LOCKED_VERSION: &str = "0.33.1";
/// Crate, exact feature set.
const PINS: &[(&str, &[&str])] = &[
    ("opentelemetry", &["logs"]),
    ("opentelemetry_sdk", &["logs"]),
    ("opentelemetry-otlp", &["http-proto", "logs"]),
    ("opentelemetry-http", &[]),
];
const FORBIDDEN_FEATURES: &[&str] = &[
    "reqwest-client",
    "reqwest-blocking-client",
    "hyper-client",
    "grpc-tonic",
    "gzip-http",
    "zstd-http",
    "http-json",
    "internal-logs",
];
const FORBIDDEN_FEATURE_PREFIXES: &[&str] = &["tls", "rt-", "experimental"];
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];
/// The only manifest that may name an OpenTelemetry crate: compared whole, never by suffix.
const OWNER_MANIFEST: &str = "crates/ocx_telemetry/Cargo.toml";
const OWNER_PACKAGE: &str = "ocx_telemetry";
const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
const WORKSPACE_DEPENDENCIES: &str = "workspace.dependencies";

fn is_opentelemetry(name: &str) -> bool {
    name.starts_with("opentelemetry")
}

/// One dependency-shaped entry: where it sits, the key it is written under and its spec.
struct Entry<'a> {
    table: String,
    key: &'a str,
    spec: &'a Value,
}

impl Entry<'_> {
    /// The package the entry means: `package = "…"` renames it, and a `[replace]` key is `name:version`.
    fn resolved(&self) -> &str {
        match self.spec.get("package").and_then(Value::as_str) {
            Some(package) => package,
            None => self.key.split(':').next().unwrap_or(self.key),
        }
    }

    fn names_opentelemetry(&self) -> bool {
        is_opentelemetry(self.key) || is_opentelemetry(self.resolved())
    }
}

/// Every dependency table a manifest can carry: the three kinds at the top and under each
/// `[target.*]`, `[workspace.dependencies]`, `[patch.*]` and `[replace]`.
fn entries(manifest: &Table) -> Vec<Entry<'_>> {
    fn add<'a>(tables: &mut Vec<(String, &'a Table)>, name: String, value: Option<&'a Value>) {
        if let Some(table) = value.and_then(Value::as_table) {
            tables.push((name, table));
        }
    }
    let mut tables: Vec<(String, &Table)> = Vec::new();
    for kind in DEPENDENCY_TABLES {
        add(&mut tables, (*kind).to_owned(), manifest.get(*kind));
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, target) in targets {
            for kind in DEPENDENCY_TABLES {
                add(&mut tables, format!("target.{cfg}.{kind}"), target.get(*kind));
            }
        }
    }
    add(
        &mut tables,
        WORKSPACE_DEPENDENCIES.to_owned(),
        manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
    );
    if let Some(patches) = manifest.get("patch").and_then(Value::as_table) {
        for (registry, patch) in patches {
            add(&mut tables, format!("patch.{registry}"), Some(patch));
        }
    }
    add(&mut tables, "replace".to_owned(), manifest.get("replace"));
    tables
        .into_iter()
        .flat_map(|(table, entries)| {
            entries.iter().map(move |(key, spec)| Entry {
                table: table.clone(),
                key: key.as_str(),
                spec,
            })
        })
        .collect()
}

/// What a report found, so a test can floor the reader as well as the subject.
#[derive(Default)]
struct Findings {
    problems: Vec<String>,
    pins_read: usize,
    locked_read: usize,
    lock_edges_read: usize,
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn parse(text: &str, label: &str) -> Table {
    text.parse::<Table>()
        .unwrap_or_else(|error| panic!("parse {label}: {error}"))
}

fn features_of(value: &Value) -> Vec<String> {
    let mut features: Vec<String> = value
        .get("features")
        .and_then(Value::as_array)
        .map(|array| array.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    features.sort();
    features
}

fn check_pins(root_manifest: &str, findings: &mut Findings) {
    let manifest = parse(root_manifest, "root Cargo.toml");
    let table = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("dependencies"))
        .and_then(Value::as_table);
    for entry in entries(&manifest)
        .iter()
        .filter(|entry| entry.table == WORKSPACE_DEPENDENCIES && entry.names_opentelemetry())
    {
        if entry.key != entry.resolved() {
            findings.problems.push(format!(
                "root Cargo.toml: [{}] {} renames {}",
                entry.table,
                entry.key,
                entry.resolved()
            ));
        }
    }
    for (name, expected_features) in PINS {
        let Some(spec) = table.and_then(|table| table.get(*name)) else {
            findings.problems.push(format!("root Cargo.toml has no pin for {name}"));
            continue;
        };
        findings.pins_read += 1;
        if spec.get("version").and_then(Value::as_str) != Some(VERSION) {
            findings.problems.push(format!("{name}: version is not {VERSION}"));
        }
        if spec.get("default-features").and_then(Value::as_bool) != Some(false) {
            findings.problems.push(format!("{name}: default-features is not false"));
        }
        let features = features_of(spec);
        let mut expected: Vec<String> = expected_features.iter().map(|feature| (*feature).to_owned()).collect();
        expected.sort();
        if features != expected {
            findings
                .problems
                .push(format!("{name}: features {features:?}, expected {expected:?}"));
        }
        for feature in &features {
            if FORBIDDEN_FEATURES.contains(&feature.as_str())
                || FORBIDDEN_FEATURE_PREFIXES
                    .iter()
                    .any(|prefix| feature.starts_with(prefix))
            {
                findings.problems.push(format!("{name}: forbidden feature {feature}"));
            }
        }
    }
}

/// Every `opentelemetry*` dependency line carries the pointer comment on the line above or its own.
fn check_pointer_comments(text: &str, label: &str, findings: &mut Findings) {
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let key = line.split(['=', ' ', '.']).next().unwrap_or("");
        let renamed = line.contains("package") && line.contains("\"opentelemetry");
        if !(is_opentelemetry(key) || renamed) || !line.contains('=') {
            continue;
        }
        let above = index.checked_sub(1).map(|previous| lines[previous].trim());
        if above != Some(POINTER) && !line.contains(POINTER) {
            findings
                .problems
                .push(format!("{label}:{}: {key} lacks the pointer comment", index + 1));
        }
    }
}

/// Every manifest, the root included: no OpenTelemetry crate is patched or replaced, none is named
/// outside [`OWNER_MANIFEST`], and there each entry is exactly `workspace = true`, so a member can
/// add no feature, version or rename to what the root pinned. `[workspace.dependencies]` is the
/// root pins' own table, read by [`check_pins`].
fn check_crate_manifest(text: &str, label: &str, findings: &mut Findings) {
    check_pointer_comments(text, label, findings);
    let manifest = parse(text, label);
    for entry in entries(&manifest) {
        if entry.table == WORKSPACE_DEPENDENCIES || !entry.names_opentelemetry() {
            continue;
        }
        let resolved = entry.resolved();
        if entry.table.starts_with("patch") || entry.table == "replace" {
            findings.problems.push(format!(
                "{label}: [{}] names {resolved}; an OpenTelemetry crate is never patched or replaced",
                entry.table
            ));
        } else if label != OWNER_MANIFEST {
            findings.problems.push(format!(
                "{label} names [{}] {resolved}; only ocx_telemetry may",
                entry.table
            ));
        } else if !entry
            .spec
            .as_table()
            .is_some_and(|spec| spec.len() == 1 && spec.get("workspace") == Some(&Value::Boolean(true)))
        {
            findings.problems.push(format!(
                "{label}: [{}] {} must be exactly `workspace = true`",
                entry.table, entry.key
            ));
        }
    }
}

/// Every `opentelemetry*` package is the registry's own, checksummed copy at the pinned version,
/// and nothing but those crates and the owner depends on one: a third party that did would unify
/// extra features into the build.
fn check_lock(lock_text: &str, findings: &mut Findings) {
    let lock = parse(lock_text, "Cargo.lock");
    let packages = lock
        .get("package")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for package in &packages {
        let name = package.get("name").and_then(Value::as_str).unwrap_or("");
        let version = package.get("version").and_then(Value::as_str).unwrap_or("");
        if name == "tonic" {
            findings.problems.push("Cargo.lock holds a tonic package".to_owned());
        }
        if is_opentelemetry(name) {
            findings.locked_read += 1;
            if version != LOCKED_VERSION {
                findings
                    .problems
                    .push(format!("Cargo.lock: {name} is {version}, expected {LOCKED_VERSION}"));
            }
            if package.get("source").and_then(Value::as_str) != Some(CRATES_IO) {
                findings
                    .problems
                    .push(format!("Cargo.lock: {name} does not come from the crates.io registry"));
            }
            if package.get("checksum").and_then(Value::as_str).is_none() {
                findings.problems.push(format!("Cargo.lock: {name} has no checksum"));
            }
        }
        let dependencies = package
            .get("dependencies")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|dependency| dependency.split(' ').next())
            .filter(|dependency| is_opentelemetry(dependency));
        for dependency in dependencies {
            findings.lock_edges_read += 1;
            if !is_opentelemetry(name) && name != OWNER_PACKAGE {
                findings.problems.push(format!(
                    "Cargo.lock: {name} depends on {dependency}; only ocx_telemetry may"
                ));
            }
        }
    }
}

fn audit(root_manifest: &str, crate_manifests: &[(String, String)], lock: &str) -> Findings {
    let mut findings = Findings::default();
    check_pins(root_manifest, &mut findings);
    check_crate_manifest(root_manifest, "root Cargo.toml", &mut findings);
    for (label, text) in crate_manifests {
        check_crate_manifest(text, label, &mut findings);
    }
    check_lock(lock, &mut findings);
    findings
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn live_inputs() -> (String, Vec<(String, String)>, String) {
    let root = workspace_root();
    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("read crates/") {
        let path = entry.expect("crates/ entry").path().join("Cargo.toml");
        if path.is_file() {
            let label = format!(
                "crates/{}/Cargo.toml",
                path.parent()
                    .and_then(Path::file_name)
                    .and_then(|name| name.to_str())
                    .unwrap_or("?")
            );
            manifests.push((label, read(&path)));
        }
    }
    manifests.sort();
    (
        read(&root.join("Cargo.toml")),
        manifests,
        read(&root.join("Cargo.lock")),
    )
}

#[test]
fn opentelemetry_pins_match_the_exemption() {
    let (root_manifest, manifests, lock) = live_inputs();

    let findings = audit(&root_manifest, &manifests, &lock);

    assert_eq!(findings.problems, Vec::<String>::new());
    // A reader that found nothing is indistinguishable from a clean tree.
    assert!(findings.pins_read >= 4, "read {} pins", findings.pins_read);
    // The four pins plus the protobuf crate the exporter pulls in: a lock reader that lost the
    // transitive one would leave its version unchecked.
    assert!(
        findings.locked_read > PINS.len(),
        "read {} locked packages",
        findings.locked_read
    );
    // The owner's four edges plus the exporter's own: a reader that lost the dependency lists
    // would leave every third-party dependent unchecked.
    assert!(
        findings.lock_edges_read >= 4,
        "read {} OpenTelemetry dependency edges in the lock",
        findings.lock_edges_read
    );
    assert!(
        lock.contains("name = \"opentelemetry-proto\""),
        "the lock no longer holds the transitive protobuf crate"
    );
    assert!(manifests.len() >= 20, "read {} crate manifests", manifests.len());
}

#[test]
fn the_audit_reds_on_each_way_to_leave_the_exemption() {
    let (root_manifest, manifests, lock) = live_inputs();
    let telemetry = manifests
        .iter()
        .find(|(label, _)| label == OWNER_MANIFEST)
        .map(|(_, text)| text.clone())
        .expect("the telemetry manifest");
    let red = |root: &str, manifests: &[(String, String)], lock: &str, needle: &str| {
        let problems = audit(root, manifests, lock).problems;
        assert!(
            problems.iter().any(|problem| problem.contains(needle)),
            "expected a finding naming {needle:?}, got {problems:?}"
        );
    };

    // A mutation that did not land would leave the audit green for the wrong reason.
    let mutate = |text: &str, from: &str, to: &str| {
        assert!(text.contains(from), "mutation target {from:?} is gone");
        text.replacen(from, to, 1)
    };
    let with_telemetry = |text: String| -> Vec<(String, String)> {
        manifests
            .iter()
            .map(|(label, original)| {
                let text = if label == OWNER_MANIFEST {
                    text.clone()
                } else {
                    original.clone()
                };
                (label.clone(), text)
            })
            .collect()
    };
    let with_extra = |label: &str, text: String| -> Vec<(String, String)> {
        let mut extended = manifests.clone();
        extended.push((label.to_owned(), text));
        extended
    };

    red(
        &mutate(&root_manifest, "\"=0.33.1\"", "\"0.33.1\""),
        &manifests,
        &lock,
        "version is not",
    );
    red(
        &mutate(
            &root_manifest,
            "opentelemetry-http = { version = \"=0.33.1\", default-features = false }",
            "opentelemetry-http = { version = \"=0.33.1\" }",
        ),
        &manifests,
        &lock,
        "default-features",
    );
    red(
        &mutate(
            &root_manifest,
            "[\"http-proto\", \"logs\"]",
            "[\"http-proto\", \"logs\", \"grpc-tonic\"]",
        ),
        &manifests,
        &lock,
        "forbidden feature",
    );
    red(
        &root_manifest,
        &with_extra(
            "a second dependent manifest",
            format!("[dependencies]\n{POINTER}\nopentelemetry.workspace = true\n"),
        ),
        &lock,
        "only ocx_telemetry may",
    );
    red(
        &root_manifest,
        &with_telemetry(mutate(
            &telemetry,
            &format!("{POINTER}\nopentelemetry.workspace"),
            "opentelemetry.workspace",
        )),
        &lock,
        "lacks the pointer comment",
    );
    red(
        &root_manifest,
        &manifests,
        &mutate(
            &lock,
            "name = \"opentelemetry\"\nversion = \"0.33.1\"",
            "name = \"opentelemetry\"\nversion = \"0.33.0\"",
        ),
        "opentelemetry is 0.33.0",
    );
    red(
        &root_manifest,
        &manifests,
        &mutate(
            &lock,
            "name = \"opentelemetry-proto\"\nversion = \"0.33.1\"",
            "name = \"opentelemetry-proto\"\nversion = \"0.34.0\"",
        ),
        "opentelemetry-proto is 0.34.0",
    );
    red(
        &mutate(
            &root_manifest,
            "opentelemetry-http = { version = \"=0.33.1\", default-features = false }",
            "opentelemetry-http = { version = \"=0.33.1\", default-features = false, features = [\"metrics\"] }",
        ),
        &manifests,
        &lock,
        "opentelemetry-http: features",
    );
    red(
        &root_manifest,
        &manifests,
        &format!("{lock}\n[[package]]\nname = \"tonic\"\nversion = \"0.14.0\"\n"),
        "tonic",
    );

    // Member entries are exactly `workspace = true`: a feature, a version or a default-features
    // key on one adds to what the root pinned.
    for to in [
        "opentelemetry-otlp = { workspace = true, features = [\"reqwest-client\"] }",
        "opentelemetry-otlp = { workspace = true, default-features = true }",
        "opentelemetry-otlp = { workspace = true, version = \"0.33\" }",
        "opentelemetry-otlp = \"=0.33.1\"",
    ] {
        red(
            &root_manifest,
            &with_telemetry(mutate(&telemetry, "opentelemetry-otlp.workspace = true", to)),
            &lock,
            "must be exactly `workspace = true`",
        );
    }

    // A rename hides an OpenTelemetry crate from a match on the key.
    let renamed = "otlp = { package = \"opentelemetry-otlp\", version = \"=0.33.1\" }";
    red(
        &root_manifest,
        &with_extra("a renaming dependent", format!("[dependencies]\n{renamed}\n")),
        &lock,
        "only ocx_telemetry may",
    );
    red(
        &mutate(
            &root_manifest,
            "opentelemetry-http = { version = \"=0.33.1\", default-features = false }",
            &format!(
                "opentelemetry-http = {{ version = \"=0.33.1\", default-features = false }}\n{}",
                renamed.replace("\"=0.33.1\"", "\"=0.33.1\", default-features = false")
            ),
        ),
        &manifests,
        &lock,
        "renames opentelemetry-otlp",
    );
    red(
        &root_manifest,
        &with_telemetry(mutate(
            &telemetry,
            "opentelemetry-otlp.workspace = true",
            "otlp = { package = \"opentelemetry-otlp\", workspace = true }",
        )),
        &lock,
        "must be exactly `workspace = true`",
    );

    // Target-specific tables are dependencies too.
    red(
        &root_manifest,
        &with_extra(
            "a target dependent",
            "[target.'cfg(unix)'.dependencies]\nopentelemetry = \"0.33\"\n".to_owned(),
        ),
        &lock,
        "only ocx_telemetry may",
    );
    red(
        &root_manifest,
        &with_telemetry(format!(
            "{telemetry}\n[target.'cfg(unix)'.dependencies]\nopentelemetry-otlp = {{ workspace = true, features = [\"reqwest-client\"] }}\n"
        )),
        &lock,
        "must be exactly `workspace = true`",
    );

    // A third party in the lock that depends on an OpenTelemetry crate unifies features invisibly.
    red(
        &root_manifest,
        &manifests,
        &format!(
            "{lock}\n[[package]]\nname = \"tracing-bridge\"\nversion = \"0.1.0\"\ndependencies = [\n \"opentelemetry_sdk 0.33.1\",\n]\n"
        ),
        "tracing-bridge depends on opentelemetry_sdk",
    );

    // A forked copy that keeps the version still leaves the registry or its checksum.
    red(
        &root_manifest,
        &manifests,
        &mutate(
            &lock,
            "name = \"opentelemetry\"\nversion = \"0.33.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"",
            "name = \"opentelemetry\"\nversion = \"0.33.1\"\nsource = \"git+https://example.invalid/fork\"",
        ),
        "opentelemetry does not come from the crates.io registry",
    );
    red(
        &root_manifest,
        &manifests,
        &mutate(
            &lock,
            "name = \"opentelemetry\"\nversion = \"0.33.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum",
            "name = \"opentelemetry\"\nversion = \"0.33.1\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nunchecked",
        ),
        "opentelemetry has no checksum",
    );
    red(
        &mutate(
            &root_manifest,
            "[patch.crates-io]\n",
            "[patch.crates-io]\nopentelemetry = { path = \"external/fork\" }\n",
        ),
        &manifests,
        &lock,
        "never patched or replaced",
    );
    red(
        &format!("{root_manifest}\n[replace]\n\"opentelemetry:0.33.1\" = {{ path = \"external/fork\" }}\n"),
        &manifests,
        &lock,
        "never patched or replaced",
    );

    // The owner is one manifest, matched whole: a sibling whose path merely ends the same is not it.
    // A template, not a literal: the sibling must not exist, and a path literal must name something live.
    red(
        &root_manifest,
        &with_extra(
            &format!("crates/{}/Cargo.toml", "xocx_telemetry"),
            format!("[dependencies]\n{POINTER}\nopentelemetry.workspace = true\n"),
        ),
        &lock,
        "only ocx_telemetry may",
    );
}
