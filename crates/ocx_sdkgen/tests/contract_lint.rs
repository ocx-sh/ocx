// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The published documents hold to the representation subset: every finding is exempted (permanent, cited) or
//! waived (a ratchet that only shrinks), and the lint provably read every node it claims to have judged.
//!
//! Reads the goldens and `crates/ocx_schema/contract/` as data, so it never rebuilds the generator.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ocx_sdkgen::lint::{self, Entry, Finding, Kind, Reconciled, Rule, Visit};
use serde_json::{Value, json};

/// One waiver file per area, so parallel work packages edit disjoint files.
const AREAS: &[&str] = &[
    "reports-g1",
    "reports-g2",
    "reports-g3",
    "reports-g4",
    "reports-g5",
    "errors",
    "cli",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn json_at(path: &Path) -> Value {
    serde_json::from_str(&read(path)).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn golden(name: &str) -> Value {
    json_at(&workspace_root().join(format!("crates/ocx_schema/tests/golden/{name}.json")))
}

fn fixture(name: &str) -> Value {
    json_at(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name))
}

fn codes(findings: &[Finding]) -> BTreeSet<Rule> {
    findings.iter().map(|finding| finding.rule).collect()
}

/// `exemptions.toml` and every `waivers/<area>.toml`; git drops an emptied `waivers/`, so absent reads as none.
fn contract_entries() -> (Vec<Entry>, Vec<Entry>) {
    let contract = workspace_root().join("crates/ocx_schema/contract");
    let exemptions = lint::parse_exemptions(&read(&contract.join("exemptions.toml"))).expect("exemptions.toml parses");
    assert!(!exemptions.is_empty(), "read no exemption");
    let mut waivers = Vec::new();
    let waiver_dir = contract.join("waivers");
    let listing = match std::fs::read_dir(&waiver_dir) {
        Ok(listing) => listing.collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("{}: {error}", waiver_dir.display()),
    };
    for entry in listing {
        let path = entry.expect("readable directory entry").path();
        let area = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned();
        assert!(
            path.extension().is_some_and(|ext| ext == "toml") && AREAS.contains(&area.as_str()),
            "{}: not one of the waiver areas {AREAS:?}",
            path.display()
        );
        waivers.extend(lint::parse_waivers(&read(&path)).unwrap_or_else(|error| panic!("{}: {error}", path.display())));
    }
    (exemptions, waivers)
}

/// Every finding over the published documents, with its document name.
fn live_findings() -> Vec<(&'static str, Finding)> {
    let reports = golden("reports");
    let cli = golden("cli");
    let mut findings = Vec::new();
    for (name, document, kind) in [
        ("reports", &reports, Kind::Reports),
        ("errors", &golden("errors"), Kind::Errors),
        ("cli", &cli, Kind::Cli),
        ("metadata", &golden("metadata"), Kind::Input),
        ("patch", &golden("patch"), Kind::Input),
    ] {
        findings.extend(
            lint::run(document, kind)
                .findings
                .into_iter()
                .map(|finding| (name, finding)),
        );
    }
    findings.extend(lint::cross(&cli, &reports).into_iter().map(|finding| ("cli", finding)));
    findings
}

/// The waiver line that would cover `finding`, for a failure message a reader can act on.
fn as_toml(document: &str, finding: &Finding) -> String {
    format!(
        "{{ rule = \"{}\", document = \"{document}\", pointer = {:?}, reason = {:?} }}  # {}",
        finding.rule,
        finding.pointer,
        finding.rule.title(),
        finding.message
    )
}

fn describe(reconciled: &Reconciled) -> String {
    let mut lines = Vec::new();
    for (document, finding) in &reconciled.unmatched {
        lines.push(format!("unwaived: {}", as_toml(document, finding)));
    }
    for entry in &reconciled.stale_exemptions {
        lines.push(format!("stale exemption: {entry:?}"));
    }
    for entry in &reconciled.stale_waivers {
        lines.push(format!("stale waiver: {entry:?}"));
    }
    for entry in &reconciled.missing_adr {
        lines.push(format!("exemption without adr: {entry:?}"));
    }
    lines.join("\n")
}

#[test]
fn the_published_documents_hold_to_the_contract() {
    let findings = live_findings();
    let (exemptions, waivers) = contract_entries();
    let reconciled = lint::reconcile(&findings, &exemptions, &waivers);
    assert!(
        reconciled.is_clean(),
        "the contract lint is red ({} findings read):\n{}",
        findings.len(),
        describe(&reconciled)
    );
}

#[test]
fn no_waiver_remains() {
    let (_, waivers) = contract_entries();
    assert!(
        waivers.is_empty(),
        "the phase-1 exit leaves `contract/waivers/` empty; fix the finding instead of waiving it: {waivers:?}"
    );
}

// ---------------------------------------------------------------------------
// Reader floor: counted here without the walker's rule logic.
// ---------------------------------------------------------------------------

/// Keywords whose value is data, never a schema.
const DATA_KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$comment",
    "title",
    "description",
    "type",
    "required",
    "const",
    "enum",
    "default",
    "examples",
    "format",
    "pattern",
    "minimum",
    "maximum",
    "maxLength",
    "uniqueItems",
    "x-ocx-enum",
    "x-ocx-unknown-variant",
    "x-ocx-opaque",
];

