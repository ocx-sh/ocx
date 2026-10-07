// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Each `#[derive(Classify)]` form on a fixture type: `classify`, `kind_detail`, and `DETAILS`.

#![expect(dead_code, reason = "fixture payloads and variants exist to be classified, not read")]

use std::fmt;
use std::sync::Arc;

use ocx_exit::{Classify, ClassifyErrorKind, ClassifyExitCode, Detail, ExitCode, Pick, Row};

/// Implements `Display` and `Error` for fixtures, whose message text no test reads.
macro_rules! fixture_error {
    ($($ty:ty),* $(,)?) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(stringify!($ty))
            }
        }
        impl std::error::Error for $ty {}
    )*};
}

fn slugs(details: &[ocx_exit::DetailEntry]) -> Vec<&'static str> {
    details.iter().map(|row| row.slug).collect()
}

fn slug_of(detail: Detail<'_>) -> &'static str {
    detail.fallback_entry().slug
}

// ── fixed rows, defer, type-level default, family override ──

#[derive(Debug, Classify)]
enum Leaf {
    #[exit(NotFound, slug = "leaf_missing", summary = "The leaf is missing")]
    Missing,
    #[exit(DataError, slug = "leaf_bad", summary = "The leaf is bad")]
    Bad { reason: String },
    #[exit(defer(Failure), slug = "leaf_deferred", summary = "The leaf defers to its cause")]
    Deferred(std::io::Error),
    // Same row as `Bad`, so `DETAILS` lists it once.
    #[exit(DataError, slug = "leaf_bad", summary = "The leaf is bad")]
    AlsoBad,
}

#[derive(Debug, Classify)]
#[exit(family = "Renamed")]
enum Aliased {
    #[exit(IoError, slug = "aliased_io", summary = "Aliased IO")]
    Io,
}

#[derive(Debug, Classify)]
#[exit(UsageError, slug = "same_for_all", summary = "Every variant shares this row")]
enum Uniform {
    One,
    Two(u8),
}

#[derive(Debug, Classify)]
#[exit(ConfigError, slug = "unit_struct", summary = "A struct with one row")]
struct Unit;

#[derive(Debug, Classify)]
#[exit(reserve(Failure, slug = "reserved_catch_all", summary = "No variant answers with this row"))]
enum Reserving {
    #[exit(NotFound, slug = "reserving_missing", summary = "Missing")]
    Missing,
}

fixture_error!(Leaf, Aliased, Uniform, Unit, Reserving);

#[test]
fn fixed_rows_answer_their_code_and_slug() {
    assert_eq!(Leaf::Missing.classify(), Some(ExitCode::NotFound));
    assert_eq!(slug_of(Leaf::Missing.kind_detail()), "leaf_missing");
    let bad = Leaf::Bad { reason: String::new() };
    assert_eq!(bad.classify(), Some(ExitCode::DataError));
    assert_eq!(slug_of(bad.kind_detail()), "leaf_bad");
}

#[test]
fn a_deferring_row_answers_none_with_its_slug() {
    let deferred = Leaf::Deferred(std::io::Error::other("x"));
    assert_eq!(deferred.classify(), None);
    let Detail::Fixed(entry) = deferred.kind_detail() else {
        panic!("a deferring literal is a fixed row");
    };
    assert_eq!((entry.slug, entry.exit_code), ("leaf_deferred", ExitCode::Failure));
}

#[test]
fn details_list_rows_in_variant_order_and_once() {
    assert_eq!(slugs(Leaf::DETAILS), ["leaf_missing", "leaf_bad", "leaf_deferred"]);
    assert_eq!(Leaf::DETAILS[2].family, "Leaf");
    assert_eq!(Leaf::DETAILS[1].summary, "The leaf is bad");
}

#[test]
fn a_reserved_row_is_listed_last_and_belongs_to_no_variant() {
    assert_eq!(slugs(Reserving::DETAILS), ["reserving_missing", "reserved_catch_all"]);
    assert_eq!(Reserving::DETAILS[1].exit_code, ExitCode::Failure);
    assert_eq!(Reserving::DETAILS[1].family, "Reserving");
}

#[test]
fn a_type_level_row_covers_every_variant_without_its_own() {
    assert_eq!(Uniform::One.classify(), Some(ExitCode::UsageError));
    assert_eq!(slug_of(Uniform::Two(1).kind_detail()), "same_for_all");
    assert_eq!(slugs(Uniform::DETAILS), ["same_for_all"]);
    assert_eq!(Unit.classify(), Some(ExitCode::ConfigError));
    assert_eq!(slugs(Unit::DETAILS), ["unit_struct"]);
}

#[test]
fn the_family_override_renames_rows() {
    assert_eq!(Aliased::DETAILS[0].family, "Renamed");
}

// ── delegate: tuple, named, boxed, shared, nested path, struct ──

#[derive(Debug, Classify)]
enum Outer {
    #[exit(delegate)]
    Tuple(Leaf),
    #[exit(delegate = source)]
    Named { source: Box<Leaf>, note: String },
    #[exit(delegate)]
    Shared(Arc<Leaf>),
    #[exit(delegate = 1.kind)]
    Nested(String, Wrapper),
    #[exit(Failure, slug = "outer_own", summary = "Outer's own row")]
    Own,
}

#[derive(Debug)]
struct Wrapper {
    kind: Leaf,
}

#[derive(Debug, Classify)]
#[exit(delegate = kind)]
struct Envelope {
    kind: Leaf,
}

#[derive(Debug, Classify)]
enum AllDelegating {
    #[exit(delegate)]
    Only(Leaf),
}

#[derive(Debug, Classify)]
#[exit(delegate = source)]
struct Boxed {
    source: Box<Leaf>,
}

#[derive(Debug, Classify)]
#[exit(delegate)]
struct Shared(Arc<Leaf>);

fixture_error!(Outer, Wrapper, Envelope, AllDelegating, Boxed, Shared);

#[test]
fn delegation_reaches_the_field_through_box_arc_and_a_path() {
    assert_eq!(Outer::Tuple(Leaf::Missing).classify(), Some(ExitCode::NotFound));
    let named = Outer::Named {
        source: Box::new(Leaf::AlsoBad),
        note: String::new(),
    };
    assert_eq!(named.classify(), Some(ExitCode::DataError));
    assert_eq!(slug_of(named.kind_detail()), "leaf_bad");
    assert_eq!(
        slug_of(Outer::Shared(Arc::new(Leaf::Missing)).kind_detail()),
        "leaf_missing"
    );
    let nested = Outer::Nested(String::new(), Wrapper { kind: Leaf::Missing });
    assert_eq!(nested.classify(), Some(ExitCode::NotFound));
    assert_eq!(slug_of(nested.kind_detail()), "leaf_missing");
}

#[test]
fn a_delegated_defer_stays_a_defer() {
    let outer = Outer::Tuple(Leaf::Deferred(std::io::Error::other("x")));
    assert_eq!(outer.classify(), None);
}

#[test]
fn a_delegating_arm_declares_no_row_and_an_all_delegating_type_owns_none() {
    assert_eq!(slugs(Outer::DETAILS), ["outer_own"]);
    assert!(AllDelegating::DETAILS.is_empty());
}

#[test]
fn a_struct_delegate_answers_with_its_field_type() {
    let envelope = Envelope { kind: Leaf::Missing };
    assert_eq!(envelope.classify(), Some(ExitCode::NotFound));
    assert_eq!(slug_of(envelope.kind_detail()), "leaf_missing");
    assert_eq!(slugs(Envelope::DETAILS), slugs(Leaf::DETAILS));
}

#[test]
fn a_struct_delegating_through_a_pointer_lists_the_pointee_rows() {
    let boxed = Boxed {
        source: Box::new(Leaf::Missing),
    };
    assert_eq!(boxed.classify(), Some(ExitCode::NotFound));
    assert_eq!(slugs(Boxed::DETAILS), slugs(Leaf::DETAILS));
    assert_eq!(slugs(Shared::DETAILS), slugs(Leaf::DETAILS));
}

// ── chain: from self, from a field ──

#[derive(Debug, Classify)]
enum Dynamic {
    #[exit(
        chain,
        fallback(Failure, slug = "dynamic_failed", summary = "Failed with an unclassified cause")
    )]
    Defers(std::io::Error),
    #[exit(chain = inner, fallback(Failure, slug = "dynamic_inner", summary = "The inner error failed"))]
    Decides { inner: Leaf },
    #[exit(chain = 0.kind, fallback(TempFail, slug = "dynamic_path", summary = "The wrapped error failed"))]
    ThroughPath(Wrapper),
}

