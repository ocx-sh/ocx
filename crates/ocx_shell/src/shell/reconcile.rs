// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt environment reconciler: the `__OCX_ENV_STATE` ledger, its codec, and the
//! typed three-way [`plan`].
//!
//! The carrier is **untrusted input**: it may only name the revert set and supply the exit
//! guard's equality operand, never a path, a consent grant, or a value for a key it is not reverting.

use std::ffi::{OsStr, OsString};

use ocx_config::env::Env;
use ocx_package::metadata::env::entry::Entry;
use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;
use ocx_package::metadata::env::modifier::ModifierKind;

mod fingerprint;
mod ledger;
mod plan;

pub use fingerprint::{current_fingerprint, fingerprint, watch_paths, watch_set_fingerprint};
pub use ledger::{
    Applied, CARRIER_KEY, LEDGER_VERSION, Ledger, LedgerEntry, MAX_CARRIER_BYTES, Prior, Priors, ProjectScope, ScopeId,
    Scopes, Verdict,
};
pub use plan::{PLAN_VERSION, Plan, capture_priors, emittable_entries, plan, summary};

/// The keys no scope may ever declare [`ModifierKind::Constant`] for.
const NEVER_CONSTANT: [&str; 2] = ["PATH", "PATHEXT"];

/// The comparison rule for a value, **selected by the kind that wrote it**.
///
/// Never wider than the emit arm: an equality the byte-exact emitter does not share suppresses a
/// removal, and the variable accumulates both.
/// Path and Constant strip one `"` pair because Windows `split_paths` unquotes the operand.
fn element_eq(left: &str, right: &str, kind: &ModifierKind) -> bool {
    match kind {
        ModifierKind::List => left == right,
        ModifierKind::Path | ModifierKind::Constant => {
            let (left, right) = (unquote(left), unquote(right));
            if cfg!(windows) {
                left.eq_ignore_ascii_case(right)
            } else {
                left == right
            }
        }
    }
}

/// The hashable form of [`element_eq`]'s equivalence class, under the same kind.
fn element_norm(value: &str, kind: &ModifierKind) -> String {
    match kind {
        ModifierKind::List => value.to_owned(),
        ModifierKind::Path | ModifierKind::Constant => {
            let value = unquote(value);
            if cfg!(windows) {
                value.to_ascii_lowercase()
            } else {
                value.to_owned()
            }
        }
    }
}

fn unquote(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

/// Key equality as `EnvKey` defines it: ASCII-case-insensitive on Windows only.
fn key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn key_norm(key: &str) -> String {
    if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    }
}

fn is_never_constant(key: &str) -> bool {
    NEVER_CONSTANT.iter().any(|reserved| key_eq(key, reserved))
}

fn effective_separator(entry: &Entry) -> String {
    entry.separator.clone().unwrap_or_else(|| DEFAULT_SEPARATOR.to_owned())
}

fn os_to_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

/// [`Env::new`] with the per-prompt reconciler's contribution taken back out; the start env of an explicit invocation.
///
/// Not [`Env::clean`], which would also strip the user's own `EDITOR` and `SSH_AUTH_SOCK`.
pub fn inherited_env() -> Env {
    let mut env = Env::new();
    revert_reconciled(&mut env);
    env
}

