// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The harness's own red/green proof (C-006): every one of its five
//! properties shown failing on a fixture tree built to violate it and passing
//! on the tree built to satisfy it. Fixture trees live under
//! `tests/boundary_fixtures/`; witnesses are `.rs.txt` so the walk never
//! picks one up as a scanned source.

use std::path::{Path, PathBuf};

use ocx_test_support::boundary::{assert_no_imports, assert_no_imports_derived, assert_no_needles};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/boundary_fixtures")
        .join(name)
}

const FORBIDDEN: &[&str] = &["project"];

// ---- property 1: a forbidden reach is reported ----------------------------

#[test]
fn p1_green_a_clean_tree_passes() {
    assert_no_imports(&[fixture("clean")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "offender/b.rs:6: crate::project::ProjectConfig")]
fn p1_red_a_use_import_is_flagged() {
    assert_no_imports(&[fixture("offender")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "nested_use/a.rs:6: crate::project::lock")]
fn p1_red_a_nested_use_group_is_flagged() {
    assert_no_imports(&[fixture("nested_use")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "expression_path/a.rs:7: crate::project::lock::is_stale")]
fn p1_red_a_fully_qualified_expression_path_is_flagged() {
    assert_no_imports(&[fixture("expression_path")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "reexport/user.rs:6: crate::project::config::ProjectConfig")]
fn p1_red_a_lib_root_reexport_is_flagged() {
    assert_no_imports(&[fixture("reexport")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "super_path/shell/reconcile.rs:13: crate::project::lock::is_stale")]
fn p1_red_a_super_chain_from_an_inline_module_is_flagged() {
    assert_no_imports(&[fixture("super_path/shell")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "super_path/shell/export.rs:7: crate::project::lock")]
fn p1_red_a_use_super_statement_is_flagged() {
    assert_no_imports(&[fixture("super_path/shell")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

// ---- property 2: the walk must find more than one file ------------------

#[test]
#[should_panic(expected = "walked 1 file(s)")]
fn p2_red_a_single_file_walk_is_refused() {
    assert_no_imports(&[fixture("single")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "walked 0 file(s)")]
fn p2_red_a_missing_subtree_is_refused() {
    assert_no_imports(&[fixture("does_not_exist")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "does_not_exist")]
fn p2_red_a_subtree_that_empties_out_inside_a_corpus_is_refused() {
    // H4's union walk: the `> 1` floor is met by the surviving subtrees, so
    // a dropped one has to be named per subtree or it reads as a clean scan.
    assert_no_imports(
        &[fixture("clean"), fixture("does_not_exist")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
fn p2_green_a_one_file_tree_is_walked_as_part_of_a_corpus() {
    assert_no_imports(
        &[fixture("single"), fixture("clean")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "offender/b.rs:6: crate::project::ProjectConfig")]
fn p2_red_a_reach_in_a_corpus_member_is_flagged() {
    assert_no_imports(
        &[fixture("single"), fixture("offender")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "single_offender/only.rs:7: crate::project::ProjectConfig")]
fn p2_red_a_reach_in_a_one_file_corpus_member_is_flagged() {
    // The H4 shape: the offending file is alone in its tree, so a walk that
    // still skipped one-file trees would green here.
    assert_no_imports(
        &[fixture("clean"), fixture("single_offender")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

// ---- property 3: the witness must reach a forbidden module ---------------

#[test]
#[should_panic(expected = "witness_none.rs.txt reaches nothing")]
fn p3_red_a_witness_without_a_reach_is_refused() {
    assert_no_imports(&[fixture("clean")], FORBIDDEN, &fixture("witness_none.rs.txt"));
}

#[test]
#[should_panic(expected = "carries no [\"crate::nowhere\"]")]
fn p3_red_a_forbidden_module_no_witness_reaches_is_refused() {
    // The blind spot WP-44 closed: `nowhere` names nothing, so it forbids
    // nothing, and before the fix it sat in the list hiding behind `project`'s
    // hit — a typo or a renamed module reads as a stricter guard while the
    // scan over it proves nothing.
    assert_no_imports(
        &[fixture("clean")],
        &["project", "nowhere"],
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
fn p3_green_each_forbidden_module_is_witnessed_exactly_or_by_extension() {
    // `witness_ok.rs.txt` reaches `crate::project::ProjectConfig`, which
    // witnesses the module it extends and the full path alike.
    assert_no_imports(
        &[fixture("clean")],
        &["project", "project::ProjectConfig"],
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
fn p3_green_a_derived_set_asks_for_one_witness_across_the_list() {
    // The named opt-out: a caller that read its forbidden set off the crate's
    // own `mod` items cannot express a needle naming nothing, so it is not made
    // to ship a witness reaching every module. Same inputs as the red above.
    assert_no_imports_derived(
        &[fixture("clean")],
        &["project", "nowhere"],
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "boundary harness: read")]
fn p3_red_a_missing_witness_is_refused() {
    assert_no_imports(&[fixture("clean")], FORBIDDEN, &fixture("witness_missing.rs.txt"));
}

// ---- property 4: comments and literals are never scanned, on both sides --

#[test]
fn p4_green_reaches_inside_comments_and_literals_are_ignored() {
    assert_no_imports(&[fixture("commented_only")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "witness_commented.rs.txt reaches nothing")]
fn p4_red_a_commented_out_witness_is_refused() {
    assert_no_imports(&[fixture("clean")], FORBIDDEN, &fixture("witness_commented.rs.txt"));
}

// ---- property 5: every scanned file parses -----------------------------

#[test]
#[should_panic(expected = "unparsable/broken.rs:7:6: expected")]
fn p5_red_an_unparsable_file_is_refused() {
    assert_no_imports(&[fixture("unparsable")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "reexport_unparsable/lib.rs:12:6: expected")]
fn p5_red_an_unparsable_crate_root_is_refused_instead_of_emptying_the_table() {
    // The root is outside the scanned subtree, so only the re-export table
    // reads it: a tolerated parse failure there would forgive
    // `sub/user.rs`'s `crate::ProjectConfig` silently.
    assert_no_imports(
        &[fixture("reexport_unparsable/sub")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "`pub use project::*;` names no module file")]
fn p5_red_an_unresolved_glob_reexport_is_refused_instead_of_dropping_its_names() {
    // Skipping the glob empties every name it would contribute, and
    // `sub/user.rs`'s `crate::ProjectConfig` then resolves to nothing
    // forbidden — the same shape a crate root's own glob re-exports are one
    // renamed module file away from (`ocx_lib`'s `lib.rs` was the example
    // until WP-37 deleted it).
    assert_no_imports(
        &[fixture("reexport_unresolved_glob/sub")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "boundary harness: parse")]
fn p5_red_an_unparsable_witness_is_refused() {
    assert_no_imports(&[fixture("clean")], FORBIDDEN, &fixture("witness_unparsable.rs.txt"));
}

// ---- the needle variant shares every property ---------------------------

#[test]
fn needles_green_on_a_clean_tree() {
    assert_no_needles(
        &[fixture("clean")],
        &["module_path!"],
        &fixture("witness_needle.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "witness_none.rs.txt reaches nothing")]
fn needles_red_when_the_witness_lacks_the_needle() {
    assert_no_needles(&[fixture("clean")], &["module_path!"], &fixture("witness_none.rs.txt"));
}

#[test]
#[should_panic(expected = "commented_only/a.rs:14: format!")]
fn needles_red_on_a_needle_in_code() {
    // In commented_only/a.rs `crate::project` sits only inside literals, but
    // `format!` is code — that is the needle that must be reported.
    assert_no_needles(
        &[fixture("commented_only")],
        &["format!", "module_path!"],
        &fixture("witness_needle.rs.txt"),
    );
}

// ---- reaches the parser must not lose ------------------------------------

#[test]
#[should_panic(expected = "macro_arguments/a.rs:7: crate::project::lock::is_stale")]
fn p1_red_a_reach_inside_macro_arguments_is_flagged() {
    // `syn` leaves `format!(…)`'s arguments as tokens; the walk over them
    // is what keeps a reach there from hiding.
    assert_no_imports(&[fixture("macro_arguments")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "macro_arguments/a.rs:15: crate::project::X")]
fn p1_red_a_reach_in_a_struct_literal_field_inside_macro_arguments_is_flagged() {
    // One `:` before `crate` is a field ascription, not a path separator.
    assert_no_imports(&[fixture("macro_arguments")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "macro_arguments/a.rs:19: crate::project::Y")]
fn p1_red_a_reach_in_a_json_key_inside_macro_arguments_is_flagged() {
    assert_no_imports(&[fixture("macro_arguments")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "macro_arguments/a.rs:23: crate::project::T")]
fn p1_red_a_reach_in_a_closure_ascription_inside_macro_arguments_is_flagged() {
    assert_no_imports(&[fixture("macro_arguments")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "macro_use_group/a.rs:9: crate::project::lock")]
fn p1_red_a_use_group_inside_a_macro_body_is_flagged() {
    // `syn` parses no macro body, so the token walk sees `crate :: {` and the
    // path stops at the brace — every name in the group is lost, and only the
    // group's own expansion finds the one that reaches `project`.
    assert_no_imports(&[fixture("macro_use_group")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/shell.rs:14: crate::project::api")]
fn p1_red_a_super_chain_from_a_mod_opened_inside_a_macro_body_is_flagged() {
    // `syn` parses no macro body, so the `mod inner {` the `macro_rules!`
    // opens never becomes an `ItemMod`: resolved against `project::shell`
    // alone, `super::super::api` pops past the crate root to `crate::api` and
    // the scan reports nothing. The module chain written in the tokens is what
    // makes it `crate::project::api`.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

// Every bypass shape of the macro-written module is a permanent case here,
// not a one-off proof: two scanners have regressed silently already. All six
// share the `macro_mod_nesting` fixture crate, one file per shape.

#[test]
#[should_panic(expected = "macro_mod_nesting/project/substitution.rs:12: crate::project::api")]
fn p1_red_a_super_chain_under_a_substituted_module_name_is_flagged() {
    // `mod $name {` — the name is not knowable before expansion, the depth
    // is. Counting no module here pops the chain past the crate root and the
    // reach disappears entirely.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/nested_macro.rs:13: crate::project::api")]
fn p1_red_a_super_chain_under_a_module_two_macros_deep_is_flagged() {
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/cfg_gated.rs:12: crate::project::api")]
fn p1_red_a_super_chain_under_a_cfg_gated_macro_written_module_is_flagged() {
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/use_super.rs:11: crate::project::api::lock")]
fn p1_red_a_use_super_inside_a_macro_written_module_is_flagged() {
    // The `use`-expansion arm reads the same module chain as the path scan.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/in_argument.rs:9: crate::project::api")]
fn p1_red_a_super_chain_under_a_module_passed_as_a_macro_argument_is_flagged() {
    // The token stream starts at `mod`, so the scan has no preceding context
    // to lean on.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/path_attr.rs:13: crate::project::api")]
fn p1_red_a_path_attribute_does_not_stop_a_macro_written_module_being_counted() {
    // `#[path]` redirects where the module's children are looked up; it does
    // not rename the module. (Pointing a *body-less* `mod x;` at another file
    // is out of scope — see `module_stack_per_token`'s doc.)
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

// Shapes absent from `crates/**`, so no differential over the live tree could
// ever have reached them — each pinned by a fixture instead. The `$crate`
// group was R20: `syn::parse_str::<ItemUse>` refused it and the old arm
// dropped it with a silent `continue`, while the token path scan stopped at
// the group's brace, so both arms read clean.

#[test]
#[should_panic(expected = "macro_mod_nesting/project/dollar_crate_group.rs:17: crate::project::api")]
fn p1_red_a_dollar_crate_use_group_inside_a_macro_body_is_flagged() {
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/dollar_crate_group.rs:20: crate::project::lock::is_stale")]
fn p1_red_a_dollar_crate_path_outside_a_group_is_flagged() {
    // The non-group form rides the token path scan, which reads past the `$`
    // to the `crate` token — it never needed the normalisation. Pinned so the
    // one arm that carries it cannot quietly stop.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/raw_ident.rs:14: crate::project::api")]
fn p1_red_a_raw_identifier_use_item_is_flagged() {
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/raw_ident.rs:17: crate::project::lock::is_stale")]
fn p1_red_a_raw_identifier_expression_path_is_flagged() {
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_mod_nesting/project/raw_ident.rs:21: crate::project::api")]
fn p1_red_a_raw_identifier_path_inside_macro_arguments_is_flagged() {
    // The token arm, which ended a path at the `#` before the fix and so lost
    // every segment behind it.
    assert_no_imports(
        &[fixture("macro_mod_nesting/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_relex_refused/a.rs:13: `use$p;` begins `use` but does not re-lex")]
fn p5_red_a_use_slice_that_does_not_relex_is_refused() {
    // The general guard R20 asks for: a `use` slice the scanner cannot read is
    // red — naming the file, the line and the reconstructed text — rather than
    // a silent skip that reads as a clean scan. Its absence is what let the
    // `$crate` group through, and would let the next unenumerated shape.
    assert_no_imports(
        &[fixture("macro_relex_refused")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
fn p5_green_precise_capturing_is_not_a_use_statement() {
    // The one carve-out above the refusal: `impl Trait + use<'_>`. Without it
    // the guard above reds every file that writes precise capturing in a
    // macro, so the carve-out needs a fixture of its own to stay exercised.
    assert_no_imports(
        &[fixture("macro_precise_capture")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "inline_mod_attrs/a.rs:7: crate::project::D")]
fn p1_red_a_reach_in_an_inline_module_attribute_is_flagged() {
    assert_no_imports(&[fixture("inline_mod_attrs")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
#[should_panic(expected = "inline_mod_attrs/a.rs:10: crate::project")]
fn p1_red_a_pub_in_restriction_on_an_inline_module_is_flagged() {
    assert_no_imports(&[fixture("inline_mod_attrs")], FORBIDDEN, &fixture("witness_ok.rs.txt"));
}

#[test]
fn p1_green_pub_super_and_pub_crate_scope_items_and_name_no_module() {
    // `restricted/project/inner.rs` sits inside the forbidden module, so a
    // `pub(super)` read as a path would resolve to `crate::project`.
    assert_no_imports(
        &[fixture("restricted/project")],
        FORBIDDEN,
        &fixture("witness_ok.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "carries no [\"type_name_of_val\"]")]
fn needles_red_when_the_witness_lacks_one_of_the_needles() {
    // A needle the witness never exercises could have stopped matching
    // releases ago: its scan is vacuous while the list's other hits keep the
    // witness non-empty.
    assert_no_needles(
        &[fixture("clean")],
        &["module_path!", "type_name_of_val"],
        &fixture("witness_needle.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "witness_commented.rs.txt reaches nothing")]
fn needles_red_when_the_witness_needle_is_commented_out_or_in_a_literal() {
    assert_no_needles(
        &[fixture("clean")],
        &["module_path!"],
        &fixture("witness_commented.rs.txt"),
    );
}

#[test]
#[should_panic(expected = "macro_arguments/a.rs:11: module_path!")]
fn needles_red_on_a_needle_inside_macro_arguments() {
    assert_no_needles(
        &[fixture("macro_arguments")],
        &["module_path!"],
        &fixture("witness_needle.rs.txt"),
    );
}

// ---- the roots a path can carry (B5R-7, B5R-8) ---------------------------

/// A crate root, the forbidden spelling for a subject that has already left
/// `ocx_lib` (DEC-30 item 1).
const FORBIDDEN_CRATE: &[&str] = &["ocx_project"];

/// `self::` roots a path at the current module, and both syn-parsed spellings
/// of it — the `use` statement and the qualified expression — reach.
///
/// It resolved to nothing at all before B5R-8: `record` matched `crate` and
/// `super` and returned on everything else, and `expand_use_tree` dropped the
/// leading `self` so `use self::project::lock` arrived as the rootless
/// `[project, lock]`, which no arm can tell from an extern crate's path.
#[test]
#[should_panic(expected = "self_path/shell.rs:8: crate::shell::project::lock")]
fn p1_red_a_self_rooted_use_statement_is_flagged() {
    assert_no_imports(
        &[fixture("self_path")],
        &["shell::project"],
        &fixture("witness_self.rs.txt"),
    );
}

/// The same root inside macro tokens, which `syn` never parses into a path —
/// so this arm needs `self` in the token scan's root list as well.
#[test]
#[should_panic(expected = "self_macro/shell.rs:9: crate::shell::project::MARKER")]
fn p1_red_a_self_rooted_path_in_macro_arguments_is_flagged() {
    assert_no_imports(
        &[fixture("self_macro")],
        &["shell::project"],
        &fixture("witness_self.rs.txt"),
    );
}

/// `::ocx_project::…` — the absolute-path spelling — is a reach.
///
/// Both arms of `reaches` dropped it: the token scan disqualified any root
/// preceded by `::`, and the guard was new with the B5 fix because `::crate::`
/// is not valid Rust, so `ocx_*` is the only root a leading `::` can carry
/// (B5R-7).
#[test]
#[should_panic(expected = "crate_root_leading/a.rs:8: ocx_project::lock::is_stale")]
fn p1_red_a_leading_colon_crate_path_in_macro_arguments_is_flagged() {
    assert_no_imports(
        &[fixture("crate_root_leading")],
        FORBIDDEN_CRATE,
        &fixture("witness_ocx_crate.rs.txt"),
    );
}

/// The green half, and the reason the disqualification exists at all:
/// `foo::ocx_project::X` names a module of somebody else's path, not the
/// crate. The two cases differ only in what precedes the `::`, so a fix that
/// simply stopped disqualifying would turn this tree red.
#[test]
fn p1_green_a_crate_name_as_a_mid_path_segment_is_not_a_reach() {
    assert_no_imports(
        &[fixture("crate_root_segment")],
        FORBIDDEN_CRATE,
        &fixture("witness_ocx_crate.rs.txt"),
    );
}