/// Keywords whose value maps names to schemas: the names are not keywords.
const NAME_MAPS: &[&str] = &["properties", "$defs", "reports"];

fn raw_keywords(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(key, child)| {
                1 + if NAME_MAPS.contains(&key.as_str()) {
                    child
                        .as_object()
                        .map_or(0, |names| names.values().map(raw_keywords).sum())
                } else if DATA_KEYWORDS.contains(&key.as_str()) {
                    0
                } else {
                    raw_keywords(child)
                }
            })
            .sum(),
        Value::Array(items) => items.iter().map(raw_keywords).sum(),
        _ => 0,
    }
}

fn raw_property_count(value: &Value) -> usize {
    match value {
        Value::Object(map) => {
            let own = map
                .get("properties")
                .and_then(Value::as_object)
                .map_or(0, |properties| properties.len());
            own + map.values().map(raw_property_count).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(raw_property_count).sum(),
        _ => 0,
    }
}

/// Objects carrying every one of `keys`, anywhere in the document.
fn raw_objects_with(value: &Value, keys: &[&str]) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(keys.iter().all(|key| map.contains_key(*key)))
                + map.values().map(|child| raw_objects_with(child, keys)).sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|item| raw_objects_with(item, keys)).sum(),
        _ => 0,
    }
}

/// The independent count of what a run over `document` must have read.
fn independent(document: &Value, kind: Kind) -> Visit {
    match kind {
        Kind::Cli => Visit {
            commands: raw_objects_with(document, &["path", "commands"]),
            args: raw_objects_with(document, &["num_args", "help"]),
            ..Visit::default()
        },
        _ => Visit {
            roots: match kind {
                Kind::Reports => document
                    .get("reports")
                    .and_then(Value::as_object)
                    .map_or(0, |roots| roots.len()),
                _ => 1,
            },
            defs: document
                .get("$defs")
                .and_then(Value::as_object)
                .map_or(0, |defs| defs.len()),
            properties: raw_property_count(document),
            keywords: raw_keywords(document),
            ..Visit::default()
        },
    }
}

/// Every way `visit` falls short of `floor`; empty is green.
fn floor_violations(visit: &Visit, floor: &Visit, kind: Kind) -> Vec<String> {
    let mut violations = Vec::new();
    let mut at_least = |what: &str, read: usize, expected: usize| {
        if read < expected {
            violations.push(format!("{what}: read {read}, independent count {expected}"));
        }
    };
    at_least("commands", visit.commands, floor.commands);
    at_least("args", visit.args, floor.args);
    at_least("properties", visit.properties, floor.properties);
    at_least("defs", visit.defs, floor.defs);
    if kind != Kind::Cli {
        if visit.roots != floor.roots {
            violations.push(format!("roots: read {}, registry holds {}", visit.roots, floor.roots));
        }
        if visit.keywords != floor.keywords {
            violations.push(format!(
                "keywords: read {}, independent count {}",
                visit.keywords, floor.keywords
            ));
        }
    }
    violations
}