fixture_error!(Dynamic);

#[test]
fn a_plain_chain_defers_the_code_and_starts_the_walk_at_itself() {
    let dynamic = Dynamic::Defers(std::io::Error::other("x"));
    assert_eq!(dynamic.classify(), None);
    let Detail::Chain { fallback, from } = dynamic.kind_detail() else {
        panic!("a chain arm answers a chain");
    };
    assert_eq!(fallback.slug, "dynamic_failed");
    assert!(std::ptr::addr_eq(from, &dynamic));
}

#[test]
fn a_chain_over_a_field_decides_the_fallback_code_and_starts_at_the_field() {
    let dynamic = Dynamic::Decides { inner: Leaf::Missing };
    // `Leaf::Missing` classifies `NotFound` itself; the direct answer stays the fallback code, because only the
    // binary's chain resolution can prefer the cause's.
    assert_eq!(dynamic.classify(), Some(ExitCode::Failure));
    let Detail::Chain { fallback, from } = dynamic.kind_detail() else {
        panic!("a chain arm answers a chain");
    };
    assert_eq!(fallback.slug, "dynamic_inner");
    let Dynamic::Decides { inner } = &dynamic else {
        unreachable!()
    };
    assert!(
        std::ptr::addr_eq(from, inner),
        "the walk starts at the field, not at the wrapper"
    );

    let through = Dynamic::ThroughPath(Wrapper { kind: Leaf::Missing });
    assert_eq!(through.classify(), Some(ExitCode::TempFail));
    let Detail::Chain { from, .. } = through.kind_detail() else {
        panic!("a chain arm answers a chain");
    };
    let Dynamic::ThroughPath(wrapper) = &through else {
        unreachable!()
    };
    assert!(std::ptr::addr_eq(from, &wrapper.kind));
}

