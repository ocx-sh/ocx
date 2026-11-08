// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Compile-time pin of the ecosystem API `ocx-mirror` links against.
//!
//! Nothing here runs the pinned items: each is bound to a typed `let`, so a signature change fails to
//! compile. Update the lockstep consumer in the same change series before touching one.

#![allow(dead_code, clippy::type_complexity)]

use std::ffi::OsString;
use std::path::Path;

use ocx_env::{EnvVar, InvalidEnv, SecretVar, Sensitive};

use crate::env::Env;
use crate::exec_state::{Inherited, Narrowing};
use crate::mirror::{MirrorConfig, MirrorConfigError};

fn pin_config_env() {
    let _: fn(Option<&str>) -> Vec<String> = crate::insecure::parse_host_list;
    let _: fn(Option<&str>) -> Result<Vec<(String, MirrorConfig)>, MirrorConfigError> =
        crate::mirror::parse_env_mirrors;
    let _: fn() -> Env = Env::clean;
}

fn pin_ocx_env() {
    let _: fn(&'static EnvVar, bool) -> Result<bool, InvalidEnv> = EnvVar::bool_or;
    let _: fn(&'static EnvVar) -> &'static EnvVar = EnvVar::declaration;
    let _: &'static str = ocx_env::OCX_OFFLINE.declaration().name;
    let _: &'static str = ocx_env::OCX_FROZEN.declaration().name;
    let _: fn(&'static EnvVar, &str) -> Option<String> = EnvVar::get_slot;
    let _: fn(&str) -> Option<String> = ocx_env::dynamic;
    let _: fn(&'static SecretVar) -> Option<Sensitive> = SecretVar::get;
    let _: fn(&'static SecretVar, &str) -> Option<Sensitive> = SecretVar::get_slot;
    let _: fn(&Sensitive) -> &str = Sensitive::expose;
    let _: &'static SecretVar = &ocx_env::OCX_ANNOUNCE_TOKEN;
    let _: &'static SecretVar = &ocx_env::OCX_AUTH_TOKEN;
    let _: &'static EnvVar = &ocx_env::OCX_AUTH_USER;
    let _: &'static EnvVar = &ocx_env::OCX_OFFLINE;
    let _: &'static EnvVar = &ocx_env::OCX_FROZEN;
}

fn pin_exec_state() {
    let _: fn() -> Inherited = Inherited::from_process_env;
    let _: fn(&Inherited) -> Narrowing = Inherited::narrowing;
    let _: for<'a> fn(&'a Inherited) -> Option<&'a Path> = Inherited::home;
    let _: Option<OsString> = None;
}

#[test]
fn ecosystem_api_pin_compiles() {
    // The pins above are the check; reaching this test means they compiled.
    let _ = (pin_config_env as fn(), pin_ocx_env as fn(), pin_exec_state as fn());
}
