// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-registry configuration: every `[registries.<name>]` key is an identifier prefix, never a
//! hostname alias.

use serde::Deserialize;

use crate::error::Error;

// No `deny_unknown_fields`, for the fleet forward-compat reason stated on `crate::Config`.
/// Configuration for a single `[registries.<name>]` entry.
///
/// Unknown keys are ignored, like in every other `config.toml` table.
#[derive(Debug, Default, Clone, Deserialize, schemars::JsonSchema)]
pub struct RegistryConfig {
    /// Base URL of the `index.ocx.sh`-protocol index this namespace resolves through, e.g.
    /// `"https://index.ocx.sh"`, or `file:///absolute/path` for a local index read with no server.
    ///
    /// **Field presence is the kind marker.** An entry that carries `index` resolves via the
    /// ocx-index protocol; one without it, or no entry at all, resolves as plain OCI, with no
    /// fallback. A `file://` base is checked at startup: a relative path or a remote authority is refused.
    ///
    /// `index = ""` resolves the namespace as plain OCI. A `[mirrors]` registry entry for the
    /// namespace in a config file you control suppresses the compiled-in index; the managed tier
    /// and `OCX_MIRRORS` cannot.
    // Protocol selector design: `adr_index_indirection.md`.
    pub index: Option<String>,

    /// SSRF escape hatch for this namespace's physical registry hosts. Each
    /// entry is an exact hostname or a CIDR block whose resolved addresses skip
    /// the default-on private/loopback/link-local/metadata refusal.
    ///
    /// A hostname entry also lets `ocx package prune --forge github` delete through the GitHub
    /// Enterprise packages API of a `containers.<host>` registry that this namespace's index names.
    /// Only an exact hostname vouches for that; a CIDR block does not.
    // An index root's `repository` pointer is remote-controlled, so the refusal runs before any fetch.
    pub trusted_hosts: Option<Vec<String>>,

    /// Contact this registry over plain HTTP instead of HTTPS.
    ///
    /// Unioned with `OCX_INSECURE_REGISTRIES`, except that a system-tier entry stating
    /// `insecure = false` locks that host shut against the env list too.
    #[schemars(extend("default" = false))]
    pub insecure: Option<bool>,

    /// Set by the loader when declared in the system tier; no lower tier can override this entry.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,

    /// `index` came from `ConfigLoader::builtin_defaults`; any file tier restating `index` clears it.
    // Lets an explicit `[mirrors."<name>"]` suppress the compiled-in index, or the operator
    // silently gains a host they never allow-listed.
    #[serde(skip)]
    #[schemars(skip)]
    pub index_is_compiled_default: bool,

    /// Whether packages of this registry must carry publisher identities declared in the index.
    ///
    /// One of `if_declared` (verify when the package declares publishers), `required` (refuse a package without
    /// them) or `off` (skip the check). Defaults to `if_declared`. An unknown value is treated as `required`,
    /// with one warning per run.
    ///
    /// A lock-first install in a fresh home makes no index request, so it installs without the publisher check.
    /// Set `required` to check those installs too: a package whose committed root declares no publishers is
    /// then refused (exit 81, `publishers_required`), and the error names `ocx index update`.
    #[serde(default)]
    pub signatures: Option<SignatureRequirement>,

    /// The signers allowed to sign this registry's index, in the shape of an index `publishers` array.
    ///
    /// Replaces the built-in list for the registry. Entries are kept raw here and compiled where the index is
    /// checked, so a malformed entry fails at that point.
    #[serde(default)]
    #[schemars(with = "Option<Vec<serde_json::Value>>")]
    pub index_publishers: Option<Vec<toml::Value>>,

    /// Whether this registry's index must carry a valid signature: `off`, `warn` or `enforce`.
    ///
    /// Defaults to `warn` for `ocx.sh`, which has a built-in signer list, and for any registry that sets
    /// `index_publishers`; every other registry defaults to `off`. `enforce` needs a signer list.
    ///
    /// `enforce` cannot hold for a namespace a local registry-role `[mirrors."<namespace>"]` entry pulls
    /// off its index: that combination is refused with exit 78. The table key must be spelled as clients
    /// spell the registry (lowercase, port as typed), or the entry is never applied.
    #[serde(default)]
    pub index_signature: Option<IndexSignatureMode>,

    /// Whether an expired index catalog is refused: `check` or `ignore`. Defaults to `check`.
    #[serde(default)]
    pub index_expiry: Option<IndexExpiryMode>,
}

