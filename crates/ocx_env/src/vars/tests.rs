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
        accepts_reference: false,
        propagation: Propagation::UserInput,
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
        // A value rename shares its variable's name; a withdrawn row keeps its declaration on purpose.
        .filter(|entry| !matches!(entry.change, Change::ValueRename { .. } | Change::Withdrawn))
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

fn credential_but_not_secret(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.propagation == Propagation::Credential && !var.secret)
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
        replacement: Replacement::Env(&OCX_QUIET),
        change: Change::Rename,
        status: Status::Removed,
    }];
    assert_eq!(duplicate_names(&[&OCX_LOG_LEVEL], &renamed), ["OCX_LOG_LEVEL"]);
    let value_rename = [Retired {
        name: "OCX_LAZY_MODE",
        replacement: Replacement::Env(&OCX_LAZY_MODE),
        change: Change::ValueRename {
            old: "lazy",
            new: "always",
        },
        status: Status::Removed,
    }];
    assert_eq!(duplicate_names(&[&OCX_LAZY_MODE], &value_rename), Vec::<String>::new());
    let withdrawn = [Retired {
        name: "OCX_LAZY_MODE",
        replacement: Replacement::Flag("--lazy"),
        change: Change::Withdrawn,
        status: Status::Removed,
    }];
    assert_eq!(duplicate_names(&[&OCX_LAZY_MODE], &withdrawn), Vec::<String>::new());
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
    assert_eq!(credential_but_not_secret(&live()), Vec::<String>::new());
    let leaky: &'static EnvVar = Box::leak(Box::new(EnvVar {
        propagation: Propagation::Credential,
        ..*declaration("OCX_X", Visibility::Public, false, "d")
    }));
    assert_eq!(credential_but_not_secret(&[leaky]), ["OCX_X"]);
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

/// Reds on: a `secret` declaration filed under a class that is neither Credential nor SecretInput.
fn secrets_outside_secret_classes(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.secret && !matches!(var.propagation, Propagation::Credential | Propagation::SecretInput))
        .map(|var| var.name.to_owned())
        .collect()
}

#[test]
fn every_secret_declaration_is_credential_or_secret_input() {
    assert_eq!(secrets_outside_secret_classes(&live()), Vec::<String>::new());
    let bad = declaration("OCX_SYNTHETIC_TOKEN", Visibility::Public, true, "x");
    assert_eq!(secrets_outside_secret_classes(&[bad]), ["OCX_SYNTHETIC_TOKEN"]);
}

#[test]
fn credential_class_is_exactly_the_scrubbed_secrets() {
    let credentials: BTreeSet<_> = keys_with(Propagation::Credential).collect();
    let scrubbed: BTreeSet<_> = credential_keys().collect();
    assert_eq!(credentials, scrubbed);
    assert_eq!(credentials.len(), 5);
}

#[test]
fn the_singleton_classes_hold_the_variables_the_adr_names() {
    let keys = |class| keys_with(class).collect::<Vec<_>>();
    assert_eq!(keys(Propagation::State), ["__OCX_ENV_STATE"]);
    assert_eq!(keys(Propagation::BinaryPin), ["OCX_BINARY_PIN"]);
    assert_eq!(keys(Propagation::SetupWritten), ["OCX_HOME"]);
    assert_eq!(
        keys(Propagation::HookLocal),
        ["__OCX_ENV_PWD", "__OCX_ENV_STOOD_DOWN", "__OCX_CREDENTIAL_HELPER"]
    );
    let secret_inputs: BTreeSet<_> = keys_with(Propagation::SecretInput).collect();
    assert_eq!(
        secret_inputs,
        BTreeSet::from([
            "OCX_ANNOUNCE_TOKEN",
            "OCX_AUTH_{REGISTRY}_TOKEN",
            "ACTIONS_ID_TOKEN_REQUEST_TOKEN",
            "CI_JOB_TOKEN",
            "SIGSTORE_ID_TOKEN",
            "CIRCLE_OIDC_TOKEN_V2",
            "OTEL_EXPORTER_OTLP_HEADERS",
            "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
        ])
    );
}

#[test]
fn the_stood_down_copy_is_declared_hook_local_plumbing() {
    assert_eq!(__OCX_ENV_STOOD_DOWN.propagation, Propagation::HookLocal);
    assert_eq!(__OCX_ENV_STOOD_DOWN.visibility, Visibility::Plumbing);
    assert!(__OCX_ENV_STOOD_DOWN.doc.contains("never transported"));
}

// ── Reference-capable declarations ──

/// The non-secret declarations that accept a reference: a user name is not secret, but it may live in a file.
const REFERENCE_USER_NAMES: &[&str] = &["OCX_ANNOUNCE_GIT_USERNAME", "OCX_AUTH_{REGISTRY}_USER"];

fn with_reference(var: &EnvVar, accepts_reference: bool) -> &'static EnvVar {
    Box::leak(Box::new(EnvVar {
        accepts_reference,
        ..*declaration(var.name, var.visibility, var.secret, "d")
    }))
}

/// Reds on: a `secret` + `Public` declaration that does not accept a reference.
fn secrets_without_reference(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.secret && var.visibility == Visibility::Public && !var.accepts_reference)
        .map(|var| var.name.to_owned())
        .collect()
}

