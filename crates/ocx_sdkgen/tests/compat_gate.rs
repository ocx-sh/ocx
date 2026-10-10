// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The compat gate over the mutation corpus (`tests/compat_corpus/`), its fixture baselines, and the live goldens
//! against the committed `baseline/` beside the contract ledger. The live case returns early only while `baseline/` is absent,
//! a state `test/lint/test_contract_baseline.py` proves legitimate or reds.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ocx_sdkgen::Documents;
use ocx_sdkgen::compat::{self, Code, EntryRule, LedgerEntry, Violation};
use ocx_sdkgen::lint::Kind;
use serde_json::{Value, json};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json_at(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/compat_corpus")
}

/// Every key under `value`, counted by a plain walk that knows no schema: the names inside `properties`, `$defs` and
/// `reports` are not keywords, everything else is.
fn raw_keys(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, child)| {
                1 + match child {
                    Value::Object(names) if ["properties", "$defs", "reports"].contains(&key.as_str()) => {
                        names.values().map(raw_keys).sum()
                    }
                    _ => raw_keys(child),
                }
            })
            .sum(),
        Value::Array(items) => items.iter().map(raw_keys).sum(),
        _ => 0,
    }
}

fn kind_of(name: &str) -> Kind {
    match name {
        "reports" => Kind::Reports,
        "errors" => Kind::Errors,
        "cli" => Kind::Cli,
        other => panic!("unknown corpus kind {other}"),
    }
}

/// A code the differ owns; a corpus case that names any other is red.
fn code(name: &str) -> &'static str {
    Code::from_id(name)
        .unwrap_or_else(|| panic!("corpus case names `{name}`, which is no differ code"))
        .id()
}

type Triple = (&'static str, String, Vec<String>);

fn triples(findings: &[compat::Finding]) -> Vec<Triple> {
    let mut all: Vec<Triple> = findings
        .iter()
        .map(|finding| {
            (
                finding.code.id(),
                finding.pointer.clone(),
                finding.subjects.iter().cloned().collect(),
            )
        })
        .collect();
    all.sort();
    all
}

struct Case {
    name: String,
    kind: Kind,
    base: Value,
    current: Value,
    expected: Vec<Triple>,
}

fn cases() -> Vec<Case> {
    let mut names: Vec<_> = std::fs::read_dir(corpus_dir())
        .expect("the corpus directory")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|dir| {
            let expect = json_at(&dir.join("expect.json"));
            let mut expected: Vec<Triple> = expect["findings"]
                .as_array()
                .expect("a findings list")
                .iter()
                .map(|finding| {
                    let mut subjects: Vec<String> = finding["subjects"]
                        .as_array()
                        .expect("a subject list")
                        .iter()
                        .map(|subject| subject.as_str().expect("a subject").to_owned())
                        .collect();
                    subjects.sort();
                    (
                        code(finding["code"].as_str().expect("a code")),
                        finding["pointer"].as_str().expect("a pointer").to_owned(),
                        subjects,
                    )
                })
                .collect();
            expected.sort();
            Case {
                name: dir.file_name().expect("a name").to_string_lossy().into_owned(),
                kind: kind_of(expect["kind"].as_str().expect("a kind")),
                base: json_at(&dir.join("base.json")),
                current: json_at(&dir.join("current.json")),
                expected,
            }
        })
        .collect()
}

#[test]
fn every_corpus_case_matches_exactly_and_the_differ_read_every_keyword() {
    let cases = cases();
    assert!(cases.len() >= 61, "read {} corpus cases", cases.len());
    let mut exercised = BTreeSet::new();
    for case in &cases {
        let run = compat::diff(&case.base, &case.current, case.kind);
        assert_eq!(triples(&run.findings), case.expected, "{}", case.name);
        assert_eq!(
            run.keywords,
            raw_keys(&case.base) + raw_keys(&case.current),
            "{}: the differ read a different keyword count than the documents hold",
            case.name
        );
        exercised.extend(case.expected.iter().map(|(code, ..)| *code));
    }
    let missing: Vec<_> = Code::ALL
        .iter()
        .map(|code| code.id())
        .filter(|code| !exercised.contains(code))
        .collect();
    assert!(missing.is_empty(), "no corpus case exercises {missing:?}");
}

