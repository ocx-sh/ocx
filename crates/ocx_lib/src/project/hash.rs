// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Declaration hash: RFC 8785 JCS canonicalization + SHA-256.

use sha2::{Digest, Sha256};

use super::ProjectConfig;

/// Canonicalization contract version baked into every lock file's
/// `metadata.declaration_hash_version`. Incrementing this is a breaking
/// change to the hash input format and must be paired with a test
/// update covering the new canonicalization.
///
/// Deliberately typed as `u8`, not a `serde_repr` enum: the field ships
/// as a plain integer in the lock file's `[metadata]` block, and the
/// version gate is enforced by an explicit comparison in
/// `ProjectLock::from_str_with_path`. A `serde_repr` enum would force
/// a variant-per-release churn in this crate for a check that's one
/// `if` statement today.
pub const DECLARATION_HASH_VERSION: u8 = 1;

/// Prefix for SHA-256 digest strings emitted by [`declaration_hash`].
const SHA256_PREFIX: &str = "sha256:";

/// Compute the declaration hash for a [`ProjectConfig`].
///
/// Algorithm (v1):
/// 1. Build a canonical JSON value of the form
///    `{ "default": [[name, identifier], ...],
///       "group.<name>": [[name, identifier], ...] }`
///    where every inner pair list is sorted lexicographically by binding
///    name, and `identifier` is the `Display` form of the parsed
///    [`crate::oci::Identifier`] (`registry/repo:tag[@digest]`).
/// 2. Serialize via RFC 8785 JCS (`serde_json_canonicalizer`).
/// 3. SHA-256 the UTF-8 bytes.
/// 4. Return `"sha256:<hex>"`.
///
/// The platform set is **not** part of the hash input. Effective
/// platforms are sourced ambient from the project tier and may evolve
/// independently of `ocx.toml`'s declared content.
///
/// Infallible: the JSON value built here only contains `String`, `Array`,
/// and `Object` nodes — no floats, no non-UTF-8 bytes are possible from
/// Rust `String` inputs. RFC 8785 JCS cannot fail on this subtree, and
/// SHA-256 + hex encoding never fail. If JCS ever gains a failure mode
/// reachable from string/array/object input (it cannot today), the
/// `.expect` below will panic loudly rather than silently returning a
/// bogus hash.
pub fn declaration_hash(config: &ProjectConfig) -> String {
    // 1. Build a canonical JSON object. Field order at insertion time is
    //    irrelevant — RFC 8785 JCS re-sorts object keys lexicographically
    //    during canonicalization.
    let mut map = serde_json::Map::new();

    // "default" — the reserved top-level group (maps to `config.tools`).
    let mut default_pairs: Vec<(&String, String)> = config.tools.iter().map(|(n, id)| (n, id.to_string())).collect();
    default_pairs.sort();
    let default_json: Vec<serde_json::Value> = default_pairs
        .into_iter()
        .map(|(n, t)| serde_json::json!([n, t]))
        .collect();
    map.insert(
        super::internal::DEFAULT_GROUP.to_string(),
        serde_json::Value::Array(default_json),
    );

    // "group.<name>" — `config.groups` is a BTreeMap, so iteration is
    // already sorted by group name.
    for (group_name, group_tools) in &config.groups {
        let mut pairs: Vec<(&String, String)> = group_tools.iter().map(|(n, id)| (n, id.to_string())).collect();
        pairs.sort();
        let json: Vec<serde_json::Value> = pairs.into_iter().map(|(n, t)| serde_json::json!([n, t])).collect();
        map.insert(format!("group.{group_name}"), serde_json::Value::Array(json));
    }

    let value = serde_json::Value::Object(map);

    // 2. Canonicalize via RFC 8785 JCS.
    let canonical = serde_json_canonicalizer::to_string(&value)
        .expect("JCS cannot fail on string/array/object JSON — no floats or invalid UTF-8 possible");

    // 3. SHA-256 + hex-encode.
    let digest = Sha256::digest(canonical.as_bytes());
    format!("{SHA256_PREFIX}{}", hex::encode(digest))
}

#[cfg(test)]
mod tests {
    //! FROZEN CORPUS — these hashes are the permanent contract for
    //! `DECLARATION_HASH_VERSION = 1`. Changing any value here is a
    //! breaking change to the lock format and requires bumping the
    //! version constant plus a migration path. Do not "fix" a failing
    //! hash by updating the expected value; fix the algorithm or
    //! document why the algorithm changed.