/// Revert the ledger's contribution by [`plan`]ning against an empty desired set.
///
/// `owned_prefixes` stays empty, or the lost-ledger repair strips `ocx self activate`'s own `bin/`.
/// An empty element or separator is skipped, since either makes the list removal a wipe.
fn revert_reconciled(env: &mut Env) {
    let Some(raw) = env.get(CARRIER_KEY).map(|v| v.to_string_lossy().into_owned()) else {
        return;
    };
    let Some(ledger) = Ledger::decode(&raw) else {
        return;
    };
    env.remove(CARRIER_KEY);

    let plan = plan(&[], env, &ledger, &[]);
    for (key, element, separator) in &plan.removes {
        if element.is_empty() {
            continue;
        }
        let Some(existing) = env.get(key) else { continue };
        let value = match separator {
            None => ocx_util::path::remove_segment(existing, OsStr::new(element)),
            Some(separator) if !separator.is_empty() => {
                // Removal is `append_unique` minus its re-append, so the flank rule has one copy.
                let appended = ocx_util::list::append_unique(&existing.to_string_lossy(), element, separator);
                let tail = format!("{separator}{element}");
                OsString::from(appended.strip_suffix(&tail).unwrap_or("").to_owned())
            }
            Some(_) => continue,
        };
        env.set(key.as_str(), value);
    }
    for (key, prior) in &plan.restores {
        match prior {
            Some(value) => env.set(key.as_str(), value.as_str()),
            None => env.remove(key),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use ocx_config::env::Env;

    use crate::shell::reconcile::{
        self, Applied, LEDGER_VERSION, Ledger, LedgerEntry, Prior, Priors, ProjectScope, Scopes,
    };
    use ocx_package::metadata::env::modifier::ModifierKind;

    /// Sorted `(key, value)` pairs — `Env` has no `PartialEq`, and "untouched"
    /// is a claim about the whole map, not about the keys a test remembers.
    fn snapshot(env: &Env) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned()))
            .collect();
        pairs.sort();
        pairs
    }

    fn recorded(key: &str, value: &str, kind: ModifierKind, separator: Option<&str>) -> LedgerEntry {
        LedgerEntry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind,
            separator: separator.map(str::to_owned),
        }
    }

    /// A well-formed carrier recording `applied`/`priors` in the project scope.
    fn carrier(applied: Applied, priors: Priors) -> String {
        Ledger {
            v: LEDGER_VERSION,
            fp: "fp-1".to_owned(),
            verdict: None,
            tiers: Vec::new(),
            // The watch-set fingerprint plays no part in the revert path this
            // fixture exercises; empty is the field's own neutral value.
            ws: String::new(),
            messages_fp: String::new(),
            over_cap: Vec::new(),
            scopes: Scopes {
                global: None,
                global_priors: Priors::new(),
                project: Some(ProjectScope {
                    key: "acme-1a2b".to_owned(),
                    dir: PathBuf::from("/p"),
                    applied,
                    priors,
                }),
            },
        }
        .encode()
        .expect("fixture ledger encodes")
    }

    fn segments(env: &Env, key: &str) -> Vec<String> {
        std::env::split_paths(env.get(key).unwrap_or_else(|| OsStr::new("")))
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// The headline case: `ocx exec` must not hand the child the toolchain the
    /// per-prompt reconciler folded into the calling shell — while the user's
    /// own `PATH` additions and their own variables ride through untouched.
    ///
    /// `/home/u/.ocx/symlinks/bin` is on `PATH` and inside an ocx home, and
    /// still survives: the ledger did not claim it, and `revert_reconciled`
    /// passes no owned prefixes precisely so the lost-ledger repair cannot
    /// take `ocx self activate`'s own directory out from under the child.
    #[test]
    fn a_ledger_names_the_revert_set_and_the_user_s_own_environment_survives() {
        let path = std::env::join_paths(["/opt/tc/bin", "/home/u/.ocx/symlinks/bin", "/home/u/bin", "/usr/bin"])
            .expect("fixture segments carry no path separator");
        let mut env = Env::clean();
        env.set("PATH", &path);
        env.set("EDITOR", "vim");
        env.set(
            reconcile::CARRIER_KEY,
            carrier(
                vec![recorded("PATH", "/opt/tc/bin", ModifierKind::Path, None)],
                Priors::new(),
            ),
        );

        revert_reconciled(&mut env);

        assert_eq!(
            segments(&env, "PATH"),
            ["/home/u/.ocx/symlinks/bin", "/home/u/bin", "/usr/bin"],
            "only the segment the ledger claims may be reverted"
        );
        assert_eq!(
            env.get("EDITOR"),
            Some(OsStr::new("vim")),
            "the user's own vars are inherited"
        );
        assert!(
            env.get(reconcile::CARRIER_KEY).is_none(),
            "the carrier describes an environment the child no longer has"
        );
    }

    /// Constants revert to their recorded prior — restoring a value, unsetting
    /// one that did not exist — and a value the user changed after ocx wrote it
    /// is left exactly as they left it.
    #[test]
    fn constants_revert_to_their_prior_and_a_user_override_is_left_alone() {
        let mut env = Env::clean();
        env.set("JAVA_HOME", "/opt/tc/jdk");
        env.set("GOFLAGS", "-mod=mod");
        env.set("CC", "/usr/bin/clang");
        env.set(
            reconcile::CARRIER_KEY,
            carrier(
                vec![
                    recorded("JAVA_HOME", "/opt/tc/jdk", ModifierKind::Constant, None),
                    recorded("GOFLAGS", "-mod=mod", ModifierKind::Constant, None),
                    recorded("CC", "/opt/tc/gcc", ModifierKind::Constant, None),
                ],
                Priors::from([
                    ("JAVA_HOME".to_owned(), Prior::Value("/usr/lib/jvm/default".to_owned())),
                    ("GOFLAGS".to_owned(), Prior::Unset),
                    ("CC".to_owned(), Prior::Value("/usr/bin/cc".to_owned())),
                ]),
            ),
        );

        revert_reconciled(&mut env);

        assert_eq!(env.get("JAVA_HOME"), Some(OsStr::new("/usr/lib/jvm/default")));
        assert_eq!(env.get("GOFLAGS"), None, "a prior of Unset removes the variable");
        assert_eq!(
            env.get("CC"),
            Some(OsStr::new("/usr/bin/clang")),
            "the live value is no longer ocx's, so it is the user's and stays"
        );
    }

    /// List-kind contributions revert through the same `append_unique` fold the
    /// emitted shell arms use, so the in-process answer and the shell answer
    /// cannot drift.
    #[test]
    fn list_contributions_are_removed_element_wise() {
        let mut env = Env::clean();
        env.set("JAVA_TOOL_OPTIONS", "-Xmx1g -Dtc=1 -ea");
        env.set(
            reconcile::CARRIER_KEY,
            carrier(
                vec![recorded("JAVA_TOOL_OPTIONS", "-Dtc=1", ModifierKind::List, Some(" "))],
                Priors::new(),
            ),
        );

        revert_reconciled(&mut env);

        assert_eq!(env.get("JAVA_TOOL_OPTIONS"), Some(OsStr::new("-Xmx1g -ea")));
    }

    /// No carrier — no shell integration, a CI runner, an un-hooked shell — is
    /// today's behaviour, variable for variable.
    #[test]
    fn no_carrier_leaves_the_environment_untouched() {
        let mut env = Env::clean();
        env.set(
            "PATH",
            std::env::join_paths(["/opt/tc/bin", "/home/u/.ocx/symlinks/bin", "/usr/bin"]).expect("joinable"),
        );
        env.set("EDITOR", "vim");
        env.set("JAVA_HOME", "/opt/tc/jdk");
        let before = snapshot(&env);

        revert_reconciled(&mut env);

        assert_eq!(snapshot(&env), before);
    }

    /// The carrier round-trips through a user-writable env var (C-007). Every
    /// malformed shape degrades to reverting nothing — never to a panic, never
    /// to a wrong path — and the value is left where it was, because a carrier
    /// ocx cannot read is one it has not acted on.
    #[test]
    fn a_malformed_carrier_reverts_nothing() {
        for raw in [
            "",
            "not-an-envelope",
            "9.eyJ2IjoxfQ",       // unknown encoder tag
            "1.!!!not-base64!!!", // payload outside the base64url alphabet
            "1.eyJ2Ijo5OTl9",     // decodes, but `{"v":999}` matches no schema
            "1.",                 // empty payload
        ] {
            let mut env = Env::clean();
            env.set(
                "PATH",
                std::env::join_paths(["/opt/tc/bin", "/usr/bin"]).expect("joinable"),
            );
            env.set("JAVA_HOME", "/opt/tc/jdk");
            env.set(reconcile::CARRIER_KEY, raw);
            let before = snapshot(&env);

            revert_reconciled(&mut env);

            assert_eq!(snapshot(&env), before, "carrier {raw:?} must revert nothing");
        }
    }

    /// A forged carrier naming an empty element or an empty separator must not
    /// wipe the variable: `append_unique`'s flank rule needs a non-empty
    /// separator, and an empty element matches every gap between elements.
    #[test]
    fn a_forged_empty_element_or_separator_never_wipes_the_variable() {
        for entry in [
            recorded("CFLAGS", "", ModifierKind::List, Some(" ")),
            recorded("CFLAGS", "-DX", ModifierKind::List, Some("")),
        ] {
            let mut env = Env::clean();
            env.set("CFLAGS", "-O2 -DX");
            env.set(reconcile::CARRIER_KEY, carrier(vec![entry.clone()], Priors::new()));

            revert_reconciled(&mut env);

            assert_eq!(env.get("CFLAGS"), Some(OsStr::new("-O2 -DX")), "entry {entry:?}");
        }
    }
}
