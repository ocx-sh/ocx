// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Conformance against bytes `index.ocx.sh` actually served.
//!
//! Every other test of the index wire format feeds the client a body some part
//! of this repository wrote — a `static_index.py` fixture, a stub transport, a
//! literal in a unit test. That is exactly how the catalog envelope drifted: the
//! site had always served `{"format_version": 1, "packages": {…}}`, the client
//! had always parsed a bare `{"<ns>/<pkg>": "sha256:…"}` map, and both suites
//! stayed green for weeks because each was arguing with its own fixture.
//!
//! The vectors under `tests/fixtures/live_index_ocx_sh/` are captured production
//! responses (see that directory's `PROVENANCE.md`). They are the one input here
//! nobody in this repository authored, so they are the one input that can prove
//! the client wrong.
//!
//! Scope: assert the *contract* — the envelope, the version pin, digest-valued
//! entries — never which packages happen to be published. `c/index.json` changes
//! on every announce; pinning its contents would trade a contract test for a
//! flaky one.

use std::path::{Path, PathBuf};

use ocx_lib::oci::index::{CatalogDocument, SUPPORTED_FORMAT_VERSION};

fn fixture(name: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/live_index_ocx_sh")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read captured fixture {}: {error}", path.display()))
}

#[test]
fn live_catalog_bytes_parse_as_the_versioned_envelope() {
    let bytes = fixture("c-index.json");

    let document: CatalogDocument = serde_json::from_slice(&bytes)
        .expect("the bytes index.ocx.sh serves at /c/index.json must parse as a CatalogDocument");
    assert_eq!(
        document.format_version, SUPPORTED_FORMAT_VERSION,
        "the live site serves the version this client supports"
    );

    let catalog = document
        .into_packages()
        .expect("the live catalog's format_version must pass the gate");
    assert!(
        !catalog.is_empty(),
        "the captured catalog listed published packages; an empty parse means the envelope was read as \
         an unrecognised shape rather than failing loudly"
    );
    for (package, entry) in &catalog {
        assert!(
            package.contains('/'),
            "every catalog key is a bare `<ns>/<pkg>` id, got {package:?}"
        );
        assert!(
            entry.starts_with("sha256:"),
            "every catalog value is the root document's digest, got {entry:?} for {package:?}"
        );
    }
}

#[test]
fn a_bare_package_map_is_not_a_catalog_document() {
    // The shape this client used to expect. Guarding it explicitly means a
    // future "tolerate both" softening cannot land quietly — reintroducing the
    // bare map as an accepted input has to break this test first.
    let bare = br#"{"bazelbuild/bazelisk": "sha256:1f6551a444edb92c688e92879d880de05d9658e872703d1b8957a04155dc2548"}"#;
    serde_json::from_slice::<CatalogDocument>(bare)
        .expect_err("the pre-fix bare map must not parse as a catalog document");
}

#[test]
fn live_config_bytes_pin_the_supported_version_and_tolerate_unknown_siblings() {
    // The live config.json carries a `name_segments` sibling the client does not
    // model. Forward-compat (no `deny_unknown_fields`) is what keeps an older
    // binary reading a newer deploy, so it is a contract, not an accident.
    let bytes = fixture("config.json");
    let text = String::from_utf8(bytes.clone()).expect("config.json is UTF-8");
    assert!(
        text.contains("name_segments"),
        "the captured config carried the unknown sibling this test exists to exercise; re-capture drifted"
    );

    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("config.json parses");
    assert_eq!(
        value.get("format_version").and_then(serde_json::Value::as_u64),
        Some(SUPPORTED_FORMAT_VERSION),
        "the live site pins the version this client supports"
    );
}
