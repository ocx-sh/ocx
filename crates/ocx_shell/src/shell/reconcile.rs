// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-prompt environment reconciler: the `__OCX_ENV_STATE` ledger, its
//! envelope codec, and the typed three-way [`plan`].
//!
//! The carrier is **untrusted input** (C-007): its only permitted effects are
//! naming the revert set and supplying the equality operand for the exit
//! guard. Nothing here constructs a path from it, re-grants consent, or
//! selects a value for a key it is not reverting.
//!
//! Split by concept ([ocx-sh/ocx#345](https://github.com/ocx-sh/ocx/issues/345)):
//! [`ledger`] is the carrier format, [`plan`] is the three-way planner, and
//! [`fingerprint`] is the watch-set fingerprint. This module retains only
//! what genuinely spans them — the key/element comparison and equivalence
//! primitives both the carrier's decode-time forging guard and the planner's
//! revert-set membership tests share.
//!
//! All three are **pure**, and that is what keeps `shell/` independent of
//! `project/`. The sequencing that binds them into one prompt's answer — and
//! which does read consent — lives at `package_manager::activation`; see its module
//! docs for why it is not a submodule here.

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

// ---------------------------------------------------------------------------
// Shared primitives — span the carrier format (ledger) and the planner (plan)
// ---------------------------------------------------------------------------

/// The keys no scope may ever declare [`ModifierKind::Constant`] for (A-02).
const NEVER_CONSTANT: [&str; 2] = ["PATH", "PATHEXT"];

/// The comparison rule for a value, **selected by the kind that wrote it**.
///
/// The two rules are the emitters' own, and the planner must not be wider than
/// the arm that will render its decision — a comparison that calls two spellings
/// equal suppresses a removal the emitter would have applied byte-exact, and the
/// variable then accumulates both.
///
/// - [`ModifierKind::List`] — **byte-exact, case-sensitive on every platform**
///   ([`crate::shell::Shell::remove_list_element`], [`crate::shell::Shell::export_list`]).
///   A list element is an opaque option string: `-DFOO=1` and `-Dfoo=1` are
///   different options, and a `"` inside one is part of the option, never a
///   quoting artefact.
/// - [`ModifierKind::Path`] — A-19: segment-exact after stripping one
///   surrounding pair of `"` (`std::env::split_paths` unquotes on Windows, so
///   the operand ocx sees may carry a pair its own emit did not write),
///   case-sensitive on Unix and ASCII-case-insensitive on Windows.
/// - [`ModifierKind::Constant`] — the `C == L.applied` exit guard, which the ADR
///   pins to the same predicate as A-19 (ASCII-case-insensitive on Windows).
///
/// The stored string is never normalised — only the comparison is (C-008).
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

/// Key equality as `EnvKey` already defines it: case-insensitive on Windows,
/// where `$env:Path` and `$env:PATH` are one variable, exact elsewhere.
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

// ---------------------------------------------------------------------------
// The ambient environment an explicit invocation starts from (design spec A.2)
// ---------------------------------------------------------------------------

/// The ambient environment an **explicit** ocx invocation starts a child
/// from: [`Env::new`] with the per-prompt shell reconciler's own
/// contribution taken back out.
///
/// `ocx exec -- cmd` names the environment it wants. The project toolchain
/// the reconciler happened to fold into the calling shell is not part of
/// it, and letting it through is exactly the ambient-`PATH` pollution
/// clean-env execution exists to prevent — ocx's own shell integration
/// would otherwise undercut the guarantee that separates it from the
/// shim-and-shell tools.
///
/// The user's *own* environment is untouched. Flipping this to
/// [`Env::clean`] instead would eat their `EDITOR`, `SSH_AUTH_SOCK` and
/// proxy vars — a far bigger break than the leak; `--clean` still means
/// what it always meant.
///
/// With no `__OCX_ENV_STATE` carrier — no shell integration, a CI runner,
/// a plain `ocx exec` from an un-hooked shell — this is [`Env::new`],
/// variable for variable.
pub fn inherited_env() -> Env {
    let mut env = Env::new();
    revert_reconciled(&mut env);
    env
}

/// Take the reconciler's contribution back out, using the
/// `__OCX_ENV_STATE` ledger as the revert set.
///
/// The revert set is [`plan`] against an **empty** desired set —
/// "ocx wants nothing here now", which is precisely what an explicit
/// invocation means. Reusing the planner is the point: it already knows
/// that a constant only reverts while the live value is still the one ocx
/// wrote, and that a key with no recorded prior is left alone. A second
/// reverter would be a second thing to drift from the shell arms.
///
/// `owned_prefixes` is deliberately empty. The lost-ledger repair (C-006)
/// would also strip `$OCX_HOME` segments *this* ledger never claimed —
/// `ocx self activate`'s own `bin/` among them, which is how the child
/// finds `ocx` at all — and the ledger is present here, so there is
/// nothing to guess.
///
/// The carrier is untrusted input (C-007): its only effect here is naming
/// the revert set. [`Ledger::decode`] answers `None` for every malformed
/// value, and this reverts nothing and leaves the carrier alone in that
/// case; an entry naming an empty element or an empty separator is skipped
/// rather than folded, because either would make the list removal a wipe.
fn revert_reconciled(env: &mut Env) {
    let Some(raw) = env.get(CARRIER_KEY).map(|v| v.to_string_lossy().into_owned()) else {
        return;
    };
    let Some(ledger) = Ledger::decode(&raw) else {
        return;
    };
    // The record describes an environment this child no longer has.
    env.remove(CARRIER_KEY);

    let plan = plan(&[], env, &ledger, &[]);
    for (key, element, separator) in &plan.removes {
        if element.is_empty() {
            continue;
        }
        let Some(existing) = env.get(key) else { continue };
        let value = match separator {
            // Path kind: the same segment-exact removal the emitted arms do.
            None => ocx_util::path::remove_segment(existing, OsStr::new(element)),
            Some(separator) if !separator.is_empty() => {
                // List kind: `append_unique` is the pinned fold, so removal
                // is that fold minus its re-append — one algorithm, not a
                // second copy of the flank rule.
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
