// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The policy a parent ocx passed down, for the seams that read a setting far from `Context`.
//!
//! `Context::try_init` installs it once. A reader asks for its field and the class decides the tier
//! order: a relaxing field yields to an explicitly set ambient variable, a neutral one to nothing.
//! Only the received half lives here; the ambient half is read live, so a process that received
//! nothing behaves exactly as it did before the state existed.

use std::sync::OnceLock;

use super::types::Policy;

static RECEIVED: OnceLock<Policy> = OnceLock::new();

/// Keep the received policy for the process lifetime; the first call wins.
pub fn install(policy: Policy) {
    let _ = RECEIVED.set(policy);
}

/// The installed received policy; `None` when this process received none.
pub fn policy() -> Option<&'static Policy> {
    RECEIVED.get()
}

/// A relaxing boolean at its env tier: an explicitly set variable (an unparsable one reads as off),
/// else the received value, else off.
pub fn relaxing_bool(var: &'static ocx_env::EnvVar, received: Option<bool>) -> bool {
    if var.get_os().is_some() {
        var.bool_or(false).unwrap_or(false)
    } else {
        received.unwrap_or(false)
    }
}

/// A neutral boolean at its env tier: the received value, else the ambient one, else off.
pub fn neutral_bool(var: &'static ocx_env::EnvVar, received: Option<bool>) -> bool {
    received.unwrap_or_else(|| var.bool_or(false).unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaxing_bool_yields_to_an_explicit_ambient_value() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_CONFIG, "0");
        assert!(!relaxing_bool(&ocx_env::OCX_NO_CONFIG, Some(true)));
        env.remove(&ocx_env::OCX_NO_CONFIG);
        assert!(relaxing_bool(&ocx_env::OCX_NO_CONFIG, Some(true)));
        assert!(!relaxing_bool(&ocx_env::OCX_NO_CONFIG, None));
    }

    #[test]
    fn neutral_bool_prefers_the_received_value() {
        let env = ocx_env::overrides::lock();
        env.set(&ocx_env::OCX_NO_PROJECT, "0");
        assert!(neutral_bool(&ocx_env::OCX_NO_PROJECT, Some(true)));
        assert!(!neutral_bool(&ocx_env::OCX_NO_PROJECT, None));
        env.set(&ocx_env::OCX_NO_PROJECT, "1");
        assert!(neutral_bool(&ocx_env::OCX_NO_PROJECT, None));
    }
}