#[test]
fn chain_rows_enter_details_in_arm_order() {
    assert_eq!(
        slugs(Dynamic::DETAILS),
        ["dynamic_failed", "dynamic_inner", "dynamic_path"]
    );
}

// ── with: guards, computed rows, a delegated pick, a chained pick ──

#[derive(Debug, Classify)]
enum Guarded {
    #[exit(
        with = by_size,
        rows(
            (DataError, slug = "guard_small", summary = "A small payload"),
            (IoError, slug = "guard_large", summary = "A large payload"),
            (defer(Failure), slug = "guard_unknown", summary = "An unmeasured payload"),
        )
    )]
    Sized(Option<usize>),
    #[exit(
        with = by_cause,
        rows((Unavailable, slug = "guard_unreachable", summary = "Nothing answered"))
    )]
    Cause(Option<Leaf>),
    #[exit(with = chained, rows((Failure, slug = "guard_chained", summary = "Chained from a pick")))]
    Chained(Leaf),
    // Declared again with the same code and summary: the type owns one row.
    #[exit(DataError, slug = "guard_small", summary = "A small payload")]
    Repeating,
}

fn by_size(error: &Guarded, rows: [Row; 3]) -> Pick<'_> {
    match error {
        Guarded::Sized(Some(size)) if *size < 10 => Pick::row(rows[0]),
        Guarded::Sized(Some(_)) => Pick::row(rows[1]),
        _ => Pick::row(rows[2]),
    }
}

fn by_cause(error: &Guarded, rows: [Row; 1]) -> Pick<'_> {
    match error {
        Guarded::Cause(Some(leaf)) => Pick::delegate(leaf),
        _ => Pick::row(rows[0]),
    }
}

fn chained(error: &Guarded, rows: [Row; 1]) -> Pick<'_> {
    match error {
        Guarded::Chained(leaf) => Pick::chain(leaf, rows[0]),
        _ => Pick::row(rows[0]),
    }
}

fixture_error!(Guarded);

#[test]
fn a_with_function_picks_among_the_declared_rows() {
    let small = Guarded::Sized(Some(3));
    assert_eq!(small.classify(), Some(ExitCode::DataError));
    assert_eq!(slug_of(small.kind_detail()), "guard_small");
    let large = Guarded::Sized(Some(300));
    assert_eq!(large.classify(), Some(ExitCode::IoError));
    assert_eq!(slug_of(large.kind_detail()), "guard_large");
}

#[test]
fn a_picked_defer_row_answers_none_but_keeps_its_slug() {
    let unknown = Guarded::Sized(None);
    assert_eq!(unknown.classify(), None);
    assert_eq!(slug_of(unknown.kind_detail()), "guard_unknown");
}

#[test]
fn a_with_function_can_delegate_instead_of_picking_a_row() {
    let delegated = Guarded::Cause(Some(Leaf::Missing));
    assert_eq!(delegated.classify(), Some(ExitCode::NotFound));
    assert_eq!(slug_of(delegated.kind_detail()), "leaf_missing");
    let fallback = Guarded::Cause(None);
    assert_eq!(fallback.classify(), Some(ExitCode::Unavailable));
    assert_eq!(slug_of(fallback.kind_detail()), "guard_unreachable");
}

#[test]
fn a_with_function_can_chain_over_the_error_it_names() {
    let chained = Guarded::Chained(Leaf::Missing);
    assert_eq!(chained.classify(), Some(ExitCode::Failure));
    let Detail::Chain { fallback, from } = chained.kind_detail() else {
        panic!("a chained pick answers a chain");
    };
    assert_eq!(fallback.slug, "guard_chained");
    let Guarded::Chained(leaf) = &chained else {
        unreachable!()
    };
    assert!(std::ptr::addr_eq(from, leaf));
}