/// The `index_signature` setting of a `[registries."<registry>"]` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexSignatureMode {
    /// Do not check the index signature.
    Off,
    /// Check it and log one warning per source on any failure, then continue.
    Warn,
    /// Check it and refuse on any failure.
    Enforce,
}

/// The `index_expiry` setting of a `[registries."<registry>"]` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexExpiryMode {
    /// Refuse a catalog past its `expires`.
    Check,
    /// Accept a catalog past its `expires`.
    Ignore,
}

/// The `signatures` setting of a `[registries."<registry>"]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignatureRequirement {
    /// Verify against the index-declared publishers when the package declares any (the default).
    IfDeclared,
    /// Refuse a package whose index declares no publishers.
    Required,
    /// Skip the publisher check.
    Off,
    /// A value from a newer ocx, kept so the warning can name it; reads as [`Self::Required`].
    #[serde(untagged)]
    #[schemars(skip)]
    Unknown(String),
}

/// The spelling clients match for the `[registries."<key>"]` key `key`, when `key` is not it.
///
/// Clients compare the registry exactly as an identifier spells it after the loader and the index sources fold
/// it: lowercase, with a numeric port in plain decimal. A key with an uppercase letter, or a port written with
/// leading zeros, never equals what a client carries, so its entry is never applied.
#[must_use]
pub fn unmatched_registry_key_spelling(key: &str) -> Option<String> {
    let lowercase = key.to_ascii_lowercase();
    let matching = match lowercase.rsplit_once(':') {
        Some((host, port)) => port
            .parse::<u16>()
            .map_or_else(|_| lowercase.clone(), |port| format!("{host}:{port}")),
        None => lowercase,
    };
    (matching != key).then_some(matching)
}

impl RegistryConfig {
    /// Mark this entry as system-locked — non-overridable by lower tiers.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Check the index-signing keys of the entry named `registry`.
    ///
    /// `has_builtin_signers` says whether ocx ships a signer list for this registry, which satisfies `enforce`.
    ///
    /// # Errors
    /// [`Error::IndexPublishersMissing`] for `index_signature = "enforce"` with no signers at all;
    /// [`Error::IndexPublishersInvalid`] for an `index_publishers` entry that is malformed (an entry from a
    /// newer ocx is not malformed, and is kept).
    pub fn validate_index_signing(&self, registry: &str, has_builtin_signers: bool) -> Result<(), Error> {
        if self.index_signature == Some(IndexSignatureMode::Enforce)
            && self.index_publishers.is_none()
            && !has_builtin_signers
        {
            return Err(Error::IndexPublishersMissing {
                registry: registry.to_owned(),
            });
        }
        let Some(entries) = self.index_publishers.as_ref() else {
            return Ok(());
        };
        let value = serde_json::to_value(entries).map_err(|error| Error::IndexPublishersInvalid {
            registry: registry.to_owned(),
            reason: error.to_string(),
        })?;
        let reasons: Vec<String> = ocx_trust::parse_publishers(&value)
            .entries
            .into_iter()
            .filter_map(|entry| match entry {
                ocx_trust::PublisherEntry::Invalid { index, reason } => Some(format!("entry {index}: {reason}")),
                _ => None,
            })
            .collect();
        // An empty list parses as one invalid entry; it is judged where it is used, as no usable signer.
        if entries.is_empty() || reasons.is_empty() {
            return Ok(());
        }
        Err(Error::IndexPublishersInvalid {
            registry: registry.to_owned(),
            reason: reasons.join("; "),
        })
    }

