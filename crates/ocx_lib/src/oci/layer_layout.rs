// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-layer placement config carried in a manifest layer descriptor's
//! `annotations` (`sh.ocx.layer.*`).
//!
//! This is the OCI read/write boundary for per-layer strip + output prefix.
//! It resolves an untrusted set of annotations into the utility-local
//! [`LayerPlacement`] that the assembler consumes, so `utility/fs` never
//! depends on `oci` (DIP — see `arch-principles.md`). The reverse direction —
//! [`LayerLayoutSpec::to_annotations`] — emits keys **only** when the publisher
//! explicitly set a field, keeping the default publish path byte-identical
//! (BC2).

use std::collections::BTreeMap;

use crate::utility::fs::LayerPlacement;
use crate::utility::fs::path::{PathEscapeError, RelativePath};

/// Publish-side layout spec. Remembers which fields the publisher set so
/// [`to_annotations`](Self::to_annotations) emits only those keys — a package
/// published with no layout produces `descriptor.annotations = None`, hence a
/// byte-identical default manifest (BC2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LayerLayoutSpec {
    /// Leading path components to strip from this layer at assemble time.
    pub strip: Option<u8>,
    /// Output prefix under which this layer is placed (bounded, non-escaping).
    pub prefix: Option<RelativePath>,
}

impl LayerLayoutSpec {
    /// Renders this spec as a deterministic annotation map, or `None` when no
    /// field is set.
    ///
    /// Emits `sh.ocx.layer.strip-components` (decimal) only when `strip` is
    /// `Some`, and `sh.ocx.layer.prefix` only when `prefix` is `Some`. Both
    /// unset ⇒ `None`, which drives `descriptor.annotations = None` — the
    /// default publish path stays byte-identical (BC2).
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

    /// Returns `true` when no layout field is set (the default spec).
    pub fn is_empty(&self) -> bool {
        self.strip.is_none() && self.prefix.is_none()
    }
}

/// Error resolving an untrusted layer-descriptor annotation into a
/// [`LayerPlacement`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayerLayoutError {
    /// The `sh.ocx.layer.strip-components` annotation is not a valid `u8`.
    #[error("layer strip-components annotation is not a u8: {0}")]
    BadStrip(String),
    /// The `sh.ocx.layer.prefix` annotation is not a valid bounded relative
    /// path.
    #[error("layer prefix annotation is invalid")]
    BadPrefix(#[source] PathEscapeError),
}

impl crate::cli::ClassifyExitCode for LayerLayoutError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        // A malformed/hostile manifest annotation read from an untrusted
        // registry is bad input data (65).
        Some(crate::cli::ExitCode::DataError)
    }
}

/// Maximum length of an untrusted annotation value echoed into an error message.
const MAX_ECHOED_ANNOTATION_CHARS: usize = 32;

/// Sanitizes an untrusted annotation value for safe inclusion in an error
/// message that reaches logs (CWE-117).
///
/// The manifest is third-party-writable, so a raw value could carry newlines
/// (log-injection) or be arbitrarily long (log bloat). Control characters are
/// dropped and the result is truncated to [`MAX_ECHOED_ANNOTATION_CHARS`].
fn sanitize_annotation_value(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(MAX_ECHOED_ANNOTATION_CHARS)
        .collect()
}

/// Resolves per-layer placement from a layer descriptor's annotations, applying
/// the fallback chain `annotation → bundle default → 0` for strip and
/// `annotation → "" (root)` for prefix (BC1).
///
/// Registries are third-party-writable, so the prefix annotation is re-validated
/// here (D10) rather than trusted. Returns the utility-local [`LayerPlacement`]
/// so no `oci` type crosses into `utility/fs` (W2).
///
/// # Errors
///
/// Returns [`LayerLayoutError`] when the strip annotation is not a `u8`
/// ([`LayerLayoutError::BadStrip`]) or the prefix annotation escapes / is
/// over-long ([`LayerLayoutError::BadPrefix`]).
pub fn resolve_layer_placement(
    annotations: Option<&BTreeMap<String, String>>,
    bundle_default: Option<u8>,
) -> Result<LayerPlacement, LayerLayoutError> {
    // strip = annotation ?? bundle default ?? 0. An annotation that is present
    // but not a valid `u8` is a hard error (registries are untrusted, D10) —
    // it does not silently fall back to the bundle default.
    let strip = match annotations.and_then(|a| a.get(super::annotations::LAYER_STRIP_COMPONENTS)) {
        Some(raw) => raw
            .parse::<u8>()
            .map_err(|_| LayerLayoutError::BadStrip(sanitize_annotation_value(raw)))?,
        None => bundle_default.unwrap_or(0),
    };

    // prefix = annotation ?? "" (root). The annotation is re-validated here even
    // though publish also validated it — the manifest is third-party-writable
    // (D10). Unknown `sh.ocx.layer.*` keys are ignored (OQ2 forward-compat).
    let prefix = match annotations.and_then(|a| a.get(super::annotations::LAYER_PREFIX)) {
        Some(raw) => RelativePath::parse(raw).map_err(LayerLayoutError::BadPrefix)?,
        None => RelativePath::default(),
    };

    Ok(LayerPlacement { strip, prefix })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::annotations::{LAYER_PREFIX, LAYER_STRIP_COMPONENTS};

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