    use std::collections::BTreeMap;

    use super::*;
    use crate::oci::Identifier;

    // --- Helpers ------------------------------------------------------------

    fn empty_config() -> ProjectConfig {
        ProjectConfig {
            tools: BTreeMap::new(),
            groups: BTreeMap::new(),
        }
    }

    /// Build a tool map from `(binding, identifier)` pairs. Every value
    /// is run through strict [`Identifier::parse`] — bare-tag forms are
    /// rejected, mirroring the production `ocx.toml` parser.
    fn tools(pairs: &[(&str, &str)]) -> BTreeMap<String, Identifier> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), Identifier::parse(v).expect("valid identifier")))
            .collect()
    }

    // --- Non-corpus tests ---------------------------------------------------

    #[test]
    fn hash_format_sha256_prefix() {
        let config = empty_config();
        let got = declaration_hash(&config);
        assert!(got.starts_with("sha256:"), "unexpected prefix: {got}");
        let hex_part = got.strip_prefix("sha256:").expect("prefix");
        assert_eq!(hex_part.len(), 64, "sha256 hex must be 64 chars: {got}");
        assert!(
            hex_part
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "sha256 hex must be lowercase [0-9a-f]: {got}"
        );
    }

    #[test]
    fn hash_deterministic_same_input() {
        let config = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28"), ("ninja", "ocx.sh/ninja:1.11")]),
            groups: BTreeMap::new(),
        };
        let a = declaration_hash(&config);
        let b = declaration_hash(&config);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_changes_on_tool_added() {
        let base = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28")]),
            groups: BTreeMap::new(),
        };
        let added = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28"), ("ninja", "ocx.sh/ninja:1.11")]),
            groups: BTreeMap::new(),
        };
        let h1 = declaration_hash(&base);
        let h2 = declaration_hash(&added);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_changes_on_group_added() {
        let base = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28")]),
            groups: BTreeMap::new(),
        };
        let mut groups = BTreeMap::new();
        groups.insert("ci".to_string(), tools(&[("shellcheck", "ocx.sh/shellcheck:0.10")]));
        let added = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28")]),
            groups,
        };
        let h1 = declaration_hash(&base);
        let h2 = declaration_hash(&added);
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_stable_with_unicode_tags() {
        // JCS normalizes strings by UTF-8 byte order; unicode must hash
        // deterministically across two invocations.
        let config = ProjectConfig {
            tools: tools(&[("tool", "ocx.sh/tool:v1.0-\u{03b1}")]), // α
            groups: BTreeMap::new(),
        };
        let h1 = declaration_hash(&config);
        let h2 = declaration_hash(&config);
        assert_eq!(h1, h2);
    }

    // --- Frozen corpus ------------------------------------------------------
    //
    // Each case documents the canonical JSON shape that goes into JCS in
    // a comment, and asserts the production function matches a frozen
    // literal hash pinned below.
    //
    // These four constants are the permanent contract for
    // `DECLARATION_HASH_VERSION = 1`. They were captured from a run of
    // `declaration_hash` and baked in by hand. Changing any value is a
    // BREAKING change to the lock format that MUST bump
    // [`DECLARATION_HASH_VERSION`].
    const HASH_CASE_1: &str = "sha256:3110f8345dc4c5212b94d6a8286773b2e86ab817db53cf02ce7eede77c2c36ab";
    const HASH_CASE_2: &str = "sha256:d825a96ebbd3fdad80884e36a68e9cf72571f341d8a8c5fcff3ddee9c685f3bd";
    const HASH_CASE_3: &str = "sha256:9a8e3121ecbce48b94e1b0c4d5c0f5c9aa4c7e703170c601afc85a50072c57a4";
    const HASH_CASE_4: &str = "sha256:11bc3307cb28c26f9308ee80b360fba44646e16c78240af1f386e515dc2b5688";

    #[test]
    fn hash_corpus_case_1_empty_config() {
        // Canonical JSON shape:
        //   {"default":[]}
        let config = empty_config();
        let got = declaration_hash(&config);
        assert_eq!(got, HASH_CASE_1, "frozen literal mismatch — algorithm drift?");
    }

    #[test]
    fn hash_corpus_case_2_single_tool() {
        // Canonical JSON shape:
        //   {"default":[["cmake","ocx.sh/cmake:3.28"]]}
        let config = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28")]),
            groups: BTreeMap::new(),
        };
        let got = declaration_hash(&config);
        assert_eq!(got, HASH_CASE_2, "frozen literal mismatch — algorithm drift?");
    }

    #[test]
    fn hash_corpus_case_3_multi_tools_and_groups() {
        // Canonical JSON shape:
        //   {"default":[["cmake","ocx.sh/cmake:3.28"],["ninja","ocx.sh/ninja:1.11"]],
        //    "group.ci":[["shellcheck","ocx.sh/shellcheck:0.10"],["shfmt","ocx.sh/shfmt:3.7"]],
        //    "group.release":[["goreleaser","ocx.sh/goreleaser:2.0"],["sbom","ocx.sh/sbom:0.1"]]}
        let mut groups = BTreeMap::new();
        groups.insert(
            "ci".to_string(),
            tools(&[("shellcheck", "ocx.sh/shellcheck:0.10"), ("shfmt", "ocx.sh/shfmt:3.7")]),
        );
        groups.insert(
            "release".to_string(),
            tools(&[("goreleaser", "ocx.sh/goreleaser:2.0"), ("sbom", "ocx.sh/sbom:0.1")]),
        );
        let config = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28"), ("ninja", "ocx.sh/ninja:1.11")]),
            groups,
        };
        let got = declaration_hash(&config);
        assert_eq!(got, HASH_CASE_3, "frozen literal mismatch — algorithm drift?");
    }

    #[test]
    fn hash_corpus_case_4_digest_suffixed_version() {
        // Tag string is opaque to the hash — the `@sha256:...` suffix
        // passes through verbatim in the canonical JSON via Identifier::Display.
        // Canonical JSON shape:
        //   {"default":[["cmake","ocx.sh/cmake:3.28@sha256:deadbeef..."]]}
        let config = ProjectConfig {
            tools: tools(&[(
                "cmake",
                "ocx.sh/cmake:3.28@sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            )]),
            groups: BTreeMap::new(),
        };
        let got = declaration_hash(&config);
        assert_eq!(got, HASH_CASE_4, "frozen literal mismatch — algorithm drift?");

        // Also: changing the digest suffix must change the hash (proving
        // the digest is part of the canonical input, not stripped).
        let config2 = ProjectConfig {
            tools: tools(&[(
                "cmake",
                "ocx.sh/cmake:3.28@sha256:cafef00dcafef00dcafef00dcafef00dcafef00dcafef00dcafef00dcafef00d",
            )]),
            groups: BTreeMap::new(),
        };
        assert_ne!(got, declaration_hash(&config2));
    }

    #[test]
    fn declaration_hash_version_is_one() {
        // Locked as a permanent contract: bumping this constant is a
        // migration event, not a drive-by edit.
        assert_eq!(DECLARATION_HASH_VERSION, 1);
    }

    // --- Insertion-order + clone determinism ------------------------------

    #[test]
    fn hash_independent_of_tool_insertion_order() {
        // BTreeMap keys sort on insertion, but this test documents the
        // contract: the hash must not depend on the order callers hand
        // tools to the builder. Insert in reverse-alphabetical order and
        // assert the hash matches a forward-order build.
        let forward = tools(&[
            ("cmake", "ocx.sh/cmake:3.28"),
            ("ninja", "ocx.sh/ninja:1.11"),
            ("zlib", "ocx.sh/zlib:1.3"),
        ]);

        let mut reversed: BTreeMap<String, Identifier> = BTreeMap::new();
        reversed.insert("zlib".to_string(), Identifier::parse("ocx.sh/zlib:1.3").expect("valid"));
        reversed.insert(
            "ninja".to_string(),
            Identifier::parse("ocx.sh/ninja:1.11").expect("valid"),
        );
        reversed.insert(
            "cmake".to_string(),
            Identifier::parse("ocx.sh/cmake:3.28").expect("valid"),
        );

        let mut groups_forward: BTreeMap<String, BTreeMap<String, Identifier>> = BTreeMap::new();
        groups_forward.insert("alpha".to_string(), tools(&[("a", "ocx.sh/a:1"), ("b", "ocx.sh/b:2")]));
        groups_forward.insert("beta".to_string(), tools(&[("c", "ocx.sh/c:3"), ("d", "ocx.sh/d:4")]));

        let mut groups_reversed: BTreeMap<String, BTreeMap<String, Identifier>> = BTreeMap::new();
        groups_reversed.insert("beta".to_string(), tools(&[("d", "ocx.sh/d:4"), ("c", "ocx.sh/c:3")]));
        groups_reversed.insert("alpha".to_string(), tools(&[("b", "ocx.sh/b:2"), ("a", "ocx.sh/a:1")]));

        let config_forward = ProjectConfig {
            tools: forward,
            groups: groups_forward,
        };
        let config_reversed = ProjectConfig {
            tools: reversed,
            groups: groups_reversed,
        };

        let h_forward = declaration_hash(&config_forward);
        let h_reversed = declaration_hash(&config_reversed);
        assert_eq!(
            h_forward, h_reversed,
            "hash must not depend on tool/group insertion order"
        );
    }

    #[test]
    fn hash_deterministic_across_cloned_config() {
        // Clone, mutate harmlessly (add+remove a tool), re-hash. The
        // clone and the original must produce the same hash — proves
        // internal state (BTreeMap buckets, etc.) does not leak into
        // the canonical JSON input.
        let config = ProjectConfig {
            tools: tools(&[("cmake", "ocx.sh/cmake:3.28"), ("ninja", "ocx.sh/ninja:1.11")]),
            groups: BTreeMap::new(),
        };

        let mut clone = config.clone();
        clone.tools.insert(
            "transient".to_string(),
            Identifier::parse("ocx.sh/transient:0.0").expect("valid"),
        );
        clone.tools.remove("transient");

        let h_original = declaration_hash(&config);
        let h_clone = declaration_hash(&clone);
        assert_eq!(h_original, h_clone);
    }

    // --- Unicode + special-character tag tests ----------------------------

    #[test]
    fn hash_differs_for_nfc_vs_nfd_tag() {
        // JCS serializes strings byte-for-byte as UTF-8; NFC vs NFD are
        // distinct byte sequences, so the hash MUST differ. If a future
        // change normalizes tags before canonicalization, this test is
        // the change marker.

        // NFC: "é" as single code point U+00E9.
        let nfc = ProjectConfig {
            tools: tools(&[("tool", "ocx.sh/tool:v1.0-\u{00e9}")]),
            groups: BTreeMap::new(),
        };
        // NFD: "e" + combining acute U+0301.
        let nfd = ProjectConfig {
            tools: tools(&[("tool", "ocx.sh/tool:v1.0-e\u{0301}")]),
            groups: BTreeMap::new(),
        };

        let h_nfc = declaration_hash(&nfc);
        let h_nfd = declaration_hash(&nfd);
        assert_ne!(
            h_nfc, h_nfd,
            "NFC and NFD byte sequences must hash differently; \
             got identical hash — has tag normalization been introduced?"
        );
    }

    #[test]
    fn hash_accepts_special_char_tags() {
        // Special-character tags must hash deterministically without
        // panic or canonicalization error. The set below covers the
        // characters most likely to round-trip through JSON badly that
        // also survive strict `Identifier::parse`:
        // - `+` is normalized to `_` by `Identifier::parse` (OCI spec
        //   forbids `+` in tags), so the *pre-normalization* form
        //   `1.0+build.1` becomes `1.0_build.1` in `Display` and the
        //   hash is computed against the normalized form.
        // - `@sha256:<hex>` digest suffix passes through verbatim.
        //
        // Whitespace (`v2 rc1`) and `=` (`k=v`) tag forms used to be
        // covered here but were dropped: although today's permissive
        // tag-splitter accepts them, they violate the OCI tag charset
        // (`[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`) and may be rejected by
        // a future `Identifier::parse` tightening.
        let special = &[
            ("build_meta", "ocx.sh/build_meta:1.0+build.1"),
            (
                "atref",
                "ocx.sh/atref:v1.0@sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            ),
        ];
        let config = ProjectConfig {
            tools: tools(special),
            groups: BTreeMap::new(),
        };
        let a = declaration_hash(&config);
        let b = declaration_hash(&config);
        assert_eq!(a, b, "special-char tags must hash deterministically");
    }
}
