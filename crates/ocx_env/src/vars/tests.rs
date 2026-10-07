// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Registry invariants. Each check is a function over a declaration list, so a synthetic bad list
//! proves it can go red and the live registry proves it is green.

use std::collections::BTreeSet;

use crate::*;

/// Names the secret-name heuristic matches that carry no credential.
const NOT_SECRET_DESPITE_NAME: &[&str] = &["ACTIONS_ID_TOKEN_REQUEST_URL"];

/// The hardening switches: an invalid value refuses instead of falling open.
const HARDENING: &[&str] = &["OCX_FROZEN", "OCX_NO_CONSENT", "OCX_NO_VERIFY", "OCX_OFFLINE"];

fn live() -> Vec<&'static EnvVar> {
    all().collect()
}

fn declaration(name: &'static str, visibility: Visibility, secret: bool, doc: &'static str) -> &'static EnvVar {
    Box::leak(Box::new(EnvVar {
        name,
        doc,
        value: EnvValue::String,
        on_invalid: OnInvalid::Default,
        visibility,
        secret,
        child: Child::Inherit,
        reader: Reader::Ocx,
    }))
}

/// The name with a pattern's `{SLOT}` replaced, so the slot does not fail the charset check.
fn filled(name: &str) -> String {
    match (name.find('{'), name.find('}')) {
        (Some(open), Some(close)) if open < close => format!("{}X{}", &name[..open], &name[close + 1..]),
        _ => name.to_owned(),
    }
}

fn upper_snake(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn duplicate_names(vars: &[&EnvVar], retired: &[Retired]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let retired_names = retired
        .iter()
        .filter(|entry| !matches!(entry.change, Change::ValueRename { .. }))
        .map(|entry| entry.name);
    vars.iter()
        .map(|var| var.name)
        .chain(retired_names)
        .filter(|name| !seen.insert(*name))
        .map(str::to_owned)
        .collect()
}

fn prefix_violations(vars: &[&EnvVar]) -> Vec<String> {
    let mut violations = Vec::new();
    for var in vars {
        let name = filled(var.name);
        let fits = match var.visibility {
            Visibility::Public => name.strip_prefix("OCX_").is_some_and(upper_snake),
            Visibility::Testing => name.strip_prefix("__OCX_TESTING_").is_some_and(upper_snake),
            Visibility::Plumbing => {
                !name.starts_with("__OCX_TESTING_") && name.strip_prefix("__OCX_").is_some_and(upper_snake)
            }
            Visibility::Foreign => true,
        };
        if !fits {
            violations.push(var.name.to_owned());
        }
        if var.visibility != Visibility::Foreign && !is_reserved_ocx_key(&name) {
            violations.push(format!("{} is not reserved", var.name));
        }
    }
    violations
}

fn unmarked_secrets(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| {
            let upper = var.name.to_ascii_uppercase();
            let looks_secret =
                ["TOKEN", "SECRET", "PASSWORD"].iter().any(|word| upper.contains(word)) || upper.ends_with("KEY");
            looks_secret && !var.secret && !NOT_SECRET_DESPITE_NAME.contains(&var.name)
        })
        .map(|var| var.name.to_owned())
        .collect()
}

fn empty_docs(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.doc.trim().is_empty())
        .map(|var| var.name.to_owned())
        .collect()
}

fn bad_patterns(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| {
            let opens = var.name.matches('{').count();
            let closes = var.name.matches('}').count();
            let slot_ok = match (var.name.find('{'), var.name.find('}')) {
                (Some(open), Some(close)) => open < close && upper_snake(&var.name[open + 1..close]),
                _ => false,
            };
            (opens, closes) != (0, 0) && !(opens == 1 && closes == 1 && slot_ok)
        })
        .map(|var| var.name.to_owned())
        .collect()
}

fn scrubbed_but_not_secret(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.child == Child::Scrub && !var.secret)
        .map(|var| var.name.to_owned())
        .collect()
}

#[test]
fn live_registry_is_large_enough_to_be_the_real_one() {
    assert!(live().len() >= 135, "{} declarations", live().len());
}

#[test]
fn names_are_unique_across_declarations_and_retired() {
    assert_eq!(duplicate_names(&live(), RETIRED), Vec::<String>::new());
    let twin = declaration("OCX_LOG_LEVEL", Visibility::Public, false, "twin");
    assert_eq!(duplicate_names(&[&OCX_LOG_LEVEL, twin], &[]), ["OCX_LOG_LEVEL"]);
    let renamed = [Retired {
        name: "OCX_LOG_LEVEL",
        replacement: &OCX_QUIET,
        change: Change::Rename,
        status: Status::Removed,
    }];
    assert_eq!(duplicate_names(&[&OCX_LOG_LEVEL], &renamed), ["OCX_LOG_LEVEL"]);
    let value_rename = [Retired {
        name: "OCX_LAZY_MODE",
        replacement: &OCX_LAZY_MODE,
        change: Change::ValueRename {
            old: "lazy",
            new: "always",
        },
        status: Status::Removed,
    }];
    assert_eq!(duplicate_names(&[&OCX_LAZY_MODE], &value_rename), Vec::<String>::new());
}