#[test]
fn the_lint_reads_every_node_of_every_document() {
    for (name, kind, minimum) in [
        ("reports", Kind::Reports, 50),
        ("errors", Kind::Errors, 1),
        ("cli", Kind::Cli, 50),
        ("metadata", Kind::Input, 1),
        ("patch", Kind::Input, 1),
    ] {
        let document = golden(name);
        let floor = independent(&document, kind);
        let substantial = if kind == Kind::Cli {
            floor.commands.min(floor.args)
        } else {
            floor.properties
        };
        assert!(
            substantial >= minimum,
            "{name}: the independent count read only {substantial} nodes"
        );
        let visit = lint::run(&document, kind).visit;
        let violations = floor_violations(&visit, &floor, kind);
        assert!(violations.is_empty(), "{name}: {violations:?}");
    }
}

#[test]
fn a_reader_below_the_independent_count_reds() {
    let document = golden("reports");
    let floor = independent(&document, Kind::Reports);
    let visit = lint::run(&document, Kind::Reports).visit;
    assert!(floor_violations(&visit, &floor, Kind::Reports).is_empty());
    for short in [
        Visit {
            properties: visit.properties - 1,
            ..visit
        },
        Visit {
            keywords: visit.keywords - 1,
            ..visit
        },
        Visit {
            roots: visit.roots - 1,
            ..visit
        },
    ] {
        assert_eq!(floor_violations(&short, &floor, Kind::Reports).len(), 1, "{short:?}");
    }
    let cli = golden("cli");
    let floor = independent(&cli, Kind::Cli);
    let visit = lint::run(&cli, Kind::Cli).visit;
    assert!(floor_violations(&visit, &floor, Kind::Cli).is_empty());
    assert_eq!(
        floor_violations(
            &Visit {
                args: visit.args - 1,
                ..visit
            },
            &floor,
            Kind::Cli
        )
        .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Red proofs.
// ---------------------------------------------------------------------------

#[test]
fn the_red_fixtures_fire_exactly_every_rule() {
    let reports = fixture("contract_lint_red.json");
    let cli = fixture("contract_lint_red_cli.json");
    let reports_findings = lint::run(&reports, Kind::Reports).findings;
    let errors_findings = lint::run(&fixture("contract_lint_red_errors.json"), Kind::Errors).findings;
    let reports_codes = codes(&reports_findings);
    let errors_codes = codes(&errors_findings);
    let mut cli_findings = lint::run(&cli, Kind::Cli).findings;
    cli_findings.extend(lint::cross(&cli, &reports));

    let mut populated = cli.clone();
    let default = populated
        .pointer_mut("/root/args/0/default")
        .expect("the red cli twin's first root arg has a default");
    assert_eq!(default, &json!(["false"]));
    *default = json!(["true"]);
    assert_eq!(
        populated.pointer("/root/args/0/default"),
        Some(&json!(["true"])),
        "the mutation landed"
    );
    cli_findings.extend(lint::defaults_drift(&cli, &populated));
    let cli_codes = codes(&cli_findings);

    let expected = |prefix: char, except: &[Rule]| -> BTreeSet<Rule> {
        Rule::ALL
            .iter()
            .copied()
            .filter(|rule| rule.id().starts_with(prefix) && !except.contains(rule))
            .collect()
    };
    assert_eq!(reports_codes, expected('L', &[Rule::L16]), "reports red fixture");
    assert_eq!(errors_codes, BTreeSet::from([Rule::L16]), "errors red fixture");
    assert_eq!(cli_codes, expected('C', &[]), "cli red twin");

    let all: BTreeSet<Rule> = reports_codes
        .iter()
        .chain(&errors_codes)
        .chain(&cli_codes)
        .copied()
        .collect();
    let declared: BTreeSet<Rule> = Rule::ALL.iter().copied().collect();
    assert_eq!(all, declared);

    // Each rule fires where its fixture node sits, once: a finding moved or doubled reds.
    assert_eq!(
        sorted_pairs(&reports_findings),
        [
            ("L01", "/$defs/Thing/properties/maybe/anyOf"),
            ("L02", "/$defs/Thing/properties/either/type"),
            ("L03", "/$defs/Thing/properties/maybe/anyOf/1/type"),
            ("L04", "/$defs/Kinds/enum"),
            ("L05", "/$defs/Thing/properties/variant/oneOf"),
            ("L06", "/$defs/Thing/properties/camelCase"),
            ("L07", "/$defs/Thing/properties/mode/x-ocx-enum/0/value"),
            ("L08", "/$defs/Thing/properties/undocumented"),
            ("L09", "/reports/Listing"),
            ("L10", "/$defs/Listing/properties/error"),
            ("L11", "/$defs/Thing/properties/created_at"),
            ("L12", "/$defs/Thing/properties/hash/pattern"),
            ("L13", "/$defs/Listing/properties/entries"),
            ("L14", "/$defs/Listing/properties/nested/$ref"),
            ("L15", "/$defs/Thing/properties/extra/additionalProperties"),
            ("L17", "/$defs/Thing/properties/merged/allOf"),
            ("L18", "/$defs/Thing/properties/dry_run"),
            ("L18", "/$defs/Thing/properties/status"),
        ]
    );
    assert_eq!(
        sorted_pairs(&errors_findings),
        [("L16", "/$defs/Context/properties/source")]
    );
    assert_eq!(
        sorted_pairs(&cli_findings),
        [
            ("C01", "ocx second --quiet"),
            ("C02", "-x {group,offline}"),
            ("C03", "ocx second --badFlag"),
            ("C03", "ocx second --badFlag=Bad-Choice"),
            ("C04", r#"--group {{"type":"path"},{"type":"string"}}"#),
            ("C05", "OCX_NETWORK_OFF"),
            ("C06", "ocx first --out-file"),
            ("C07", "ocx --offline"),
            ("C08", "ocx first --token"),
            ("C09", "ocx first -> Missing"),
            ("C09", "ocx second"),
        ]
    );
}

fn sorted_pairs(findings: &[Finding]) -> Vec<(&'static str, &str)> {
    let mut pairs: Vec<_> = findings
        .iter()
        .map(|finding| (finding.rule.id(), finding.pointer.as_str()))
        .collect();
    pairs.sort_unstable();
    pairs
}

/// A minimal reports document whose one root is a clean wrapper around `payload`'s properties.
fn reports_with(payload: Value) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "reports": { "ThingRoot": { "$ref": "#/$defs/ThingRoot" } },
        "$defs": {
            "ThingRoot": {
                "description": "Wrapper.",
                "type": "object",
                "properties": {
                    "schema_version": { "description": "Version.", "type": "integer", "const": 1 },
                    "value": payload.clone()
                },
                "required": ["schema_version", "value"]
            },
            "Thing": {
                "description": "Payload.",
                "type": "object",
                "properties": { "value": payload },
                "required": ["value"]
            }
        }
    })
}