#[test]
fn with_rows_are_deduplicated_across_arms_in_declaration_order() {
    assert_eq!(
        slugs(Guarded::DETAILS),
        [
            "guard_small",
            "guard_large",
            "guard_unknown",
            "guard_unreachable",
            "guard_chained"
        ]
    );
}

// ── every accepted form in one type ──

#[derive(Debug, Classify)]
#[exit(reserve(TempFail, slug = "mixed_retired", summary = "A retired catch-all"))]
enum Mixed {
    #[exit(NotFound, slug = "mixed_fixed", summary = "A fixed row")]
    Fixed,
    #[exit(defer(Failure), slug = "mixed_deferred", summary = "A deferring row")]
    Deferred(std::io::Error),
    #[exit(delegate)]
    Tuple(Leaf),
    #[exit(delegate = source)]
    Named { source: Box<Leaf> },
    #[exit(chain, fallback(Failure, slug = "mixed_chain", summary = "Chained from itself"))]
    Chained(std::io::Error),
    #[exit(chain = inner, fallback(IoError, slug = "mixed_field", summary = "Chained from a field"))]
    FromField { inner: Leaf },
    #[exit(
        with = pick_mixed,
        rows(
            (DataError, slug = "mixed_picked", summary = "A picked row"),
            (defer(Failure), slug = "mixed_unpicked", summary = "A picked deferring row"),
        )
    )]
    Picked(bool),
    // Same row as `Fixed`: listed once.
    #[exit(NotFound, slug = "mixed_fixed", summary = "A fixed row")]
    FixedAgain,
}

fn pick_mixed(error: &Mixed, rows: [Row; 2]) -> Pick<'_> {
    match error {
        Mixed::Picked(true) => Pick::row(rows[0]),
        _ => Pick::row(rows[1]),
    }
}

fixture_error!(Mixed);

#[test]
fn every_form_classifies_and_names_its_detail_in_one_type() {
    let io = || std::io::Error::other("x");
    let cases: [(Mixed, Option<ExitCode>, &str); 8] = [
        (Mixed::Fixed, Some(ExitCode::NotFound), "mixed_fixed"),
        (Mixed::FixedAgain, Some(ExitCode::NotFound), "mixed_fixed"),
        (Mixed::Deferred(io()), None, "mixed_deferred"),
        (Mixed::Tuple(Leaf::Missing), Some(ExitCode::NotFound), "leaf_missing"),
        (
            Mixed::Named {
                source: Box::new(Leaf::AlsoBad),
            },
            Some(ExitCode::DataError),
            "leaf_bad",
        ),
        (Mixed::Chained(io()), None, "mixed_chain"),
        (
            Mixed::FromField { inner: Leaf::Missing },
            Some(ExitCode::IoError),
            "mixed_field",
        ),
        (Mixed::Picked(true), Some(ExitCode::DataError), "mixed_picked"),
    ];
    for (error, code, slug) in cases {
        assert_eq!(error.classify(), code, "{error:?}");
        assert_eq!(slug_of(error.kind_detail()), slug, "{error:?}");
    }
    let unpicked = Mixed::Picked(false);
    assert_eq!(unpicked.classify(), None);
    assert_eq!(slug_of(unpicked.kind_detail()), "mixed_unpicked");
}

#[test]
fn mixed_chain_arms_start_the_walk_at_self_or_at_the_field() {
    let chained = Mixed::Chained(std::io::Error::other("x"));
    let Detail::Chain { from, .. } = chained.kind_detail() else {
        panic!("a chain arm answers a chain");
    };
    assert!(std::ptr::addr_eq(from, &chained));

    let from_field = Mixed::FromField { inner: Leaf::Missing };
    let Detail::Chain { from, .. } = from_field.kind_detail() else {
        panic!("a chain arm answers a chain");
    };
    let Mixed::FromField { inner } = &from_field else {
        unreachable!()
    };
    assert!(std::ptr::addr_eq(from, inner));
}

#[test]
fn mixed_details_follow_arm_order_then_reserved_rows_once_each() {
    assert_eq!(
        slugs(Mixed::DETAILS),
        [
            "mixed_fixed",
            "mixed_deferred",
            "mixed_chain",
            "mixed_field",
            "mixed_picked",
            "mixed_unpicked",
            "mixed_retired"
        ]
    );
    assert_eq!(Mixed::DETAILS[6].exit_code, ExitCode::TempFail);
}
