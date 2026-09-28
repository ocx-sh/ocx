// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Preview leg for the managed-config tier — `ocx config test`; writes nothing.
//!
//! Validation must stay [`validate_managed_config_payload`] itself, or preview and
//! `ocx config push` disagree on which payloads they accept.

use ocx_config::Config;

use super::publish::{ManagedConfigPublishError, validate_managed_config_payload};

/// What [`preview_managed_config`] found.
#[derive(Debug)]
pub struct ManagedConfigPreview {
    /// What the machine would resolve once this payload is adopted.
    pub effective: Config,
    /// Sorted, deduplicated dotted paths of keys the schema ignored, e.g. `registry.defalt`.
    ///
    /// Advisory only: an ignored key may be a setting a newer ocx understands. Keys
    /// inside a `[mirrors."<host>"]` entry are not reported (parsed from a raw value).
    pub unknown_keys: Vec<String>,
}

/// Validates a candidate managed-config payload and previews the configuration
/// adopting it would produce: `base`, then the candidate, then `overlay` (`OCX_CONFIG` / `--config`).
///
/// # Errors
///
/// Whatever [`validate_managed_config_payload`] raises: oversize payload, invalid
/// TOML/UTF-8, or a `[managed]` section.
pub fn preview_managed_config(
    bytes: &[u8],
    base: Config,
    overlay: &Config,
) -> Result<ManagedConfigPreview, ManagedConfigPublishError> {
    let text = validate_managed_config_payload(bytes)?;

    let mut unknown_keys: Vec<String> = Vec::new();
    let deserializer =
        toml::Deserializer::parse(text).map_err(|source| ManagedConfigPublishError::InvalidToml { source })?;
    let candidate: Config = serde_ignored::deserialize(deserializer, |path| unknown_keys.push(dotted_path(&path)))
        .map_err(|source| ManagedConfigPublishError::InvalidToml { source })?;
    unknown_keys.sort_unstable();
    unknown_keys.dedup();

    // ponytail: plain merge, not the loader's `[managed]`-stripping fold; validation already refused `[managed]`.
    let mut effective = base;
    effective.merge(candidate);
    effective.merge(overlay.clone());

    Ok(ManagedConfigPreview {
        effective,
        unknown_keys,
    })
}