    /// Merge `other` into `self`: `other`'s `Some` values win, unless `self` is system-locked.
    ///
    /// The lock is adopted from `other`, since `Config::merge` folds through `or_default()`; without
    /// that, the user tier and the managed payload could override a system entry.
    pub fn merge(&mut self, other: RegistryConfig) {
        if self.system_locked {
            return;
        }
        self.system_locked = other.system_locked;
        if other.index.is_some() {
            self.index = other.index;
            // Provenance travels with the value, even when byte-identical to the compiled-in default.
            self.index_is_compiled_default = other.index_is_compiled_default;
        }
        if other.trusted_hosts.is_some() {
            self.trusted_hosts = other.trusted_hosts;
        }
        if other.insecure.is_some() {
            self.insecure = other.insecure;
        }
        if other.signatures.is_some() {
            self.signatures = other.signatures;
        }
        if other.index_publishers.is_some() {
            self.index_publishers = other.index_publishers;
        }
        if other.index_signature.is_some() {
            self.index_signature = other.index_signature;
        }
        if other.index_expiry.is_some() {
            self.index_expiry = other.index_expiry;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_key_clients_never_carry_names_the_spelling_that_matches() {
        let rows = [
            ("ocx.sh", None),
            ("localhost:5000", None),
            ("registry.corp:99999", None),
            ("OCX.SH", Some("ocx.sh")),
            ("Ghcr.io", Some("ghcr.io")),
            ("localhost:05000", Some("localhost:5000")),
            ("Registry.Corp:0443", Some("registry.corp:443")),
        ];
        for (key, expected) in rows {
            assert_eq!(unmatched_registry_key_spelling(key).as_deref(), expected, "key {key}");
        }
    }

    use super::*;

    #[test]
    fn index_signing_keys_deserialize_and_merge_last_tier_wins() {
        let mut lower: RegistryConfig = toml::from_str(
            "index_signature = \"warn\"\nindex_expiry = \"check\"\nindex_publishers = [{ kind = \"keyless\", identity = \"a\", oidc_issuer = \"i\" }]\n",
        )
        .expect("lower parses");
        assert_eq!(lower.index_signature, Some(IndexSignatureMode::Warn));
        assert_eq!(lower.index_expiry, Some(IndexExpiryMode::Check));
        assert_eq!(lower.index_publishers.as_ref().map(Vec::len), Some(1));
        let higher: RegistryConfig = toml::from_str("index_signature = \"enforce\"\n").expect("higher parses");
        lower.merge(higher);
        assert_eq!(lower.index_signature, Some(IndexSignatureMode::Enforce));
        assert_eq!(lower.index_expiry, Some(IndexExpiryMode::Check));
        assert_eq!(lower.index_publishers.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn unknown_index_signature_value_is_a_parse_error() {
        assert!(toml::from_str::<RegistryConfig>("index_signature = \"strict\"\n").is_err());
        assert!(toml::from_str::<RegistryConfig>("index_expiry = \"never\"\n").is_err());
    }

    // ── Test 3.1.2: RegistryConfig::merge None-preserves ────────────────────
    // Plan: unit test gap — lower has Some(index), higher has None → lower preserved.

    #[test]
    fn registry_config_merge_none_in_higher_does_not_clobber_lower_index() {
        // Lower config has `index` set; higher config has None for the same field.
        // After merge, lower's `index` must be preserved (None never wins).
        let mut lower = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        };
        let higher = RegistryConfig {
            index: None,
            ..Default::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.index.as_deref(),
            Some("https://index.ocx.sh"),
            "None in higher should not clobber lower's Some(index)"
        );
    }

    #[test]
    fn registry_config_merge_some_in_higher_overrides_lower_index() {
        // When higher has Some(index), it wins over lower's value.
        let mut lower = RegistryConfig {
            index: Some("https://old.example".to_string()),
            ..Default::default()
        };
        let higher = RegistryConfig {
            index: Some("https://new.example".to_string()),
            ..Default::default()
        };
        lower.merge(higher);
        assert_eq!(
            lower.index.as_deref(),
            Some("https://new.example"),
            "Some in higher should override lower's index"
        );
    }

    // ── Compiled-default provenance ──────────────────────────────────────────

    /// Writing `index` in a config file takes ownership of the value, so the
    /// compiled-default provenance does not survive — even when the string is
    /// byte-identical to the built-in one. That is what lets an operator who
    /// wants both a `[mirrors]` entry and the index path keep the index by
    /// naming it explicitly.
    #[test]
    fn a_declared_index_clears_the_compiled_default_provenance() {
        let mut builtin = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            index_is_compiled_default: true,
            signatures: None,
            ..Default::default()
        };
        builtin.merge(RegistryConfig {
            // Byte-identical to the built-in: only provenance distinguishes them.
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        });
        assert!(
            !builtin.index_is_compiled_default,
            "a file tier restating `index` must clear the compiled-default marker"
        );
    }

    /// An entry that declares only `trusted_hosts` leaves `index` — and its
    /// provenance — alone, so the namespace stays suppressible by a mirror.
    #[test]
    fn an_entry_without_index_keeps_the_compiled_default_provenance() {
        let mut builtin = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            index_is_compiled_default: true,
            signatures: None,
            ..Default::default()
        };
        builtin.merge(RegistryConfig {
            trusted_hosts: Some(vec!["registry.corp".to_string()]),
            ..Default::default()
        });
        assert!(
            builtin.index_is_compiled_default,
            "an entry that never declares `index` must not claim ownership of it"
        );
    }

    // ── RegistryConfig system lock (mirrors MirrorConfig, per registry entry) ──

    /// `lock_as_system` is unconditional — no opt-out field like
    /// `[patches].required` exists for a `[registries.<name>]` entry.
    #[test]
    fn registry_config_lock_as_system_sets_locked() {
        let mut registry = RegistryConfig {
            index: Some("https://index.ocx.sh".to_string()),
            ..Default::default()
        };
        registry.lock_as_system();
        assert!(registry.system_locked, "lock_as_system must set system_locked");
    }

    /// A system-locked `RegistryConfig` entry ignores a lower-tier override;
    /// the lock flag stays sticky after merge.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier() {
        let mut system = RegistryConfig {
            index: Some("https://system-index.corp".to_string()),
            ..Default::default()
        };
        system.lock_as_system();
        assert!(system.system_locked);

        let user = RegistryConfig {
            index: Some("https://user-index.corp".to_string()),
            ..Default::default()
        };
        system.merge(user);

        assert_eq!(
            system.index.as_deref(),
            Some("https://system-index.corp"),
            "locked system registry entry must not be redirected by a lower tier"
        );
        assert!(system.system_locked, "lock flag stays sticky after merge");
    }

