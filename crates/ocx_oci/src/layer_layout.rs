// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-layer placement config carried in a manifest layer descriptor's
//! `annotations` (`sh.ocx.layer.*`).

use std::collections::BTreeMap;

use ocx_util::fs::path::{LayerPlacement, PathEscapeError, RelativePath};

/// Publish-side layout spec; `None` fields emit no annotation key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayerLayoutSpec {
    pub strip: Option<u8>,
    pub prefix: Option<RelativePath>,
}

impl LayerLayoutSpec {
    /// Renders this spec as an annotation map, or `None` when no field is set.
    ///
    /// Emits only the keys set, or a default publish stops producing a byte-identical manifest.
    pub fn to_annotations(&self) -> Option<BTreeMap<String, String>> {
        if self.strip.is_none() && self.prefix.is_none() {
            return None;
        }
        let mut map = BTreeMap::new();
        if let Some(strip) = self.strip {
            map.insert(
                super::annotations::LAYER_STRIP_COMPONENTS.to_string(),
                strip.to_string(),
            );
        }
        if let Some(prefix) = &self.prefix {
            map.insert(super::annotations::LAYER_PREFIX.to_string(), prefix.to_wire());
        }
        Some(map)
    }

    pub fn is_empty(&self) -> bool {
        self.strip.is_none() && self.prefix.is_none()
    }
}

/// Error resolving an untrusted layer-descriptor annotation into a
/// [`LayerPlacement`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayerLayoutError {
    #[error("layer strip-components annotation is not a u8: {0}")]
    BadStrip(String),
    #[error("layer prefix annotation is invalid")]
    BadPrefix(#[source] PathEscapeError),
}

const MAX_ECHOED_ANNOTATION_CHARS: usize = 32;

/// Strips control characters and truncates, or a third-party manifest injects lines into logs (CWE-117).
fn sanitize_annotation_value(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_ECHOED_ANNOTATION_CHARS)
        .collect()
}

