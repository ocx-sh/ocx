// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Layer reference type for multi-layer package push operations.

use std::path::PathBuf;

use crate::{MEDIA_TYPE_TAR_GZ, MEDIA_TYPE_TAR_XZ, MEDIA_TYPE_TAR_ZSTD, oci};

/// Supported archive media types for digest layer references.
///
/// The OCI distribution spec does not expose a layer's media type via
/// blob HEAD, so when pushing a cross-package digest reference the
/// publisher must re-declare the format. This closed enum makes the
/// set of acceptable values total — `FromStr` and `Display` round-trip
/// without any runtime fallback — and prevents stringly-typed drift in
/// callers constructing `LayerRef::Digest` directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveMediaType {
    /// `application/vnd.oci.image.layer.v1.tar+gzip`
    TarGz,
    /// `application/vnd.oci.image.layer.v1.tar+xz`
    TarXz,
    /// `application/vnd.oci.image.layer.v1.tar+zstd`
    TarZstd,
}

impl ArchiveMediaType {
    /// All supported archive media types. The single source of truth
    /// for iteration — `FromStr` walks this set when resolving an
    /// extension suffix back to a variant.
    pub const ALL: &'static [Self] = &[Self::TarGz, Self::TarXz, Self::TarZstd];

    /// Returns the OCI media type string for the manifest descriptor.
    pub fn as_media_type(self) -> &'static str {
        match self {
            Self::TarGz => MEDIA_TYPE_TAR_GZ,
            Self::TarXz => MEDIA_TYPE_TAR_XZ,
            Self::TarZstd => MEDIA_TYPE_TAR_ZSTD,
        }
    }

    /// Filename extensions (without the leading dot) that map to this
    /// media type. The first entry is the canonical form; any
    /// additional entries are accepted aliases. `FromStr` tries them
    /// in order, so the canonical form wins when a string could match
    /// multiple.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::TarGz => &["tar.gz", "tgz"],
            Self::TarXz => &["tar.xz", "txz"],
            Self::TarZstd => &["tar.zst", "tzst", "tar.zstd"],
        }
    }

    /// Returns the canonical filename extension (without the leading dot).
    pub fn canonical_extension(self) -> &'static str {
        self.extensions()[0]
    }
}

impl std::fmt::Display for ArchiveMediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_media_type())
    }
}

/// Error produced when a string cannot be parsed as a [`LayerRef`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LayerRefParseError {
    /// A bare digest (`sha256:abc...`) was supplied without a media
    /// type extension suffix. Digest references must spell out the
    /// original archive format because the OCI distribution spec does
    /// not return a usable media type from a blob HEAD.
    #[error("{}", format_bare_digest(.0))]
    BareDigest(String),

    /// The optional `:strip=…,prefix=…` layout tail could not be parsed
    /// (unknown key, non-`u8` strip, duplicate key, empty value, or an entry
    /// missing its `=` separator). A bad `prefix` value is reported separately
    /// via [`MalformedPrefix`](Self::MalformedPrefix), which carries the
    /// structured cause.
    #[error("malformed layer layout '{spec}': {reason}")]
    MalformedLayout { spec: String, reason: String },

    /// The `prefix=…` layout value is not a valid bounded, non-escaping relative
    /// path. Carries the [`PathEscapeError`](crate::utility::fs::path::PathEscapeError)
    /// cause via `#[source]` so callers can recover the precise reason
    /// (absolute, Windows-prefixed, escaping, or over-long) instead of a
    /// flattened string.
    #[error("malformed layer layout '{spec}': invalid prefix '{prefix}'")]
    MalformedPrefix {
        spec: String,
        prefix: String,
        #[source]
        source: crate::utility::fs::path::PathEscapeError,
    },
}

impl crate::cli::ClassifyExitCode for LayerRefParseError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        // A layer-ref string comes from the CLI (publish side); a bad one is a
        // usage error (64), whether a bare digest or a malformed layout tail.
        Some(crate::cli::ExitCode::UsageError)
    }
}