/// Defaults for the two documents a case does not vary: small, versioned, inside the subset.
fn baseline_of(case: &str) -> Value {
    json_at(&corpus_dir().join(case).join("base.json"))
}

fn base_documents() -> Documents {
    Documents {
        reports: baseline_of("b02_property_removed"),
        errors: baseline_of("r01_exit_code_removed"),
        cli: baseline_of("g13_public_env_removed"),
    }
}

fn slot(documents: &mut Documents, kind: Kind) -> &mut Value {
    match kind {
        Kind::Reports => &mut documents.reports,
        Kind::Errors => &mut documents.errors,
        Kind::Cli => &mut documents.cli,
        Kind::Input => panic!("no input document"),
    }
}

/// The documents of one corpus case in the slot of its kind, the fixture baselines everywhere else.
fn pair(case: &Case) -> (Documents, Documents) {
    let (mut base, mut current) = (base_documents(), base_documents());
    *slot(&mut base, case.kind) = case.base.clone();
    *slot(&mut current, case.kind) = case.current.clone();
    (base, current)
}

fn case(name: &str) -> Case {
    cases()
        .into_iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no corpus case {name}"))
}

fn set(documents: &mut Documents, kind: Kind, pointer: &str, to: Value) {
    *slot(documents, kind)
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("no node at {pointer}")) = to;
}

fn entry(document: &str, subjects: &[&str], rule: EntryRule, pointer: &str) -> LedgerEntry {
    LedgerEntry {
        document: document.to_owned(),
        subjects: subjects.iter().map(|subject| (*subject).to_owned()).collect(),
        rule,
        pointer: pointer.to_owned(),
        reason: "the fixture says so".to_owned(),
    }
}

const PUSH_VERSION: &str = "/$defs/PushReportRoot/properties/schema_version/const";
const MESSAGE: &str = "/$defs/PushReportRoot/properties/message";

#[test]
fn enum_outside_an_unknown_arm_is_unmodelled() {
    let mut current = base_documents();
    current.reports["$defs"]["PushReportRoot"]["properties"]["message"]["enum"] = json!(["a"]);
    let run = compat::diff(&base_documents().reports, &current.reports, Kind::Reports);
    assert_eq!(
        triples(&run.findings),
        [("U01", MESSAGE.to_owned(), vec!["PushReport".to_owned()])]
    );
}

#[test]
fn a_property_dropped_from_the_live_reports_golden_is_found() {
    let base = json_at(&workspace_root().join("crates/ocx_schema/tests/golden/reports.json"));
    let mut current = base.clone();
    let pointer = "/$defs/AboutRoot/properties/version";
    assert!(current.pointer(pointer).is_some(), "the golden no longer has {pointer}");
    current["$defs"]["AboutRoot"]["properties"]
        .as_object_mut()
        .expect("properties")
        .remove("version");
    let run = compat::diff(&base, &current, Kind::Reports);
    assert_eq!(
        triples(&run.findings),
        [("B02", pointer.to_owned(), vec!["About".to_owned()])]
    );
}

#[test]
fn a_verdict_is_a_gate_verdict_for_every_corpus_case() {
    for case in cases() {
        let (base, current) = pair(&case);
        let unacknowledged = compat::gate(&base, &current, &[])
            .into_iter()
            .filter(|violation| matches!(violation, Violation::Unacknowledged { .. }))
            .count();
        assert_eq!(unacknowledged, case.expected.len(), "{}", case.name);
    }
}