/// Renders an ignored path as TOML spells it (`registry.defalt`), not `serde_ignored`'s
/// `Display`, which adds a `?` segment per `Option` wrapper (`registry.?.defalt`).
fn dotted_path(path: &serde_ignored::Path<'_>) -> String {
    fn collect(path: &serde_ignored::Path<'_>, segments: &mut Vec<String>) {
        match path {
            serde_ignored::Path::Root => {}
            serde_ignored::Path::Seq { parent, index } => {
                collect(parent, segments);
                segments.push(index.to_string());
            }
            serde_ignored::Path::Map { parent, key } => {
                collect(parent, segments);
                segments.push(key.clone());
            }
            serde_ignored::Path::Some { parent }
            | serde_ignored::Path::NewtypeStruct { parent }
            | serde_ignored::Path::NewtypeVariant { parent } => collect(parent, segments),
        }
    }

    let mut segments = Vec::new();
    collect(path, &mut segments);
    segments.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(payload: &str) -> ManagedConfigPreview {
        preview_managed_config(payload.as_bytes(), Config::default(), &Config::default()).expect("payload must preview")
    }

    fn parse(toml_text: &str) -> Config {
        toml::from_str(toml_text).expect("test config must parse")
    }

    #[test]
    fn candidate_value_lands_in_the_effective_config() {
        let result = preview("[registry]\ndefault = \"corp.example.com\"\n");
        assert_eq!(result.effective.resolved_default_registry(), Some("corp.example.com"));
        assert!(result.unknown_keys.is_empty());
    }

    #[test]
    fn local_value_the_candidate_does_not_set_survives() {
        let base = parse("[registry]\ndefault = \"machine.example\"\n");
        let result = preview_managed_config(
            b"[patches]\nregistry = \"corp.example.com/patches\"\n",
            base,
            &Config::default(),
        )
        .expect("payload must preview");
        assert_eq!(result.effective.resolved_default_registry(), Some("machine.example"));
        assert_eq!(
            result.effective.patches.and_then(|patches| patches.registry).as_deref(),
            Some("corp.example.com/patches")
        );
    }

    #[test]
    fn candidate_overrides_the_local_value() {
        let base = parse("[registry]\ndefault = \"machine.example\"\n");
        let result = preview_managed_config(
            b"[registry]\ndefault = \"corp.example.com\"\n",
            base,
            &Config::default(),
        )
        .expect("payload must preview");
        assert_eq!(result.effective.resolved_default_registry(), Some("corp.example.com"));
    }

    /// The explicit overlay outranks the candidate, mirroring the tier order
    /// `load_with_local_view` applies to a real managed snapshot.
    #[test]
    fn overlay_outranks_the_candidate() {
        let base = parse("[registry]\ndefault = \"machine.example\"\n");
        let overlay = parse("[registry]\ndefault = \"overlay.example\"\n");
        let result = preview_managed_config(b"[registry]\ndefault = \"corp.example.com\"\n", base, &overlay)
            .expect("payload must preview");
        assert_eq!(result.effective.resolved_default_registry(), Some("overlay.example"));
    }

    /// ...but only for keys the overlay actually sets, so the candidate is not
    /// simply discarded whenever an overlay exists.
    #[test]
    fn candidate_wins_where_the_overlay_is_silent() {
        let overlay = parse("[registry]\ndefault = \"overlay.example\"\n");
        let result = preview_managed_config(
            b"[patches]\nregistry = \"corp.example.com/patches\"\n",
            Config::default(),
            &overlay,
        )
        .expect("payload must preview");
        assert_eq!(result.effective.resolved_default_registry(), Some("overlay.example"));
        assert_eq!(
            result.effective.patches.and_then(|patches| patches.registry).as_deref(),
            Some("corp.example.com/patches")
        );
    }

    /// A typo'd section and a typo'd key inside a known section are both
    /// reported by their dotted path — the loader silently ignores both.
    #[test]
    fn unknown_keys_are_reported_by_dotted_path() {
        let result = preview("[patchs]\nregistry = \"x\"\n\n[registry]\ndefalt = \"y\"\n");
        assert_eq!(
            result.unknown_keys,
            vec!["patchs".to_string(), "registry.defalt".to_string()]
        );
        assert_eq!(
            result.effective.resolved_default_registry(),
            None,
            "an ignored key must contribute nothing to the effective config"
        );
    }

    /// An unknown key is advisory, not a rejection: the payload still previews
    /// and everything the schema does recognize takes effect.
    #[test]
    fn an_unknown_key_does_not_fail_the_preview() {
        let result = preview("[registry]\ndefault = \"corp.example.com\"\ntimeuot = 30\n");
        assert_eq!(result.unknown_keys, vec!["registry.timeuot".to_string()]);
        assert_eq!(result.effective.resolved_default_registry(), Some("corp.example.com"));
    }

    /// The rejection set is the publish leg's, unchanged — the two commands
    /// cannot disagree about what is publishable.
    #[test]
    fn rejections_match_the_publish_validator() {
        let managed = preview_managed_config(
            b"[managed]\nsource = \"corp/ocx-config:user\"\n",
            Config::default(),
            &Config::default(),
        )
        .expect_err("[managed] must be rejected");
        assert!(matches!(managed, ManagedConfigPublishError::ContainsManagedSection));

        let broken = preview_managed_config(b"not = [valid", Config::default(), &Config::default())
            .expect_err("invalid TOML must be rejected");
        assert!(matches!(broken, ManagedConfigPublishError::InvalidToml { .. }));

        let oversize = "# padding\n".repeat(7_000);
        let too_large = preview_managed_config(oversize.as_bytes(), Config::default(), &Config::default())
            .expect_err("oversize must be rejected");
        assert!(matches!(too_large, ManagedConfigPublishError::PayloadTooLarge { .. }));
    }
}
