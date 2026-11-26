// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The resolved posture of the signed index-catalog check: the one place its inputs are gathered.

use ocx_store::file_structure::StateStore;

use crate::command::package_sign_common::resolve_public_rekor;

/// Condition 2 of the published-namespace test, without its warning: a compiled-in index that a local
/// registry-role `[mirrors]` entry pins at a mirror is suppressed, so no source is built for it.
pub(super) fn index_suppressed_by_local_mirror(
    entry: &ocx_config::RegistryConfig,
    namespace: &str,
    local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
) -> bool {
    // Only the REGISTRY role suppresses: the index role is keyed on the base's
    // own host, so `[mirrors."<ns>"] index` redirects nothing for `<ns>`, and
    // suppressing on it would leave neither the index nor the mirror. A
    // bare-string `[mirrors]` entry sets `registry`, so it counts too.
    let locally_pinned_at_a_mirror =
        local_mirrors.is_some_and(|table| table.get(namespace).is_some_and(|entry| entry.registry.is_some()));
    entry.index_is_compiled_default && locally_pinned_at_a_mirror
}

/// The one resolved decision about the signed index-catalog check: the trust inputs every source's
/// check is built from, and the `OCX_NO_VERIFY` opt-out each command resolves its flag against.
///
/// Sources are always built with their check; `--verify` / `--no-verify` ▸ `OCX_NO_VERIFY` ▸ checked
/// decides per command whether it runs, through [`Self::enabled`] and [`Self::resolved`].
#[derive(Clone)]
pub(super) struct IndexCheckPosture {
    env_opt_out: bool,
    instances: ocx_trust::SigstoreInstances,
    state: StateStore,
    rekor_cache_key: String,
}

impl IndexCheckPosture {
    pub(super) fn new(
        env_opt_out: bool,
        sigstore: Option<ocx_trust::SigstoreInstances>,
        state: StateStore,
    ) -> anyhow::Result<Self> {
        // The key names the Rekor cache a keyless check reads, so a bad URL fails the run here too.
        let rekor = resolve_public_rekor(None, sigstore.as_ref());
        let rekor_url = ocx_oci::endpoint::validate_sigstore_url(&rekor, "[sigstore] rekor_url")?;
        Ok(Self {
            env_opt_out,
            instances: sigstore.unwrap_or_default(),
            state,
            rekor_cache_key: ocx_sign::verify::trust_cache::cache_key_for_rekor(&rekor_url),
        })
    }

    /// Whether a command's check runs: its flag (`Some(true)` is `--verify`, `Some(false)` is
    /// `--no-verify`) over `OCX_NO_VERIFY`, with neither meaning checked.
    pub(super) fn enabled(&self, flag: Option<bool>) -> bool {
        flag.unwrap_or(!self.env_opt_out)
    }

    /// `checked` with the check dropped from every source when [`Self::enabled`] says the command skips it.
    ///
    /// The dropped check never touches the recorded expiry bound, since the clone it ran through has none.
    pub(super) fn resolved(&self, checked: &[ocx_index::OcxIndex], flag: Option<bool>) -> Vec<ocx_index::OcxIndex> {
        if self.enabled(flag) {
            return checked.to_vec();
        }
        checked
            .iter()
            .cloned()
            .map(ocx_index::OcxIndex::without_catalog_check)
            .collect()
    }

    /// Whether a run that skips the check owes the bypass warning, given the sources as built (`checked`) and
    /// the namespaces whose enforce refusal the skip clears (`refused`).
    ///
    /// A typed `--no-verify` drops a check that was attached, so it warns only when one was. The
    /// environment opt-out, with no flag typed, warns whenever a source exists. A typed `--verify`
    /// skips nothing.
    pub(super) fn bypass_warning_due(
        &self,
        flag: Option<bool>,
        checked: &[ocx_index::OcxIndex],
        refused: &[String],
    ) -> bool {
        bypass_warning_due(
            flag,
            self.env_opt_out,
            !refused.is_empty() || checked.iter().any(|source| source.catalog_check_mode().is_some()),
            !refused.is_empty() || !checked.is_empty(),
        )
    }