/// Renders the bare-digest error message, enumerating every accepted
/// extension from [`ArchiveMediaType::ALL`] so adding a new archive
/// format updates the hint automatically.
fn format_bare_digest(digest: &str) -> String {
    let extensions: Vec<String> = ArchiveMediaType::ALL
        .iter()
        .flat_map(|mt| mt.extensions().iter().map(|ext| format!("'.{ext}'")))
        .collect();
    let canonical = ArchiveMediaType::ALL
        .first()
        .expect("ArchiveMediaType::ALL is a non-empty const")
        .canonical_extension();
    format!(
        "'{digest}' is a bare layer digest; append an extension suffix (one of {}) to declare the media type, e.g. '{digest}.{canonical}'",
        extensions.join(", ")
    )
}

/// A reference to a layer in a multi-layer package.
///
/// Layers are ordered: index 0 is the base layer, index N is the top
/// layer. With overlap-free semantics, order doesn't affect the
/// assembled result, but it determines error messages and manifest
/// descriptor order.
#[derive(Debug, Clone)]
pub enum LayerRef {
    /// An archive file to upload as a new layer. Media type is
    /// inferred from the file extension at push time. `layout` carries
    /// optional per-layer strip + output prefix (default: none).
    File {
        path: PathBuf,
        layout: oci::LayerLayoutSpec,
    },
    /// An existing layer already present in the registry, referenced
    /// by digest. The `media_type` is declared by the caller because
    /// the OCI spec does not expose it via blob HEAD; see the
    /// [`FromStr`](std::str::FromStr) impl for the CLI syntax. `layout`
    /// carries optional per-layer strip + output prefix (default: none).
    Digest {
        digest: oci::Digest,
        media_type: ArchiveMediaType,
        layout: oci::LayerLayoutSpec,
    },
}

impl std::fmt::Display for LayerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerRef::File { path, layout } => {
                write!(f, "{}{}", path.display(), layout_suffix(layout))
            }
            LayerRef::Digest {
                digest,
                media_type,
                layout,
            } => {
                write!(
                    f,
                    "{digest}.{}{}",
                    media_type.canonical_extension(),
                    layout_suffix(layout)
                )
            }
        }
    }
}

/// Renders the `:strip=…,prefix=…` layout tail, emitting only fields the
/// publisher set (order: strip, then prefix). Returns an empty string for the
/// default (empty) layout so a layout-free ref round-trips to today's output.
fn layout_suffix(layout: &oci::LayerLayoutSpec) -> String {
    let mut parts = Vec::new();
    if let Some(strip) = layout.strip {
        parts.push(format!("strip={strip}"));
    }
    if let Some(prefix) = &layout.prefix {
        parts.push(format!("prefix={}", prefix.as_path().display()));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(":{}", parts.join(","))
    }
}

/// Splits off an optional `:strip=…,prefix=…` layout tail per the commit rule:
/// the split point is the **last** `:` whose tail begins with `strip=` or
/// `prefix=`. Returns `(ref, tail)` when such a `:` exists, else `None` (the
/// whole string is the ref).
fn split_layout_tail(s: &str) -> Option<(&str, &str)> {
    let mut search_end = s.len();
    while let Some(idx) = s[..search_end].rfind(':') {
        let tail = &s[idx + 1..];
        if tail.starts_with("strip=") || tail.starts_with("prefix=") {
            return Some((&s[..idx], tail));
        }
        search_end = idx;
    }
    None
}

