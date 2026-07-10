// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod architecture;
pub mod error;
pub mod operating_system;

pub use architecture::Architecture;
pub use error::{PlatformError, PlatformErrorKind};
pub use operating_system::OperatingSystem;

use serde::{Deserialize, Serialize};

use super::native;
use crate::Result;

const ANY_STR: &str = "any";

/// Target platform for an OCX package.
///
/// This is OCX's owned representation of the
/// [OCI Image Index platform object](https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions).
/// It wraps the same concept but uses closed enums ([`OperatingSystem`],
/// [`Architecture`]) instead of the upstream open enums with `Other(String)`
/// fallback, giving compile-time enforcement of supported values.
///
/// # `Any` — an OCX extension
///
/// The OCI specification does not define a concept of a platform-agnostic
/// package. Every Image Index entry has a concrete `os`/`architecture` pair.
/// OCX extends this with [`Platform::Any`] to represent packages that are
/// not tied to any specific platform (e.g. Java JARs, shell scripts, data
/// bundles). When serialized to OCI JSON, `Any` is represented as
/// `{"os":"any","architecture":"any"}` — these are not valid OCI values but
/// are a convention understood by OCX tooling.
///
/// # Serialization
///
/// Serde goes through [`native::Platform`] to guarantee OCI-compatible JSON
/// field names (`"os"`, `"architecture"`, `"variant"`, `"os.version"`,
/// `"os.features"`). Deserialization validates that the OS and architecture
/// are in OCX's supported set, rejecting unsupported values from registries
/// (e.g. `ppc64le`, `s390x`).
///
/// # String format
///
/// [`Display`] is the single canonical string form and the inverse of
/// [`FromStr`]: it renders the slash-separated core
/// (`"linux/amd64"`, `"darwin/arm64/v8"`, or `"any"`) and appends each
/// `os.features` value as a `+`-separated suffix (sorted + deduped, so the
/// output is canonical and stable): `os/arch[/variant][/os_version][+feature...]`.
/// This is the form the CLI `--platform` flag accepts. Filesystem paths use
/// [`segments`](Self::segments) / [`ascii_segments`](Self::ascii_segments)
/// instead, so carrying features in `Display` does not affect path building.
///
/// [`FromStr`] accepts the same core plus the optional `+`-separated
/// `os.features` suffix: `os/arch[/variant][+feature[+feature...]]`. Examples:
/// `"linux/amd64+libc.glibc"`, `"linux/amd64/v3+libc.musl"`,
/// `"linux/amd64+a.x+b.y"`. The trailing features are normalized (sorted +
/// deduped) so the parsed value is canonical. `"any"` with `+features` is an
/// error — the agnostic platform carries no features.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Platform-agnostic package. Not part of the OCI spec — this is an OCX
    /// extension for packages that run on any OS and architecture.
    /// Serializes to `{"os":"any","architecture":"any"}` in OCI JSON.
    Any,

    /// A concrete OS/architecture target, corresponding to a single entry
    /// in an OCI Image Index.
    Specific {
        /// Target operating system (required by OCI spec).
        os: OperatingSystem,
        /// Target CPU architecture (required by OCI spec).
        arch: Architecture,
        /// CPU variant, e.g. `"v7"` or `"v8"` for ARM. Defined by the
        /// [OCI platform variants table](https://github.com/opencontainers/image-spec/blob/main/image-index.md#platform-variants).
        variant: Option<String>,
        /// Minimum OS version required, e.g. `"10.0.14393.1066"` for Windows.
        /// Implementations may refuse manifests where os_version is not known
        /// to work with the host OS version.
        os_version: Option<String>,
        /// Mandatory OS features required for this image. For Windows, the
        /// OCI spec defines `"win32k"` (image requires `win32k.sys` on the
        /// host, which is missing on Nano Server). For other operating
        /// systems, values are implementation-defined. An empty `Vec` means no
        /// features are declared (undetected / no libc requirement).
        os_features: Vec<String>,
        /// Required CPU features, e.g. `["sse4", "aes"]`. Defined by the
        /// [OCI Image Index specification](https://github.com/opencontainers/image-spec/blob/main/image-index.md#image-index-property-descriptions).
        features: Option<Vec<String>>,
    },
}

impl Platform {
    /// Creates a platform-agnostic platform value.
    ///
    /// Use this to indicate that a package is compatible with any platform,
    /// for example a Java JAR or a data bundle.
    pub fn any() -> Self {
        Self::Any
    }

