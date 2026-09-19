// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[records]` configuration section, and the shape every tier folds in.
//!
//! One type serves all three tiers — config file, environment, CLI flags — so
//! the fold in `record::policy::resolve_records` is a merge of like with like
//! rather than three bespoke conversions. That fold lives above this module and
//! is named in prose rather than linked: this is `ocx_config`, and an intra-doc
//! link into `ocx_package_manager` is the very reach the split removes.
//!
//! The section lives here rather than under `record/` because the crate-private
//! `config::Config` carries it as a field: owning the type where the
//! config file is parsed is what keeps `ocx_config` from depending on the
//! recording subsystem it merely configures.

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
    /// Reads the environment tier of the `[records]` fold —
    /// [`OCX_RECORDS_DIR`](crate::env::keys::OCX_RECORDS_DIR) and
    /// [`OCX_RECORDS_NAME`](crate::env::keys::OCX_RECORDS_NAME) — into the same
    /// shape the config file and the CLI flags produce, so
    /// `record::policy::resolve_records` merges like with like.
    ///
    /// An absent **or empty** value is unset, following
    /// [`OCX_MANAGED_CONFIG`](crate::env::keys::OCX_MANAGED_CONFIG) and
    /// [`OCX_CONFIG`](crate::env::keys::OCX_CONFIG): an exported-but-empty
    /// variable is how a shell spells "no value", and reading
    /// `OCX_RECORDS_DIR=""` as a sink would scatter records across whatever
    /// directory each tool happened to run in.
    ///
    /// Infallible by construction — the two values are a path and a template
    /// string, so there is nothing to reject here. A malformed *template* is a
    /// resolve-time error raised by `record::NameTemplate::parse`, which sees
    /// the merged value and can therefore name the tier that won.
    ///
    /// `required` is never populated from this tier. There is no
    /// `OCX_RECORDS_REQUIRED` env var, and the fold refuses the field from a
    /// non-config tier regardless of the shape it receives — recording posture
    /// is an operator decision, not a per-invocation one.
    pub fn from_env() -> Self {
        use crate::env::keys;
        use ocx_util::env::var;

        Self {
            dir: var(keys::OCX_RECORDS_DIR)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            name: var(keys::OCX_RECORDS_NAME).filter(|value| !value.is_empty()),
            required: None,
            system_locked: false,
        }
    }

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

#[cfg(test)]
mod env_tier_tests {
    use super::*;
    use crate::env::{Env, OcxConfigView, keys};

    fn view(self_exe: &str) -> OcxConfigView {
        OcxConfigView::new(std::path::PathBuf::from(self_exe))
    }

    /// `required` has no environment peer, in either direction. Nothing reads
    /// an `OCX_RECORDS_REQUIRED`, and nothing writes one — the fail-closed
    /// posture is an operator decision that lives in the config file, so a
    /// forwarded env var would hand it to any caller who can set one.
    #[test]
    fn records_required_is_never_an_env_var() {
        const NOT_A_KEY: &str = "OCX_RECORDS_REQUIRED";

        let guard = ocx_util::env::overrides::lock();
        guard.set(NOT_A_KEY, "1");
        guard.set(keys::OCX_RECORDS_DIR, "/var/log/ocx-records");

        // Read side: the env tier never populates `required`, whatever the
        // ambient environment claims.
        let from_env = RecordsOptions::from_env();
        assert!(
            from_env.required.is_none(),
            "the env tier must leave `required` absent; OCX_RECORDS_REQUIRED is not consulted"
        );
        assert_eq!(
            from_env.dir.as_deref(),
            Some(std::path::Path::new("/var/log/ocx-records"))
        );

        // Write side: a view carrying `required` forwards nothing for it.
        let mut cfg = view("/abs/ocx");
        cfg.records.dir = Some(std::path::PathBuf::from("/var/log/ocx-records"));
        cfg.records.required = Some(true);
        let mut env = Env::clean();
        env.apply_ocx_config(&cfg);
        assert!(
            env.get(NOT_A_KEY).is_none(),
            "`required` must never be forwarded to a child env"
        );
    }

    /// The env tier reads both variables into the shape the fold merges.
    #[test]
    fn records_reads_dir_and_name_from_the_environment() {
        let guard = ocx_util::env::overrides::lock();
        guard.set(keys::OCX_RECORDS_DIR, "/var/log/ocx-records");
        guard.set(keys::OCX_RECORDS_NAME, "{time}-{pid}-{rand}.json");

        let options = RecordsOptions::from_env();
        assert_eq!(
            options.dir.as_deref(),
            Some(std::path::Path::new("/var/log/ocx-records"))
        );
        assert_eq!(options.name.as_deref(), Some("{time}-{pid}-{rand}.json"));
        assert!(
            !options.system_locked,
            "SYSTEM-scope provenance is loader-set; the env tier can never claim it"
        );
    }