#[test]
fn a_break_without_an_entry_is_unacknowledged() {
    let case = case("b02_property_removed");
    let (base, current) = pair(&case);
    let violations = compat::gate(&base, &current, &[]);
    let [
        Violation::Unacknowledged {
            kind: Kind::Reports,
            finding,
        },
    ] = violations.as_slice()
    else {
        panic!("{violations:?}");
    };
    assert_eq!((finding.code, finding.pointer.as_str()), (Code::B02, MESSAGE));
}

#[test]
fn an_entry_and_a_bump_make_one_finding_green() {
    let case = case("b02_property_removed");
    let (base, mut current) = pair(&case);
    set(&mut current, Kind::Reports, PUSH_VERSION, json!(2));
    let ledger = [entry("reports", &["PushReport"], EntryRule::Code(Code::B02), MESSAGE)];
    assert_eq!(
        compat::diff(&base.reports, &current.reports, Kind::Reports)
            .findings
            .len(),
        1
    );
    assert_eq!(compat::gate(&base, &current, &ledger), []);
    // The same entry without the bump leaves the version where it was.
    let (base, current) = pair(&case);
    assert_eq!(
        compat::gate(&base, &current, &ledger),
        [Violation::Version {
            kind: Kind::Reports,
            subject: "PushReport".to_owned(),
            expected: 2,
            found: 1
        }]
    );
}

#[test]
fn a_bump_without_an_entry_is_a_version_violation() {
    let mut current = base_documents();
    set(&mut current, Kind::Reports, PUSH_VERSION, json!(2));
    assert_eq!(
        compat::gate(&base_documents(), &current, &[]),
        [Violation::Version {
            kind: Kind::Reports,
            subject: "PushReport".to_owned(),
            expected: 1,
            found: 2
        }]
    );
}

#[test]
fn an_entry_that_matches_nothing_is_stale_unless_it_is_semantic() {
    let stale = entry("reports", &["PushReport"], EntryRule::Code(Code::B02), MESSAGE);
    // The entry still lists PushReport, so the version step expects the bump it implies.
    assert_eq!(
        compat::gate(&base_documents(), &base_documents(), std::slice::from_ref(&stale)),
        [
            Violation::Stale(stale),
            Violation::Version {
                kind: Kind::Reports,
                subject: "PushReport".to_owned(),
                expected: 2,
                found: 1
            }
        ]
    );
    // A `semantic` entry names a meaning change the differ cannot see; it bumps and is never stale.
    let semantic = entry("reports", &["PushReport"], EntryRule::Semantic, MESSAGE);
    let mut current = base_documents();
    set(&mut current, Kind::Reports, PUSH_VERSION, json!(2));
    assert_eq!(compat::gate(&base_documents(), &current, &[semantic]), []);
}

#[test]
fn min_items_is_unmodelled_and_needs_an_entry() {
    let (base, current) = pair(&case("u01_min_items"));
    let violations = compat::gate(&base, &current, &[]);
    let [Violation::Unacknowledged { finding, .. }] = violations.as_slice() else {
        panic!("{violations:?}");
    };
    assert_eq!(finding.code, Code::U01);
}

#[test]
fn a_description_needs_a_doc_entry_and_a_semantic_one_also_bumps() {
    let doc = case("d01_description_doc");
    let (base, current) = pair(&doc);
    let violations = compat::gate(&base, &current, &[]);
    assert!(matches!(violations.as_slice(), [Violation::Unacknowledged { finding, .. }] if finding.code == Code::D01));
    assert_eq!(
        compat::gate(
            &base,
            &current,
            &[entry("reports", &["PushReport"], EntryRule::Doc, MESSAGE)]
        ),
        []
    );

    let size = "/$defs/PushReportRoot/properties/size";
    let (base, mut current) = pair(&case("d01_description_semantic"));
    let semantic = [entry("reports", &["PushReport"], EntryRule::Semantic, size)];
    assert_eq!(
        compat::gate(&base, &current, &semantic),
        [Violation::Version {
            kind: Kind::Reports,
            subject: "PushReport".to_owned(),
            expected: 2,
            found: 1
        }]
    );
    set(&mut current, Kind::Reports, PUSH_VERSION, json!(2));
    assert_eq!(compat::gate(&base, &current, &semantic), []);
}

