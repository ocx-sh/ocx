// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The claim root renderer (C-047).
//!
//! Nine fields, in this order: `name`, `repository`, `owners`, `status`,
//! `deprecated_message`, `created`, `desc`, `upstream` (omitted entirely when
//! absent, never `null`), `tags`.
//!
//! Three traps, each of which ships a wrong root if missed:
//!
//! - An owner object is **`login` and `id` only**. The four-key form the
//!   vendored golden roots carry is indexbot's output; a builder copying them
//!   validates against the live schema and then diverges from every byte
//!   assertion here.
//! - `upstream`'s **inner** optionals omit rather than emit `null`, while
//!   `deprecated_message` and `desc` **are** `null`. A `skip_serializing_if`
//!   applied uniformly to every `Option` drops the wrong two.
//! - `serialize_root` takes an order-preserving [`serde_json::Value`] and
//!   `preserve_order` is on crate-wide, so insertion order *is* emission order —
//!   but **`IndexMap::eq` is order-independent**, so comparing two parsed
//!   `Value`s passes with the fields in any order. Byte-level assertion is the
//!   only order check there is; a later "simplification" to
//!   `assert_eq!(Value, Value)` is a regression, not a cleanup.

use serde_json::{Map, Value};

use super::error::ClaimError;
use super::owners::ResolvedOwner;
use super::request::Upstream;
use crate::oci;

/// The root's `name`: `<registry>/<namespace>/<package>`, with no tag and no
/// digest (C-047).
///
/// The registry comes from the identifier the caller was handed — the
/// `OCX_DEFAULT_REGISTRY` resolution lives at the CLI boundary, so a library-side
/// read of that variable would measure nothing. `Identifier`'s own `Display`
/// appends `:tag` and `@digest`, which is why this is not `format!("{package}")`.
#[must_use]
pub fn root_name(package: &oci::Identifier) -> String {
    format!("{}/{}", package.registry(), package.repository())
}

/// Validate `--repository` as an `oci://host/path` pointer, returning it
/// verbatim (C-047).
///
/// The parse is `oci::index::parse_physical_repository`'s, which demands an exact
/// `Identifier` round-trip — so every accepted value reconstructs byte-identically
/// and the "verbatim" half has no reachable red. The refusal is the half that
/// does, and it is [`ClaimError::MalformedRepository`] at **exit 64**, never the
/// index error's own 65.
///
/// # Errors
///
/// [`ClaimError::MalformedRepository`] for a missing or unknown scheme, a missing
/// slash, an empty host or path, or a smuggled tag, digest, uppercase segment or
/// stray colon.
pub fn parse_repository(value: &str) -> Result<String, ClaimError> {
    // The index error is deliberately dropped rather than carried as a
    // `#[source]`: it classifies to `DataError` (65), and a malformed *flag
    // value* is operator input, which is `EX_USAGE` (64). The refused value is
    // named back instead, which is what an operator acts on.
    oci::index::parse_physical_repository(value)
        .map(|_| value.to_string())
        .map_err(|_| ClaimError::MalformedRepository {
            value: value.to_string(),
        })
}

