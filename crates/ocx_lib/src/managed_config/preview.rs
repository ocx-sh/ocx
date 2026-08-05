// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Preview leg for the managed-config tier — `ocx config test`.
//!
//! Answers the operator's pre-rollout question — *if I published this file as
//! the managed-config payload, does it parse, and what would a machine's
//! configuration look like afterwards?* — without publishing, adopting, or
//! writing anything.
//!
//! Validation is [`validate_managed_config_payload`] verbatim (the same
//! function `ocx config push` runs), so the preview can never accept a payload
//! the publish leg would reject, or vice versa.

use crate::config::Config;

use super::publish::{ManagedConfigPublishError, validate_managed_config_payload};

/// What [`preview_managed_config`] found.
#[derive(Debug)]
pub struct ManagedConfigPreview {
    /// What the machine would resolve once this payload is adopted — the
    /// candidate folded into the caller's tiers in the real adoption order.
    pub effective: Config,
    /// Dotted paths of keys the config schema ignored, e.g. `registry.defalt`
    /// or a typo'd top-level section. Sorted and deduplicated.
    ///
    /// Advisory, never a failure: an ignored key is equally a typo and a
    /// setting a newer ocx understands, and this binary cannot tell them
    /// apart. Coverage is what the derived schema sees — the
    /// `[mirrors."<host>"]` table is parsed value-first from a raw
    /// [`toml::Value`] (see [`crate::config::mirror`]), so keys inside a
    /// mirror entry are not reported.
    pub unknown_keys: Vec<String>,
}

/// Validates a candidate managed-config payload and previews the configuration
/// adopting it would produce.
///
/// The candidate takes the managed tier's place in the caller's own fold order:
/// `base` (compiled-in defaults, system, user, `$OCX_HOME`), then the
/// candidate, then `overlay` (`OCX_CONFIG` / `--config`). That is byte-for-byte
/// what [`ConfigLoader::load_with_local_view`](crate::config::loader::ConfigLoader::load_with_local_view)
/// does to a real snapshot, so an explicit overlay outranks the candidate here
/// exactly as it would after adoption.
///
/// The candidate can never carry a `[managed]` section of its own: validation
/// rejects one outright, exactly as `ocx config push` does.
///
/// # Errors
///
/// Every [`ManagedConfigPublishError`] variant
/// [`validate_managed_config_payload`] raises — oversize payload, invalid
/// TOML/UTF-8, or a `[managed]` section.
pub fn preview_managed_config(
    bytes: &[u8],
    base: Config,
    overlay: &Config,
) -> Result<ManagedConfigPreview, ManagedConfigPublishError> {
    let text = validate_managed_config_payload(bytes)?;

    // Second parse, this time through the ignored-field reporter. Validation
    // already proved the text parses, so the only reachable error here is one
    // it would have raised itself.
    let mut unknown_keys: Vec<String> = Vec::new();
    let deserializer =
        toml::Deserializer::parse(text).map_err(|source| ManagedConfigPublishError::InvalidToml { source })?;
    let candidate: Config = serde_ignored::deserialize(deserializer, |path| unknown_keys.push(dotted_path(&path)))
        .map_err(|source| ManagedConfigPublishError::InvalidToml { source })?;
    unknown_keys.sort_unstable();
    unknown_keys.dedup();

    // The adoption fold order, reproduced: base -> payload -> explicit overlay.
    // ponytail: a plain merge, not the loader's strip-then-merge fold — the
    // `[managed]` strip that fold performs is unreachable here because
    // validation already refused a payload carrying one.
    let mut effective = base;
    effective.merge(candidate);
    effective.merge(overlay.clone());

    Ok(ManagedConfigPreview {
        effective,
        unknown_keys,
    })
}

/// Renders an ignored field's path the way an operator writes it in TOML:
/// `registry.defalt`, not `serde_ignored`'s `Display`, which spells the
/// `Option` every [`Config`] section is wrapped in as a `?` segment
/// (`registry.?.defalt`).
///
/// Recursion depth is bounded by the config schema's nesting, not by the
/// payload: an unrecognized table is reported once, at the level the schema
/// stopped understanding it, and its contents are never walked.
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
            // Wrapper hops carry no name an operator ever typed.
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
