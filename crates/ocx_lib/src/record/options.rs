// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[records]` configuration section, and the shape every tier folds in.
//!
//! One type serves all three tiers — config file, environment, CLI flags — so
//! the fold in [`super::policy::resolve_records`] is a merge of like with like
//! rather than three bespoke conversions.

use std::path::PathBuf;

use serde::Deserialize;

/// Where and how execution records are written.
///
/// No `deny_unknown_fields`: this is a fleet-read config surface, and a section
/// written for a newer ocx must not brick an older binary reading the same file.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RecordsOptions {
    /// Directory records are written to. Absent → recording is off.
    pub dir: Option<PathBuf>,

    /// Name template for each record file, e.g. `{time}-{host}-{pid}-{rand}.json`.
    ///
    /// Must contain a component that varies per record, or two frames sharing a
    /// sink resolve to one path.
    pub name: Option<String>,

    /// Fail posture when a record cannot be written.
    ///
    /// `true` aborts the launch; `false` warns and proceeds. **Config-file only
    /// at every tier** — never settable from the environment or a flag, so a
    /// developer who fat-fingers `--records-dir` gets a warning rather than a
    /// dead build. Defaults `false` unlocked, `true` when SYSTEM-locked.
    pub required: Option<bool>,

    /// Runtime provenance marker: this section was declared at the SYSTEM config
    /// scope (`/etc/ocx/config.toml`), so no lower tier can redirect or disable
    /// it.
    ///
    /// Never serialized — set by the loader after parsing the system-scope file,
    /// not read from disk.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,
}

impl RecordsOptions {
    /// Merge `other` into `self`; `other` has higher precedence.
    ///
    /// Early-returns when `self` is SYSTEM-locked, which is what makes the
    /// clamp free for every file tier.
    pub fn merge(&mut self, other: RecordsOptions) {
        // The clamp is binary and per-block: with no operator policy the sink is
        // the caller's, filename pattern included; the moment one is declared at
        // SYSTEM scope the whole block is theirs, because a collector downstream
        // now depends on all of it. The loader folds the system tier in first as
        // the accumulator base, so `self` is the system tier when locked — and
        // every file tier gets the clamp for free from this one return.
        if self.system_locked {
            return;
        }
        if other.dir.is_some() {
            self.dir = other.dir;
        }
        if other.name.is_some() {
            self.name = other.name;
        }
        if other.required.is_some() {
            self.required = other.required;
        }
        // `other.system_locked` is deliberately not folded in: only the loader
        // locks, and only for the system-scope file.
    }