    /// Absent or empty is unset. An exported-but-empty variable is how a shell
    /// spells "no value" — reading it as a sink named `""` would write records
    /// into whatever directory each tool happened to run in.
    #[test]
    fn records_treats_absent_and_empty_as_unset() {
        let guard = ocx_util::env::overrides::lock();

        guard.remove(keys::OCX_RECORDS_DIR);
        guard.remove(keys::OCX_RECORDS_NAME);
        let absent = RecordsOptions::from_env();
        assert!(absent.dir.is_none());
        assert!(absent.name.is_none());

        guard.set(keys::OCX_RECORDS_DIR, "");
        guard.set(keys::OCX_RECORDS_NAME, "");
        let empty = RecordsOptions::from_env();
        assert!(empty.dir.is_none(), "OCX_RECORDS_DIR=\"\" must read as unset");
        assert!(empty.name.is_none(), "OCX_RECORDS_NAME=\"\" must read as unset");
    }

    /// Round-trip across the process boundary: what `apply_ocx_config` writes
    /// onto a child env is exactly what the child's env tier reads back. The
    /// two halves are one contract — a launcher re-entry must resolve the same
    /// sink and template its parent did.
    #[test]
    fn records_round_trip_from_forwarded_child_env() {
        let guard = ocx_util::env::overrides::lock();

        let mut cfg = view("/abs/ocx");
        cfg.records.dir = Some(std::path::PathBuf::from("/var/log/ocx-records"));
        cfg.records.name = Some("{time}-{host}-{pid}-{rand}.json".to_string());

        let mut child = Env::clean();
        child.apply_ocx_config(&cfg);
        for key in [keys::OCX_RECORDS_DIR, keys::OCX_RECORDS_NAME] {
            let value = child
                .get(key)
                .unwrap_or_else(|| panic!("{key} must be set on the child env"))
                .to_str()
                .expect("a forwarded records value must be valid UTF-8")
                .to_string();
            guard.set(key, value);
        }

        let parsed = RecordsOptions::from_env();
        assert_eq!(parsed.dir, cfg.records.dir, "the sink must survive the hop");
        assert_eq!(parsed.name, cfg.records.name, "the template must survive the hop");
        assert!(parsed.required.is_none(), "no posture rides along");
    }

    /// D-007: the relocated reader answers what the contract names, field by
    /// field.
    ///
    /// Each of the four fields is compared against a literal expectation rather
    /// than against a second reader — the `env::records()` spelling this once
    /// stood beside was a one-line forward to the body under test, so comparing
    /// the two was tautological, and it is gone (its sole caller,
    /// `ocx_cli`'s `app/context.rs`, now names `RecordsOptions::from_env`
    /// directly). A perturbation here — a dropped empty-is-unset filter, a
    /// `required` that starts reading an env var, a swapped field — reds
    /// against the table below.
    #[test]
    fn from_env_reads_each_field_as_the_contract_names_it() {
        let guard = ocx_util::env::overrides::lock();

        for (dir, name, expected_dir, expected_name) in [
            (
                Some("/var/log/ocx"),
                Some("{time}-{pid}.json"),
                Some("/var/log/ocx"),
                Some("{time}-{pid}.json"),
            ),
            (Some(""), Some(""), None, None),
            (None, Some("{rand}.json"), None, Some("{rand}.json")),
            (Some("/only/dir"), None, Some("/only/dir"), None),
            (None, None, None, None),
        ] {
            match dir {
                Some(value) => guard.set(keys::OCX_RECORDS_DIR, value),
                None => guard.remove(keys::OCX_RECORDS_DIR),
            }
            match name {
                Some(value) => guard.set(keys::OCX_RECORDS_NAME, value),
                None => guard.remove(keys::OCX_RECORDS_NAME),
            }

            let moved = RecordsOptions::from_env();
            assert_eq!(
                moved.dir.as_deref(),
                expected_dir.map(std::path::Path::new),
                "dir, for OCX_RECORDS_DIR={dir:?}"
            );
            assert_eq!(
                moved.name.as_deref(),
                expected_name,
                "name, for OCX_RECORDS_NAME={name:?}"
            );
            assert_eq!(moved.required, None, "required has no environment tier");
            assert!(!moved.system_locked, "only the loader locks");
        }
    }
}
