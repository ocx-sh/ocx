// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one answer to "may this registry be contacted over plain HTTP?".
//!
//! Every gate (OCI client, mirror role, index URL, `ocx login` ping) must take this one
//! union, or a host is insecure for one gate and secure for another.

use crate::Config;

/// The deduped union of config- and env-declared plain-HTTP hosts, less any host a
/// system-locked entry states `insecure = false` for (else one env var defeats the lock).
///
/// Granting matches `host[:port]` byte-exact, failing closed; revoking folds case and
/// port spelling, since strictness there fails open (`adr_managed_config_tier.md` § Rationale from code: ocx_config).
pub fn insecure_hosts(config: &Config, env: &[String]) -> Vec<String> {
    let mut hosts: Vec<String> = config
        .registries
        .iter()
        .flatten()
        .filter(|(name, registry)| registry.insecure.unwrap_or(false) && !system_locked_shut(config, name))
        .map(|(name, _)| name.clone())
        .collect();

    for host in env {
        if system_locked_shut(config, host) || hosts.contains(host) {
            continue;
        }
        hosts.push(host.clone());
    }
    hosts
}

/// Whether the system scope declared this name plaintext-forbidden.
///
/// A linear scan, not a map lookup: the map is keyed on the verbatim name and this match folds case.
fn system_locked_shut(config: &Config, host: &str) -> bool {
    config
        .registries
        .iter()
        .flatten()
        .any(|(name, entry)| entry.system_locked && entry.insecure == Some(false) && same_registry_name(name, host))
}