#[test]
fn a_clean_wrapper_is_clean() {
    let document = reports_with(json!({ "description": "A value.", "type": "string" }));
    assert_eq!(lint::run(&document, Kind::Reports).findings, Vec::new());
}

fn pairs(findings: &[Finding]) -> BTreeSet<(&'static str, &str)> {
    findings
        .iter()
        .map(|finding| (finding.rule.id(), finding.pointer.as_str()))
        .collect()
}

/// Each off-allowlist keyword is its own finding, so a waiver for one cannot mask a second at the same node.
#[test]
fn an_unlisted_keyword_is_l17() {
    let document = reports_with(json!({ "description": "A value.", "allOf": [{ "type": "string" }], "default": "x" }));
    let findings = lint::run(&document, Kind::Reports).findings;
    assert_eq!(
        pairs(&findings),
        BTreeSet::from([
            ("L17", "/$defs/Thing/properties/value/allOf"),
            ("L17", "/$defs/Thing/properties/value/default"),
            ("L17", "/$defs/ThingRoot/properties/value/allOf"),
            ("L17", "/$defs/ThingRoot/properties/value/default"),
        ]),
        "{findings:?}"
    );
}

/// `reports_with` a string payload, then `mutate`; the mutation must land before the run is trusted.
fn l09_after(mutate: impl FnOnce(&mut Value)) -> Vec<Finding> {
    let clean = reports_with(json!({ "description": "A value.", "type": "string" }));
    let mut document = clean.clone();
    mutate(&mut document);
    assert_ne!(document, clean, "the mutation landed");
    lint::run(&document, Kind::Reports).findings
}

#[test]
fn a_wrapper_without_a_pinned_version_is_l09() {
    let findings = l09_after(|document| {
        document
            .pointer_mut("/$defs/ThingRoot/properties/schema_version")
            .and_then(Value::as_object_mut)
            .expect("the wrapper has schema_version")
            .remove("const");
    });
    assert_eq!(
        pairs(&findings),
        BTreeSet::from([("L09", "/reports/ThingRoot")]),
        "{findings:?}"
    );
}

