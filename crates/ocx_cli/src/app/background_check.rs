// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The shared gate in front of every background check that reaches the network.

use std::io::IsTerminal;

/// Why a background check did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipReason {
    /// The named kill-switch environment variable is set.
    KillSwitch(&'static str),
    Ci,
    Offline,
    NotATerminal,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KillSwitch(key) => write!(f, "{key} is set"),
            Self::Ci => f.write_str("CI environment detected"),
            Self::Offline => f.write_str("offline mode"),
            Self::NotATerminal => f.write_str("stderr is not a terminal"),
        }
    }
}

/// The first gate that skips a background check, or `None` when it may run.
///
/// Order: kill switch, CI, offline, stderr not a terminal. A notice nobody reads must not cost
/// a registry round trip, and CI must never see one.
pub(crate) fn skip_reason(kill_switch: &'static ocx_env::EnvVar, offline: bool) -> Option<SkipReason> {
    if kill_switch.bool_or(false).unwrap_or(false) {
        return Some(SkipReason::KillSwitch(kill_switch.name));
    }
    if ocx_env::CI.bool_or(false).unwrap_or(false) {
        return Some(SkipReason::Ci);
    }
    if offline {
        return Some(SkipReason::Offline);
    }
    if !std::io::stderr().is_terminal() {
        return Some(SkipReason::NotATerminal);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SWITCH: &ocx_env::EnvVar = &ocx_env::OCX_NO_UPDATE_CHECK;

    #[test]
    fn kill_switch_wins_over_every_other_gate() {
        let guard = ocx_env::overrides::lock();
        guard.set(SWITCH, "1");
        guard.set(&ocx_env::CI, "1");
        assert_eq!(skip_reason(SWITCH, true), Some(SkipReason::KillSwitch(SWITCH.name)));
    }

    #[test]
    fn ci_wins_over_offline() {
        let guard = ocx_env::overrides::lock();
        guard.remove(SWITCH);
        guard.set(&ocx_env::CI, "1");
        assert_eq!(skip_reason(SWITCH, true), Some(SkipReason::Ci));
    }

    #[test]
    fn offline_skips_outside_ci() {
        let guard = ocx_env::overrides::lock();
        guard.remove(SWITCH);
        guard.remove(&ocx_env::CI);
        assert_eq!(skip_reason(SWITCH, true), Some(SkipReason::Offline));
    }

    #[test]
    fn a_falsy_kill_switch_does_not_skip() {
        let guard = ocx_env::overrides::lock();
        guard.set(SWITCH, "0");
        guard.set(&ocx_env::CI, "1");
        assert_eq!(skip_reason(SWITCH, false), Some(SkipReason::Ci));
    }
}