/// Whether two `host[:port]` names denote the same socket (revoking side only): case folded,
/// numeric port re-rendered, since `url` dials `:05000` on TCP 5000.
///
/// A non-`u16` port is not normalised, or `registry.corp:99999` would revoke `registry.corp`.
/// No default-port fill: the four gates span two schemes, so any default is wrong for some.
fn same_registry_name(left: &str, right: &str) -> bool {
    fn normalize(name: &str) -> String {
        match name.rsplit_once(':') {
            Some((host, port)) => port.parse::<u16>().map_or_else(
                |_| name.to_ascii_lowercase(),
                |port| format!("{}:{port}", host.to_ascii_lowercase()),
            ),
            None => name.to_ascii_lowercase(),
        }
    }
    normalize(left) == normalize(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RegistryConfig;
    use std::collections::HashMap;

    fn config_with(entries: &[(&str, Option<bool>)]) -> Config {
        let registries = entries
            .iter()
            .map(|(name, insecure)| {
                (
                    (*name).to_string(),
                    RegistryConfig {
                        insecure: *insecure,
                        ..Default::default()
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        Config {
            registries: Some(registries),
            ..Default::default()
        }
    }

    /// One `[registries.<name>]` entry as the SYSTEM scope would leave it:
    /// `lock_as_system` is what the loader calls on every entry of
    /// `/etc/ocx/config.toml`, so the flag is set the same way here.
    fn system_config_with(name: &str, insecure: Option<bool>) -> Config {
        let mut entry = RegistryConfig {
            insecure,
            ..Default::default()
        };
        entry.lock_as_system();
        Config {
            registries: Some(HashMap::from([(name.to_string(), entry)])),
            ..Default::default()
        }
    }

    /// A system lock plus a lower tier's entry under a DIFFERENT map key — the
    /// shape `RegistryConfig::merge` never sees, because the two names never
    /// collide in the `HashMap` and so the locked entry's early return never runs.
    fn system_locked_plus_lower_tier(locked: &str, lower: &str, lower_insecure: bool) -> Config {
        let mut locked_entry = RegistryConfig {
            insecure: Some(false),
            ..Default::default()
        };
        locked_entry.lock_as_system();
        Config {
            registries: Some(HashMap::from([
                (locked.to_string(), locked_entry),
                (
                    lower.to_string(),
                    RegistryConfig {
                        insecure: Some(lower_insecure),
                        ..Default::default()
                    },
                ),
            ])),
            ..Default::default()
        }
    }

    #[test]
    fn only_entries_declaring_true_are_insecure() {
        let config = config_with(&[
            ("plain.corp:5000", Some(true)),
            ("secure.corp", Some(false)),
            ("unstated.corp", None),
        ]);

        let hosts = insecure_hosts(&config, &[]);

        assert_eq!(hosts, vec!["plain.corp:5000".to_string()]);
    }

    #[test]
    fn config_and_env_union_without_duplicates() {
        let config = config_with(&[("plain.corp:5000", Some(true))]);
        let env = vec!["plain.corp:5000".to_string(), "localhost:5001".to_string()];

        let hosts = insecure_hosts(&config, &env);

        assert_eq!(hosts, vec!["plain.corp:5000".to_string(), "localhost:5001".to_string()]);
    }

    /// An ORDINARY tier saying `insecure = false` revokes nothing: the union is
    /// additive everywhere the system scope has not spoken, so the answer never
    /// depends on which source was consulted last.
    #[test]
    fn an_unlocked_false_cannot_revoke_an_env_declared_host() {
        let config = config_with(&[("plain.corp:5000", Some(false))]);
        let env = vec!["plain.corp:5000".to_string()];

        assert_eq!(insecure_hosts(&config, &env), vec!["plain.corp:5000".to_string()]);
    }

    /// The one subtraction. A system-scope entry that *states* `insecure = false`
    /// is the platform engineer's lever, and it has to bind the environment or it
    /// is not a lever at all — `RegistryConfig::merge`'s per-entry lock already
    /// stops every lower CONFIG tier, and the env var is the only way left in.
    ///
    /// Asserted as a pair with the unlocked case above: dropping the
    /// `system_locked` half of the predicate leaves this one red, dropping the
    /// `Some(false)` half leaves the `None` case below red.
    #[test]
    fn a_system_locked_false_subtracts_the_host_from_the_environment() {
        let config = system_config_with("locked.corp:5000", Some(false));
        let env = vec!["locked.corp:5000".to_string()];

        assert!(
            insecure_hosts(&config, &env).is_empty(),
            "a system-locked `insecure = false` must forbid plaintext for that host, env included"
        );
    }

    /// Silence is not a decision: a locked entry that never states `insecure`
    /// has said nothing about transport, so it subtracts nothing.
    #[test]
    fn a_system_locked_entry_that_never_states_insecure_subtracts_nothing() {
        let config = system_config_with("locked.corp:5000", None);
        let env = vec!["locked.corp:5000".to_string()];

        assert_eq!(
            insecure_hosts(&config, &env),
            vec!["locked.corp:5000".to_string()],
            "`None` is silence, not a refusal — only an explicit `false` locks a host shut"
        );
    }

    /// The revoke direction folds ASCII case, because exactness there fails
    /// OPEN: hostnames are case-insensitive and DNS lands every spelling on the
    /// same socket, so a byte-exact revoke lets one changed letter in an
    /// environment variable walk past the only lever the design gives a
    /// platform engineer (CWE-319/CWE-522).
    ///
    /// Paired with `mixed_case_grants_nothing_even_when_a_lock_would_fold_to_it`
    /// below: this asserts the fold happens on the revoking side, that one
    /// asserts it does NOT happen on the granting side. Either alone would also
    /// pass with the fold applied everywhere, which is the wrong fix.
    #[test]
    fn a_system_lock_revokes_a_differently_cased_environment_entry() {
        let config = system_config_with("registry.corp:5000", Some(false));

        for spelling in ["Registry.Corp:5000", "REGISTRY.CORP:5000", "registry.CORP:5000"] {
            assert!(
                insecure_hosts(&config, &[spelling.to_string()]).is_empty(),
                "`{spelling}` must not slip past a lowercase system lock — DNS reaches the same host"
            );
        }
    }

    /// The same fold on the other half of the union. A lower tier's
    /// `[registries."Registry.Corp:5000"] insecure = true` is a DIFFERENT
    /// `HashMap` key from the system tier's lowercase entry, so
    /// `RegistryConfig::merge` never folds the two together and its per-entry
    /// `system_locked` early return never runs — the subtraction is the only
    /// thing standing between that entry and a plaintext session.
    #[test]
    fn a_system_lock_revokes_a_differently_cased_config_entry_from_a_lower_tier() {
        let config = system_locked_plus_lower_tier("registry.corp:5000", "Registry.Corp:5000", true);

        assert!(
            insecure_hosts(&config, &[]).is_empty(),
            "a lower tier must not re-grant a locked host by re-spelling its case"
        );
    }

    /// The premise the port half of [`same_registry_name`] rests on, asserted
    /// rather than assumed: `url` parses the port numerically and re-renders it,
    /// so a zero-padded spelling is not a different endpoint that merely looks
    /// alike — it is a different SPELLING of the same socket, and the whole
    /// bypass below follows from that. If this ever stops holding, the
    /// normalisation is over-subtraction and should be revisited, not kept.
    #[test]
    fn a_zero_padded_port_is_the_same_socket_as_the_bare_one() {
        let padded = reqwest::Url::parse("http://registry.corp:05000/v2/").expect("valid url");

        assert_eq!(padded.port(), Some(5000), "url must parse the port numerically");
        assert_eq!(
            padded.as_str(),
            "http://registry.corp:5000/v2/",
            "url must re-render the canonical port, which is what the socket then uses"
        );
    }

    /// The bypass: a system lock on `registry.corp:5000`, and an unprivileged
    /// `OCX_INSECURE_REGISTRIES=registry.corp:05000`. Nothing between the env
    /// var and the socket re-spells the port — `split_registry_repository` takes
    /// the first path segment verbatim, and the granting comparison is
    /// byte-exact by design — so before the port normalisation the host was
    /// granted plaintext and `url` then dialled the very port the operator
    /// locked shut, at the cost of one character (CWE-319).
    #[test]
    fn a_system_lock_revokes_a_zero_padded_environment_port() {
        let config = system_config_with("registry.corp:5000", Some(false));

        for spelling in ["registry.corp:05000", "registry.corp:0005000", "Registry.Corp:05000"] {
            assert!(
                insecure_hosts(&config, &[spelling.to_string()]).is_empty(),
                "`{spelling}` reaches TCP 5000, so it must not slip past a lock on `registry.corp:5000`"
            );
        }
    }

    /// A port no `u16` accepts is not silently treated as portless — that would
    /// let one unparseable name subtract a DIFFERENT host. It falls back to the
    /// case-folded whole string: it matches its own spelling, and nothing else.
    #[test]
    fn an_unparseable_port_matches_only_its_own_spelling() {
        let locked_bare = system_config_with("registry.corp", Some(false));
        for spelling in ["registry.corp:99999", "registry.corp:abc", "registry.corp:5000"] {
            assert_eq!(
                insecure_hosts(&locked_bare, &[spelling.to_string()]),
                vec![spelling.to_string()],
                "a lock on the bare host must not reach `{spelling}` — it names a port, the lock does not"
            );
        }

        let locked_junk = system_config_with("registry.corp:abc", Some(false));
        assert!(
            insecure_hosts(&locked_junk, &["registry.corp:ABC".to_string()]).is_empty(),
            "case folding still applies to a name whose port cannot be normalised"
        );
        assert_eq!(
            insecure_hosts(&locked_junk, &["registry.corp".to_string()]),
            vec!["registry.corp".to_string()],
            "an unparseable port must not collapse to the bare host and revoke it"
        );
    }

    /// Granting stays byte-exact even where a revoke would have folded, so the
    /// asymmetry is real rather than a fold that leaked into both directions.
    /// Being stricter than the transport here fails CLOSED — the entry licenses
    /// nothing, and nothing is the safe answer.
    #[test]
    fn mixed_case_grants_nothing_even_when_a_lock_would_fold_to_it() {
        let config = config_with(&[("Registry.Corp:5000", Some(true))]);

        assert_eq!(
            insecure_hosts(&config, &[]),
            vec!["Registry.Corp:5000".to_string()],
            "the set carries the key verbatim; the gate below is what refuses it"
        );
        assert!(
            !ocx_oci::ssrf::allows_plain_http(&insecure_hosts(&config, &[]), "registry.corp:5000"),
            "a mis-cased entry must not license the lowercase name the transport will ask about"
        );
    }

    /// Matching is exact, asserted where the exactness is *consumed*: a bare
    /// host does not license the same host on a port, and case is not folded.
    /// The predicate's own return value cannot show this — a set built from a
    /// disjoint literal never contains the literal it was not built from — so
    /// the assertion has to run through a gate.
    #[test]
    fn host_and_port_are_one_opaque_name() {
        let bare = insecure_hosts(&config_with(&[("registry.corp", Some(true))]), &[]);
        let ported = insecure_hosts(&config_with(&[("registry.corp:5001", Some(true))]), &[]);
        let mixed_case = insecure_hosts(&config_with(&[("Registry.Corp:5001", Some(true))]), &[]);

        let gate = |hosts: &[String]| {
            crate::mirror::resolve_mirror_map(
                &Config::default(),
                vec![(
                    "up.example".to_string(),
                    crate::MirrorConfig {
                        registry: Some("http://registry.corp:5001".to_string()),
                        ..Default::default()
                    },
                )],
                hosts,
            )
        };

        assert!(
            matches!(
                gate(&bare),
                Err(crate::mirror::MirrorConfigError::PlainHttpMirrorNotAllowed { .. })
            ),
            "an entry for the bare host must not license the same host on a port"
        );
        assert!(
            matches!(
                gate(&mixed_case),
                Err(crate::mirror::MirrorConfigError::PlainHttpMirrorNotAllowed { .. })
            ),
            "the comparison is byte-exact — a differently-cased name licenses nothing"
        );
        assert!(gate(&ported).is_ok(), "the exact `host:port` name must license it");
    }

    #[test]
    fn no_registries_table_is_just_the_environment() {
        let config = Config::default();

        assert_eq!(
            insecure_hosts(&config, &["localhost:5000".to_string()]),
            vec!["localhost:5000".to_string()]
        );
    }
}