/// Parses a committed layout tail (`strip=N`, `prefix=P`, comma-separated) into
/// a [`oci::LayerLayoutSpec`]. `full` is the original ref string, echoed in the
/// error for context.
///
/// Once the tail is committed to layout parsing (see [`split_layout_tail`]), any
/// invalid value — non-`u8` strip, escaping/over-long prefix, unknown key,
/// duplicate key, or empty value — is a hard [`LayerRefParseError::MalformedLayout`],
/// never a silent fallback to a file ref.
fn parse_layout_tail(full: &str, tail: &str) -> Result<oci::LayerLayoutSpec, LayerRefParseError> {
    let malformed = |reason: String| LayerRefParseError::MalformedLayout {
        spec: full.to_string(),
        reason,
    };

    let mut layout = oci::LayerLayoutSpec::default();
    for entry in tail.split(',') {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| malformed(format!("layout entry '{entry}' is missing '='")))?;
        match key {
            "strip" => {
                if layout.strip.is_some() {
                    return Err(malformed("duplicate 'strip' key".to_string()));
                }
                if value.is_empty() {
                    return Err(malformed("empty 'strip' value".to_string()));
                }
                let strip = value
                    .parse::<u8>()
                    .map_err(|_| malformed(format!("strip must be a u8, got '{value}'")))?;
                layout.strip = Some(strip);
            }
            "prefix" => {
                if layout.prefix.is_some() {
                    return Err(malformed("duplicate 'prefix' key".to_string()));
                }
                if value.is_empty() {
                    return Err(malformed("empty 'prefix' value".to_string()));
                }
                let prefix = crate::utility::fs::path::RelativePath::parse(value).map_err(|source| {
                    LayerRefParseError::MalformedPrefix {
                        spec: full.to_string(),
                        prefix: value.to_string(),
                        source,
                    }
                })?;
                // A value that lexically normalizes to the containment root (`.`,
                // `./`, `a/..`) parses to an empty `RelativePath`, which `Display`
                // would render as `:prefix=` and re-parsing would reject as an
                // empty value. Reject it here so the Display→FromStr round-trip
                // stays total, mirroring the literal-empty-string rejection above.
                if prefix.is_empty() {
                    return Err(malformed("prefix resolves to the package root; omit it".to_string()));
                }
                layout.prefix = Some(prefix);
            }
            other => return Err(malformed(format!("unknown layout key '{other}'"))),
        }
    }
    Ok(layout)
}

impl std::str::FromStr for LayerRef {
    type Err = LayerRefParseError;

    /// Parses a string as a `LayerRef`.
    ///
    /// Recognised shapes, in order:
    ///
    /// 1. **`sha256:<hex>.<ext>`** — a layer digest with an archive
    ///    extension suffix declaring the media type. Accepted
    ///    extensions are every extension declared by
    ///    [`ArchiveMediaType::ALL`] (`tar.gz`, `tgz`, `tar.xz`, `txz`,
    ///    `tar.zst`, `tzst`, `tar.zstd`). Produces [`LayerRef::Digest`].
    ///
    /// 2. **Bare digest** (`sha256:<hex>` with no suffix) — rejected
    ///    with [`LayerRefParseError::BareDigest`]. Fabricating a media
    ///    type here would break consumers that pull a reused
    ///    non-gzip layer, so OCX requires the caller to spell it out.
    ///
    /// 3. **Anything else** — treated as a file path and produces
    ///    [`LayerRef::File`]. To force file interpretation of a
    ///    pathological filename that happens to match shape 1, prefix
    ///    it with `./` (standard Unix disambiguation).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split off an optional `:strip=…,prefix=…` layout tail first. Once a
        // tail is committed to layout parsing, an invalid value is a hard error
        // (never a silent fallback to a file ref). A string with no such tail is
        // the whole ref, preserving today's behaviour (incl. bare-digest S1).
        let (ref_str, layout) = match split_layout_tail(s) {
            Some((base, tail)) => (base, parse_layout_tail(s, tail)?),
            None => (s, oci::LayerLayoutSpec::default()),
        };

        // Pathological filename escape: a leading `./` or `/` means
        // "definitely a file path," even if the remainder would
        // otherwise parse as a digest+ext. This is the standard Unix
        // convention for disambiguating filenames that resemble
        // special tokens.
        let looks_like_path = ref_str.starts_with("./") || ref_str.starts_with('/');

        if !looks_like_path {
            for media_type in ArchiveMediaType::ALL {
                for ext in media_type.extensions() {
                    let suffix = format!(".{ext}");
                    if let Some(hex_part) = ref_str.strip_suffix(&suffix)
                        && let Ok(digest) = oci::Digest::try_from(hex_part)
                    {
                        return Ok(LayerRef::Digest {
                            digest,
                            media_type: *media_type,
                            layout,
                        });
                    }
                }
            }

            if oci::Digest::try_from(ref_str).is_ok() {
                return Err(LayerRefParseError::BareDigest(ref_str.to_string()));
            }
        }