    // ── `index` field TOML presence/absence (F5a) ────────────────────────────

    /// `[registries."ocx.sh"] index = "..."` parses into
    /// `RegistryConfig.index`.
    #[test]
    fn registries_table_parses_index_field_when_present() {
        let config: crate::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://index.ocx.sh\"\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert_eq!(entry.index.as_deref(), Some("https://index.ocx.sh"));
    }

    /// An entry declaring no fields at all leaves `index` absent.
    #[test]
    fn registries_table_index_field_absent_when_not_declared() {
        let config: crate::Config = toml::from_str("[registries.ghcr]\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ghcr").expect("ghcr entry must exist");
        assert!(
            entry.index.is_none(),
            "an entry declaring no fields must leave index absent"
        );
    }

    /// §6 ratified simplification: `RegistryConfig` no longer has a `url`
    /// field. It parses as any other unknown key does — ignored, with no
    /// effect on the entry — proving the alias is gone rather than merely
    /// renamed.
    #[test]
    fn registries_table_ignores_removed_url_field() {
        let config: crate::Config = toml::from_str("[registries.ghcr]\nurl = \"ghcr.io\"\n")
            .expect("a removed field must parse as an ignored unknown key");
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ghcr").expect("ghcr entry must exist");
        assert!(entry.index.is_none(), "'url' must not resurrect as another field");
    }

    /// A typo'd field name near `index` (e.g. `indx`) is ignored rather than
    /// rejected, and — the part that matters — it does NOT become `index`.
    ///
    /// The typo-rejection contract was traded away deliberately for fleet
    /// forward-compat (see [`crate::Config`]): a `[registries.<name>]`
    /// field added by a later ocx must not fail the whole config on an older
    /// binary. The cost is that this typo silently no-ops.
    #[test]
    fn registries_table_ignores_typo_of_index_field() {
        let config: crate::Config = toml::from_str("[registries.\"ocx.sh\"]\nindx = \"https://index.ocx.sh\"\n")
            .expect("a typo'd field must parse as an ignored unknown key");
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert!(
            entry.index.is_none(),
            "a typo'd key must be ignored, never absorbed into the field it resembles"
        );
    }

    // ── `trusted_hosts` SSRF escape hatch (X2) ───────────────────────────────

    /// `trusted_hosts` parses a mixed list of hostnames and CIDR blocks — the
    /// per-entry SSRF escape hatch.
    #[test]
    fn registries_table_parses_trusted_hosts_list() {
        let config: crate::Config =
            toml::from_str("[registries.\"ocx.sh\"]\ntrusted_hosts = [\"10.0.0.0/8\", \"registry.corp\"]\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert_eq!(
            entry.trusted_hosts.as_deref(),
            Some(["10.0.0.0/8".to_string(), "registry.corp".to_string()].as_slice())
        );
    }

    /// A system-locked entry ignores a lower-tier `trusted_hosts` override — the
    /// whole-entry lock covers the SSRF escape hatch too, so a lower tier can
    /// never widen a locked trust set.
    #[test]
    fn registry_config_merge_system_locked_ignores_lower_tier_trusted_hosts() {
        let mut system = RegistryConfig {
            trusted_hosts: Some(vec!["10.0.0.0/8".to_string()]),
            ..Default::default()
        };
        system.lock_as_system();

        let user = RegistryConfig {
            trusted_hosts: Some(vec!["0.0.0.0/0".to_string()]),
            ..Default::default()
        };
        system.merge(user);

        assert_eq!(
            system.trusted_hosts.as_deref(),
            Some(["10.0.0.0/8".to_string()].as_slice()),
            "a locked entry's trust set must not be widened by a lower tier"
        );
    }

