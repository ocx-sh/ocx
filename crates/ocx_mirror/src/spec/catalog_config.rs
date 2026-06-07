// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Catalog publishing configuration for the mirror tool.
//!
//! `pipeline describe` reads the optional `catalog:` block to discover the
//! README and logo files to publish to the registry as catalog metadata
//! (the `__ocx.desc` referrer tag).
//!
//! When the block is omitted, defaults pick up `CATALOG.md` and probe for
//! `logo.svg` then `logo.png` relative to the spec file.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Catalog publishing settings.
///
/// All fields are optional; sensible defaults apply when omitted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CatalogConfig {
    /// Path to the README file, relative to the spec file. Defaults to
    /// `CATALOG.md`.
    pub readme: Option<PathBuf>,
    /// Path to an optional logo file, relative to the spec file. When unset,
    /// the resolver probes for `logo.svg` then `logo.png`.
    pub logo: Option<PathBuf>,
}

impl CatalogConfig {
    /// Resolve the README path against `spec_dir`, applying the default
    /// (`CATALOG.md`) when no explicit path is configured.
    pub fn resolved_readme(&self, spec_dir: &Path) -> PathBuf {
        match &self.readme {
            Some(p) => spec_dir.join(p),
            None => spec_dir.join("CATALOG.md"),
        }
    }

    /// Resolve the logo path against `spec_dir`. Returns `Some` only when
    /// either an explicit path is configured or a default candidate
    /// (`logo.svg`, then `logo.png`) exists on disk.
    ///
    /// The probe order favors SVG over PNG so vector logos win when both are
    /// present; callers wanting PNG-only must set `catalog.logo: logo.png`
    /// explicitly.
    pub fn resolved_logo(&self, spec_dir: &Path) -> Option<PathBuf> {
        if let Some(p) = &self.logo {
            return Some(spec_dir.join(p));
        }
        for candidate in ["logo.svg", "logo.png"] {
            let path = spec_dir.join(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn catalog_config_default_readme_is_catalog_md() {
        let cfg = CatalogConfig::default();
        let dir = Path::new("/mirrors/shfmt");
        assert_eq!(cfg.resolved_readme(dir), dir.join("CATALOG.md"));
    }

    #[test]
    fn catalog_config_resolved_readme_joins_spec_dir() {
        let cfg = CatalogConfig {
            readme: Some(PathBuf::from("docs/catalog.md")),
            logo: None,
        };
        let dir = Path::new("/mirrors/shfmt");
        assert_eq!(cfg.resolved_readme(dir), dir.join("docs/catalog.md"));
    }

    #[test]
    fn catalog_config_explicit_logo_path_resolves() {
        let cfg = CatalogConfig {
            readme: None,
            logo: Some(PathBuf::from("brand/logo.png")),
        };
        let dir = Path::new("/mirrors/shfmt");
        assert_eq!(cfg.resolved_logo(dir), Some(dir.join("brand/logo.png")));
    }

    #[test]
    fn catalog_config_logo_probe_returns_svg_when_present() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("logo.svg"), b"<svg/>").unwrap();
        std::fs::write(tmp.path().join("logo.png"), b"\x89PNG").unwrap();
        let cfg = CatalogConfig::default();
        assert_eq!(cfg.resolved_logo(tmp.path()), Some(tmp.path().join("logo.svg")));
    }

    #[test]
    fn catalog_config_logo_probe_falls_back_to_png() {
        let tmp = tempdir().unwrap();
        std::fs::write(tmp.path().join("logo.png"), b"\x89PNG").unwrap();
        let cfg = CatalogConfig::default();
        assert_eq!(cfg.resolved_logo(tmp.path()), Some(tmp.path().join("logo.png")));
    }

    #[test]
    fn catalog_config_logo_probe_returns_none_when_neither_exists() {
        let tmp = tempdir().unwrap();
        let cfg = CatalogConfig::default();
        assert_eq!(cfg.resolved_logo(tmp.path()), None);
    }
}
