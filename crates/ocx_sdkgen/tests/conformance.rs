// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The generated Rust SDK (`tests/golden/rust/`) decodes every case of the language-neutral conformance corpus
//! (`crates/ocx_schema/contract/conformance/`) to the result its `expect.json` names. The case format is that
//! directory's `README.md`.

#![expect(
    dead_code,
    unused_imports,
    reason = "the generated SDK is compiled whole; the tests use a part of it"
)]

#[expect(
    unreachable_pub,
    reason = "the generated SDK is a published crate's public API, compiled here as a private module"
)]
#[expect(
    clippy::disallowed_types,
    reason = "the generated SDK spawns the caller's `ocx`, never runs inside ocx, and resolves no package"
)]
#[path = "golden/rust/lib.rs"]
mod sdk;

use std::path::{Path, PathBuf};

use serde_json::Value;

use sdk::contract;
use sdk::types::ExitCode;
use sdk::{Error, Outcome, Raw};

/// Fewer cases than this means the walk read a different directory than the corpus.
const CASE_FLOOR: usize = 30;

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_schema/contract/conformance")
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&read(path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn pointers(expect: &Value, key: &str) -> Vec<String> {
    expect
        .get(key)
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|pointer| pointer.as_str().expect("a pointer").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Decodes one case the way a caller would, and returns what disagrees with its `expect.json`.
fn check(case: &Path) -> Vec<String> {
    let instance_bytes = read(&case.join("instance.json"));
    let instance: Value = serde_json::from_slice(&instance_bytes).expect("instance.json is JSON");
    let expect = json(&case.join("expect.json"));
    let root: &'static str = Box::leak(expect["root"].as_str().expect("root").to_owned().into_boxed_str());
    let exit_code = ExitCode::from_value(expect["exit_code"].as_i64().expect("exit_code"));
    let run = Raw {
        exit_code: exit_code.clone(),
        stdout: instance_bytes,
        stderr: Vec::new(),
    };
    let mut problems = Vec::new();

    let decoded = contract::decode_root(root, &instance);
    match expect["decode"].as_str().expect("decode") {
        "refused" => {
            if expect["error"] != "contract_mismatch" {
                problems.push(format!("unknown refusal {}", expect["error"]));
            }
            if !matches!(decoded, Some(Err(Error::ContractMismatch { .. }))) {
                problems.push("decode_root did not refuse with a contract mismatch".to_owned());
            }
            if !matches!(run.into_outcome::<Value>(root), Err(Error::ContractMismatch { .. })) {
                problems.push("the outcome path did not refuse with a contract mismatch".to_owned());
            }
            return problems;
        }
        "ok" => {}
        other => panic!("unknown decode {other}"),
    }

    let decoded = match decoded {
        Some(Ok(decoded)) => decoded,
        other => {
            problems.push(format!(
                "decode_root did not decode the root: {:?}",
                other.map(|result| result.err())
            ));
            return problems;
        }
    };

    let unknown = pointers(&expect, "unknown");
    if decoded.unknowns() != unknown {
        problems.push(format!(
            "unknown pointers {:?}, expected {unknown:?}",
            decoded.unknowns()
        ));
    }
    let again = decoded.to_value().expect("the model serializes");
    if let Some(fields) = expect.get("fields").and_then(Value::as_object) {
        for (pointer, value) in fields {
            if again.pointer(pointer) != Some(value) {
                problems.push(format!("{pointer} is {:?}, expected {value}", again.pointer(pointer)));
            }
        }
    }
    for pointer in pointers(&expect, "absent") {
        if again.pointer(&pointer).is_some() {
            problems.push(format!("{pointer} is set; an unset optional must stay absent"));
        }
    }

    let outcome = run.into_outcome::<Value>(root);
    match (expect["outcome"].as_str().expect("outcome"), outcome) {
        ("success", Ok(Outcome::Success(_))) => {}
        ("failed_with_report", Ok(Outcome::Failed { exit_code: failed, .. })) if failed == exit_code => {}
        ("error_document", Err(Error::Ocx(document))) if document.exit_code == exit_code => {}
        (expected, got) => problems.push(format!("outcome {expected} expected, got {got:?}")),
    }
    problems
}

#[test]
fn the_sdk_decodes_every_conformance_case_to_its_expectation() {
    let mut cases: Vec<_> = std::fs::read_dir(corpus())
        .expect("the corpus directory exists")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.join("expect.json").is_file())
        .collect();
    cases.sort();
    assert!(
        cases.len() >= CASE_FLOOR,
        "read {} cases, expected at least {CASE_FLOOR}",
        cases.len()
    );

    let failures: Vec<_> = cases
        .iter()
        .filter_map(|case| {
            let problems = check(case);
            (!problems.is_empty()).then(|| {
                format!(
                    "{}: {}",
                    case.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    problems.join("; ")
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} of {} cases disagree:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
