// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Cross-repo decode parity for dispatch objects — the OCI image indices an
//! index tag points at (`adr_oci_index_only_dispatch.md` D1).
//!
//! Every vector under `tests/fixtures/index_wire/dispatch/sha256/` is a real
//! registry document vendored verbatim from `ocx-sh/index` (pinned by
//! `SOURCE_COMMIT`), named by the sha256 of its own bytes. `expected_platforms.json`
//! — authored once, upstream — says which descriptors are platform-selection
//! candidates, so that question has one answer instead of two filters that
//! happen to agree today.
//!
//! Nothing here re-serializes a vector: ocx stores these bytes as served and
//! never renders them (ADR R4). The CAS assertion below is a *fixture* guard —
//! it catches a hand-edited or regenerated vector, not a re-serializing ocx.
//! That ocx itself keeps the served bytes is enforced by
//! `LocalIndex::stage_dispatch_bytes`, which recomputes the digest before
//! writing, and pinned by its own unit tests in
//! `crates/ocx_lib/src/oci/index/local_index.rs` — a `serde_json::to_vec` in
//! `persist_dispatch` turns those red and leaves this suite green.

use std::path::{Path, PathBuf};

use ocx_lib::oci;

fn dispatch_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index_wire/dispatch")
}

fn read_fixture(relative: &str) -> Vec<u8> {
    let path = dispatch_dir().join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn expected_vectors() -> Vec<serde_json::Value> {
    let document: serde_json::Value =
        serde_json::from_slice(&read_fixture("expected_platforms.json")).expect("parse expected_platforms.json");
    document["vectors"]
        .as_array()
        .expect("expected_platforms.json has a `vectors` array")
        .clone()
}

fn parse_image_index(bytes: &[u8], label: &str) -> oci::ImageIndex {
    match serde_json::from_slice::<oci::Manifest>(bytes) {
        Ok(oci::Manifest::ImageIndex(index)) => index,
        Ok(oci::Manifest::Image(_)) => panic!("{label} decoded as a bare image manifest, not an image index"),
        Err(error) => panic!("{label} does not decode as an OCI manifest: {error}"),
    }
}

/// The `(platform, digest)` pairs a correct selection derives from `index`,
/// through the one shared eligibility rule
/// [`oci::Platform::candidate_from_descriptor`] that `Index::fetch_candidates`
/// uses. Platforms are compared as their canonical grammar strings.
fn derive_candidates(index: &oci::ImageIndex) -> Vec<(String, String)> {
    index
        .manifests
        .iter()
        .filter_map(|entry| {
            let platform = oci::Platform::candidate_from_descriptor(entry)?;
            Some((platform.to_string(), entry.digest.clone()))
        })
        .collect()
}

/// The canonical platform string for an `expected_platforms.json` entry. Only
/// `os` and `architecture` are read: every committed vector carries just those
/// two, and an upstream vector that added `variant` would produce a shorter
/// string here than the derived one and fail loudly rather than silently pass.
fn expected_candidates(vector: &serde_json::Value) -> Vec<(String, String)> {
    vector["platforms"]
        .as_array()
        .expect("vector has a `platforms` array")
        .iter()
        .map(|entry| {
            let platform = &entry["platform"];
            let operating_system = platform["os"].as_str().expect("platform has `os`");
            let architecture = platform["architecture"].as_str().expect("platform has `architecture`");
            let digest = entry["digest"].as_str().expect("entry has `digest`");
            (format!("{operating_system}/{architecture}"), digest.to_string())
        })
        .collect()
}

#[test]
fn every_vector_is_named_by_the_sha256_of_its_own_bytes() {
    let vectors = expected_vectors();
    assert!(
        vectors.len() >= 3,
        "expected at least 3 dispatch vectors, found {}",
        vectors.len()
    );

    for vector in &vectors {
        let file = vector["file"].as_str().expect("vector has `file`");
        let bytes = read_fixture(file);
        let actual = oci::Algorithm::Sha256.hash(&bytes).to_string();
        assert_eq!(
            actual,
            vector["digest"].as_str().expect("vector has `digest`"),
            "CAS invariant broken for {file} — the bytes were edited, not re-captured"
        );
        let stem = Path::new(file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("file stem");
        assert_eq!(
            actual,
            format!("sha256:{stem}"),
            "filename does not encode the digest of {file}"
        );
    }
}

#[test]
fn every_vector_decodes_to_the_vendored_platform_candidate_list() {
    for vector in &expected_vectors() {
        let name = vector["name"].as_str().expect("vector has `name`");
        let file = vector["file"].as_str().expect("vector has `file`");
        let index = parse_image_index(&read_fixture(file), name);

        assert_eq!(
            derive_candidates(&index),
            expected_candidates(vector),
            "derived selection candidates for vector `{name}` do not match expected_platforms.json"
        );

        // `ocx index list --platforms` reads the same document through a second
        // enumeration; a `?` that aborts on one unrepresentable descriptor (N-15)
        // truncates this list while leaving the candidate list above intact.
        let listed: Vec<String> = oci::Platform::from_image_index(&index)
            .iter()
            .map(ToString::to_string)
            .collect();
        let expected: Vec<String> = expected_candidates(vector)
            .into_iter()
            .map(|(platform, _)| platform)
            .collect();
        assert_eq!(
            listed, expected,
            "Platform::from_image_index disagrees for vector `{name}`"
        );
    }
}

#[test]
fn attestation_and_platform_less_descriptors_are_never_candidates() {
    // The load-bearing vector: one descriptor declares the attestation
    // placeholder `unknown/unknown`, another omits `platform` entirely. N-2 —
    // `None => Platform::any()` — made the second a *universal* candidate,
    // because an `Any` offer satisfies every requirement.
    let vector = expected_vectors()
        .into_iter()
        .find(|vector| vector["name"] == "attestation_descriptor")
        .expect("the attestation_descriptor vector is vendored");
    let file = vector["file"].as_str().expect("vector has `file`");
    let index = parse_image_index(&read_fixture(file), "attestation_descriptor");

    let unrepresentable: Vec<&oci::ImageIndexEntry> = index
        .manifests
        .iter()
        .filter(|entry| {
            entry.platform.as_ref().is_none_or(|platform| {
                platform.os.to_string() == "unknown" && platform.architecture.to_string() == "unknown"
            })
        })
        .collect();
    assert_eq!(
        unrepresentable.len(),
        2,
        "the vector must still carry both an `unknown/unknown` and a platform-less descriptor"
    );

    let candidates = derive_candidates(&index);
    for entry in unrepresentable {
        assert!(
            !candidates.iter().any(|(_, digest)| digest == &entry.digest),
            "descriptor {} is not selectable but appears as a candidate",
            entry.digest
        );
    }
    // Pair the negative with a positive so it cannot pass by deriving nothing.
    assert_eq!(
        candidates.len(),
        1,
        "exactly the one real platform leaf stays selectable"
    );
}

#[test]
fn a_vector_carries_annotations_and_an_artifact_type() {
    // Corpus shape, not ocx behaviour: keep a vector whose index carries the
    // `annotations` and `artifactType` an ADR R4 publish emits, so the decode
    // assertions above are exercised against that shape. Asserted on the raw
    // bytes so a regenerated fixture that dropped them fails loudly instead of
    // quietly narrowing the corpus.
    let file = expected_vectors()
        .into_iter()
        .find(|vector| vector["name"] == "annotations_artifacttype")
        .and_then(|vector| vector["file"].as_str().map(str::to_string))
        .expect("the annotations_artifacttype vector is vendored");
    let raw = String::from_utf8(read_fixture(&file)).expect("vector is UTF-8");
    assert!(raw.contains("\"artifactType\""), "{file} lost its artifactType");
    assert!(raw.contains("\"annotations\""), "{file} lost its annotations");
}