/// Reds on: a `Foreign`, `Plumbing` or `Testing` declaration that accepts a reference.
fn references_outside_public(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| var.accepts_reference && var.visibility != Visibility::Public)
        .map(|var| var.name.to_owned())
        .collect()
}

/// Reds on: a path-valued declaration that accepts a reference, which would read the file named by the path.
fn references_on_paths(vars: &[&EnvVar]) -> Vec<String> {
    vars.iter()
        .filter(|var| {
            var.accepts_reference && matches!(var.value, EnvValue::Path | EnvValue::PathList | EnvValue::PathOrPem)
        })
        .map(|var| var.name.to_owned())
        .collect()
}

/// Reds on: a non-secret declaration that accepts a reference outside the allow list, or one on the list
/// that is secret, does not accept one or is not `UserInput`.
fn unlisted_reference_users(vars: &[&EnvVar]) -> Vec<String> {
    let mut found: Vec<String> = vars
        .iter()
        .filter(|var| var.accepts_reference && !var.secret)
        .filter(|var| !REFERENCE_USER_NAMES.contains(&var.name) || var.propagation != Propagation::UserInput)
        .map(|var| var.name.to_owned())
        .collect();
    for name in REFERENCE_USER_NAMES {
        let listed = vars.iter().find(|var| var.name == *name);
        if !listed.is_some_and(|var| var.accepts_reference && !var.secret) {
            found.push((*name).to_owned());
        }
    }
    found
}

#[test]
fn every_public_secret_accepts_a_reference() {
    assert_eq!(secrets_without_reference(&live()), Vec::<String>::new());
    let bad = with_reference(declaration("OCX_X_TOKEN", Visibility::Public, true, "d"), false);
    assert_eq!(secrets_without_reference(&[bad]), ["OCX_X_TOKEN"]);
}

#[test]
fn only_a_public_declaration_accepts_a_reference() {
    assert_eq!(references_outside_public(&live()), Vec::<String>::new());
    for visibility in [Visibility::Foreign, Visibility::Plumbing, Visibility::Testing] {
        let bad = with_reference(declaration("X", visibility, false, "d"), true);
        assert_eq!(references_outside_public(&[bad]), ["X"], "{visibility:?}");
    }
}

#[test]
fn no_path_valued_declaration_accepts_a_reference() {
    assert_eq!(references_on_paths(&live()), Vec::<String>::new());
    for value in [EnvValue::Path, EnvValue::PathList, EnvValue::PathOrPem] {
        let bad: &'static EnvVar = Box::leak(Box::new(EnvVar {
            value,
            accepts_reference: true,
            ..*declaration("OCX_X", Visibility::Public, false, "d")
        }));
        assert_eq!(references_on_paths(&[bad]), ["OCX_X"], "{value:?}");
    }
}

#[test]
fn the_non_secret_reference_users_are_exactly_the_two_user_names() {
    assert_eq!(unlisted_reference_users(&live()), Vec::<String>::new());
    let stray = with_reference(declaration("OCX_STRAY", Visibility::Public, false, "d"), true);
    assert_eq!(
        unlisted_reference_users(&[stray]).first().map(String::as_str),
        Some("OCX_STRAY")
    );
    let listed = live()
        .into_iter()
        .filter(|var| REFERENCE_USER_NAMES.contains(&var.name))
        .collect::<Vec<_>>();
    assert_eq!(listed.len(), REFERENCE_USER_NAMES.len());
    assert!(
        listed
            .iter()
            .all(|var| !var.secret && var.propagation == Propagation::UserInput)
    );
}

#[test]
fn a_reference_capable_declaration_is_only_read_through_a_typed_reader() {
    let _: &ReferenceVar = &OCX_ANNOUNCE_GIT_USERNAME;
    let _: &ReferenceVar = &OCX_AUTH_USER;
    let _: &SecretVar = &OCX_AUTH_TOKEN;
    let env = overrides::lock();
    let capable: Vec<_> = live().into_iter().filter(|var| var.accepts_reference).collect();
    assert!(capable.len() > REFERENCE_USER_NAMES.len());
    for var in capable {
        let value = "plain://x";
        if var.is_pattern() {
            env.set_raw(var.slot_name("PROBE").unwrap(), value);
            assert_eq!(var.get_slot("PROBE"), None, "{}", var.name);
        } else {
            env.set(var, value);
            assert_eq!(var.get(), None, "{}", var.name);
            assert_eq!(var.get_os(), None, "{}", var.name);
            assert_eq!(var.get_raw(), None, "{}", var.name);
        }
    }
}

#[test]
fn a_foreign_variable_never_accepts_a_reference() {
    let offenders: Vec<&str> = live()
        .into_iter()
        .filter(|var| var.visibility == Visibility::Foreign && var.accepts_reference)
        .map(|var| var.name)
        .collect();
    assert_eq!(offenders, Vec::<&str>::new());
}

#[test]
fn every_owned_secret_accepts_a_reference() {
    let offenders: Vec<&str> = live()
        .into_iter()
        .filter(|var| var.secret && var.visibility != Visibility::Foreign && !var.accepts_reference)
        .map(|var| var.name)
        .collect();
    assert_eq!(offenders, Vec::<&str>::new());
}