    // ── `insecure` plain-HTTP opt-in ─────────────────────────────────────────

    /// `insecure = true` parses into the field; an entry that never states it
    /// stays `None`, which is what distinguishes "not stated" from an explicit
    /// `false` during the merge.
    #[test]
    fn registries_table_parses_insecure_field() {
        let config: crate::Config =
            toml::from_str("[registries.\"registry.corp:5000\"]\ninsecure = true\n[registries.ghcr]\n").unwrap();
        let registries = config.registries.expect("registries table must be present");
        assert_eq!(
            registries
                .get("registry.corp:5000")
                .expect("the declaring entry must exist")
                .insecure,
            Some(true)
        );
        assert_eq!(
            registries.get("ghcr").expect("ghcr entry must exist").insecure,
            None,
            "an entry that never states `insecure` must stay unstated, not default to false"
        );
    }

    /// A typo of `insecure` is ignored — and, security-relevant, must not turn
    /// plain HTTP on. Ignoring fails safe; absorbing would drop TLS on a typo.
    #[test]
    fn registries_table_ignores_typo_of_insecure_field() {
        let config: crate::Config = toml::from_str("[registries.\"registry.corp\"]\ninsecre = true\n")
            .expect("a typo'd field must parse as an ignored unknown key");
        let registries = config.registries.as_ref().expect("registries table must be present");
        let entry = registries.get("registry.corp").expect("entry must exist");
        assert!(
            entry.insecure.is_none(),
            "a typo'd key must not deserialize as `insecure`"
        );
        // The parse outcome is one call short of the security property. The
        // licensing decision is `insecure_hosts`'s `unwrap_or(false)`, so assert
        // it there or the test's own message is a claim it never checked.
        assert!(
            crate::insecure::insecure_hosts(&config, &[]).is_empty(),
            "a typo'd key must never license plaintext transport"
        );
    }

    /// A typo of `trusted_hosts` is ignored — and, security-relevant, does not
    /// widen the trust set: an unknown key can never populate the SSRF escape
    /// hatch. Ignoring fails safe here; absorbing would not.
    #[test]
    fn registries_table_ignores_typo_of_trusted_hosts_field() {
        let config: crate::Config = toml::from_str("[registries.\"ocx.sh\"]\ntrusted_host = [\"registry.corp\"]\n")
            .expect("a typo'd field must parse as an ignored unknown key");
        let registries = config.registries.expect("registries table must be present");
        let entry = registries.get("ocx.sh").expect("ocx.sh entry must exist");
        assert!(
            entry.trusted_hosts.is_none(),
            "a typo'd key must never widen the trust set"
        );
    }

    #[test]
    fn enforce_without_any_signer_is_refused_and_a_signer_source_satisfies_it() {
        let enforce: RegistryConfig = toml::from_str("index_signature = \"enforce\"\n").expect("parses");
        let error = enforce
            .validate_index_signing("corp.example", false)
            .expect_err("no signers");
        assert!(
            matches!(error, Error::IndexPublishersMissing { ref registry } if registry == "corp.example"),
            "{error:?}"
        );
        enforce
            .validate_index_signing("ocx.sh", true)
            .expect("the built-in list satisfies enforce");
        let with_list: RegistryConfig = toml::from_str(
            "index_signature = \"enforce\"\nindex_publishers = [{ kind = \"keyless\", identity = \"a\", oidc_issuer = \"i\" }]\n",
        )
        .expect("parses");
        with_list
            .validate_index_signing("corp.example", false)
            .expect("an explicit list satisfies enforce");
        let warn: RegistryConfig = toml::from_str("index_signature = \"warn\"\n").expect("parses");
        warn.validate_index_signing("corp.example", false)
            .expect("only enforce needs a list");
    }

    #[test]
    fn a_malformed_index_publishers_entry_is_refused_and_a_newer_kind_is_kept() {
        let validate = |entries: &str| {
            toml::from_str::<RegistryConfig>(&format!("index_publishers = {entries}\n"))
                .expect("parses")
                .validate_index_signing("corp.example", false)
        };
        let error = validate("[{ kind = \"keyless\", identity = \"a\" }]").expect_err("no oidc_issuer");
        assert!(matches!(error, Error::IndexPublishersInvalid { .. }), "{error:?}");
        let error = validate("[\"keyless\"]").expect_err("not an object");
        assert!(matches!(error, Error::IndexPublishersInvalid { .. }), "{error:?}");
        validate("[{ kind = \"future\" }]").expect("an entry from a newer ocx narrows, never refuses the file");
        validate("[]").expect("an empty list is judged where it is used, not here");
    }
}