    /// Returns `true` if this is the platform-agnostic [`Any`](Self::Any) variant.
    ///
    /// This is an OCX concept with no direct equivalent in the OCI spec.
    /// The OCI spec requires every Image Index entry to have a concrete
    /// `os`/`architecture` pair; OCX uses the sentinel values `"any"/"any"`
    /// as a convention for platform-agnostic packages.
    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }

    /// Extracts the platform from a single-image OCI manifest.
    ///
    /// Single-image manifests (as opposed to multi-platform Image Indexes)
    /// do not carry platform metadata in the manifest itself — the platform
    /// is only known from the Image Index entry that references it. When we
    /// encounter a bare image manifest without index context, we treat it as
    /// platform-agnostic.
    pub fn from_image_manifest(_manifest: &native::ImageManifest) -> Self {
        Self::Any
    }

    /// Extracts all platforms from an OCI Image Index manifest.
    ///
    /// Each entry in the index references a platform-specific image manifest.
    /// Returns an error if any entry's platform uses an OS or architecture
    /// that OCX does not support.
    pub fn from_image_index(manifest: &native::ImageIndex) -> Result<Vec<Self>> {
        let mut platforms = Vec::with_capacity(manifest.manifests.len());
        for entry in &manifest.manifests {
            let platform = Self::try_from(entry.platform.clone())?;
            platforms.push(platform);
        }
        Ok(platforms)
    }

    /// Extracts platforms from any OCI manifest type.
    ///
    /// Dispatches to [`from_image_manifest`](Self::from_image_manifest) for
    /// single images or [`from_image_index`](Self::from_image_index) for
    /// multi-platform indexes.
    pub fn from_manifest(manifest: &native::Manifest) -> Result<Vec<Self>> {
        match manifest {
            native::Manifest::Image(image_manifest) => Ok(vec![Self::from_image_manifest(image_manifest)]),
            native::Manifest::ImageIndex(image_index) => Self::from_image_index(image_index),
        }
    }

    /// Returns the platform components as string segments.
    ///
    /// - `Any` → `["any"]`
    /// - `Specific` → `["linux", "amd64"]` or `["linux", "arm64", "v8"]` etc.
    ///
    /// Used for constructing filesystem paths and display formatting.
    pub fn segments(&self) -> Vec<String> {
        match self {
            Self::Any => vec![ANY_STR.to_string()],
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                ..
            } => {
                let mut segments = vec![os.to_string(), arch.to_string()];
                if let Some(variant) = variant {
                    segments.push(variant.clone());
                }
                if let Some(os_version) = os_version {
                    segments.push(os_version.clone());
                }
                segments
            }
        }
    }

    /// Returns `true` if `self` can run `other`.
    ///
    /// `other` is `Any` → `true`. `self` is `Any` → `false`. Both `Specific`:
    /// equality on `os` + `arch`; strict `variant` equality only when
    /// `other.variant` is set; strict `os_version` equality only when
    /// `other.os_version` is set; subset on `os_features`
    /// (`other.os_features ⊆ self.os_features`).
    ///
    /// Subset semantics apply only to `os_features`. `variant` and `os_version`
    /// are matched by strict equality when the candidate declares them:
    /// [`current`](Self::current) never populates `os_version`, so a
    /// version-bearing candidate stays fail-closed (unselectable). Widening
    /// either to a range/subset rule requires a new ADR
    /// (adr_platform_libc_os_features.md).
    pub fn can_run(&self, other: &Platform) -> bool {
        // An `Any` other runs everywhere — `self` can always run it. Checked
        // before the `self`-`Any` branch so an `Any` self still runs an `Any`
        // other.
        if other.is_any() {
            return true;
        }

        // An `Any` `self` cannot run a `Specific` other.
        let (
            Self::Specific {
                os: self_os,
                arch: self_arch,
                variant: self_variant,
                os_version: self_os_version,
                os_features: self_features,
                ..
            },
            Self::Specific {
                os: other_os,
                arch: other_arch,
                variant: other_variant,
                os_version: other_os_version,
                os_features: other_features,
                ..
            },
        ) = (self, other)
        else {
            return false;
        };

        if self_os != other_os || self_arch != other_arch {
            return false;
        }

        // Strict equality on `variant` only when `other` declares one.
        if other_variant.is_some() && other_variant != self_variant {
            return false;
        }

        // Strict equality on `os_version` only when `other` declares one.
        // `Platform::current()` never populates `os_version`, so a
        // version-bearing candidate was already unselectable under the deleted
        // strict-equality `matches`; this preserves that fail-closed behaviour
        // until a version-range ADR supersedes it.
        if other_os_version.is_some() && other_os_version != self_os_version {
            return false;
        }

        // Subset on `os_features`: every feature `other` requires must be
        // present on `self`. An empty `other` set is a subset of anything.
        other_features.iter().all(|feature| self_features.contains(feature))
    }

    /// Compares only the OS and architecture of two `Specific` platforms,
    /// ignoring `variant`, `os_version`, `os_features`, and `features`.
    ///
    /// [`current`](Self::current) can only ever populate `os`/`arch` (the
    /// remaining fields are always `None`), so a full-struct equality comparison
    /// (the deleted `matches` semantics) would wrongly reject a host-runnable
    /// image-index entry that merely carries an `os_version` or `os_features`
    /// refinement. The host-only symlink gate
    /// (issue #179) therefore compares os+arch only. Two [`Any`](Self::Any)
    /// values compare equal; `Any` vs `Specific` never do.
    pub fn same_os_arch(&self, other: &Platform) -> bool {
        match (self, other) {
            (Self::Any, Self::Any) => true,
            (
                Self::Specific {
                    os: self_os,
                    arch: self_arch,
                    ..
                },
                Self::Specific {
                    os: other_os,
                    arch: other_arch,
                    ..
                },
            ) => self_os == other_os && self_arch == other_arch,
            _ => false,
        }
    }

    /// Whether a package that resolved to `platform` is runnable on the current
    /// host — the host-only symlink gate for `candidates/{tag}` and `current`
    /// (issue #179).
    ///
    /// `None` (undeterminable platform) and [`any`](Self::any) (platform-agnostic
    /// package) are always host-appropriate. A specific platform is
    /// host-appropriate only when its os+arch equal the host's; when the host
    /// itself is undeterminable, the write is allowed — never suppress on an
    /// unknown host.
    pub fn host_can_run(platform: Option<&Platform>) -> bool {
        Self::host_can_run_on(platform, Self::current().as_ref())
    }

    /// Host-injectable core of [`host_can_run`](Self::host_can_run).
    ///
    /// Takes the host platform explicitly so the gating contract can be unit
    /// tested against literal os/arch pairs, independent of the test runner's
    /// actual OS/arch (issue #179).
    pub fn host_can_run_on(platform: Option<&Platform>, host: Option<&Platform>) -> bool {
        match platform {
            None => true,
            Some(p) if p.is_any() => true,
            Some(p) => host.is_none_or(|h| h.same_os_arch(p)),
        }
    }

    /// Returns [`segments`](Self::segments) lowercased to ASCII.
    pub fn ascii_segments(&self) -> Vec<String> {
        self.segments().into_iter().map(|s| s.to_ascii_lowercase()).collect()
    }

    /// Detects the platform of the current host.
    ///
    /// Delegates to [`OperatingSystem::current`] and [`Architecture::current`]
    /// to map Rust's compile-time target to OCI values. Returns `None` if the
    /// host OS or architecture is not in OCX's supported set.
    ///
    /// Note: an unsupported OS or arch returns `None` (no `Platform` at all),
    /// whereas an undetected libc (non-Linux, NixOS, failed probe) returns
    /// `Some(Specific { os_features: <empty>, .. })`. The host is known, but
    /// its libc family is not — subset matching then only matches entries with
    /// empty `os_features`.
    pub fn current() -> Option<Self> {
        let os = OperatingSystem::current()?;
        let arch = Architecture::current()?;
        Some(Self::Specific {
            os,
            arch,
            variant: None,
            os_version: None,
            os_features: super::host_capabilities::cached_os_features(),
            features: None,
        })
    }

    /// Returns a lossless, injective key string for this platform, used as
    /// the `ocx.lock` V2 `platforms` map key and for host-platform lookup.
    ///
    /// The common case is byte-identical to [`Display`](std::fmt::Display)
    /// (`"any"`, `"linux/amd64"`, `"darwin/arm64/v8"`). Unlike `Display` —
    /// which drops `os_features` and `features` — this encoding extends the
    /// `Display` form with those fields **only when present**, so two
    /// advertised platforms that differ solely in `os_features`/`features`
    /// can never collide on the same key (Codex R1: a lossy key would
    /// convert a `select`-installable multi-variant package into a hard lock
    /// failure).
    ///
    /// # Injectivity
    ///
    /// The encoding is **structurally injective for arbitrary field values**,
    /// not just for the common platforms. Every registry-controlled string —
    /// `variant`, `os_version`, and each `os_features`/`features` value — is
    /// percent-escaped ([`escape_feature_component`]) before it enters the key.
    /// Escaping the base segments (not only the suffix values) is what closes
    /// the marker-forging hole: a crafted `os_version` such as `"10;osf=win32k"`
    /// encodes as `10%3Bosf%3Dwin32k`, so it cannot forge a `;osf=` marker and
    /// collide with a genuine `os_version = "10"` + `os_features = ["win32k"]`
    /// platform (Finding 2). The same base escaping covers `/`, the
    /// base-segment separator: a `variant = "v8/10"` (no `os_version`) encodes
    /// as `v8%2F10`, so it cannot forge the extra slash and collide with
    /// `variant = "v8", os_version = "10"` (Block B). The `,` separator inside a single feature value is
    /// likewise escaped, so `["a,b"]` (one element holding a comma) encodes as
    /// `a%2Cb` while `["a","b"]` (two elements) encodes as `a,b` — distinct
    /// keys. The `;osf=` / `;feat=` markers themselves are emitted only by this
    /// method (never from an escaped value), so the encoding also never collides
    /// with a marker-free base key.
    ///
    /// Escaping is **byte-transparent for the common case**: legitimate
    /// `variant` (`v7`, `v8`), `os_version` (`10.0.14393.1066`), and feature
    /// names (`win32k`, `sse4`, `aes`) contain none of `% , ; =`, so the
    /// typical `linux/amd64` / `darwin/arm64/v8` keys are byte-identical to
    /// [`Display`](std::fmt::Display).
    ///
    /// This is the only key encoding the lock uses; `Platform` is never
    /// serialized as a value, so no `impl JsonSchema for Platform`, no
    /// `Ord`, and no `canonical_set()` are required.
    pub fn lock_key(&self) -> String {
        let (os, arch, variant, os_version, os_features, features) = match self {
            // `Any` carries no registry-controlled fields → nothing to escape.
            Self::Any => return ANY_STR.to_string(),
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                os_features,
                features,
            } => (os, arch, variant, os_version, os_features, features),
        };

        // Base mirrors `Display` (`os/arch[/variant[/os_version]]`) but escapes
        // the registry-controlled `variant` and `os_version` segments so they
        // cannot forge a `;osf=`/`;feat=` marker. `os`/`arch` are closed enums
        // (no special characters) and pass through verbatim.
        let mut key = format!("{os}/{arch}");
        if let Some(variant) = variant {
            key.push('/');
            key.push_str(&escape_feature_component(variant));
        }
        if let Some(os_version) = os_version {
            key.push('/');
            key.push_str(&escape_feature_component(os_version));
        }

        // `os_features`/`features` are appended as explicit, parseable suffixes
        // only when present, so two children differing only in those fields get
        // distinct keys.
        if !os_features.is_empty() {
            key.push_str(";osf=");
            key.push_str(&join_feature_values(os_features));
        }
        if let Some(values) = features {
            key.push_str(";feat=");
            key.push_str(&join_feature_values(values));
        }
        key
    }

    /// Returns the [`lock_key`](Self::lock_key) this platform would have with an
    /// empty `os_features` set — its `os/arch[/variant[/os_version]]` base with
    /// no `;osf=`/`;feat=` suffix.
    ///
    /// The V2 lock host lookup uses this as a backward-compatibility tier: a
    /// libc-detecting host's own key (e.g. `linux/amd64;osf=libc.glibc`) will
    /// not match a plain `linux/amd64` entry — one keyed before libc detection
    /// existed, or a deliberately libc-agnostic static-linked build — so the
    /// lookup falls back to this base key before giving up. See
    /// [`crate::project::lookup_host_leaf`].
    pub fn base_lock_key(&self) -> String {
        match self {
            Self::Any => self.lock_key(),
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                ..
            } => Self::Specific {
                os: *os,
                arch: *arch,
                variant: variant.clone(),
                os_version: os_version.clone(),
                os_features: Vec::new(),
                features: None,
            }
            .lock_key(),
        }
    }

    /// Reconstructs a [`Platform`] from a [`lock_key`](Self::lock_key) string.
    ///
    /// It is an exact inverse for every platform whose base is
    /// `os/arch[/variant[/os_version]]` filled left-to-right (the shape every
    /// real index candidate and every `--platform any` map key carries). It
    /// shares one pre-existing positional ambiguity with
    /// [`FromStr`](std::str::FromStr): a `Specific` with `variant: None` but
    /// `os_version: Some(_)` encodes to a 3-segment base identical to one with
    /// `variant: Some(_)` and no `os_version`, so it reconstructs the value into
    /// the `variant` slot. That shape does not occur in the dependency-pinning
    /// path, and the sole caller only reads the result back through
    /// `lock_key`/`base_lock_key` string matching (blind to which slot holds the
    /// value). See `from_lock_key_positional_ambiguity_for_os_version_without_variant`.
    ///
    /// `"any"` maps to [`Platform::Any`]. Otherwise the encoding is
    /// `os/arch[/variant[/os_version]][;osf=feat,...][;feat=feat,...]`: the
    /// `;osf=` and `;feat=` marker suffixes are peeled off (their values escape
    /// `;` as `%3B`, so the literal markers are unambiguous separators), the
    /// base is split on unescaped `/` into positional `os`/`arch`/`variant`/
    /// `os_version` segments, and every base segment and feature value is
    /// percent-unescaped — the reverse of [`escape_feature_component`]. `os`
    /// and `arch` are parsed through the same [`OperatingSystem`] /
    /// [`Architecture`] parsers as [`FromStr`](std::str::FromStr).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] with [`PlatformErrorKind::InvalidFormat`] for a
    /// malformed structure or escape, and the OS/architecture parser's error
    /// kind for an unsupported `os`/`arch`.
    pub fn from_lock_key(key: &str) -> std::result::Result<Self, PlatformError> {
        let invalid = || PlatformError {
            input: key.to_string(),
            kind: PlatformErrorKind::InvalidFormat,
        };

        if key == ANY_STR {
            return Ok(Self::Any);
        }

        // Peel `;feat=` first (the trailing marker), then `;osf=`. Marker values
        // escape `;` (`%3B`) and `=` (`%3D`), so `split_once` on the literal
        // marker text can only land on a genuine marker.
        let (without_features, features) = match key.split_once(";feat=") {
            Some((head, joined)) => (head, Some(split_marker_values(joined).ok_or_else(invalid)?)),
            None => (key, None),
        };
        let (base, os_features) = match without_features.split_once(";osf=") {
            Some((base, joined)) => (base, split_marker_values(joined).ok_or_else(invalid)?),
            None => (without_features, Vec::new()),
        };

        // Base segments are `/`-separated; any `/` inside a `variant`/`os_version`
        // value was escaped to `%2F`, so a literal `/` is always a separator.
        let parts: Vec<&str> = base.split('/').collect();
        if parts.len() < 2 || parts.len() > 4 {
            return Err(invalid());
        }

        let os_str = unescape_feature_component(parts[0]).ok_or_else(invalid)?;
        let arch_str = unescape_feature_component(parts[1]).ok_or_else(invalid)?;
        let os: OperatingSystem = os_str.parse().map_err(|kind| PlatformError {
            input: key.to_string(),
            kind,
        })?;
        let arch: Architecture = arch_str.parse().map_err(|kind| PlatformError {
            input: key.to_string(),
            kind,
        })?;

        let variant = match parts.get(2) {
            Some(segment) => Some(unescape_feature_component(segment).ok_or_else(invalid)?),
            None => None,
        };
        let os_version = match parts.get(3) {
            Some(segment) => Some(unescape_feature_component(segment).ok_or_else(invalid)?),
            None => None,
        };

        Ok(Self::Specific {
            os,
            arch,
            variant,
            os_version,
            os_features,
            features,
        })
    }

    /// Returns the default supported-platform list for the current host.
    ///
    /// The list always contains at most two entries:
    /// - [`Platform::current()`] — the host-native platform, when detectable.
    /// - [`Platform::any()`] — the platform-agnostic fallback, always present.
    ///
    /// This is the canonical source of truth for "which platforms should this
    /// host install?". Both the CLI install path and the self-update path
    /// consume this helper so the set stays in sync.
    pub fn supported_set() -> Vec<Self> {
        let mut platforms = Vec::new();
        if let Some(platform) = Self::current() {
            platforms.push(platform);
        }
        platforms.push(Self::any());
        platforms
    }

    /// Returns the full supported-platform matrix: every OS/architecture
    /// combination OCX ships and tests, regardless of which platform is
    /// running the command.
    ///
    /// Distinct from [`supported_set`](Self::supported_set), which is scoped
    /// to the *current host* (its own platform plus [`any()`](Self::any)).
    /// `all_supported` is the single source of truth for "every platform a
    /// team might run" — consumed by `ocx patch sync`'s no-`--platform`
    /// default, which must pin multi-platform manifests covering every
    /// platform a team runs (mirroring `ocx lock`), not just the host that
    /// happens to run the sync. A host-only default would silently miss
    /// non-host companions and break an offline or required-patch launch on
    /// a teammate's machine running a different platform.
    ///
    /// The five concrete OS/arch ship targets come first, kept in sync with
    /// `product-context.md` "Platform support": `linux/amd64`, `linux/arm64`,
    /// `darwin/amd64`, `darwin/arm64`, `windows/amd64`. [`Any`](Self::Any)
    /// trails the list as a platform-agnostic fallback, mirroring the
    /// suitable-first / `any`-last shape of [`supported_set`](Self::supported_set):
    /// [`select`](crate::oci::Index::select) iterates the list as a preference
    /// order and short-circuits on the first match, so the trailing `Any` is
    /// consulted only when no concrete platform matched. That lets an
    /// `any`-published companion (CA bundle, tool config) resolve during
    /// `ocx patch sync` while normal multi-platform companions still match
    /// their specific manifest first.
    pub fn all_supported() -> Vec<Self> {
        vec![
            Self::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Arm64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Darwin,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Darwin,
                arch: Architecture::Arm64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            },
            Self::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            },
            // Trailing platform-agnostic fallback (mirrors `supported_set`): only
            // reached by `select` when no concrete platform above matched.
            Self::Any,
        ]
    }
}