    /// Mark this section as declared at SYSTEM scope.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn options(dir: Option<&str>, name: Option<&str>, required: Option<bool>) -> RecordsOptions {
        RecordsOptions {
            dir: dir.map(PathBuf::from),
            name: name.map(str::to_string),
            required,
            system_locked: false,
        }
    }

    // ── deserialization ──────────────────────────────────────────────────────

    /// All three authored fields round-trip through TOML.
    #[test]
    fn toml_full_block_parses() {
        let parsed: RecordsOptions = toml::from_str(
            r#"
            dir = "/var/log/ocx/records"
            name = "{time}-{host}-{pid}.json"
            required = true
            "#,
        )
        .expect("a full [records] block must parse");
        assert_eq!(parsed.dir.as_deref(), Some(Path::new("/var/log/ocx/records")));
        assert_eq!(parsed.name.as_deref(), Some("{time}-{host}-{pid}.json"));
        assert_eq!(parsed.required, Some(true));
    }

    /// A section written for a newer ocx must not brick an older binary reading
    /// the same fleet-distributed file — no `deny_unknown_fields`.
    #[test]
    fn toml_unknown_fields_are_ignored() {
        let parsed: RecordsOptions =
            toml::from_str("dir = \"/records\"\nrotation = \"daily\"\n").expect("unknown fields must not fail");
        assert_eq!(parsed.dir.as_deref(), Some(Path::new("/records")));
    }

    /// `system_locked` is runtime provenance set by the loader, never read from
    /// disk — a config file cannot declare itself the operator's policy.
    #[test]
    fn toml_cannot_set_system_locked() {
        let parsed: RecordsOptions =
            toml::from_str("dir = \"/records\"\nsystem_locked = true\n").expect("the key is merely unknown");
        assert!(
            !parsed.system_locked,
            "a config file must not be able to declare itself SYSTEM-locked"
        );
    }

    /// Every field starts absent so tier merge and the resolve-time defaults
    /// both work.
    #[test]
    fn default_is_all_absent() {
        let default = RecordsOptions::default();
        assert!(default.dir.is_none());
        assert!(default.name.is_none());
        assert!(default.required.is_none());
        assert!(!default.system_locked);
    }

    // ── merge: precedence matrix ─────────────────────────────────────────────

    /// Each field, independently: the higher tier's `Some` wins.
    #[test]
    fn merge_higher_tier_wins_field_by_field() {
        let mut lower = options(Some("/lower"), Some("{pid}.json"), Some(false));
        lower.merge(options(Some("/higher"), Some("{rand}.json"), Some(true)));
        assert_eq!(lower.dir.as_deref(), Some(Path::new("/higher")));
        assert_eq!(lower.name.as_deref(), Some("{rand}.json"));
        assert_eq!(lower.required, Some(true));
    }

    /// Each field, independently: the higher tier's `None` does not clobber a
    /// lower tier that spoke.
    #[test]
    fn merge_higher_tier_none_never_clobbers() {
        let mut lower = options(Some("/lower"), Some("{pid}.json"), Some(true));
        lower.merge(RecordsOptions::default());
        assert_eq!(lower.dir.as_deref(), Some(Path::new("/lower")));
        assert_eq!(lower.name.as_deref(), Some("{pid}.json"));
        assert_eq!(lower.required, Some(true));
    }

    /// Fields move independently: a higher tier setting only `dir` leaves the
    /// lower tier's `name` and `required` standing.
    #[test]
    fn merge_is_per_field_not_whole_block() {
        let mut lower = options(Some("/lower"), Some("{pid}.json"), Some(true));
        lower.merge(options(Some("/higher"), None, None));
        assert_eq!(lower.dir.as_deref(), Some(Path::new("/higher")));
        assert_eq!(lower.name.as_deref(), Some("{pid}.json"), "name must survive");
        assert_eq!(lower.required, Some(true), "required must survive");
    }

    /// Folding three tiers in order: each field is won by the highest tier that
    /// spoke for it.
    #[test]
    fn merge_chain_resolves_each_field_at_its_own_highest_tier() {
        let mut folded = options(None, None, Some(true));
        folded.merge(options(None, Some("{time}-{rand}.json"), None));
        folded.merge(options(Some("/from-args"), None, None));
        assert_eq!(folded.dir.as_deref(), Some(Path::new("/from-args")));
        assert_eq!(folded.name.as_deref(), Some("{time}-{rand}.json"));
        assert_eq!(folded.required, Some(true));
    }

    // ── merge: the SYSTEM clamp ──────────────────────────────────────────────

    /// The clamp is binary and per-block: once an operator declares `[records]`
    /// at SYSTEM scope, `dir`, `name` and `required` are all theirs, because a
    /// collector downstream now depends on all of it.
    #[test]
    fn merge_system_locked_ignores_every_field() {
        let mut system = options(Some("/var/log/ocx/records"), Some("{time}-{pid}.json"), Some(true));
        system.lock_as_system();
        system.merge(options(Some("/tmp/mine"), Some("{rand}.json"), Some(false)));
        assert_eq!(
            system.dir.as_deref(),
            Some(Path::new("/var/log/ocx/records")),
            "a locked sink must not be redirected"
        );
        assert_eq!(
            system.name.as_deref(),
            Some("{time}-{pid}.json"),
            "a locked filename pattern must not be changed — the collector parses it"
        );
        assert_eq!(system.required, Some(true), "a locked posture must not be flipped");
    }

    /// A locked accumulator stays locked through the whole fold, not just the
    /// first merge.
    #[test]
    fn merge_lock_is_sticky() {
        let mut system = options(Some("/records"), None, None);
        system.lock_as_system();
        system.merge(RecordsOptions::default());
        system.merge(options(Some("/tmp/mine"), None, None));
        assert!(system.system_locked, "the lock must survive the fold");
        assert_eq!(system.dir.as_deref(), Some(Path::new("/records")));
    }

    /// Only the loader locks, and only for the system-scope file. A higher tier
    /// carrying the flag must not be able to lock the accumulator.
    #[test]
    fn merge_does_not_inherit_a_lock_from_a_higher_tier() {
        let mut lower = options(Some("/lower"), None, None);
        let mut higher = options(Some("/higher"), None, None);
        higher.lock_as_system();
        lower.merge(higher);
        assert!(!lower.system_locked, "a lower tier must not acquire the lock by merge");
        assert_eq!(
            lower.dir.as_deref(),
            Some(Path::new("/higher")),
            "normal last-wins applies"
        );
    }

    /// An unlocked section clamps nothing — the whole point of the binary lock.
    #[test]
    fn merge_unlocked_section_clamps_nothing() {
        let mut lower = options(Some("/lower"), Some("{pid}.json"), Some(true));
        lower.merge(options(Some("/higher"), Some("{rand}.json"), None));
        assert_eq!(lower.dir.as_deref(), Some(Path::new("/higher")));
        assert_eq!(lower.name.as_deref(), Some("{rand}.json"));
    }
}