/// Resolves per-layer placement from a layer descriptor's annotations: strip
/// falls back to `bundle_default` then 0, prefix to the root.
///
/// # Errors
///
/// [`LayerLayoutError::BadStrip`] when the strip annotation is not a `u8`;
/// [`LayerLayoutError::BadPrefix`] when the prefix escapes or is over-long.
pub fn resolve_layer_placement(
    annotations: Option<&BTreeMap<String, String>>,
    bundle_default: Option<u8>,
) -> Result<LayerPlacement, LayerLayoutError> {
    // A present-but-invalid annotation errors rather than falling back, or a
    // tampered manifest silently changes the extracted layout.
    let strip = match annotations.and_then(|a| a.get(super::annotations::LAYER_STRIP_COMPONENTS)) {
        Some(raw) => raw
            .parse::<u8>()
            .map_err(|_| LayerLayoutError::BadStrip(sanitize_annotation_value(raw)))?,
        None => bundle_default.unwrap_or(0),
    };

    // Re-validated although publish validated it: the registry copy is
    // third-party-writable, and an unchecked prefix escapes the package root.
    let prefix = match annotations.and_then(|a| a.get(super::annotations::LAYER_PREFIX)) {
        Some(raw) => RelativePath::parse(raw).map_err(LayerLayoutError::BadPrefix)?,
        None => RelativePath::default(),
    };

    Ok(LayerPlacement { strip, prefix })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{LAYER_PREFIX, LAYER_STRIP_COMPONENTS};

    /// Builds an annotation map from `(key, value)` pairs.
    fn annotations(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ── resolve_layer_placement (U16, U17) ───────────────────────────────────
    //
    // Cover the strip/prefix fallback chain and rejection of malformed
    // (untrusted) annotations at the read boundary.

    /// U16 (BC1 · D3): the strip fallback chain is
    /// `annotation → bundle default → 0`; an absent prefix annotation resolves to
    /// the empty (root) prefix; an annotation strip overrides the bundle default.
    #[test]
    fn resolve_fallback_chain() {
        // No annotations, no bundle default → strip 0, empty prefix (BC1).
        let placement = resolve_layer_placement(None, None).expect("resolves with no inputs");
        assert_eq!(placement.strip, 0, "no annotation, no bundle default → 0");
        assert!(placement.prefix.is_empty(), "absent prefix annotation → root");

        // No annotations, bundle default present → bundle default strip.
        let placement = resolve_layer_placement(None, Some(2)).expect("resolves with bundle default");
        assert_eq!(placement.strip, 2, "bundle default applies when no annotation");
        assert!(placement.prefix.is_empty());

        // Annotation strip overrides the bundle default.
        let with_strip = annotations(&[(LAYER_STRIP_COMPONENTS, "5")]);
        let placement = resolve_layer_placement(Some(&with_strip), Some(2)).expect("resolves");
        assert_eq!(placement.strip, 5, "annotation strip wins over bundle default");

        // Prefix annotation resolves into the placement prefix.
        let with_prefix = annotations(&[(LAYER_PREFIX, "share")]);
        let placement = resolve_layer_placement(Some(&with_prefix), None).expect("resolves");
        assert_eq!(placement.strip, 0);
        assert_eq!(placement.prefix.as_path(), std::path::Path::new("share"));
    }

    /// U17 (error · D10): a malformed strip or prefix annotation is rejected —
    /// registries are untrusted, so the read boundary re-validates.
    #[test]
    fn resolve_rejects_bad_annotations() {
        let non_numeric = annotations(&[(LAYER_STRIP_COMPONENTS, "notanumber")]);
        assert!(
            matches!(
                resolve_layer_placement(Some(&non_numeric), None),
                Err(LayerLayoutError::BadStrip(_))
            ),
            "a non-numeric strip annotation must be BadStrip"
        );

        let over_u8 = annotations(&[(LAYER_STRIP_COMPONENTS, "999")]);
        assert!(
            matches!(
                resolve_layer_placement(Some(&over_u8), None),
                Err(LayerLayoutError::BadStrip(_))
            ),
            "a >u8 strip annotation must be BadStrip"
        );

        let escaping = annotations(&[(LAYER_PREFIX, "../evil")]);
        assert!(
            matches!(
                resolve_layer_placement(Some(&escaping), None),
                Err(LayerLayoutError::BadPrefix(_))
            ),
            "an escaping prefix annotation must be BadPrefix"
        );
    }

    /// CWE-117: a hostile strip annotation carrying newlines and excess length
    /// is sanitized before it reaches the error message — control characters are
    /// dropped and the echoed value is truncated, so a third-party manifest
    /// cannot inject log lines or bloat diagnostics.
    #[test]
    fn resolve_bad_strip_sanitizes_untrusted_value() {
        let hostile = format!("not\na\rnumber{}", "x".repeat(200));
        let map = annotations(&[(LAYER_STRIP_COMPONENTS, hostile.as_str())]);
        let LayerLayoutError::BadStrip(echoed) = resolve_layer_placement(Some(&map), None).unwrap_err() else {
            panic!("a non-numeric strip annotation must be BadStrip");
        };
        assert!(
            !echoed.contains('\n') && !echoed.contains('\r'),
            "control characters must be stripped, got {echoed:?}"
        );
        assert!(
            echoed.chars().count() <= MAX_ECHOED_ANNOTATION_CHARS,
            "the echoed value must be truncated, got {echoed:?}"
        );
    }

    // ── to_annotations (U18, U19) — GREEN (implemented in the stub) ──────────

    /// U18 (BC2): the default spec (both fields None) emits `None`, which drives
    /// `descriptor.annotations = None` — the byte-identical default publish path.
    #[test]
    fn to_annotations_default_is_none() {
        assert!(LayerLayoutSpec::default().to_annotations().is_none());
        assert!(
            LayerLayoutSpec {
                strip: None,
                prefix: None
            }
            .to_annotations()
            .is_none()
        );
    }

    /// U19 (BC2, strip half): a strip-only spec emits ONLY the strip-components
    /// key. The prefix half is covered by
    /// `to_annotations_prefix_only_emits_prefix_key` below.
    #[test]
    fn to_annotations_strip_only_emits_strip_key() {
        let map = LayerLayoutSpec {
            strip: Some(1),
            prefix: None,
        }
        .to_annotations()
        .expect("strip-only spec emits a map");
        assert_eq!(map.len(), 1, "only the strip key is emitted");
        assert_eq!(map.get(LAYER_STRIP_COMPONENTS).map(String::as_str), Some("1"));
        assert!(!map.contains_key(LAYER_PREFIX), "no prefix key when prefix is None");
    }

    /// U19 (BC2, prefix half): a prefix-only spec emits ONLY the prefix key.
    #[test]
    fn to_annotations_prefix_only_emits_prefix_key() {
        let prefix = RelativePath::parse("share").expect("prefix parses");
        let map = LayerLayoutSpec {
            strip: None,
            prefix: Some(prefix),
        }
        .to_annotations()
        .expect("prefix-only spec emits a map");
        assert_eq!(map.len(), 1, "only the prefix key is emitted");
        assert_eq!(map.get(LAYER_PREFIX).map(String::as_str), Some("share"));
        assert!(
            !map.contains_key(LAYER_STRIP_COMPONENTS),
            "no strip key when strip is None"
        );
    }
}
