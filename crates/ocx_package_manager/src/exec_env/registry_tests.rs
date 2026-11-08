// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The registry table test: every declaration in `ocx_env::all()` reaches a child the way its
//! propagation class says, under each mode, as seen through [`compose`].

use std::path::PathBuf;

use ocx_config::exec_state::Inherited;
use ocx_env::{EnvVar, Propagation, Visibility};

use super::compose_tests::{Fixture, ambient, constant, get, hermetic};
use super::{ExecMode, compose};

const MODES: [ExecMode; 3] = [ExecMode::Layer, ExecMode::Isolate, ExecMode::Clean];
const SENTINEL: &str = "fixture-secret-value";

/// The name a test can set for `var`: a pattern declaration is filled with a fixture slot.
fn concrete_name(var: &EnvVar) -> String {
    match (var.name.find('{'), var.name.find('}')) {
        (Some(open), Some(close)) => format!("{}FIXTURE{}", &var.name[..open], &var.name[close + 1..]),
        _ => var.name.to_owned(),
    }
}

/// Whether `name` is one of the declared names, a pattern declaration matching any slot.
fn is_declared(name: &str, var: &EnvVar) -> bool {
    match (var.name.find('{'), var.name.find('}')) {
        (Some(open), Some(close)) => {
            let (head, tail) = (&var.name[..open], &var.name[close + 1..]);
            name.len() > head.len() + tail.len()
                && name.get(..head.len()).is_some_and(|h| h.eq_ignore_ascii_case(head))
                && name
                    .get(name.len() - tail.len()..)
                    .is_some_and(|t| t.eq_ignore_ascii_case(tail))
        }
        _ => name.eq_ignore_ascii_case(var.name),
    }
}

/// Sets `var` in the hermetic environment under its concrete name.
fn set_ambient(lock: &ocx_env::overrides::EnvLock, var: &'static EnvVar) {
    ambient(lock, &concrete_name(var), SENTINEL);
}

fn child_value(var: &EnvVar, mode: ExecMode, pin: Option<&str>) -> Option<String> {
    let mut fixture = Fixture::new(mode);
    fixture.binary_pin = pin.map(PathBuf::from);
    let projected = compose(&Inherited::Absent, &fixture.spawn());
    get(&projected.env, &concrete_name(var))
}

#[test]
fn the_table_has_a_declaration_of_every_class() {
    for propagation in [
        Propagation::UserInput,
        Propagation::SecretInput,
        Propagation::SetupWritten,
        Propagation::State,
        Propagation::BinaryPin,
        Propagation::HookLocal,
        Propagation::Credential,
    ] {
        assert!(
            ocx_env::keys_with(propagation).next().is_some(),
            "no declaration is {propagation:?}, so the table below checks nothing for it"
        );
    }
}

#[test]
fn every_declaration_reaches_a_child_the_way_its_class_says_under_every_mode() {
    let mut checked = 0;
    for var in ocx_env::all() {
        for mode in MODES {
            let lock = hermetic();
            set_ambient(&lock, var);
            let seen = child_value(var, mode, None);
            let name = var.name;
            match var.propagation {
                // Inherited as the caller had them under layer and isolate; absent under clean.
                Propagation::UserInput | Propagation::SecretInput | Propagation::SetupWritten => {
                    let expected = (mode != ExecMode::Clean).then_some(SENTINEL);
                    assert_eq!(
                        seen.as_deref(),
                        expected,
                        "{name} ({:?}) under {mode:?}",
                        var.propagation
                    );
                }
                // Written by the encoder, never carried over from the caller; with no pin there is no pin.
                Propagation::State | Propagation::BinaryPin | Propagation::HookLocal | Propagation::Credential => {
                    assert_eq!(seen, None, "{name} ({:?}) under {mode:?}", var.propagation);
                }
            }
            checked += 1;
        }
    }
    assert!(
        checked >= 3 * 20,
        "only {checked} declaration/mode pairs were checked: the registry reader stopped"
    );
}

#[test]
fn the_binary_pin_is_set_under_every_mode_and_is_the_only_pin() {
    for var in ocx_env::all().filter(|var| var.propagation == Propagation::BinaryPin) {
        for mode in MODES {
            let lock = hermetic();
            set_ambient(&lock, var);
            assert_eq!(
                child_value(var, mode, Some("/pinned/ocx")).as_deref(),
                Some("/pinned/ocx"),
                "{} under {mode:?}",
                var.name
            );
        }
    }
    assert_eq!(
        ocx_env::keys_with(Propagation::BinaryPin).collect::<Vec<_>>(),
        ["OCX_BINARY_PIN"]
    );
}

#[test]
fn ocx_writes_no_public_variable_but_the_pin_into_a_child() {
    for mode in MODES {
        let _lock = hermetic();
        let mut fixture = Fixture::new(mode);
        fixture.binary_pin = Some(PathBuf::from("/pinned/ocx"));
        fixture.composed = vec![constant("FIXTURE_COMPOSED", "value")];

        let projected = compose(&Inherited::Absent, &fixture.spawn());

        let written: Vec<String> = projected
            .env
            .iter()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .filter(|key| {
                ocx_env::all()
                    .filter(|var| var.visibility != Visibility::Foreign)
                    .any(|var| is_declared(key, var))
            })
            .collect();
        assert_eq!(
            written,
            ["OCX_BINARY_PIN"],
            "compose wrote a declared ocx variable other than the pin under {mode:?}"
        );
    }
}

#[test]
fn every_secret_declaration_is_a_credential_or_a_secret_input() {
    let secrets: Vec<_> = ocx_env::all().filter(|var| var.secret).collect();
    assert!(!secrets.is_empty(), "the registry declares no secret");
    for var in secrets {
        assert!(
            matches!(var.propagation, Propagation::Credential | Propagation::SecretInput),
            "{} is secret but {:?}",
            var.name,
            var.propagation
        );
    }
}