#[test]
fn an_entry_must_list_every_subject_the_finding_reaches() {
    let (base, mut current) = pair(&case("b06_pattern_changed"));
    for root in ["PushReport", "StatusReport"] {
        set(
            &mut current,
            Kind::Reports,
            &format!("/$defs/{root}Root/properties/schema_version/const"),
            json!(2),
        );
    }
    let short = [entry(
        "reports",
        &["PushReport"],
        EntryRule::Code(Code::B06),
        "/$defs/Digest",
    )];
    assert!(
        compat::gate(&base, &current, &short)
            .iter()
            .any(|violation| matches!(violation, Violation::Unacknowledged { .. }))
    );
    let whole = [entry(
        "reports",
        &["PushReport", "StatusReport"],
        EntryRule::Code(Code::B06),
        "/$defs/Digest",
    )];
    assert_eq!(compat::gate(&base, &current, &whole), []);
}

#[test]
fn an_acknowledged_root_deletion_and_a_new_root_at_one_are_green() {
    let (base, current) = pair(&case("b01_root_removed"));
    let removal = [entry(
        "reports",
        &["StatusReport"],
        EntryRule::Code(Code::B01),
        "/reports/StatusReport",
    )];
    assert_eq!(compat::gate(&base, &current, &removal), []);

    let (base, mut current) = pair(&case("root_added"));
    assert_eq!(compat::gate(&base, &current, &[]), []);
    set(
        &mut current,
        Kind::Reports,
        "/$defs/TagReportRoot/properties/schema_version/const",
        json!(2),
    );
    assert_eq!(
        compat::gate(&base, &current, &[]),
        [Violation::Version {
            kind: Kind::Reports,
            subject: "TagReport".to_owned(),
            expected: 1,
            found: 2
        }]
    );
}

#[test]
fn an_errors_description_change_bumps_nothing_and_a_break_bumps_both_version_spellings() {
    let mut current = base_documents();
    set(
        &mut current,
        Kind::Errors,
        "/$defs/ErrorDetail/description",
        json!("Reworded."),
    );
    let doc = [entry("errors", &["*"], EntryRule::Doc, "/$defs/ErrorDetail")];
    assert_eq!(compat::gate(&base_documents(), &current, &doc), []);

    let (base, mut current) = pair(&case("r02_exit_code_category_changed"));
    let ledger = [entry(
        "errors",
        &["*"],
        EntryRule::Code(Code::R02),
        "/$defs/ExitCode/x-ocx-enum/3",
    )];
    assert_eq!(compat::gate(&base, &current, &ledger).len(), 2);
    set(
        &mut current,
        Kind::Errors,
        "/$id",
        json!("https://ocx.sh/schemas/errors/v2.json"),
    );
    set(&mut current, Kind::Errors, "/properties/schema_version/const", json!(2));
    assert_eq!(compat::gate(&base, &current, &ledger), []);
}

#[test]
fn the_grammar_gates_env_narrowing_and_renames_without_a_window() {
    let (base, current) = pair(&case("g14_env_on_invalid_error"));
    let violations = compat::gate(&base, &current, &[]);
    assert!(matches!(violations.as_slice(), [Violation::Unacknowledged { finding, .. }] if finding.code == Code::G14));

    let (base, mut current) = pair(&case("env_renamed_with_window"));
    assert_eq!(compat::gate(&base, &current, &[]), []);
    set(&mut current, Kind::Cli, "/retired", json!([]));
    let violations = compat::gate(&base, &current, &[]);
    assert!(matches!(violations.as_slice(), [Violation::Unacknowledged { finding, .. }] if finding.code == Code::G13));
}