    /// The namespaces a registry-role `[mirrors."<ns>"]` entry pulled off their compiled-in index while
    /// `index_signature` is `enforce`: with no index source built, nothing would be checked, so the first
    /// read of each is refused (see [`Self::refusing_sources`]). Sorted.
    pub(super) fn enforce_through_mirror(
        config: &ocx_config::Config,
        local_mirrors: Option<&std::collections::HashMap<String, ocx_config::mirror::MirrorConfig>>,
    ) -> Vec<String> {
        let mut mirrored: Vec<String> = config
            .registries
            .iter()
            .flatten()
            .filter(|(namespace, entry)| {
                entry.index_signature == Some(ocx_config::IndexSignatureMode::Enforce)
                    && index_suppressed_by_local_mirror(entry, namespace, local_mirrors)
            })
            .map(|(namespace, _)| namespace.clone())
            .collect();
        mirrored.sort();
        mirrored
    }

    /// One refusing chain source per namespace of `refused`, for a command whose check [`Self::enabled`]
    /// runs; none when it opted out, since the opt-out covers the catalog check and this is its enforce form.
    pub(super) fn refusing_sources(&self, refused: &[String], flag: Option<bool>) -> Vec<ocx_index::Index> {
        if !self.enabled(flag) {
            return Vec::new();
        }
        refused
            .iter()
            .cloned()
            .map(ocx_index::Index::refusing_catalog_through_mirror)
            .collect()
    }

    /// `source` with the check its registry's signer list and mode call for; unchanged when the mode is `off`.
    pub(super) fn attach(
        &self,
        source: ocx_index::OcxIndex,
        namespace: &str,
        registry: Option<&ocx_config::RegistryConfig>,
    ) -> anyhow::Result<ocx_index::OcxIndex> {
        let signing = ocx_package_manager::index_trust::resolve_index_signing(namespace, registry)?;
        if signing.mode == ocx_config::IndexSignatureMode::Off {
            return Ok(source);
        }
        let trust = ocx_package_manager::index_trust::IndexTrust::new(
            namespace.to_owned(),
            signing.signers,
            signing.expiry,
            self.instances.clone(),
        )
        .with_trust_roots(ocx_package_manager::index_trust::IndexTrustRoots {
            home_root_of: Box::new(ocx_config::loader::ConfigLoader::home_sigstore_trusted_root_path_for),
            state: ocx_sign::sign::state::SigningStatePaths::new(self.state.root()),
            rekor_cache_key: self.rekor_cache_key.clone(),
            // A source exists only online.
            offline: false,
        });
        let high_water = self.state.index_catalog(namespace);
        Ok(source
            .with_expiry_high_water_file(high_water)
            .with_catalog_verifier(std::sync::Arc::new(trust), signing.mode))
    }
}

