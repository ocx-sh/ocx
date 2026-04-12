// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Layer reference type for multi-layer package push operations.

use std::path::PathBuf;

use crate::{MEDIA_TYPE_TAR_GZ, MEDIA_TYPE_TAR_XZ, oci};

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
}

impl ArchiveMediaType {
    /// All supported archive media types. The single source of truth
    /// for iteration — `FromStr` walks this set when resolving an
    /// extension suffix back to a variant.
    pub const ALL: &'static [Self] = &[Self::TarGz, Self::TarXz];

    /// Returns the OCI media type string for the manifest descriptor.
    pub fn as_media_type(self) -> &'static str {
        match self {
            Self::TarGz => MEDIA_TYPE_TAR_GZ,
            Self::TarXz => MEDIA_TYPE_TAR_XZ,
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
    #[error(
        "'{0}' is a bare layer digest; append an extension suffix to declare the media type, e.g. '{0}.tar.gz' or '{0}.tar.xz'"
    )]
    BareDigest(String),
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
    /// inferred from the file extension at push time.
    File(PathBuf),
    /// An existing layer already present in the registry, referenced
    /// by digest. The `media_type` is declared by the caller because
    /// the OCI spec does not expose it via blob HEAD; see the
    /// [`FromStr`](std::str::FromStr) impl for the CLI syntax.
    Digest {
        digest: oci::Digest,
        media_type: ArchiveMediaType,
    },
}

impl std::fmt::Display for LayerRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerRef::File(path) => write!(f, "{}", path.display()),
            LayerRef::Digest { digest, media_type } => {
                write!(f, "{digest}.{}", media_type.canonical_extension())
            }
        }
    }
}

impl std::str::FromStr for LayerRef {
    type Err = LayerRefParseError;

    /// Parses a string as a `LayerRef`.
    ///
    /// Recognised shapes, in order:
    ///
    /// 1. **`sha256:<hex>.<ext>`** — a layer digest with an archive
    ///    extension suffix declaring the media type. Accepted
    ///    extensions are `tar.gz`, `tgz`, `tar.xz`, `txz`. Produces
    ///    [`LayerRef::Digest`].
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
        // Pathological filename escape: a leading `./` or `/` means
        // "definitely a file path," even if the remainder would
        // otherwise parse as a digest+ext. This is the standard Unix
        // convention for disambiguating filenames that resemble
        // special tokens.
        let looks_like_path = s.starts_with("./") || s.starts_with('/');

        if !looks_like_path {
            for media_type in ArchiveMediaType::ALL {
                for ext in media_type.extensions() {
                    let suffix = format!(".{ext}");
                    if let Some(hex_part) = s.strip_suffix(&suffix)
                        && let Ok(digest) = oci::Digest::try_from(hex_part)
                    {
                        return Ok(LayerRef::Digest {
                            digest,
                            media_type: *media_type,
                        });
                    }
                }
            }

            if oci::Digest::try_from(s).is_ok() {
                return Err(LayerRefParseError::BareDigest(s.to_string()));
            }
        }

        Ok(LayerRef::File(PathBuf::from(s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_file_path() {
        let lr: LayerRef = "./archive.tar.xz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File(p) if p == std::path::Path::new("./archive.tar.xz")));
    }

    #[test]
    fn parse_digest_tar_gz() {
        let hex = "a".repeat(64);
        let input = format!("sha256:{hex}.tar.gz");
        let lr: LayerRef = input.parse().unwrap();
        match lr {
            LayerRef::Digest { digest, media_type } => {
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
        assert!(msg.contains(".tar.gz") || msg.contains(".tar.xz"), "got: {msg}");
    }

    #[test]
    fn parse_invalid_digest_hex_falls_back_to_file() {
        // "sha256:tooshort.tar.gz" — algorithm prefix looks right but
        // hex length is invalid, so it doesn't match as a digest and
        // falls through to the file-path fallback.
        let lr: LayerRef = "sha256:tooshort.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File(p) if p == std::path::Path::new("sha256:tooshort.tar.gz")));
    }

    #[test]
    fn parse_no_prefix_becomes_file() {
        let lr: LayerRef = "just-a-filename.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File(p) if p == std::path::Path::new("just-a-filename.tar.gz")));
    }

    #[test]
    fn parse_dot_slash_forces_file_even_on_digest_shape() {
        // Pathological: a file in cwd literally named after a digest +
        // extension. The `./` prefix forces file-path interpretation.
        let hex = "a".repeat(64);
        let input = format!("./sha256:{hex}.tar.gz");
        let lr: LayerRef = input.parse().unwrap();
        assert!(matches!(lr, LayerRef::File(_)));
    }

    #[test]
    fn parse_absolute_path() {
        let lr: LayerRef = "/tmp/layer.tar.gz".parse().unwrap();
        assert!(matches!(lr, LayerRef::File(p) if p == std::path::Path::new("/tmp/layer.tar.gz")));
    }

    #[test]
    fn display_file() {
        let lr = LayerRef::File(PathBuf::from("my/archive.tar.xz"));
        assert_eq!(lr.to_string(), "my/archive.tar.xz");
    }

    #[test]
    fn display_digest_tar_gz() {
        let hex = "a".repeat(64);
        let lr = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarGz,
        };
        assert_eq!(lr.to_string(), format!("sha256:{hex}.tar.gz"));
    }

    #[test]
    fn display_digest_tar_xz() {
        let hex = "b".repeat(64);
        let lr = LayerRef::Digest {
            digest: oci::Digest::Sha256(hex.clone()),
            media_type: ArchiveMediaType::TarXz,
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
                },
                LayerRef::Digest {
                    digest: d2,
                    media_type: m2,
                },
            ) => {
                assert_eq!(d1, d2);
                assert_eq!(m1, m2);
            }
            _ => panic!("expected Digest variants"),
        }
    }

    #[test]
    fn archive_media_type_as_media_type_matches_constants() {
        assert_eq!(ArchiveMediaType::TarGz.as_media_type(), MEDIA_TYPE_TAR_GZ);
        assert_eq!(ArchiveMediaType::TarXz.as_media_type(), MEDIA_TYPE_TAR_XZ);
    }
}
