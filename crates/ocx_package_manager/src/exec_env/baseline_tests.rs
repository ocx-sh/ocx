// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_config::env::Env;

use super::carry_system_root;

fn ambient_with(key: &str, value: &str) -> Env {
    let mut ambient = Env::clean();
    ambient.set(key, value);
    ambient
}

#[test]
fn carries_the_callers_system_root_into_an_empty_environment() {
    let mut env = Env::clean();
    carry_system_root(&mut env, &ambient_with("SystemRoot", r"C:\Windows"));
    assert_eq!(env.get("SystemRoot").and_then(|v| v.to_str()), Some(r"C:\Windows"));
}

#[test]
fn keeps_a_system_root_already_set() {
    let mut env = ambient_with("SystemRoot", r"D:\Win");
    carry_system_root(&mut env, &ambient_with("SystemRoot", r"C:\Windows"));
    assert_eq!(env.get("SystemRoot").and_then(|v| v.to_str()), Some(r"D:\Win"));
}

#[test]
fn carries_nothing_else_and_nothing_when_the_caller_has_none() {
    let mut env = Env::clean();
    carry_system_root(&mut env, &ambient_with("TEMP", "/tmp"));
    assert_eq!(env.iter().count(), 0);
}