/// Render the claim root's bytes (C-047).
///
/// `created` is read from `oci::index::current_date()` — a **date**, not a
/// timestamp — so both renderings derive from one instant and one seam.
///
/// The bytes go through `oci::index::serialize_root`; there is no second
/// serializer. That is observable, and it is what carries `ensure_ascii`: a
/// non-ASCII scalar anywhere in the root emits as `\uXXXX`, and the document ends
/// in exactly one `\n`.
#[must_use]
pub fn render_root(name: &str, repository: &str, owners: &[ResolvedOwner], upstream: Option<&Upstream>) -> Vec<u8> {
    // `serde_json`'s `preserve_order` feature is on crate-wide, so `Map` is an
    // `IndexMap` and insertion order below *is* emission order. Fields are
    // inserted in C-047's order; nothing here may be reordered for tidiness.
    let mut root = Map::new();
    root.insert("name".to_string(), Value::from(name));
    root.insert("repository".to_string(), Value::from(repository));
    root.insert(
        "owners".to_string(),
        Value::Array(
            owners
                .iter()
                .map(|owner| {
                    // `login` and `id` only — the four-key form in the vendored
                    // golden roots is indexbot's output, not ocx's.
                    let mut object = Map::new();
                    object.insert("login".to_string(), Value::from(owner.login.as_str()));
                    object.insert("id".to_string(), Value::from(owner.id));
                    Value::Object(object)
                })
                .collect(),
        ),
    );
    root.insert("status".to_string(), Value::from("active"));
    root.insert("deprecated_message".to_string(), Value::Null);
    root.insert("created".to_string(), Value::from(oci::index::current_date()));
    root.insert("desc".to_string(), Value::Null);
    if let Some(upstream) = upstream {
        // The OUTER object is omitted when absent; the INNER optionals are
        // omitted when absent too — the live root schema sets
        // `additionalProperties: false` and takes no placeholder null, while
        // `deprecated_message` and `desc` above deliberately do emit one.
        let mut object = Map::new();
        object.insert("org".to_string(), Value::from(upstream.org.as_str()));
        if let Some(url) = &upstream.repository_url {
            object.insert("repository_url".to_string(), Value::from(url.as_str()));
        }
        if let Some(disclaimer) = &upstream.disclaimer {
            object.insert("disclaimer".to_string(), Value::from(disclaimer.as_str()));
        }
        root.insert("upstream".to_string(), Value::Object(object));
    }
    root.insert("tags".to_string(), Value::Object(Map::new()));
    oci::index::serialize_root(&Value::Object(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    /// A pinned instant in the **past**, so a production read of the wrong
    /// variable spelling leaves the pin inert and both renderings fall back to
    /// today — which reds every assertion below. A pin set to "today" would pass
    /// on the day it was written and red at the next midnight or on a CI box in
    /// another timezone.
    const PINNED_INSTANT: &str = "2026-01-02T03:04:05Z";
    const PINNED_DATE: &str = "2026-01-02";

    /// Pins `__OCX_TESTING_ANNOUNCE_CLOCK` for the test's lifetime.
    ///
    /// **`crate::oci::index::current_timestamp` reads `std::env::var` directly,
    /// not `crate::env::var`** — so `EnvLock::set` is a silent no-op here and the
    /// pin must go through `std::env::set_var` under the same lock. The
    /// equivalent guard in `oci/index.rs` is private to that module's test
    /// module, and `oci/index.rs` is not in WP-9's file set, so it is duplicated
    /// rather than shared.
    struct ClockSeam {
        _lock: crate::test::env::EnvLock,
    }

    impl ClockSeam {
        fn pinned(instant: &str) -> Self {
            let lock = crate::test::env::lock();
            // SAFETY: `EnvLock` serialises every env-touching test in this
            // process against this write, and `Drop` clears it unconditionally
            // so a stub that panics mid-test cannot leak the pin into a sibling
            // — which is exactly what the Specify phase does.
            unsafe { std::env::set_var("__OCX_TESTING_ANNOUNCE_CLOCK", instant) };
            Self { _lock: lock }
        }
    }

    impl Drop for ClockSeam {
        fn drop(&mut self) {
            // SAFETY: see `ClockSeam::pinned`. A struct's own `Drop` runs before
            // its fields', so the pin is gone before the lock releases.
            unsafe { std::env::remove_var("__OCX_TESTING_ANNOUNCE_CLOCK") };
        }
    }

    fn alice() -> ResolvedOwner {
        ResolvedOwner {
            login: "alice".to_string(),
            id: 1234,
        }
    }

    fn bob() -> ResolvedOwner {
        ResolvedOwner {
            login: "bob".to_string(),
            id: 7,
        }
    }

    const NAME: &str = "ocx.sh/acme/widget";
    const REPOSITORY: &str = "oci://ghcr.io/acme/widget";

    /// The claim root, upstream present with every optional given.
    const FULL_ROOT: &str = r#"{
  "name": "ocx.sh/acme/widget",
  "repository": "oci://ghcr.io/acme/widget",
  "owners": [
    {
      "login": "alice",
      "id": 1234
    },
    {
      "login": "bob",
      "id": 7
    }
  ],
  "status": "active",
  "deprecated_message": null,
  "created": "2026-01-02",
  "desc": null,
  "upstream": {
    "org": "Acme Org",
    "repository_url": "https://github.com/acme/widget",
    "disclaimer": "Community-maintained mirror"
  },
  "tags": {}
}
"#;

    /// The claim root, first-party: no `upstream` key at all.
    const MINIMAL_ROOT: &str = r#"{
  "name": "ocx.sh/acme/widget",
  "repository": "oci://ghcr.io/acme/widget",
  "owners": [
    {
      "login": "alice",
      "id": 1234
    }
  ],
  "status": "active",
  "deprecated_message": null,
  "created": "2026-01-02",
  "desc": null,
  "tags": {}
}
"#;

    fn full_upstream() -> Upstream {
        Upstream {
            org: "Acme Org".to_string(),
            repository_url: Some("https://github.com/acme/widget".to_string()),
            disclaimer: Some("Community-maintained mirror".to_string()),
        }
    }

    fn rendered(owners: &[ResolvedOwner], upstream: Option<&Upstream>) -> String {
        String::from_utf8(render_root(NAME, REPOSITORY, owners, upstream)).expect("the root is valid UTF-8")
    }

    /// C-047 — the whole nine-field root, **byte for byte** (renamed from
    /// `root_field_order_matches_fixture`, DX-15 shape).
    ///
    /// The expectation is an **inline literal**, not a new file under
    /// `crates/ocx_lib/tests/fixtures/index_wire/root/`: that directory is
    /// globbed whole by `root_fixtures_round_trip_byte_exact`, its README pins
    /// its files as vendored-verbatim upstream vectors (they carry a
    /// `superseded_by` field a claim never writes and a four-key owner object
    /// ocx does not emit), and `crates/ocx_lib/tests/**` is outside WP-9's file
    /// set. A claim fixture dropped there would be round-tripped as if it were
    /// an upstream vector.
    ///
    /// **The comparison must stay on bytes.** `serialize_root` takes an
    /// order-preserving `Value`, `preserve_order` is on crate-wide, and
    /// `IndexMap::eq` is `other.get(key)` — order-independent. So
    /// `assert_eq!(value_a, value_b)` over two parsed roots passes with the
    /// fields in any order, and a later "simplification" to that form is a
    /// regression, not a cleanup.
    ///
    /// Reds on: reordering any two field insertions; emitting the derived
    /// `github` / `github_id` owner pair; spelling `status` `"Active"`;
    /// `skip_serializing_if` on `desc` or `deprecated_message`; expanding the
    /// empty `tags` object across lines.
    #[test]
    fn root_bytes_match_the_claim_root_form() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        assert_eq!(
            rendered(&[alice(), bob()], Some(&full_upstream())),
            FULL_ROOT,
            "field order, the two-key owner object and the owner list order are all emitted, not merely present"
        );
        assert_eq!(
            rendered(&[alice()], None),
            MINIMAL_ROOT,
            "a first-party claim omits `upstream` entirely and still emits both `null` fields"
        );
    }

    /// C-047 — `upstream` is omitted entirely when absent, never `null`.
    ///
    /// The negative half alone also passes for a builder that never emits
    /// `upstream` in any state, so the present-case is the positive control that
    /// makes it a check.
    ///
    /// Reds on: emitting `"upstream": null` (negative half), or dropping the
    /// object when it was given (positive half).
    #[test]
    fn upstream_omitted_when_absent() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let absent = rendered(&[alice()], None);
        assert!(
            !absent.contains("upstream"),
            "no `upstream` key at all, not a null placeholder: {absent}"
        );

        let present = rendered(&[alice()], Some(&full_upstream()));
        assert!(
            present.contains("\"upstream\": {"),
            "the object is emitted when `--upstream-org` was given: {present}"
        );
    }

    /// C-047, as amended — `upstream`'s **inner** optionals omit rather than
    /// emit `null`.
    ///
    /// The contract states the omission rule for the outer object only, so the
    /// natural symmetric implementation ships `Option` fields serialized as
    /// `null` — a root the live schema refuses, because it sets
    /// `additionalProperties: false` and takes no placeholder null.
    ///
    /// Reds on: dropping `skip_serializing_if` from either inner field.
    #[test]
    fn upstream_inner_fields_omit_rather_than_null() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let org_only = Upstream {
            org: "Acme Org".to_string(),
            repository_url: None,
            disclaimer: None,
        };
        assert!(
            rendered(&[alice()], Some(&org_only)).contains("\"upstream\": {\n    \"org\": \"Acme Org\"\n  },\n"),
            "org alone emits exactly one key inside the object: {}",
            rendered(&[alice()], Some(&org_only))
        );

        let url_only = Upstream {
            org: "Acme Org".to_string(),
            repository_url: Some("https://github.com/acme/widget".to_string()),
            disclaimer: None,
        };
        let rendered_url_only = rendered(&[alice()], Some(&url_only));
        assert!(
            rendered_url_only.contains("\"repository_url\": \"https://github.com/acme/widget\""),
            "a given optional is emitted: {rendered_url_only}"
        );
        assert!(
            !rendered_url_only.contains("disclaimer"),
            "an absent optional leaves no key behind: {rendered_url_only}"
        );
    }

    /// C-047 — `tags` is always `{}`, and it emits **inline**.
    ///
    /// The inline-vs-expanded form is the shared formatter's empty-object rule,
    /// and it is invisible to any assertion over a parsed value — which is why
    /// this asserts the bytes.
    ///
    /// Reds on: emitting a tag entry, or hand-formatting the empty object across
    /// two lines.
    #[test]
    fn tags_is_always_empty_object() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        assert!(
            rendered(&[alice()], None).ends_with("  \"tags\": {}\n}\n"),
            "empty objects emit inline and the document ends in exactly one newline"
        );
    }

    /// C-047 — `created` is a **date**, not the tag `observed` timestamp.
    ///
    /// The instant is pinned through `std::env::set_var`, never `EnvLock::set`:
    /// `current_timestamp` reads `std::env::var` directly, so the seam's own
    /// override map never reaches it and the assertion would silently degrade to
    /// "today equals today".
    ///
    /// Reds on: calling `current_timestamp()` instead of `current_date()` (the
    /// `T…Z` suffix appears), or reading an independent `Utc::now()` (the pinned
    /// past date is displaced by today's).
    #[test]
    fn created_is_a_date_not_a_timestamp() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let root = rendered(&[alice()], None);
        assert!(
            root.contains(&format!("\"created\": \"{PINNED_DATE}\"")),
            "the pinned instant's date is what lands in `created`: {root}"
        );
        assert!(
            !root.contains(PINNED_INSTANT),
            "a timestamp in `created` would collide with the tag `observed` format: {root}"
        );
    }

    /// C-047 — the bytes go through `oci::index::serialize_root`, not a second
    /// emitter.
    ///
    /// A source-text scan for the function name would measure itself (this
    /// module's own doc comment names it). The behavioural discriminator is the
    /// one rule OCX owns that no other JSON emitter implements: `ensure_ascii`,
    /// which escapes every scalar outside printable ASCII — plus the single
    /// trailing newline `serde_json::to_vec_pretty` does not write.
    ///
    /// Reds on: emitting through `serde_json::to_vec_pretty` (the raw UTF-8
    /// travels and the trailing newline vanishes).
    #[test]
    fn root_serializer_is_the_shared_one_with_ensure_ascii() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let upstream = Upstream {
            org: "Café".to_string(),
            repository_url: None,
            disclaimer: Some("Ünicode disclaimer — em dash".to_string()),
        };
        let root = rendered(&[alice()], Some(&upstream));

        assert!(
            root.contains("Caf\\u00e9"),
            "`ensure_ascii` escapes every non-ASCII scalar: {root}"
        );
        assert!(!root.contains('é'), "no raw non-ASCII byte survives: {root}");
        assert!(
            root.contains("\\u00dcnicode"),
            "and it applies to every scalar, not only the first: {root}"
        );
        assert!(root.ends_with("}\n"), "exactly one trailing newline");
        assert!(!root.ends_with("}\n\n"), "and not two");
    }

    /// C-047 — `name` is the identifier's registry and repository, with no tag
    /// and no digest.
    ///
    /// `root_name_uses_the_default_registry_prefix` moved to WP-14: both halves
    /// of the `OCX_DEFAULT_REGISTRY` resolution live in `ocx_cli`, and the
    /// library entry point receives an already-domained identifier. A library
    /// test that set the variable and expected this renderer to honour it would
    /// measure nothing — and would pass for a hardcoded `ocx.sh` whenever the
    /// override happened to be `ocx.sh`.
    ///
    /// Reds on: `format!("{package}")` (the tag rides along, because
    /// `Identifier`'s own `Display` appends `:tag` and `@digest`), or a hardcoded
    /// `ocx.sh` prefix (the non-default registry row).
    #[test]
    fn root_name_is_the_identifier_registry_and_repository() {
        assert_eq!(root_name(&oci::Identifier::new_registry("acme/widget", "ocx.sh")), NAME);
        assert_eq!(
            root_name(&oci::Identifier::new_registry("acme/widget", "registry.example")),
            "registry.example/acme/widget",
            "the registry comes from the identifier, never from a library-side default"
        );

        let tagged = oci::Identifier::parse("ocx.sh/acme/widget:1.0").expect("a tagged identifier parses");
        assert_eq!(root_name(&tagged), NAME, "a tag never reaches the logical name");
    }

    /// C-047 — a malformed `--repository` is refused at **exit 64**, from
    /// `ClaimError`'s own arm.
    ///
    /// Not from its source: `OciIndexError::MalformedPhysicalRef` classifies to
    /// `DataError` (65), and a malformed flag value is operator input, which is
    /// `EX_USAGE`. C-047's "verbatim" clause has **no reachable red** — the parse
    /// demands an exact `Identifier` round-trip, so every accepted value
    /// reconstructs byte-identically from its `(host, path)` tuple — so the
    /// accepted row below is a positive control for the parse being reached at
    /// all, not a verbatim assertion.
    ///
    /// Reds on: `#[error(transparent)] MalformedRepository(#[from] OciIndexError)`
    /// (the delegated classify yields 65), or accepting any refused row.
    #[test]
    fn malformed_repository_is_refused_at_usage_error() {
        assert_eq!(
            parse_repository(REPOSITORY).expect("a well-formed pointer is accepted"),
            REPOSITORY,
            "the positive control: an accepted value comes back verbatim"
        );

        for value in [
            "ghcr.io/acme/widget",           // no scheme
            "https://ghcr.io/acme/widget",   // wrong scheme
            "oci://ghcr.io",                 // no slash, so no path
            "oci:///acme/widget",            // empty host
            "oci://ghcr.io/",                // empty path
            "oci://ghcr.io/acme/widget:1.0", // smuggled tag
            "oci://ghcr.io/acme/widget@sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "oci://ghcr.io/Acme/Widget", // uppercase segment
            "",                          // empty
        ] {
            let error = parse_repository(value).expect_err("a malformed pointer is refused");
            assert!(
                matches!(&error, ClaimError::MalformedRepository { value: refused } if refused == value),
                "the refused value is named back to the operator: {error:?}"
            );
            assert_eq!(
                error.classify(),
                Some(ExitCode::UsageError),
                "a malformed flag value is EX_USAGE, not the index error's own EX_DATAERR"
            );
        }
    }
}