#[test]
fn a_bump_from_a_command_entry_reaches_every_subject_it_lists() {
    let (base, mut current) = pair(&case("g06_value_spec_changed"));
    let ledger = [entry(
        "cli",
        &["package push"],
        EntryRule::Code(Code::G06),
        "/root/commands/0/commands/0/args/3",
    )];
    assert_eq!(
        compat::gate(&base, &current, &ledger),
        [Violation::Version {
            kind: Kind::Cli,
            subject: "package push".to_owned(),
            expected: 2,
            found: 1
        }]
    );
    set(&mut current, Kind::Cli, "/root/commands/0/commands/0/version", json!(2));
    assert_eq!(compat::gate(&base, &current, &ledger), []);
}

#[test]
fn a_walk_that_does_not_descend_reds_the_reader_floor() {
    let mut documents = base_documents();
    documents.reports["unread"] = json!({"a": {"b": 1}});
    let violations = compat::gate(&documents, &documents, &[]);
    let [
        Violation::ReaderFloor {
            kind: Kind::Reports,
            read,
            floor,
        },
    ] = violations.as_slice()
    else {
        panic!("{violations:?}");
    };
    assert!(read < floor);
}

#[test]
fn the_goldens_compare_equal_to_themselves_and_are_read_whole() {
    let golden = |name: &str| json_at(&workspace_root().join(format!("crates/ocx_schema/tests/golden/{name}.json")));
    let documents = Documents {
        reports: golden("reports"),
        errors: golden("errors"),
        cli: golden("cli"),
    };
    for (kind, document) in [
        (Kind::Reports, &documents.reports),
        (Kind::Errors, &documents.errors),
        (Kind::Cli, &documents.cli),
    ] {
        let run = compat::diff(document, document, kind);
        assert_eq!(run.findings, [], "{kind:?}");
        assert_eq!(run.keywords, 2 * raw_keys(document), "{kind:?}");
    }
    assert_eq!(compat::gate(&documents, &documents, &[]), []);
}

#[test]
fn the_ledger_parses_and_refuses_a_misspelt_key() {
    let ledger = compat::parse_ledger(
        r#"
        [[break]]
        document = "reports"
        subjects = ["PushReport"]
        rule = "B02"
        pointer = "/$defs/PushReportRoot/properties/message"
        reason = "gone"
        "#,
    )
    .expect("a well-formed entry");
    assert_eq!(
        ledger,
        [entry("reports", &["PushReport"], EntryRule::Code(Code::B02), MESSAGE)].map(|mut entry| {
            "gone".clone_into(&mut entry.reason);
            entry
        })
    );
    assert!(compat::parse_ledger("[[break]]\ndocument = \"cli\"\nsubject = []\n").is_err());
    assert_eq!(compat::parse_ledger("# nothing yet\n").expect("an empty ledger"), []);
}

fn ledger_rule(rule: &str) -> Result<EntryRule, String> {
    let text = format!(
        "[[break]]\ndocument = \"cli\"\nsubjects = [\"x\"]\nrule = \"{rule}\"\npointer = \"p\"\nreason = \"r\"\n"
    );
    compat::parse_ledger(&text)
        .map(|entries| entries[0].rule)
        .map_err(|error| error.to_string())
}

#[test]
fn a_ledger_entry_naming_an_unknown_rule_fails_at_parse() {
    for rule in ["Z99", "C02", "Doc", ""] {
        let error = ledger_rule(rule).expect_err(rule);
        assert!(error.contains(&format!("unknown ledger rule `{rule}`")), "{error}");
    }
}

#[test]
fn ledger_rules_parse_to_their_entry_rule() {
    assert_eq!(ledger_rule("doc"), Ok(EntryRule::Doc));
    assert_eq!(ledger_rule("semantic"), Ok(EntryRule::Semantic));
    assert_eq!(ledger_rule("B01"), Ok(EntryRule::Code(Code::B01)));
    for code in Code::ALL {
        assert_eq!(ledger_rule(code.id()), Ok(EntryRule::Code(*code)));
    }
}