/// Defaults to [`Platform::Any`] (platform-agnostic).
impl Default for Platform {
    fn default() -> Self {
        Self::any()
    }
}

/// Join a feature-value vector for [`Platform::lock_key`] with a `,` separator,
/// percent-escaping each value first so the join is injective.
///
/// Without per-component escaping the `,` join is ambiguous: `["a,b"]` and
/// `["a","b"]` both naively render `a,b`. Escaping the separator inside each
/// value (`a%2Cb` vs `a,b`) restores injectivity over arbitrary values.
fn join_feature_values(values: &[String]) -> String {
    values
        .iter()
        .map(|value| escape_feature_component(value))
        .collect::<Vec<_>>()
        .join(",")
}

/// Percent-escape every character that carries structural meaning anywhere in a
/// [`Platform::lock_key`] — both the slash-separated base segments and the
/// suffix feature values — so a registry-controlled string can never forge a
/// separator or marker.
///
/// Escapes `%` (the escape introducer — first, so escapes are unambiguous),
/// `/` (the base-segment separator), `,` (the suffix value separator), `;` (the
/// marker separator), and `=` (the marker delimiter). Escaping `/` is what keeps
/// the base injective: without it a `variant = "v8/10"` (no `os_version`) would
/// stringify to `…/v8/10` and collide with `variant = "v8", os_version = "10"`,
/// since the base joins segments with `/`. All other bytes pass through verbatim,
/// so the common ASCII values — `variant` (`v7`, `v8`), `os_version`
/// (`10.0.14393.1066`), feature names (`win32k`, `sse4`, `aes`) — are unchanged,
/// preserving the `lock_key`-equals-`Display` common case.
fn escape_feature_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '%' => out.push_str("%25"),
            '/' => out.push_str("%2F"),
            ',' => out.push_str("%2C"),
            ';' => out.push_str("%3B"),
            '=' => out.push_str("%3D"),
            other => out.push(other),
        }
    }
    out
}