#[test]
fn a_wrapper_not_led_by_schema_version_is_l09() {
    let findings = l09_after(|document| {
        *document.pointer_mut("/$defs/ThingRoot/required").expect("required") = json!(["value", "schema_version"]);
    });
    assert_eq!(
        pairs(&findings),
        BTreeSet::from([("L09", "/reports/ThingRoot")]),
        "{findings:?}"
    );
}

#[test]
fn a_wrapper_with_a_property_beyond_its_payload_is_l09() {
    let findings = l09_after(|document| {
        document
            .pointer_mut("/$defs/ThingRoot/properties")
            .and_then(Value::as_object_mut)
            .expect("the wrapper has properties")
            .insert("extra".to_owned(), json!({ "description": "Extra.", "type": "string" }));
    });
    assert_eq!(
        pairs(&findings),
        BTreeSet::from([("L09", "/reports/ThingRoot")]),
        "{findings:?}"
    );
}

#[test]
fn a_payload_carrying_schema_version_is_l09() {
    let findings = l09_after(|document| {
        document
            .pointer_mut("/$defs/Thing/properties")
            .and_then(Value::as_object_mut)
            .expect("the payload has properties")
            .insert(
                "schema_version".to_owned(),
                json!({ "description": "Version.", "type": "integer", "const": 1 }),
            );
    });
    assert_eq!(
        pairs(&findings),
        BTreeSet::from([("L09", "/$defs/Thing/properties/schema_version")]),
        "{findings:?}"
    );
}

fn union_with(unknown_arm: Value) -> Value {
    reports_with(json!({
        "description": "A union.",
        "oneOf": [
            {
                "type": "object",
                "properties": { "type": { "type": "string", "const": "known" } },
                "required": ["type"]
            },
            unknown_arm
        ]
    }))
}

#[test]
fn an_unknown_arm_without_required_type_is_l05() {
    let unknown = json!({
        "x-ocx-unknown-variant": true,
        "type": "object",
        "properties": { "type": { "type": "string", "not": { "enum": ["known"] } } },
        "required": ["type"]
    });
    assert_eq!(
        lint::run(&union_with(unknown.clone()), Kind::Reports).findings,
        Vec::new()
    );

    let mut untagged = unknown;
    untagged.as_object_mut().expect("an object").remove("required");
    assert!(untagged.get("required").is_none(), "the mutation landed");
    let findings = lint::run(&union_with(untagged), Kind::Reports).findings;
    assert_eq!(codes(&findings), BTreeSet::from([Rule::L05]), "{findings:?}");
}

#[test]
fn an_unknown_arm_that_does_not_exclude_every_known_tag_is_l05() {
    for excluded in [json!([]), json!(["other"]), json!(["known", "other"])] {
        let unknown = json!({
            "x-ocx-unknown-variant": true,
            "type": "object",
            "properties": { "type": { "type": "string", "not": { "enum": excluded } } },
            "required": ["type"]
        });
        let findings = lint::run(&union_with(unknown), Kind::Reports).findings;
        assert_eq!(
            pairs(&findings),
            BTreeSet::from([
                ("L05", "/$defs/Thing/properties/value/oneOf/1"),
                ("L05", "/$defs/ThingRoot/properties/value/oneOf/1"),
            ]),
            "{excluded}: {findings:?}"
        );
    }
}

#[test]
fn a_new_violation_reds() {
    let mut reports = golden("reports");
    let defs = reports
        .get_mut("$defs")
        .and_then(Value::as_object_mut)
        .expect("reports has $defs");
    assert!(!defs.contains_key("ZzFresh"));
    defs.insert(
        "ZzFresh".to_owned(),
        json!({ "description": "New.", "type": "object", "properties": { "fresh": { "type": "string" } } }),
    );
    let findings: Vec<(&str, Finding)> = lint::run(&reports, Kind::Reports)
        .findings
        .into_iter()
        .map(|finding| ("reports", finding))
        .collect();
    let (exemptions, waivers) = contract_entries();
    let reconciled = lint::reconcile(&findings, &exemptions, &waivers);
    let unmatched: Vec<(&str, &str)> = reconciled
        .unmatched
        .iter()
        .map(|(_, finding)| (finding.rule.id(), finding.pointer.as_str()))
        .collect();
    assert_eq!(unmatched, vec![("L08", "/$defs/ZzFresh/properties/fresh")]);
}