/// The pure decision behind [`IndexCheckPosture::bypass_warning_due`].
const fn bypass_warning_due(flag: Option<bool>, env_opt_out: bool, checked_sources: bool, any_sources: bool) -> bool {
    match flag {
        Some(false) => checked_sources,
        Some(true) => false,
        None => env_opt_out && any_sources,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn posture(env_opt_out: bool) -> IndexCheckPosture {
        IndexCheckPosture::new(env_opt_out, None, StateStore::new("/ocx/state")).expect("default rekor url is valid")
    }

    /// flag ▸ `OCX_NO_VERIFY` ▸ checked, every cell.
    #[test]
    fn the_flag_outranks_the_env_opt_out_which_outranks_the_default() {
        let table = [
            // (flag, env opt-out, check runs)
            (None, false, true),
            (None, true, false),
            (Some(true), false, true),
            (Some(true), true, true),
            (Some(false), false, false),
            (Some(false), true, false),
        ];
        for (flag, env_opt_out, runs) in table {
            assert_eq!(
                posture(env_opt_out).enabled(flag),
                runs,
                "flag {flag:?}, OCX_NO_VERIFY {env_opt_out}"
            );
        }
    }

    #[test]
    fn the_bypass_warning_is_due_for_the_flag_only_when_a_check_was_attached() {
        assert!(bypass_warning_due(Some(false), false, true, true));
        assert!(!bypass_warning_due(Some(false), false, false, true));
    }

    #[test]
    fn the_bypass_warning_is_due_for_the_environment_opt_out_whenever_a_source_exists() {
        assert!(bypass_warning_due(None, true, false, true));
        assert!(!bypass_warning_due(None, true, false, false));
        assert!(!bypass_warning_due(None, false, false, true));
    }

    #[test]
    fn a_typed_verify_skips_nothing_so_it_never_warns() {
        assert!(!bypass_warning_due(Some(true), true, true, true));
    }

    #[test]
    fn no_source_means_no_warning_through_the_posture() {
        assert!(!posture(true).bypass_warning_due(None, &[], &[]));
        assert!(!posture(false).bypass_warning_due(Some(false), &[], &[]));
    }

    fn config_with(mode: Option<ocx_config::IndexSignatureMode>) -> ocx_config::Config {
        let entry = ocx_config::RegistryConfig {
            index_is_compiled_default: true,
            index_signature: mode,
            ..Default::default()
        };
        ocx_config::Config {
            registries: Some([("ocx.sh".to_string(), entry)].into()),
            ..Default::default()
        }
    }

    fn registry_mirror() -> std::collections::HashMap<String, ocx_config::mirror::MirrorConfig> {
        let mirror = ocx_config::mirror::MirrorConfig {
            registry: Some("mirror.example".to_string()),
            ..Default::default()
        };
        [("ocx.sh".to_string(), mirror)].into()
    }

    #[test]
    fn an_enforce_namespace_behind_a_registry_mirror_yields_a_refusing_source_while_the_check_runs() {
        let enforce = config_with(Some(ocx_config::IndexSignatureMode::Enforce));
        let refused = IndexCheckPosture::enforce_through_mirror(&enforce, Some(&registry_mirror()));
        assert_eq!(refused, ["ocx.sh"]);
        assert_eq!(posture(false).refusing_sources(&refused, None).len(), 1);
        assert_eq!(posture(false).refusing_sources(&refused, Some(true)).len(), 1);
    }

    #[test]
    fn the_opt_out_clears_the_refusal_through_the_flag_and_through_the_environment() {
        let refused = ["ocx.sh".to_string()];
        assert!(posture(false).refusing_sources(&refused, Some(false)).is_empty());
        assert!(posture(true).refusing_sources(&refused, None).is_empty());
        assert_eq!(
            posture(true).refusing_sources(&refused, Some(true)).len(),
            1,
            "a typed --verify outranks the environment opt-out"
        );
    }

    #[test]
    fn warn_off_unset_and_an_unmirrored_namespace_refuse_nothing() {
        let mirrors = registry_mirror();
        for mode in [
            None,
            Some(ocx_config::IndexSignatureMode::Warn),
            Some(ocx_config::IndexSignatureMode::Off),
        ] {
            assert!(IndexCheckPosture::enforce_through_mirror(&config_with(mode), Some(&mirrors)).is_empty());
        }
        let enforce = config_with(Some(ocx_config::IndexSignatureMode::Enforce));
        assert!(IndexCheckPosture::enforce_through_mirror(&enforce, None).is_empty());
    }

    #[test]
    fn a_skipped_refusal_owes_the_bypass_warning() {
        let refused = ["ocx.sh".to_string()];
        assert!(posture(false).bypass_warning_due(Some(false), &[], &refused));
        assert!(posture(true).bypass_warning_due(None, &[], &refused));
        assert!(!posture(false).bypass_warning_due(Some(true), &[], &refused));
    }
}