#[test]
fn every_code_has_a_title() {
    assert!(!Code::ALL.is_empty());
    for code in Code::ALL {
        assert!(!code.title().trim().is_empty(), "{code} has no title");
    }
}

#[test]
fn the_live_goldens_hold_against_the_baseline_and_the_ledger() {
    let contract = workspace_root().join("crates/ocx_schema/contract");
    let text = std::fs::read_to_string(contract.join("ledger.toml")).expect("ledger.toml");
    let ledger = compat::parse_ledger(&text).expect("ledger.toml parses");
    let baseline = contract.join("baseline");
    if !baseline.exists() {
        return;
    }
    let read = |dir: &Path, name: &str| json_at(&dir.join(format!("{name}.json")));
    let goldens = workspace_root().join("crates/ocx_schema/tests/golden");
    let load = |dir: &Path| Documents {
        reports: read(dir, "reports"),
        errors: read(dir, "errors"),
        cli: read(dir, "cli"),
    };
    let violations = compat::gate(&load(&baseline), &load(&goldens), &ledger);
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(
        violations.is_empty(),
        "the goldens break the last release; make each edit below:\n{}",
        report.join("\n\n")
    );
}

#[test]
fn a_wildcard_entry_acknowledges_an_env_finding_and_bumps_no_command() {
    let (base, current) = pair(&case("g14_env_on_invalid_error"));
    let ledger = [entry("cli", &["*"], EntryRule::Code(Code::G14), "/env/0")];
    assert_eq!(compat::gate(&base, &current, &ledger), []);
}

#[test]
fn a_red_names_the_finding_and_the_edit_that_clears_it() {
    let (base, current) = pair(&case("b02_property_removed"));
    let text = compat::gate(&base, &current, &[])[0].to_string();
    for needle in [
        "property removed",
        "[[break]]",
        "document = \"reports\"",
        "subjects = [\"PushReport\"]",
        "rule = \"B02\"",
        &format!("pointer = \"{MESSAGE}\""),
    ] {
        assert!(text.contains(needle), "{needle} missing from:\n{text}");
    }
    let ledger = [entry("reports", &["PushReport"], EntryRule::Code(Code::B02), MESSAGE)];
    assert_eq!(
        compat::gate(&base, &current, &ledger)[0].to_string(),
        "bump reports PushReport to version 2 (it is 1)"
    );
}

/// A report whose payload reaches `leaf` through a chain of `depth` defs named `<prefix><n>`.
fn chained(prefix: &str, depth: usize, leaf: &str) -> Value {
    let mut defs = serde_json::Map::new();
    defs.insert(
        "RRoot".to_owned(),
        json!({"type": "object", "properties": {"next": {"$ref": format!("#/$defs/{prefix}0")}}}),
    );
    for index in 0..depth {
        let body = if index + 1 == depth {
            json!({"type": "object", "properties": {"leaf": {"type": leaf}}})
        } else {
            json!({"type": "object", "properties": {"next": {"$ref": format!("#/$defs/{prefix}{}", index + 1)}}})
        };
        defs.insert(format!("{prefix}{index}"), body);
    }
    json!({"reports": {"R": {"$ref": "#/$defs/RRoot"}}, "$defs": defs})
}

#[test]
fn a_break_below_a_deep_chain_of_renamed_defs_is_still_found() {
    let found = |leaf: &str| {
        let run = compat::diff(&chained("A", 12, "string"), &chained("B", 12, leaf), Kind::Reports);
        triples(&run.findings)
    };
    assert_eq!(found("string"), []);
    assert_eq!(
        found("integer"),
        [("B04", "/$defs/RRoot/properties/next".to_owned(), vec!["R".to_owned()])]
    );
}