        Ok(LayerRef::File {
            path: PathBuf::from(ref_str),
            layout,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_path() {
        let lr: LayerRef = "./archive.tar.xz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File { path: p, .. } if p == std::path::Path::new("./archive.tar.xz")));
    }

    #[test]
    fn parse_digest_tar_gz() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}.tar.gz");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { digest, media_type, .. } => {
                assert!(matches!(digest, oci::Digest::Sha256(ref h) if h == &hex));
                assert_eq!(media_type, ArchiveMediaType::TarGz);
            }
            _ => panic!("expected Digest variant, got {lr:?}"),
        }
    }

    #[test]
    fn parse_digest_tgz() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}.tgz");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarGz),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn parse_digest_tar_xz() {
        let hex = "b".repeat(64);
        let input = format!("sha256:{hex}.tar.xz");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarXz),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn parse_digest_txz() {
        let hex = "c".repeat(64);
        let input = format!("sha256:{hex}.txz");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarXz),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn parse_digest_tar_zst() {
        let hex = "e".repeat(64);
        let input = format!("sha256:{hex}.tar.zst");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarZstd),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn parse_digest_tzst() {
        let hex = "f".repeat(64);
        let input = format!("sha256:{hex}.tzst");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarZstd),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn parse_digest_tar_zstd_alias() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}.tar.zstd");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { media_type, .. } => assert_eq!(media_type, ArchiveMediaType::TarZstd),
            _ => panic!("expected Digest variant"),
        }
    }

    #[test]
    fn zstd_aliases_round_trip_to_canonical_tar_zst() {
        let hex = "a".repeat(64);
        for alias in ["tzst", "tar.zstd"] {
            let short = format!("sha256:{hex}.{alias}");
            let lr: LayerRef = short.parse().unwrap();
            // Canonical display normalizes the alias to `tar.zst`.
            assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.zst"), "alias {alias}");
        }
    }

    #[test]
    fn parse_digest_sha512_with_ext() {
        let hex = "d".repeat(128);
        let input = format!("sha512:{hex}.tar.xz");
        let lr: LayerRef = input.parse().unwrap();
        assert!(matches!(
            lr,
            LayerRef::Digest { digest: oci::Digest::Sha512(ref h), .. } if h == &hex
        ));
    }

    #[test]
    fn parse_bare_digest_is_rejected() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}");
        let err = input.parse::<LayerRef>().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bare layer digest"), "got: {msg}");
        // Every canonical + alias extension declared by ArchiveMediaType::ALL
        // must surface in the hint so callers discover the accepted syntax
        // without consulting docs.
        for media_type in ArchiveMediaType::ALL {
            for ext in media_type.extensions() {
                let needle = format!(".{ext}");
                assert!(msg.contains(&needle), "missing extension '{needle}' in: {msg}");
            }
        }
    }

    #[test]
    fn parse_invalid_digest_hex_falls_back_to_file() {
        // "sha256:tooshort.tar.gz" — algorithm prefix looks right but
        // hex length is invalid, so it doesn't match as a digest and
        // falls through to the file-path fallback.
        let lr: LayerRef = "sha256:tooshort.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File { path: p, .. } if p == std::path::Path::new("sha256:tooshort.tar.gz")));
    }

    #[test]
    fn parse_no_prefix_becomes_file() {
        let lr: LayerRef = "just-a-filename.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File { path: p, .. } if p == std::path::Path::new("just-a-filename.tar.gz")));
    }

    #[test]
    fn parse_dot_slash_forces_file_even_on_digest_shape() {
        // Pathological: a file in cwd literally named after a digest +
        // extension. The `./` prefix forces file-path interpretation.
        let hex = "a".repeat(64);
        let input = format!("./sha256:{hex}.tar.gz");
        let lr: LayerRef = input.parse().unwrap();
        assert!(matches!(lr, LayerRef::File { .. }));
    }

    #[test]
    fn parse_absolute_path() {
        let lr: LayerRef = "/tmp/layer.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File { path: p, .. } if p == std::path::Path::new("/tmp/layer.tar.gz")));
    }

    #[test]
    fn display_file() {
        let lr = LayerRef::File {
            path: PathBuf::from("my/archive.tar.xz"),
            layout: oci::LayerLayoutSpec::default(),
        };
        assert_eq!(lr.to_string(), "my/archive.tar.xz");
    }

    #[test]
    fn display_digest_tar_gz() {
        let hex = "a".repeat(64);
        let lr = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarGz,
            layout: oci::LayerLayoutSpec::default(),
        };
        assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.gz"));
    }

    #[test]
    fn display_digest_tar_xz() {
        let hex = "b".repeat(64);
        let lr = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarXz,
            layout: oci::LayerLayoutSpec::default(),
        };
        assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.xz"));
    }

    #[test]
    fn display_round_trips_for_digest() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}.tar.gz");
        let lr: LayerRef = input.parse().unwrap();
        assert_eq!(lr.to_string(), input);
    }

    #[test]
    fn tgz_alias_round_trips_to_canonical_tar_gz() {
        let hex = "a".repeat(64);
        let short = format!("sha256:{hex}.tgz");
        let lr: LayerRef = short.parse().unwrap();
        // Canonical display normalizes the alias to `tar.gz`.
        assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.gz"));
        // The re-parsed canonical form is structurally equal to the original parse.
        let reparsed: LayerRef = lr.to_string().parse().unwrap();
        match (lr, reparsed) {
            (
                LayerRef::Digest {
                    digest: d1,
                    media_type: m1,
                    ..
                },
                LayerRef::Digest {
                    digest: d2,
                    media_type: m2,
                    ..
                },
            ) => {
                assert_eq!(d1, d2);
                assert_eq!(m1, m2);
            }
            _ => panic!("expected Digest variants"),
        }
    }

    #[test]
    fn display_round_trips_for_non_empty_prefix() {
        // A layout tail carrying a non-empty prefix must survive Display→FromStr:
        // the empty-prefix guard added for the root-normalizing case must not
        // reject legitimate prefixes.
        let hex = "a".repeat(64);
        for prefix in ["share", "share/lib", "a/b/c"] {
            let input = format!("sha256:{hex}.tar.gz:strip=1,prefix={prefix}");
            let parsed: LayerRef = input.parse().expect("a non-empty prefix parses");
            assert_eq!(parsed.to_string(), input, "prefix {prefix} must round-trip");
            match parsed {
                LayerRef::Digest { layout, .. } => {
                    assert_eq!(layout.strip, Some(1));
                    assert_eq!(
                        layout.prefix.as_ref().map(|p| p.as_path()),
                        Some(std::path::Path::new(prefix)),
                        "prefix {prefix} preserved"
                    );
                }
                other => panic!("expected Digest, got {other:?}"),
            }
        }
    }

    #[test]
    fn display_digest_tar_zstd() {
        let hex = "e".repeat(64);
        let lr = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarZstd,
            layout: oci::LayerLayoutSpec::default(),
        };
        assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.zst"));
    }

    #[test]
    fn archive_media_type_as_media_type_matches_constants() {
        assert_eq!(ArchiveMediaType::TarGz.as_media_type(), MEDIA_TYPE_TAR_GZ);
        assert_eq!(ArchiveMediaType::TarXz.as_media_type(), MEDIA_TYPE_TAR_XZ);
        assert_eq!(ArchiveMediaType::TarZstd.as_media_type(), MEDIA_TYPE_TAR_ZSTD);
    }

    // ── Part 2 layout grammar (U20–U22) ──────────────────────────────────────
    //
    // U20/U21 pin PRESERVED behaviour (GREEN now): a ref with no `:strip=…`
    // tail parses exactly as before, with the default (empty) layout, and a bare
    // digest is still rejected. U22 specifies the NEW `:strip=…,prefix=…` grammar
    // (RED until P2.10): the tail is not split yet, so the layout-bearing
    // assertions fail. All the pre-existing tests above stay unmodified (S1/D1).

    /// U20 (grammar · S1/D1): a digest+ext or a file path with no layout tail
    /// parses as today, carrying the default (empty) `LayerLayoutSpec`.
    #[test]
    fn from_str_no_layout_yields_default_layout() {
        let hex = "a".repeat(64);
        let digest: LayerRef = format!("sha256:{hex}.tar.gz").parse().expect("digest parses");
        match digest {
            LayerRef::Digest { layout, .. } => assert!(layout.is_empty(), "no tail → default layout"),
            other => panic!("expected Digest, got {other:?}"),
        }

        let file: LayerRef = "some/archive.tar.gz".parse().expect("file parses");
        match file {
            LayerRef::File { layout, .. } => assert!(layout.is_empty(), "no tail → default layout"),
            other => panic!("expected File, got {other:?}"),
        }
    }

    /// U21 (S1): a bare digest with no extension suffix and no layout tail is
    /// still rejected as `BareDigest` — the grammar must not turn it into a File
    /// or a Digest. (The tail-split variant `sha256:<hex>:strip=1` → `BareDigest`
    /// requires the new grammar and lives in `from_str_layout_grammar`.)
    #[test]
    fn from_str_plain_bare_digest_rejected() {
        let hex = "a".repeat(64);
        let err = format!("sha256:{hex}")
            .parse::<LayerRef>()
            .expect_err("a bare digest with no suffix must be rejected");
        assert!(matches!(err, LayerRefParseError::BareDigest(_)), "got {err:?}");
    }

    /// U22 (grammar/error · D10 publish-side): the `:strip=…,prefix=…` tail
    /// parses into a `LayerLayoutSpec`; malformed tails are `MalformedLayout`;
    /// `Display` round-trips a layout tail; and a bare digest with a layout tail
    /// still rejects the bare digest (S1). RED until P2.10 implements the grammar.
    #[test]
    fn from_str_layout_grammar() {
        // A file ref with a strip+prefix tail: the tail splits off, leaving the
        // path, and the layout carries both fields.
        let parsed: LayerRef = "layer.tar.gz:strip=1,prefix=share".parse().expect("parses");
        match parsed {
            LayerRef::File { path, layout } => {
                assert_eq!(
                    path,
                    std::path::Path::new("layer.tar.gz"),
                    "path is the ref before the layout tail"
                );
                assert_eq!(layout.strip, Some(1), "strip parsed from the tail");
                assert_eq!(
                    layout.prefix.as_ref().map(|p| p.as_path()),
                    Some(std::path::Path::new("share")),
                    "prefix parsed from the tail"
                );
            }
            other => panic!("expected File with a layout, got {other:?}"),
        }

        // Malformed tails (tail begins with a layout key → committed to layout
        // parsing, so an invalid value is an error, not a silent File fallback).
        assert!(
            matches!(
                "layer.tar.gz:strip=1,bogus=2".parse::<LayerRef>(),
                Err(LayerRefParseError::MalformedLayout { .. })
            ),
            "an unknown layout key must be MalformedLayout"
        );
        assert!(
            matches!(
                "layer.tar.gz:strip=999".parse::<LayerRef>(),
                Err(LayerRefParseError::MalformedLayout { .. })
            ),
            "a >u8 strip must be MalformedLayout"
        );
        // A prefix that lexically normalizes to the containment root parses to an
        // empty `RelativePath`; reject it as `MalformedLayout` so `Display` never
        // emits an empty `:prefix=` that would fail to re-parse (round-trip).
        for root_prefix in [
            "layer.tar.gz:prefix=.",
            "layer.tar.gz:prefix=./",
            "layer.tar.gz:prefix=a/..",
        ] {
            assert!(
                matches!(
                    root_prefix.parse::<LayerRef>(),
                    Err(LayerRefParseError::MalformedLayout { .. })
                ),
                "a prefix resolving to the package root must be MalformedLayout: {root_prefix}"
            );
        }

        // An escaping prefix carries the structured `PathEscapeError` cause via
        // `#[source]` (recoverable), not a flattened string.
        let escaping = "layer.tar.gz:prefix=../evil".parse::<LayerRef>();
        assert!(
            matches!(&escaping, Err(LayerRefParseError::MalformedPrefix { .. })),
            "an escaping prefix must be MalformedPrefix, got {escaping:?}"
        );
        let source = std::error::Error::source(escaping.as_ref().unwrap_err());
        assert!(
            source
                .and_then(|e| e.downcast_ref::<crate::utility::fs::path::PathEscapeError>())
                .is_some(),
            "MalformedPrefix must expose the PathEscapeError via source()"
        );

        // Display round-trips a strip layout tail (no RelativePath needed).
        let hex = "a".repeat(64);
        let with_layout = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarGz,
            layout: oci::LayerLayoutSpec {
                strip: Some(1),
                prefix: None,
            },
        };
        assert_eq!(
            with_layout.to_string(),
            format!("sha256:{hex}.tar.gz:strip=1"),
            "Display must emit the strip layout tail"
        );

        // S1 under the grammar: a bare digest with a layout tail splits the tail
        // off and STILL rejects the bare digest (no extension to declare media).
        let err = format!("sha256:{hex}:strip=1")
            .parse::<LayerRef>()
            .expect_err("a bare digest with a layout tail must still be rejected");
        assert!(
            matches!(err, LayerRefParseError::BareDigest(_)),
            "tail-split bare digest must be BareDigest, got {err:?}"
        );
    }
}