fn entry(rule: Rule, document: &str, pointer: &str, adr: Option<&str>) -> Entry {
    Entry {
        rule,
        document: document.to_owned(),
        pointer: pointer.to_owned(),
        reason: "test".to_owned(),
        adr: adr.map(str::to_owned),
    }
}

fn one_finding() -> Vec<(&'static str, Finding)> {
    vec![(
        "reports",
        Finding {
            rule: Rule::L08,
            pointer: "/$defs/A/properties/b".to_owned(),
            message: "no description".to_owned(),
        },
    )]
}

#[test]
fn reconcile_greens_on_an_exact_cover() {
    let waiver = entry(Rule::L08, "reports", "/$defs/A/properties/b", None);
    assert!(lint::reconcile(&one_finding(), &[], &[waiver]).is_clean());
    let exemption = entry(Rule::L08, "reports", "/$defs/A/properties/b", Some("adr_x.md"));
    assert!(lint::reconcile(&one_finding(), &[exemption], &[]).is_clean());
}

#[test]
fn a_stale_waiver_reds() {
    let waivers = [
        entry(Rule::L08, "reports", "/$defs/A/properties/b", None),
        entry(Rule::L08, "errors", "/$defs/A/properties/b", None),
    ];
    let reconciled = lint::reconcile(&one_finding(), &[], &waivers);
    assert!(!reconciled.is_clean());
    assert_eq!(reconciled.stale_waivers, vec![waivers[1].clone()]);
    assert!(reconciled.unmatched.is_empty());
}

#[test]
fn a_stale_exemption_reds() {
    let exemptions = [
        entry(Rule::L08, "reports", "/$defs/A/properties/b", Some("adr_x.md")),
        entry(Rule::L06, "reports", "/$defs/A/properties/b", Some("adr_x.md")),
    ];
    let reconciled = lint::reconcile(&one_finding(), &exemptions, &[]);
    assert!(!reconciled.is_clean());
    assert_eq!(reconciled.stale_exemptions, vec![exemptions[1].clone()]);
}

#[test]
fn an_exemption_without_adr_reds() {
    for adr in [None, Some(""), Some("  ")] {
        let exemption = entry(Rule::L08, "reports", "/$defs/A/properties/b", adr);
        let reconciled = lint::reconcile(&one_finding(), std::slice::from_ref(&exemption), &[]);
        assert!(!reconciled.is_clean(), "{adr:?}");
        assert_eq!(reconciled.missing_adr, vec![exemption]);
    }
}

#[test]
fn an_unmatched_finding_reds() {
    let reconciled = lint::reconcile(&one_finding(), &[], &[]);
    assert!(!reconciled.is_clean());
    assert_eq!(reconciled.unmatched.len(), 1);
}

/// A non-hidden arg whose help went blank is a missing-help finding, and no waiver covers it.
#[test]
fn a_blanked_arg_help_is_c01() {
    let mut cli = golden("cli");
    let args = cli
        .pointer_mut("/root/args")
        .and_then(Value::as_array_mut)
        .expect("the root command has args");
    let arg = args
        .iter_mut()
        .find(|arg| arg["long"] == "offline")
        .expect("the root command carries --offline");
    assert_eq!(arg["hidden"], false);
    assert!(
        arg["help"].as_str().is_some_and(|help| !help.is_empty()),
        "--offline has help today"
    );
    arg["help"] = json!("");
    assert_eq!(arg["help"], "", "the mutation landed");

    let findings: Vec<(&str, Finding)> = lint::run(&cli, Kind::Cli)
        .findings
        .into_iter()
        .map(|finding| ("cli", finding))
        .collect();
    let (exemptions, waivers) = contract_entries();
    let reconciled = lint::reconcile(&findings, &exemptions, &waivers);
    let unmatched: Vec<(&str, &str)> = reconciled
        .unmatched
        .iter()
        .map(|(_, finding)| (finding.rule.id(), finding.pointer.as_str()))
        .collect();
    assert_eq!(unmatched, vec![("C01", "ocx --offline")]);
}