/// Reverses [`escape_feature_component`]: decodes the five percent-escapes it
/// emits (`%25 %2F %2C %3B %3D`) back to their literal bytes and passes every
/// other character through verbatim.
///
/// Returns `None` on a malformed escape (`%` not followed by one of the five
/// known two-character codes) — the input is not a value this codec produced.
fn unescape_feature_component(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            let decoded = match (chars.next(), chars.next()) {
                (Some('2'), Some('5')) => '%',
                (Some('2'), Some('F')) => '/',
                (Some('2'), Some('C')) => ',',
                (Some('3'), Some('B')) => ';',
                (Some('3'), Some('D')) => '=',
                _ => return None,
            };
            out.push(decoded);
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

/// Splits a `;osf=` / `;feat=` marker's joined value on unescaped `,` and
/// unescapes each component — the inverse of [`join_feature_values`].
///
/// An empty joined string yields an empty vector (the marker carried no
/// values). Returns `None` when any component fails to unescape.
fn split_marker_values(joined: &str) -> Option<Vec<String>> {
    if joined.is_empty() {
        return Some(Vec::new());
    }
    joined.split(',').map(unescape_feature_component).collect()
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Any => write!(f, "{}", ANY_STR),
            Self::Specific {
                os,
                arch,
                variant,
                os_version,
                os_features,
                ..
            } => {
                write!(f, "{}/{}", os, arch)?;
                if let Some(variant) = variant {
                    write!(f, "/{}", variant)?;
                }
                if let Some(os_version) = os_version {
                    write!(f, "/{}", os_version)?;
                }
                // Append each `os.features` value as a `+`-separated suffix,
                // normalized (sorted + deduped) so the output is canonical and
                // round-trips through `FromStr`. Empty set → no suffix.
                for feature in normalize_os_features(os_features) {
                    write!(f, "+{}", feature)?;
                }
                Ok(())
            }
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = PlatformError;

    fn from_str(value: &str) -> std::result::Result<Self, PlatformError> {
        // Split off the optional `+`-separated `os.features` suffix from the
        // `os/arch[/variant]` core. Anything after the first `+` is a feature
        // token; an empty feature token (`linux/amd64+`) is rejected as a
        // malformed format. No suffix → empty feature set.
        let (core, os_features) = match value.split_once('+') {
            None => (value, Vec::new()),
            Some((core, features_str)) => {
                let features: Vec<String> = features_str.split('+').map(str::to_string).collect();
                if features.iter().any(|feature| feature.is_empty()) {
                    return Err(PlatformError {
                        input: value.to_string(),
                        kind: PlatformErrorKind::InvalidFormat,
                    });
                }
                (core, features)
            }
        };

        if core == ANY_STR {
            // `any` carries no os.features — mirrors the mirror-resolver rule.
            if !os_features.is_empty() {
                return Err(PlatformError {
                    input: value.to_string(),
                    kind: PlatformErrorKind::InvalidFormat,
                });
            }
            return Ok(Self::Any);
        }

        let parts: Vec<&str> = core.split('/').collect();
        if parts.len() < 2 || parts.len() > 4 {
            return Err(PlatformError {
                input: value.to_string(),
                kind: PlatformErrorKind::InvalidFormat,
            });
        }

        let os_str = parts[0];
        let arch_str = parts[1];
        if parts.len() == 2 && os_str == ANY_STR && arch_str == ANY_STR {
            // `any/any` is the agnostic platform; it likewise carries no features.
            if !os_features.is_empty() {
                return Err(PlatformError {
                    input: value.to_string(),
                    kind: PlatformErrorKind::InvalidFormat,
                });
            }
            return Ok(Self::Any);
        }

        let os: OperatingSystem = os_str.parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;

        let arch: Architecture = arch_str.parse().map_err(|kind| PlatformError {
            input: value.to_string(),
            kind,
        })?;

        let variant = if parts.len() > 2 {
            Some(parts[2].to_string())
        } else {
            None
        };

        let os_version = if parts.len() > 3 {
            Some(parts[3].to_string())
        } else {
            None
        };

        Ok(Self::Specific {
            os,
            arch,
            variant,
            os_version,
            // Normalize (sort + dedup) so the parsed value matches the
            // canonical wire form used everywhere else.
            os_features: normalize_os_features(&os_features),
            features: None,
        })
    }
}

/// Sort + dedup `os_features` so the value is canonical.
///
/// Returns an empty `Vec` for an empty input. Keeps cascade eviction
/// (positional `Vec` equality on the wire) and `Display` output deterministic
/// regardless of the order the caller supplied.
fn normalize_os_features(os_features: &[String]) -> Vec<String> {
    // Fast path: 0 or 1 element is already sorted and deduplicated.
    if os_features.len() <= 1 {
        return os_features.to_vec();
    }
    let mut features = os_features.to_vec();
    features.sort();
    features.dedup();
    features
}

// --- Native conversion impls ---

impl From<&Platform> for native::Platform {
    fn from(p: &Platform) -> Self {
        match p {
            Platform::Any => native::Platform {
                os: native::Os::Other(ANY_STR.to_string()),
                architecture: native::Arch::Other(ANY_STR.to_string()),
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            },
            Platform::Specific {
                os,
                arch,
                variant,
                os_version,
                os_features,
                // `features` is RESERVED by OCI v1.1.1 and must never be
                // serialized — emit `None` regardless of any inner value.
                features,
            } => {
                debug_assert!(
                    features.is_none(),
                    "RESERVED platform.features must never be set on Platform::Specific"
                );
                // Normalize (sort + dedup) so the wire format is canonical.
                // Cascade eviction at `oci/client.rs` compares `os_features`
                // by positional `Vec` equality; a reordered array would
                // otherwise fail to evict the prior entry and bloat the index.
                // Convert OCX's empty-`Vec`-means-none to the native
                // `Option<Vec>` at this boundary so an empty set is omitted
                // from the serialized JSON (`skip_serializing_if`).
                let normalized = normalize_os_features(os_features);
                native::Platform {
                    os: (*os).into(),
                    architecture: (*arch).into(),
                    variant: variant.clone(),
                    // RESERVED — never serialize (see above).
                    features: None,
                    os_version: os_version.clone(),
                    os_features: if normalized.is_empty() { None } else { Some(normalized) },
                }
            }
        }
    }
}

impl From<Platform> for native::Platform {
    fn from(p: Platform) -> Self {
        native::Platform::from(&p)
    }
}

impl TryFrom<native::Platform> for Platform {
    type Error = crate::Error;

    fn try_from(platform: native::Platform) -> Result<Self> {
        let input = platform.to_string();

        if let (native::Os::Other(os), native::Arch::Other(arch)) = (&platform.os, &platform.architecture) {
            if os == ANY_STR && arch == ANY_STR && platform.variant.is_none() && platform.os_version.is_none() {
                return Ok(Platform::Any);
            }
            return Err(PlatformError {
                input,
                kind: PlatformErrorKind::Unsupported(platform.to_string()),
            }
            .into());
        }

        let os = OperatingSystem::try_from(platform.os).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;
        let arch = Architecture::try_from(platform.architecture).map_err(|kind| PlatformError {
            input: input.clone(),
            kind,
        })?;

        // `features` is RESERVED by OCI v1.1.1. Foreign manifests written by
        // stale tooling may still carry a value — warn and drop it rather than
        // hard-erroring, so OCX stays interoperable with such registries.
        if platform.features.is_some() {
            tracing::warn!(
                "dropping RESERVED `features` field from platform '{}' (OCI v1.1.1 reserves it)",
                input
            );
        }

        Ok(Self::Specific {
            os,
            arch,
            variant: platform.variant,
            os_version: platform.os_version,
            // Convert the native `Option<Vec>` to OCX's empty-means-none `Vec`
            // at this boundary: both `None` and `Some(empty)` become an empty
            // set. Normalize (sort + dedup) inbound: foreign manifests may carry
            // duplicated or unordered `os_features`, and selection scores
            // specificity by `os_features.len()` — an un-deduped array would
            // inflate that score and skew candidate ranking.
            os_features: normalize_os_features(&platform.os_features.unwrap_or_default()),
            features: None,
        })
    }
}

impl TryFrom<Option<native::Platform>> for Platform {
    type Error = crate::Error;

    fn try_from(platform: Option<native::Platform>) -> Result<Self> {
        match platform {
            Some(p) => Self::try_from(p),
            None => Ok(Self::default()),
        }
    }
}

// --- Serde via native::Platform ---
//
// Serialization converts to native::Platform first, ensuring OCI-compatible
// JSON field names. Deserialization goes the reverse path: JSON → native::Platform
// → Platform (via TryFrom, which validates supported OS/arch).

impl Serialize for Platform {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        native::Platform::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Platform {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let native = native::Platform::deserialize(deserializer)?;
        Platform::try_from(native).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Display / FromStr ---

    #[test]
    fn display_fromstr_roundtrip() {
        let cases = [
            "any",
            "linux/amd64",
            "darwin/arm64",
            "windows/amd64",
            "linux/amd64/v8",
            "linux/arm64/v8/10.0.14393",
        ];
        for case in cases {
            let platform: Platform = case.parse().unwrap();
            let displayed = platform.to_string();
            let reparsed: Platform = displayed.parse().unwrap();
            assert_eq!(platform, reparsed, "roundtrip failed for '{}'", case);
        }
    }

    #[test]
    fn fromstr_invalid_format() {
        assert!("".parse::<Platform>().is_err());
        assert!("just_one".parse::<Platform>().is_err());
        assert!("a/b/c/d/e".parse::<Platform>().is_err());
    }

    #[test]
    fn fromstr_invalid_os() {
        let err = "linus/amd64".parse::<Platform>().unwrap_err();
        assert!(matches!(err.kind, PlatformErrorKind::UnsupportedOs { .. }));
    }

    #[test]
    fn fromstr_invalid_arch() {
        let err = "linux/x86".parse::<Platform>().unwrap_err();
        assert!(matches!(err.kind, PlatformErrorKind::UnsupportedArch { .. }));
    }

    #[test]
    fn any_any_parses_as_any() {
        let platform: Platform = "any/any".parse().unwrap();
        assert!(platform.is_any());
    }

    // --- FromStr with os.features (`+feature`) suffix ---

    #[test]
    fn fromstr_single_feature() {
        let platform: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(os_features.as_slice(), &["libc.glibc".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_feature_with_variant() {
        let platform: Platform = "linux/amd64/v3+libc.musl".parse().unwrap();
        match &platform {
            Platform::Specific {
                variant, os_features, ..
            } => {
                assert_eq!(variant.as_deref(), Some("v3"));
                assert_eq!(os_features.as_slice(), &["libc.musl".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_multiple_features_sorted_and_deduped() {
        // Input order `b.y` before `a.x` plus a duplicate; the parsed value
        // must be sorted and deduped.
        let platform: Platform = "linux/amd64+b.y+a.x+a.x".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["a.x".to_string(), "b.y".to_string()],
                    "features must be sorted + deduped"
                );
            }
            _ => panic!("expected Specific"),
        }
    }

    #[test]
    fn fromstr_dual_libc_features() {
        // A dual-libc `--platform` override carries both libc tags via repeated
        // `+`; they parse into a sorted os_features Vec.
        let platform: Platform = "linux/amd64+libc.glibc+libc.musl".parse().unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.glibc".to_string(), "libc.musl".to_string()],
                    "both libc features must parse, sorted"
                );
            }
            _ => panic!("expected Specific"),
        }
        // Round-trips through Display.
        let reparsed: Platform = platform.to_string().parse().unwrap();
        assert_eq!(platform, reparsed, "dual-libc --platform must round-trip");
    }

    #[test]
    fn fromstr_feature_roundtrips_via_display() {
        for case in [
            "linux/amd64+libc.glibc",
            "linux/amd64/v3+libc.musl",
            "linux/amd64+a.x+b.y",
        ] {
            let platform: Platform = case.parse().unwrap();
            let arg = platform.to_string();
            let reparsed: Platform = arg.parse().unwrap();
            assert_eq!(platform, reparsed, "round-trip via Display failed for '{case}'");
        }
    }

    #[test]
    fn fromstr_empty_feature_token_is_error() {
        assert!(
            "linux/amd64+".parse::<Platform>().is_err(),
            "trailing + must be rejected"
        );
        assert!(
            "linux/amd64+libc.glibc+".parse::<Platform>().is_err(),
            "empty trailing feature token must be rejected"
        );
    }

    #[test]
    fn fromstr_any_with_features_is_error() {
        assert!(
            "any+libc.glibc".parse::<Platform>().is_err(),
            "`any` must reject os.features"
        );
        assert!(
            "any/any+libc.glibc".parse::<Platform>().is_err(),
            "`any/any` must reject os.features"
        );
    }

    #[test]
    fn display_renders_sorted_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["b.y".to_string(), "a.x".to_string()],
            features: None,
        };
        assert_eq!(platform.to_string(), "linux/amd64+a.x+b.y");
    }

    #[test]
    fn display_without_features_omits_suffix() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert_eq!(platform.to_string(), "linux/amd64");
        assert_eq!(Platform::Any.to_string(), "any");
    }

    #[test]
    fn display_includes_os_features() {
        // Display is now the canonical --platform form: it carries the
        // +features suffix. Filesystem paths use segments()/ascii_segments().
        let platform: Platform = "linux/amd64+libc.glibc".parse().unwrap();
        assert_eq!(platform.to_string(), "linux/amd64+libc.glibc");
    }

    /// Returns `linux/amd64` with a non-`None` `os_version` — the shape a
    /// mirrored image-index entry carries but [`Platform::current`] can never
    /// produce.
    fn linux_amd64_with_os_version() -> Platform {
        let base: Platform = "linux/amd64".parse().unwrap();
        let Platform::Specific { os, arch, .. } = base else {
            panic!("linux/amd64 parses to Specific");
        };
        Platform::Specific {
            os,
            arch,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        }
    }

    #[test]
    fn same_os_arch_ignores_refinement_fields() {
        let plain: Platform = "linux/amd64".parse().unwrap();
        // os+arch equal, differ only in os_version → same_os_arch ignores the refinement.
        assert!(plain.same_os_arch(&linux_amd64_with_os_version()));
        // Different arch / os → not same.
        assert!(!plain.same_os_arch(&"linux/arm64".parse().unwrap()));
        assert!(!plain.same_os_arch(&"windows/amd64".parse().unwrap()));
        // Any algebra.
        assert!(Platform::Any.same_os_arch(&Platform::Any));
        assert!(!Platform::Any.same_os_arch(&plain));
    }

    /// Regression (issue #179): the host-only gate compares os+arch only, so a
    /// host-runnable platform carrying an `os_version` is NOT suppressed.
    #[test]
    fn host_can_run_on_gate() {
        let host: Platform = "linux/amd64".parse().unwrap();
        let host = Some(&host);

        // Undeterminable platform and platform-agnostic package always run.
        assert!(Platform::host_can_run_on(None, host));
        assert!(Platform::host_can_run_on(Some(&Platform::any()), host));

        // Matching os+arch runs; a differing os or arch does not.
        assert!(Platform::host_can_run_on(Some(&"linux/amd64".parse().unwrap()), host));
        assert!(!Platform::host_can_run_on(
            Some(&"windows/amd64".parse().unwrap()),
            host
        ));
        assert!(!Platform::host_can_run_on(Some(&"linux/arm64".parse().unwrap()), host));

        // Key case: matching os+arch with an os_version refinement still runs.
        assert!(Platform::host_can_run_on(Some(&linux_amd64_with_os_version()), host));

        // Unknown host never suppresses.
        let foreign: Platform = "windows/amd64".parse().unwrap();
        assert!(Platform::host_can_run_on(Some(&foreign), None));
    }

    // --- segments() ---

    #[test]
    fn segments_any() {
        assert_eq!(Platform::Any.segments(), vec!["any"]);
    }

    #[test]
    fn segments_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "amd64"]);
    }

    #[test]
    fn segments_with_variant() {
        let platform: Platform = "linux/arm64/v8".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "arm64", "v8"]);
    }

    #[test]
    fn segments_with_variant_and_os_version() {
        let platform: Platform = "linux/arm64/v8/10.0.14393".parse().unwrap();
        assert_eq!(platform.segments(), vec!["linux", "arm64", "v8", "10.0.14393"]);
    }

    // --- ascii_segments() ---

    #[test]
    fn ascii_segments_lowercases() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("V8".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        assert_eq!(platform.ascii_segments(), vec!["linux", "arm64", "v8"]);
    }

    #[test]
    fn ascii_segments_any() {
        assert_eq!(Platform::Any.ascii_segments(), vec!["any"]);
    }

    // --- Misc ---

    #[test]
    fn current_returns_some() {
        let current = Platform::current();
        assert!(current.is_some());
        assert!(!current.unwrap().is_any());
    }

    // --- all_supported() ---

    #[test]
    fn all_supported_returns_five_concrete_then_any_fallback() {
        let platforms = Platform::all_supported();
        let displayed: Vec<String> = platforms.iter().map(ToString::to_string).collect();
        assert_eq!(
            displayed,
            vec![
                "linux/amd64",
                "linux/arm64",
                "darwin/amd64",
                "darwin/arm64",
                "windows/amd64",
                "any"
            ],
            "all_supported must return the five concrete platforms in canonical order, then `any` last"
        );
    }

    #[test]
    fn all_supported_includes_any_as_trailing_fallback() {
        let platforms = Platform::all_supported();
        assert!(
            platforms.last().is_some_and(Platform::is_any),
            "all_supported must end with the platform-agnostic `any` fallback so an `any`-published \
             companion resolves when no concrete platform matches; got {:?}",
            platforms.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    /// `all_supported` is distinct from `supported_set` — the latter is
    /// scoped to the host (its own platform + `any`, at most 2 entries),
    /// while `all_supported` carries the full concrete matrix plus `any`.
    /// The distinction is concrete breadth, not `any`-presence (both now
    /// end with `any`).
    #[test]
    fn all_supported_is_distinct_from_supported_set() {
        assert_ne!(Platform::all_supported().len(), Platform::supported_set().len());
    }

    // --- lock_key (V2 map key encoding) ---

    /// The common case is byte-identical to `Display` — `lock_key` must not
    /// bloat the typical `linux/amd64` / `darwin/arm64/v8` / `any` keys.
    #[test]
    fn lock_key_common_case_equals_display() {
        for case in ["any", "linux/amd64", "darwin/arm64", "windows/amd64", "linux/arm64/v8"] {
            let platform: Platform = case.parse().unwrap();
            assert_eq!(
                platform.lock_key(),
                platform.to_string(),
                "lock_key must equal Display for the common case '{case}'"
            );
        }
    }

    /// Lossless + injective: two advertised children that differ ONLY in
    /// `os_features` must produce DISTINCT keys — a lossy key (plain Display)
    /// would collide them and convert a `select`-installable multi-variant
    /// package into a hard lock failure (Codex R1).
    #[test]
    fn lock_key_distinguishes_os_features() {
        let base = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let with_win32k = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["win32k".to_string()],
            features: None,
        };
        assert_ne!(
            base.lock_key(),
            with_win32k.lock_key(),
            "platforms differing only in os_features must get distinct lock keys"
        );
    }

    /// Lossless + injective: two advertised children that differ ONLY in CPU
    /// `features` must produce DISTINCT keys.
    #[test]
    fn lock_key_distinguishes_cpu_features() {
        let base = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let with_sse4 = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: Some(vec!["sse4".to_string()]),
        };
        assert_ne!(
            base.lock_key(),
            with_sse4.lock_key(),
            "platforms differing only in CPU features must get distinct lock keys"
        );
    }

    /// Injectivity for separator-bearing feature VALUES (Codex R1 / F4): a
    /// single value that itself contains the `,` separator must NOT alias a
    /// two-element vector. Without per-component escaping, `["a,b"]` and
    /// `["a","b"]` both naively render `;feat=a,b` and collide — which turns a
    /// VALID multi-variant manifest into a spurious `DuplicatePlatformKey` lock
    /// failure. The escaped encoding keeps them distinct.
    #[test]
    fn lock_key_injective_for_separator_bearing_feature_values() {
        let with_features = |features: Vec<String>| Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: Some(features),
        };
        let one_element_with_comma = with_features(vec!["a,b".to_string()]);
        let two_elements = with_features(vec!["a".to_string(), "b".to_string()]);
        assert_ne!(
            one_element_with_comma.lock_key(),
            two_elements.lock_key(),
            "`[\"a,b\"]` (one value containing a comma) must not collide with `[\"a\",\"b\"]` (two values)"
        );

        // Same hazard via the os_features vector and via a value that contains
        // the literal marker text `;feat=` (which must be escaped, not forge a
        // second marker).
        let osf_one = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["x,y".to_string()],
            features: None,
        };
        let osf_two = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["x".to_string(), "y".to_string()],
            features: None,
        };
        assert_ne!(
            osf_one.lock_key(),
            osf_two.lock_key(),
            "separator-bearing os_features values must not collide with a multi-element vector"
        );

        let forged_marker = with_features(vec![";feat=win32k".to_string()]);
        let plain = with_features(vec!["win32k".to_string()]);
        assert_ne!(
            forged_marker.lock_key(),
            plain.lock_key(),
            "a feature value containing the literal `;feat=` marker text must not forge a collision"
        );
    }

    /// Injectivity for marker-bearing `os_version` / `variant` VALUES (Codex
    /// R1 backstop — Finding 2): the registry-controlled `variant` and
    /// `os_version` strings enter the key base via `Display`. Without escaping
    /// them, a crafted `os_version` such as `"10;osf=win32k"` forges a `;osf=`
    /// marker, so a platform with that bare `os_version` (and no real
    /// `os_features`) collides on one key with a DIFFERENT platform that has a
    /// plain `os_version` plus a genuine `os_features = ["win32k"]`. Two
    /// distinct advertised platforms must NOT share a `lock_key`.
    #[test]
    fn lock_key_injective_for_marker_bearing_os_version() {
        // Platform A: os_version forges a `;osf=win32k` marker, no real os_features.
        let forged = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10;osf=win32k".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        // Platform B: plain os_version `10`, genuine os_features `["win32k"]`.
        let genuine = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10".to_string()),
            os_features: vec!["win32k".to_string()],
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a marker-forging os_version must not collide with a genuine os_features platform"
        );
    }

    /// Same hazard via the registry-controlled `variant` string: a crafted
    /// `variant = "v8;feat=sse4"` must not forge a `;feat=` marker that
    /// collides with a genuine `features = ["sse4"]` platform.
    #[test]
    fn lock_key_injective_for_marker_bearing_variant() {
        let forged = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8;feat=sse4".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let genuine = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: Some(vec!["sse4".to_string()]),
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a marker-forging variant must not collide with a genuine features platform"
        );
    }

    /// Injectivity for slash-bearing `variant` VALUES (Block B): the base is
    /// joined with `/`, so a `variant = "v8/10"` (no `os_version`) would
    /// stringify to `…/v8/10` — byte-identical to `variant = "v8",
    /// os_version = "10"` — unless the base segments escape `/`. Two distinct
    /// advertised platforms must NOT share a `lock_key`, or a valid
    /// multi-variant manifest becomes a spurious `DuplicatePlatformKey` lock
    /// failure.
    #[test]
    fn lock_key_injective_for_slash_bearing_variant() {
        // Platform A: variant carries an embedded slash, no os_version.
        let forged = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8/10".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        // Platform B: plain variant `v8`, separate os_version `10`.
        let genuine = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: Some("v8".to_string()),
            os_version: Some("10".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a slash-bearing variant must not forge an os_version segment and collide"
        );
    }

    /// Same hazard via the registry-controlled `os_version` string: a crafted
    /// `os_version` containing a `/` must not forge an extra base segment. A
    /// platform with `os_version = "10/x"` (no `os_features`) must stay distinct
    /// from any other platform whose base segments would otherwise align.
    #[test]
    fn lock_key_injective_for_slash_bearing_os_version() {
        // Platform A: os_version itself contains a slash.
        let forged = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_version: Some("10/x".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        // Platform B: variant `v3/10`, os_version `x` — the naive (unescaped)
        // base join of both is `windows/amd64/v3/10/x`, so they would collide
        // without `/` escaping.
        let genuine = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3/10".to_string()),
            os_version: Some("x".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        assert_ne!(
            forged.lock_key(),
            genuine.lock_key(),
            "a slash-bearing os_version must not forge an extra base segment and collide"
        );
    }

    /// The escape is confined to feature values: a value with no structural
    /// characters round-trips verbatim, so common feature names are unchanged
    /// and the key stays human-readable.
    #[test]
    fn lock_key_leaves_plain_feature_values_unescaped() {
        let plain = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["win32k".to_string()],
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
        };
        assert_eq!(
            plain.lock_key(),
            "windows/amd64;osf=win32k;feat=sse4,aes",
            "plain ASCII feature names must pass through unescaped"
        );
    }

    /// Injectivity backstop: a set of platforms that are pairwise distinct
    /// (including the features-only differences) must yield pairwise-distinct
    /// keys — the structural no-collision guarantee the dup-key guard relies
    /// on.
    #[test]
    fn lock_key_is_injective_over_distinct_platforms() {
        let win = |os_features: Vec<String>, features: Option<Vec<String>>| Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features,
            features,
        };
        let platforms = vec![
            "linux/amd64".parse::<Platform>().unwrap(),
            "linux/arm64".parse::<Platform>().unwrap(),
            "darwin/arm64".parse::<Platform>().unwrap(),
            Platform::Any,
            win(Vec::new(), None),
            win(vec!["win32k".to_string()], None),
            win(Vec::new(), Some(vec!["sse4".to_string()])),
        ];
        let mut keys = std::collections::HashSet::new();
        for platform in &platforms {
            assert!(
                keys.insert(platform.lock_key()),
                "lock_key collision for {platform:?}; keys so far: {keys:?}"
            );
        }
        assert_eq!(
            keys.len(),
            platforms.len(),
            "every distinct platform must get a distinct key"
        );
    }

    // --- from_lock_key (exact inverse of lock_key) ---

    /// `from_lock_key` is the exact inverse of `lock_key` in both directions:
    /// `from_lock_key(p.lock_key()) == p` (value round-trip) AND
    /// `from_lock_key(k).lock_key() == k` (key round-trip). Covers every
    /// supported platform plus crafted feature-bearing platforms whose
    /// `variant`/`os_version`/feature values carry the escaped structural
    /// characters `;`, `,`, and `/`.
    #[test]
    fn from_lock_key_is_exact_inverse_of_lock_key() {
        let linux = |variant: Option<&str>,
                     os_version: Option<&str>,
                     os_features: Vec<&str>,
                     features: Option<Vec<&str>>| Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: variant.map(str::to_string),
            os_version: os_version.map(str::to_string),
            os_features: os_features.into_iter().map(str::to_string).collect(),
            features: features.map(|values| values.into_iter().map(str::to_string).collect()),
        };

        let mut cases = Platform::all_supported();
        cases.extend([
            // Single + multiple os.features.
            linux(None, None, vec!["libc.glibc"], None),
            linux(None, None, vec!["libc.glibc", "libc.musl"], None),
            // Feature values carrying every escaped structural character.
            linux(None, None, vec!["a;b", "c,d", "e/f"], None),
            // Escaped `/` in variant and `;` in os_version (base-segment escapes).
            linux(Some("v8/10"), Some("10;x"), vec![], None),
            // CPU `features` with an escaped `,` in a value.
            linux(None, None, vec![], Some(vec!["sse4", "a,b"])),
            // All fields populated at once.
            linux(
                Some("v3"),
                Some("10.0.14393"),
                vec!["win32k"],
                Some(vec!["sse4", "aes"]),
            ),
        ]);

        for platform in &cases {
            let key = platform.lock_key();
            let parsed = Platform::from_lock_key(&key).expect("lock key round-trips");
            assert_eq!(&parsed, platform, "value round-trip failed for key '{key}'");
            assert_eq!(parsed.lock_key(), key, "key round-trip failed for '{key}'");
        }
    }

    /// A malformed base structure is rejected as an invalid format rather than
    /// silently producing a bogus platform.
    #[test]
    fn from_lock_key_rejects_malformed_input() {
        assert!(Platform::from_lock_key("linux").is_err(), "single segment must fail");
        assert!(
            Platform::from_lock_key("linux/amd64/v8/10/extra").is_err(),
            "too many base segments must fail"
        );
        assert!(
            Platform::from_lock_key("linux/amd64;osf=%ZZ").is_err(),
            "malformed percent-escape must fail"
        );
        assert!(
            Platform::from_lock_key("nope/amd64").is_err(),
            "unsupported os must fail"
        );
    }

    #[test]
    fn from_lock_key_positional_ambiguity_for_os_version_without_variant() {
        // Documented limitation (shared with FromStr): the base is positional,
        // so a `variant: None, os_version: Some(_)` platform round-trips into the
        // `variant` slot. This shape never occurs in the dependency-pinning path;
        // this test pins the KNOWN behavior so a future change is deliberate, not
        // accidental.
        let original = linux_amd64_with_os_version();
        let key = original.lock_key();
        assert_eq!(key, "linux/amd64/10.0.14393", "os_version fills base slot 3");
        let reconstructed = Platform::from_lock_key(&key).expect("valid base parses");
        let Platform::Specific {
            variant, os_version, ..
        } = &reconstructed
        else {
            panic!("expected Specific");
        };
        assert_eq!(variant.as_deref(), Some("10.0.14393"), "value lands in variant slot");
        assert_eq!(os_version.as_deref(), None, "os_version slot stays empty");
        // The value is preserved even though the slot differs, so lock_key (the
        // only way the result is consumed) still round-trips string-for-string.
        assert_eq!(reconstructed.lock_key(), key, "lock_key round-trips despite slot swap");
    }

    #[test]
    fn hash_equality() {
        let a: Platform = "linux/amd64".parse().unwrap();
        let b: Platform = "linux/amd64".parse().unwrap();
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }

    #[test]
    fn default_is_any() {
        assert!(Platform::default().is_any());
    }

    // --- from_image_manifest / from_image_index / from_manifest ---

    #[test]
    fn from_image_manifest_returns_any() {
        let manifest = native::ImageManifest {
            schema_version: 2,
            media_type: None,
            config: native::OciDescriptor {
                media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                digest: "sha256:abc".to_string(),
                size: 100,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: vec![],
            annotations: None,
            artifact_type: None,
            subject: None,
        };
        assert!(Platform::from_image_manifest(&manifest).is_any());
    }

    #[test]
    fn from_image_index_extracts_platforms() {
        let index = native::ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests: vec![
                native::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: "sha256:aaa".to_string(),
                    size: 100,
                    platform: Some(native::Platform {
                        os: native::Os::Linux,
                        architecture: native::Arch::Amd64,
                        variant: None,
                        features: None,
                        os_version: None,
                        os_features: None,
                    }),
                    artifact_type: None,
                    annotations: None,
                },
                native::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: "sha256:bbb".to_string(),
                    size: 200,
                    platform: None,
                    artifact_type: None,
                    annotations: None,
                },
            ],
            artifact_type: None,
            annotations: None,
        };
        let platforms = Platform::from_image_index(&index).unwrap();
        assert_eq!(platforms.len(), 2);
        assert_eq!(platforms[0].to_string(), "linux/amd64");
        assert!(platforms[1].is_any());
    }

    #[test]
    fn from_image_index_rejects_unsupported_platform() {
        let index = native::ImageIndex {
            schema_version: 2,
            media_type: None,
            manifests: vec![native::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: "sha256:ccc".to_string(),
                size: 100,
                platform: Some(native::Platform {
                    os: native::Os::FreeBSD,
                    architecture: native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                }),
                artifact_type: None,
                annotations: None,
            }],
            artifact_type: None,
            annotations: None,
        };
        assert!(Platform::from_image_index(&index).is_err());
    }

    // --- Serde ---

    #[test]
    fn serde_roundtrip_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
    }

    #[test]
    fn serde_oci_field_names() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os"], "linux");
        assert_eq!(value["architecture"], "amd64");
    }

    #[test]
    fn serde_any_serializes_with_any_values() {
        let json = serde_json::to_string(&Platform::Any).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os"], "any");
        assert_eq!(value["architecture"], "any");
    }

    #[test]
    fn serde_any_roundtrip() {
        let json = serde_json::to_string(&Platform::Any).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_any());
    }

    #[test]
    fn serde_os_version_field_name() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["os.version"], "10.0.14393");
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
    }

    #[test]
    fn serde_os_features_accepted_and_roundtripped() {
        let json = r#"{"os":"windows","architecture":"amd64","os.features":["win32k"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(os_features.as_slice(), &["win32k".to_string()]);
            }
            _ => panic!("expected Specific"),
        }
        let json_out = serde_json::to_string(&platform).unwrap();
        let reparsed: Platform = serde_json::from_str(&json_out).unwrap();
        assert_eq!(platform, reparsed);
    }

    #[test]
    fn serde_rejects_unsupported_os_from_registry() {
        let json = r#"{"architecture":"ppc64le","os":"linux"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_rejects_unsupported_arch_from_registry() {
        let json = r#"{"architecture":"s390x","os":"linux"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_rejects_unsupported_os() {
        let json = r#"{"architecture":"amd64","os":"freebsd"}"#;
        assert!(serde_json::from_str::<Platform>(json).is_err());
    }

    #[test]
    fn serde_roundtrip_all_supported_combinations() {
        for os in OperatingSystem::VARIANTS {
            for arch in Architecture::VARIANTS {
                let platform = Platform::Specific {
                    os: *os,
                    arch: *arch,
                    variant: None,
                    os_version: None,
                    os_features: Vec::new(),
                    features: None,
                };
                let json = serde_json::to_string(&platform).unwrap();
                let parsed: Platform = serde_json::from_str(&json).unwrap();
                assert_eq!(platform, parsed, "roundtrip failed for {}/{}", os, arch);
            }
        }
    }

    #[test]
    fn serde_variant_preserved_through_roundtrip() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v8".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let json = serde_json::to_string(&platform).unwrap();
        let parsed: Platform = serde_json::from_str(&json).unwrap();
        assert_eq!(platform, parsed);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["variant"], "v8");
    }

    #[test]
    fn serde_optional_fields_absent_when_none() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("variant").is_none());
        assert!(value.get("os.version").is_none());
        assert!(value.get("os.features").is_none());
        assert!(value.get("features").is_none());
    }

    // --- OCI JSON compatibility tests ---

    #[test]
    fn serde_deserialize_oci_index_platform_entry() {
        let json = r#"{"architecture":"amd64","os":"linux"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Amd64,
                variant: None,
                os_version: None,
                os_features: Vec::new(),
                features: None,
            }
        );
    }

    #[test]
    fn serde_deserialize_platform_with_variant() {
        let json = r#"{"architecture":"arm64","os":"linux","variant":"v8"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Linux,
                arch: Architecture::Arm64,
                variant: Some("v8".to_string()),
                os_version: None,
                os_features: Vec::new(),
                features: None,
            }
        );
    }

    #[test]
    fn serde_deserialize_platform_with_os_version() {
        let json = r#"{"architecture":"amd64","os":"windows","os.version":"10.0.14393.1066"}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: None,
                os_version: Some("10.0.14393.1066".to_string()),
                os_features: Vec::new(),
                features: None,
            }
        );
    }

    // --- Native roundtrip (lossless: native → Platform → native) ---

    #[test]
    fn native_roundtrip_specific() {
        let platform: Platform = "linux/amd64".parse().unwrap();
        let native_plat: native::Platform = platform.clone().into();
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_any() {
        let native_plat: native::Platform = Platform::Any.into();
        let back = Platform::try_from(native_plat).unwrap();
        assert!(back.is_any());
    }

    #[test]
    fn native_roundtrip_with_variant() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v8".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.variant, Some("v8".to_string()));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_os_version() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.os_version, Some("10.0.14393.1066".to_string()));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_os_features() {
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: vec!["win32k".to_string()],
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        assert_eq!(native_plat.os_features, Some(vec!["win32k".to_string()]));
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_with_all_optional_fields() {
        // `features` is RESERVED — deliberately dropped on conversion (see
        // `native_features_dropped_in_conversion`). The remaining fields must
        // survive a native → Platform → native roundtrip.
        let platform = Platform::Specific {
            os: OperatingSystem::Windows,
            arch: Architecture::Amd64,
            variant: Some("v3".to_string()),
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: vec!["win32k".to_string()],
            features: None,
        };
        let native_plat: native::Platform = platform.clone().into();
        let back = Platform::try_from(native_plat).unwrap();
        assert_eq!(platform, back);
    }

    #[test]
    fn native_roundtrip_all_supported_combinations() {
        for os in OperatingSystem::VARIANTS {
            for arch in Architecture::VARIANTS {
                let platform = Platform::Specific {
                    os: *os,
                    arch: *arch,
                    variant: None,
                    os_version: None,
                    os_features: Vec::new(),
                    features: None,
                };
                let native_plat: native::Platform = platform.clone().into();
                let back = Platform::try_from(native_plat).unwrap();
                assert_eq!(platform, back, "native roundtrip failed for {}/{}", os, arch);
            }
        }
    }

    // ── can_run: subset semantics (3.1) ──────────────────────────────

    fn linux_amd64_with_features(features: Vec<String>) -> Platform {
        Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: features,
            features: None,
        }
    }

    /// Helper: linux/amd64 host carrying `{libc.glibc}`.
    fn glibc_host() -> Platform {
        linux_amd64_with_features(vec!["libc.glibc".to_string()])
    }

    /// Helper: linux/amd64 candidate with `{libc.glibc}`.
    fn glibc_candidate() -> Platform {
        linux_amd64_with_features(vec!["libc.glibc".to_string()])
    }

    /// Helper: linux/amd64 candidate with `{libc.musl}`.
    fn musl_candidate() -> Platform {
        linux_amd64_with_features(vec!["libc.musl".to_string()])
    }

    /// Helper: linux/amd64 with no os_features.
    fn untagged_amd64() -> Platform {
        linux_amd64_with_features(Vec::new())
    }

    // 3.1 — Specific × Specific cases

    #[test]
    fn can_run_empty_candidate_subset_of_any_host() {
        // Host linux/amd64 {libc.glibc}, candidate linux/amd64 {} → true (empty ⊆ any)
        let host = glibc_host();
        let candidate = untagged_amd64();
        assert!(
            host.can_run(&candidate),
            "empty candidate os_features must match any host"
        );
    }

    #[test]
    fn can_run_same_libc_matches() {
        // Host {libc.glibc}, candidate {libc.glibc} → true
        let host = glibc_host();
        let candidate = glibc_candidate();
        assert!(host.can_run(&candidate), "matching libc must match");
    }

    #[test]
    fn can_run_different_libc_no_match() {
        // Host {libc.glibc}, candidate {libc.musl} → false
        let host = glibc_host();
        let candidate = musl_candidate();
        assert!(
            !host.can_run(&candidate),
            "host with libc.glibc must not match libc.musl candidate"
        );
    }

    #[test]
    fn can_run_host_missing_required_feature() {
        // Host {}, candidate {libc.glibc} → false (host cannot satisfy requirement)
        let host = untagged_amd64();
        let candidate = glibc_candidate();
        assert!(
            !host.can_run(&candidate),
            "host without libc tags must not match libc-tagged candidate"
        );
    }

    #[test]
    fn can_run_multi_libc_host_matches_musl_candidate() {
        // Host {libc.glibc, libc.musl}, candidate {libc.musl} → true
        let host = linux_amd64_with_features(vec!["libc.glibc".to_string(), "libc.musl".to_string()]);
        let candidate = musl_candidate();
        assert!(host.can_run(&candidate), "multi-libc host must match musl candidate");
    }

    #[test]
    fn can_run_different_os_no_match() {
        // Different os → false regardless of features
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["libc.glibc".to_string()],
            features: None,
        };
        let candidate = Platform::Specific {
            os: OperatingSystem::Darwin,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        assert!(!host.can_run(&candidate), "different os must not match");
    }

    #[test]
    fn can_run_different_arch_no_match() {
        // Different arch → false regardless of features
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["libc.glibc".to_string()],
            features: None,
        };
        let candidate = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: None,
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        assert!(!host.can_run(&candidate), "different arch must not match");
    }

    #[test]
    fn can_run_variant_mismatch_no_match() {
        // variant mismatch (both set) → false (strict equality on variant)
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v7".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        let candidate = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Arm64,
            variant: Some("v8".to_string()),
            os_version: None,
            os_features: Vec::new(),
            features: None,
        };
        assert!(!host.can_run(&candidate), "variant mismatch must not match");
    }

    // 3.1 — os_version strict equality when the candidate declares one.
    // `Platform::current()` never populates os_version, so a version-bearing
    // candidate must stay fail-closed until a version-range ADR exists.

    #[test]
    fn can_run_candidate_os_version_host_none_no_match() {
        // Host os_version None (the `current()` shape), candidate declares one → false.
        let host = untagged_amd64();
        let candidate = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        assert!(
            !host.can_run(&candidate),
            "a version-bearing candidate must not match a host with no os_version"
        );
    }

    #[test]
    fn can_run_differing_os_version_no_match() {
        // Both declare os_version but they differ → false (strict equality).
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        let candidate = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.19045".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        assert!(!host.can_run(&candidate), "differing os_version must not match");
    }

    #[test]
    fn can_run_equal_os_version_matches() {
        // Both declare the same os_version → true.
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        let candidate = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        assert!(host.can_run(&candidate), "equal os_version must match");
    }

    #[test]
    fn can_run_candidate_no_os_version_host_some_matches() {
        // Candidate declares no os_version (no requirement); host carries one → true.
        let host = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: Some("10.0.14393".to_string()),
            os_features: Vec::new(),
            features: None,
        };
        let candidate = untagged_amd64();
        assert!(
            host.can_run(&candidate),
            "a candidate declaring no os_version must match a version-bearing host"
        );
    }

    // 3.1 — Any semantics (amended after architect review B1)

    #[test]
    fn can_run_any_host_any_candidate() {
        // Host Any × candidate Any → true
        assert!(
            Platform::Any.can_run(&Platform::Any),
            "Any host must support Any candidate"
        );
    }

    #[test]
    fn can_run_specific_host_any_candidate() {
        // Host Specific × candidate Any → true (Any candidate runs everywhere)
        let host = glibc_host();
        assert!(
            host.can_run(&Platform::Any),
            "Specific host must support Any candidate (static-musl / scripts / JARs)"
        );
    }

    #[test]
    fn can_run_any_host_specific_candidate() {
        // Host Any × candidate Specific → false (detection failed; cannot satisfy Specific)
        let candidate = glibc_candidate();
        assert!(
            !Platform::Any.can_run(&candidate),
            "Any host must not match Specific candidate (host detection failed)"
        );
    }

    // ── serde: RESERVED features field rewrites (3.3) ──────────────

    /// REWRITE of serde_features_accepted_and_roundtripped (was: round-tripped; now: dropped on serialize).
    /// The OCI v1.1.1 `features` field is RESERVED — serializing it is a spec violation.
    /// After impl, a Platform with populated `features` must serialize WITHOUT a `features` key.
    #[test]
    fn serde_features_dropped_on_serialize() {
        // Populate a Platform with features (via native conversion path to bypass any guards).
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            variant: None,
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
            os_version: None,
            os_features: None,
        };
        // The Platform struct may or may not carry the value internally; what matters is the wire format.
        let platform = Platform::try_from(native_plat.clone()).unwrap();
        let json = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // After impl: the RESERVED `features` key must NOT appear in the serialized JSON.
        assert!(
            value.get("features").is_none(),
            "RESERVED `features` field must be absent from serialized Platform JSON; got: {}",
            json
        );
    }

    /// Features key in input JSON is ignored/stripped during deserialization.
    #[test]
    fn serde_features_in_input_json_is_ignored() {
        // A registry may emit a `features` value (stale tooling); we must not error.
        let json = r#"{"os":"linux","architecture":"amd64","features":["sse4","aes"]}"#;
        // Deserialization must succeed (warn-and-drop, not hard error).
        let result = serde_json::from_str::<Platform>(json);
        assert!(
            result.is_ok(),
            "Deserializing a platform with RESERVED `features` must not hard-error; err: {:?}",
            result.err()
        );
        // And serialize back: features must not reappear.
        let platform = result.unwrap();
        let json_out = serde_json::to_string(&platform).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert!(
            value.get("features").is_none(),
            "`features` reappeared after warn-and-drop deserialization; got: {}",
            json_out
        );
    }

    /// REWRITE of serde_deserialize_platform_with_all_optional_fields.
    /// `features` is removed from the test JSON; the existing fields must still deserialize.
    #[test]
    fn serde_deserialize_platform_with_all_optional_fields_no_features() {
        // `features` intentionally omitted — it is RESERVED.
        let json = r#"{
            "architecture":"amd64",
            "os":"windows",
            "variant":"v3",
            "os.version":"10.0.14393.1066",
            "os.features":["win32k"]
        }"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        assert_eq!(
            platform,
            Platform::Specific {
                os: OperatingSystem::Windows,
                arch: Architecture::Amd64,
                variant: Some("v3".to_string()),
                os_version: Some("10.0.14393.1066".to_string()),
                os_features: vec!["win32k".to_string()],
                features: None,
            }
        );
    }

    /// REWRITE of native_roundtrip_with_features.
    /// Any populated `features` in a native::Platform must be dropped (not round-tripped)
    /// through From<&Platform> for native::Platform because the field is RESERVED.
    #[test]
    fn native_features_dropped_in_conversion() {
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::Amd64,
            variant: None,
            features: Some(vec!["sse4".to_string(), "aes".to_string()]),
            os_version: None,
            os_features: None,
        };
        let platform = Platform::try_from(native_plat).unwrap();
        let roundtripped: native::Platform = native::Platform::from(&platform);
        // After impl: features must be None on the wire (RESERVED).
        assert_eq!(
            roundtripped.features, None,
            "RESERVED `features` must be emitted as None by From<&Platform> for native::Platform"
        );
    }

    /// Extension of serde_os_features_accepted_and_roundtripped with libc.* values (3.3).
    #[test]
    fn serde_os_features_libc_values_roundtripped() {
        // glibc
        let json_glibc = r#"{"os":"linux","architecture":"amd64","os.features":["libc.glibc"]}"#;
        let platform: Platform = serde_json::from_str(json_glibc).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.glibc".to_string()],
                    "libc.glibc must be preserved"
                );
            }
            _ => panic!("expected Specific platform"),
        }
        let json_out = serde_json::to_string(&platform).unwrap();
        let reparsed: Platform = serde_json::from_str(&json_out).unwrap();
        assert_eq!(platform, reparsed, "libc.glibc roundtrip failed");

        // musl
        let json_musl = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl"]}"#;
        let platform_musl: Platform = serde_json::from_str(json_musl).unwrap();
        match &platform_musl {
            Platform::Specific { os_features, .. } => {
                assert_eq!(
                    os_features.as_slice(),
                    &["libc.musl".to_string()],
                    "libc.musl must be preserved"
                );
            }
            _ => panic!("expected Specific platform"),
        }
        let json_out_musl = serde_json::to_string(&platform_musl).unwrap();
        let reparsed_musl: Platform = serde_json::from_str(&json_out_musl).unwrap();
        assert_eq!(platform_musl, reparsed_musl, "libc.musl roundtrip failed");
    }

    // ── os_features normalization via From<&Platform> for native::Platform (3.7) ──

    /// Roundtrip Platform::Specific with unsorted os_features through native conversion.
    /// The resulting native::Platform.os_features must be sorted + deduped.
    #[test]
    fn native_os_features_sorted_and_deduped_on_conversion() {
        // Unsorted input: "b.x" before "a.y"
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["b.x".to_string(), "a.y".to_string()],
            features: None,
        };
        let native_plat: native::Platform = native::Platform::from(&platform);
        assert_eq!(
            native_plat.os_features,
            Some(vec!["a.y".to_string(), "b.x".to_string()]),
            "os_features must be sorted ascending in native Platform"
        );
    }

    /// Deduplication: duplicate os_features entries must be collapsed to one.
    #[test]
    fn native_os_features_deduped_on_conversion() {
        let platform = Platform::Specific {
            os: OperatingSystem::Linux,
            arch: Architecture::Amd64,
            variant: None,
            os_version: None,
            os_features: vec!["libc.glibc".to_string(), "libc.glibc".to_string()],
            features: None,
        };
        let native_plat: native::Platform = native::Platform::from(&platform);
        assert_eq!(
            native_plat.os_features,
            Some(vec!["libc.glibc".to_string()]),
            "duplicate os_features must be deduped"
        );
    }

    #[test]
    fn deserialize_normalizes_duplicate_and_unsorted_os_features() {
        // A foreign manifest may carry duplicated / unordered os.features.
        // Selection scores specificity by os_features.len(), so an un-normalized
        // inbound array would inflate the score and skew candidate ranking.
        // Deserialization must sort + dedup at the boundary.
        let json = r#"{"os":"linux","architecture":"amd64","os.features":["libc.musl","libc.glibc","libc.musl"]}"#;
        let platform: Platform = serde_json::from_str(json).unwrap();
        match &platform {
            Platform::Specific { os_features, .. } => assert_eq!(
                os_features.as_slice(),
                &["libc.glibc".to_string(), "libc.musl".to_string()],
                "inbound os_features must be sorted + deduped so specificity scoring is not skewed"
            ),
            _ => panic!("expected Specific platform"),
        }
    }

    /// UPDATE of native_lossless_roundtrip_full_fidelity.
    /// `features` is removed from the asserted-lossless set (RESERVED — deliberately dropped).
    #[test]
    fn native_lossless_roundtrip_full_fidelity_without_features() {
        // features is intentionally omitted — it is RESERVED and must not round-trip.
        let original = native::Platform {
            os: native::Os::Windows,
            architecture: native::Arch::Amd64,
            variant: Some("v3".to_string()),
            features: None, // RESERVED — must not be asserted lossless
            os_version: Some("10.0.14393.1066".to_string()),
            os_features: Some(vec!["win32k".to_string()]),
        };
        let ocx = Platform::try_from(original.clone()).unwrap();
        let roundtripped: native::Platform = ocx.into();
        // os, architecture, variant, os_version, os_features must all survive.
        assert_eq!(roundtripped.os, original.os);
        assert_eq!(roundtripped.architecture, original.architecture);
        assert_eq!(roundtripped.variant, original.variant);
        assert_eq!(roundtripped.os_version, original.os_version);
        // os_features is sorted+deduped; since win32k is already sorted+unique it should match.
        assert_eq!(roundtripped.os_features, original.os_features);
        // features must be None (RESERVED, not round-tripped).
        assert_eq!(roundtripped.features, None, "RESERVED features must not round-trip");
    }

    // --- Native rejection ---

    #[test]
    fn native_rejects_unsupported_arch() {
        let native_plat = native::Platform {
            os: native::Os::Linux,
            architecture: native::Arch::PowerPC64le,
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_unsupported_os() {
        let native_plat = native::Platform {
            os: native::Os::FreeBSD,
            architecture: native::Arch::Amd64,
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_other_os_and_arch() {
        let native_plat = native::Platform {
            os: native::Os::Other("custom-os".to_string()),
            architecture: native::Arch::Other("custom-arch".to_string()),
            variant: None,
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_rejects_any_with_variant() {
        let native_plat = native::Platform {
            os: native::Os::Other("any".to_string()),
            architecture: native::Arch::Other("any".to_string()),
            variant: Some("v7".to_string()),
            features: None,
            os_version: None,
            os_features: None,
        };
        assert!(Platform::try_from(native_plat).is_err());
    }

    #[test]
    fn native_option_none_is_any() {
        let platform = Platform::try_from(None::<native::Platform>).unwrap();
        assert!(platform.is_any());
    }
}