#[test]
fn every_name_carries_the_prefix_of_its_visibility() {
    assert_eq!(prefix_violations(&live()), Vec::<String>::new());
    let bad = [
        declaration("OCX_lower", Visibility::Public, false, "d"),
        declaration("NOT_OCX", Visibility::Public, false, "d"),
        declaration("__OCX_HIDDEN", Visibility::Testing, false, "d"),
        declaration("__OCX_TESTING_SEAM", Visibility::Plumbing, false, "d"),
        declaration("OCX_STATE", Visibility::Plumbing, false, "d"),
    ];
    assert_eq!(prefix_violations(&bad).len(), 6, "{:?}", prefix_violations(&bad));
    let good = [
        declaration("OCX_AUTH_{REGISTRY}_TYPE", Visibility::Public, false, "d"),
        declaration("__OCX_TESTING_SEAM", Visibility::Testing, false, "d"),
        declaration("__OCX_STATE", Visibility::Plumbing, false, "d"),
        declaration("ProgramFiles(x86)", Visibility::Foreign, false, "d"),
    ];
    assert_eq!(prefix_violations(&good), Vec::<String>::new());
}

#[test]
fn a_credential_shaped_name_is_secret() {
    assert_eq!(unmarked_secrets(&live()), Vec::<String>::new());
    let bad = [
        declaration("OCX_FORGE_TOKEN", Visibility::Public, false, "d"),
        declaration("CLIENT_SECRET", Visibility::Foreign, false, "d"),
        declaration("db_password", Visibility::Foreign, false, "d"),
        declaration("OCX_API_KEY", Visibility::Public, false, "d"),
    ];
    assert_eq!(unmarked_secrets(&bad).len(), 4);
    let good = [
        declaration("OCX_FORGE_TOKEN", Visibility::Public, true, "d"),
        declaration("OCX_KEYRING_DIR", Visibility::Public, false, "d"),
    ];
    assert_eq!(unmarked_secrets(&good), Vec::<String>::new());
}

#[test]
fn every_declaration_is_documented() {
    assert_eq!(empty_docs(&live()), Vec::<String>::new());
    let bad = [
        declaration("OCX_A", Visibility::Public, false, ""),
        declaration("OCX_B", Visibility::Public, false, " \n"),
    ];
    assert_eq!(empty_docs(&bad), ["OCX_A", "OCX_B"]);
}

#[test]
fn a_pattern_carries_exactly_one_slot() {
    assert_eq!(bad_patterns(&live()), Vec::<String>::new());
    let bad = [
        declaration("OCX_{A}_{B}", Visibility::Public, false, "d"),
        declaration("OCX_{}_X", Visibility::Public, false, "d"),
        declaration("OCX_{A_X", Visibility::Public, false, "d"),
        declaration("OCX_}A{_X", Visibility::Public, false, "d"),
    ];
    assert_eq!(bad_patterns(&bad).len(), 4);
    assert!(
        live().iter().any(|var| var.is_pattern()),
        "the pattern check read no pattern"
    );
}

#[test]
fn only_a_secret_is_scrubbed() {
    assert_eq!(scrubbed_but_not_secret(&live()), Vec::<String>::new());
    let leaky: &'static EnvVar = Box::leak(Box::new(EnvVar {
        child: Child::Scrub,
        ..*declaration("OCX_X", Visibility::Public, false, "d")
    }));
    assert_eq!(scrubbed_but_not_secret(&[leaky]), ["OCX_X"]);
}

#[test]
fn the_hardening_set_refuses_invalid_values() {
    let refusing: Vec<&str> = live()
        .into_iter()
        .filter(|var| var.on_invalid == OnInvalid::Error)
        .map(|var| var.name)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(refusing, HARDENING);
}

#[test]
fn ceiling_path_is_public_configuration() {
    assert_eq!(OCX_CEILING_PATH.visibility, Visibility::Public);
}

#[test]
fn secret_declarations_are_secret_vars() {
    let _: &SecretVar = &OCX_IDENTITY_TOKEN;
    let _: &SecretVar = &OCX_AUTH_TOKEN;
    assert!(OCX_IDENTITY_TOKEN.declaration().secret);
    assert!(!OCX_LOG_LEVEL.secret);
}