/// A short or long flag that gains a further meaning or value spec reds past the entry that covered the old set.
#[test]
fn a_grown_meaning_or_spec_set_reds() {
    let mut cli = golden("cli");
    let args = cli
        .pointer_mut("/root/args")
        .and_then(Value::as_array_mut)
        .expect("the root command has args");
    let before = args.len();
    let template = args.first().cloned().expect("the root command has an arg");
    let with = |patch: Value| {
        let mut arg = template.clone();
        for (key, value) in patch.as_object().expect("an object patch") {
            arg[key] = value.clone();
        }
        arg
    };
    args.push(with(
        json!({ "id": "gizmo", "long": "gizmo", "short": "g", "help": "Gizmo.", "env": null }),
    ));
    args.push(with(json!({ "id": "tags", "long": "tags", "short": null, "value": { "type": "path" }, "help": "Tags.", "env": null })));
    assert_eq!(args.len(), before + 2, "the mutation landed");

    let findings: Vec<(&str, Finding)> = lint::run(&cli, Kind::Cli)
        .findings
        .into_iter()
        .map(|finding| ("cli", finding))
        .collect();
    let (exemptions, waivers) = contract_entries();
    let reconciled = lint::reconcile(&findings, &exemptions, &waivers);
    let unmatched: BTreeSet<(&str, &str)> = reconciled
        .unmatched
        .iter()
        .map(|(_, finding)| (finding.rule.id(), finding.pointer.as_str()))
        .collect();
    assert_eq!(
        unmatched,
        BTreeSet::from([
            ("C02", "-g {gizmo,global,group}"),
            ("C04", r#"--tags {{"type":"path"},{"type":"string"}}"#),
        ])
    );
    let stale: BTreeSet<&str> = reconciled
        .stale_exemptions
        .iter()
        .chain(&reconciled.stale_waivers)
        .filter(|entry| entry.document == "cli")
        .map(|entry| entry.pointer.as_str())
        .collect();
    assert_eq!(stale, BTreeSet::from(["-g {global,group}"]));
}

#[test]
fn an_entry_naming_an_unknown_rule_fails_at_parse() {
    let table = |rule: &str| format!("rule = \"{rule}\"\ndocument = \"cli\"\npointer = \"-g\"\nreason = \"r\"\n");
    for rule in ["Z99", "B02", "doc"] {
        let exemption = format!("[[exemption]]\n{}adr = \"a.md\"\n", table(rule));
        let error = lint::parse_exemptions(&exemption).expect_err(rule).to_string();
        assert!(error.contains(&format!("unknown rule `{rule}`")), "{error}");
        let error = lint::parse_waivers(&format!("[[waiver]]\n{}", table(rule)))
            .expect_err(rule)
            .to_string();
        assert!(error.contains(&format!("unknown rule `{rule}`")), "{error}");
    }
    let parsed =
        lint::parse_exemptions(&format!("[[exemption]]\n{}adr = \"a.md\"\n", table("C02"))).expect("C02 parses");
    assert_eq!(parsed[0].rule, Rule::C02);
}

#[test]
fn every_rule_has_a_title() {
    for rule in Rule::ALL {
        assert!(!rule.title().trim().is_empty(), "{rule} has no title");
    }
}

#[test]
fn each_kind_runs_exactly_its_rules() {
    use Rule::*;
    let schema_rules = [
        L01, L02, L03, L04, L05, L06, L07, L08, L09, L10, L11, L12, L13, L14, L15, L16, L17, L18,
    ];
    let command_rules = [C01, C02, C03, C04, C05, C06, C07, C08, C09];
    let every_rule: BTreeSet<Rule> = schema_rules.into_iter().chain(command_rules).collect();
    assert_eq!(
        every_rule,
        Rule::ALL.iter().copied().collect(),
        "a rule is missing from the table below"
    );
    let applying =
        |kind: Kind| -> BTreeSet<Rule> { Rule::ALL.iter().copied().filter(|rule| rule.applies(kind)).collect() };
    let without = |dropped: &[Rule]| -> BTreeSet<Rule> {
        every_rule
            .iter()
            .copied()
            .filter(|rule| !dropped.contains(rule))
            .collect()
    };
    assert_eq!(applying(Kind::Input), BTreeSet::from([L06]));
    assert_eq!(applying(Kind::Errors), without(&[L09, L10, L13, L14, L18]));
    assert_eq!(applying(Kind::Reports), without(&[L16]));
    assert_eq!(applying(Kind::Cli), BTreeSet::new());
}
