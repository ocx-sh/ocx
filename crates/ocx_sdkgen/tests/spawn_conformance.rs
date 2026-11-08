// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The generated Rust SDK's spawner builds the child environment every case of
//! `crates/ocx_schema/contract/conformance/spawn/cases.json` names. The spec is the SDK contract's spawn
//! amendment in `adr_ocx_interface_contract.md`; the file has no `expect.json`, so the report corpus walk skips it.

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

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Fewer cases than this means the file read is not the spawn corpus.
const CASE_FLOOR: usize = 8;

fn cases_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_schema/contract/conformance/spawn/cases.json")
}

fn pairs(value: &Value) -> Vec<(OsString, OsString)> {
    value
        .as_array()
        .expect("a list of pairs")
        .iter()
        .map(|pair| {
            let pair = pair.as_array().expect("a [key, value] pair");
            assert_eq!(pair.len(), 2, "a pair has a key and a value");
            (
                OsString::from(pair[0].as_str().expect("a key")),
                OsString::from(pair[1].as_str().expect("a value")),
            )
        })
        .collect()
}

#[test]
fn spawner_builds_the_child_environment_of_every_case() {
    let path = cases_file();
    let bytes = std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let document: Value = serde_json::from_slice(&bytes).expect("cases.json is JSON");
    let cases = document["cases"].as_array().expect("a cases list");
    assert!(cases.len() >= CASE_FLOOR, "only {} spawn cases read", cases.len());

    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().expect("a case name");
        let binary = PathBuf::from(case["binary"].as_str().expect("a binary"));
        let mut actual = sdk::spawn::child_environment(pairs(&case["parent"]), &pairs(&case["caller"]), &binary);
        let mut expected = pairs(&case["child"]);
        actual.sort();
        expected.sort();
        if actual != expected {
            failures.push(format!("{name}: expected {expected:?}, got {actual:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
