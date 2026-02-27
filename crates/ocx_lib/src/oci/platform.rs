use serde::{Deserialize, Serialize};

use crate::{Error, Result};
use super::native;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Platform {
    #[serde(flatten)]
    pub(crate) inner: Option<native::Platform>,
}

const ANY_STR: &str = "any";

impl Platform {
    pub fn new() -> Self {
        Self::any()
    }

    pub fn from_image_manifest(_manifest: &native::ImageManifest) -> Self {
        Self::any()
    }

    pub fn from_image_index(manifest: &native::ImageIndex) -> Result<Vec<Self>> {
        let mut platforms = Vec::with_capacity(manifest.manifests.len());
        for entry in &manifest.manifests {
            let platform = Self::try_from(entry.platform.clone())?;
            platforms.push(platform);
        }
        Ok(platforms)
    }

    pub fn from_manifest(manifest: &native::Manifest) -> Result<Vec<Self>> {
        match manifest {
            native::Manifest::Image(image_manifest) => Ok(vec![Self::from_image_manifest(image_manifest)]),
            native::Manifest::ImageIndex(image_index) => Self::from_image_index(image_index),
        }
    }

    pub fn segments(&self) -> Vec<String> {
        let platform = match &self.inner {
            Some(platform) => platform,
            None => return vec![ANY_STR.to_string()],
        };

        let mut segments = Vec::new();
        segments.push(platform.os.to_string());
        segments.push(platform.architecture.to_string());
        if let Some(variant) = &platform.variant {
            segments.push(variant.clone());
        }
        if let Some(os_version) = &platform.os_version {
            segments.push(os_version.clone());
        }
        segments
    }

    /// Checks if this platform matches the given platform.
    /// 
    /// Currently this checks for equality, but in the future we may want to support more complex matching logic.
    pub fn matches(&self, other: &Platform) -> bool {
        self == other
    }

    pub fn ascii_segments(&self) -> Vec<String> {
        self.segments().into_iter().map(|s| s.to_ascii_lowercase()).collect()
    }

    /// A special platform that matches any platform.
    /// This can be used to indicate that a package is compatible with any platform.
    /// For example a Java package.
    pub fn any() -> Self {
        Self { inner: None }
    }

    pub fn is_any(&self) -> bool {
        self.inner.is_none()
    }

    pub fn current() -> Option<Self> {
        let os = match std::env::consts::OS {
            "linux" => native::Os::Linux,
            "windows" => native::Os::Windows,
            "macos" => native::Os::Darwin,
            _ => return None,
        };
        let architecture = match std::env::consts::ARCH {
            "x86_64" => native::Arch::Amd64,
            "aarch64" => native::Arch::ARM64,
            _ => return None,
        };
        Some(Self {
            inner: Some(native::Platform {
                os,
                architecture,
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            }),
        })
    }

    /// Subset of supported operating systems.
    /// We can add more as needed.
    pub fn os_variants() -> Vec<native::Os> {
        use native::Os;

        vec![
            // Os::AIX,
            // Os::Android,
            Os::Darwin,
            // Os::DragonFlyBSD,
            // Os::FreeBSD,
            // Os::Hurd,
            // Os::Illumos,
            // Os::iOS,
            // Os::Js,
            Os::Linux,
            // Os::Nacl,
            // Os::NetBSD,
            // Os::OpenBSD,
            // Os::Plan9,
            // Os::Solaris,
            Os::Windows,
            // Os::zOS,
        ]
    }

    pub fn arch_variants() -> Vec<native::Arch> {
        use native::Arch;

        vec![
            // Arch::i386,
            Arch::Amd64,
            // Arch::Amd64p32,
            // Arch::ARM,
            // Arch::ARMbe,
            Arch::ARM64,
            // Arch::ARM64be,
            // Arch::LoongArch64,
            // Arch::Mips,
            // Arch::Mipsle,
            // Arch::Mips64,
            // Arch::Mips64le,
            // Arch::Mips64p32,
            // Arch::Mips64p32le,
            // Arch::PowerPC,
            // Arch::PowerPC64,
            // Arch::PowerPC64le,
            // Arch::RISCV,
            // Arch::RISCV64,
            // Arch::s390,
            // Arch::s390x,
            // Arch::SPARC,
            // Arch::SPARC64,
            // Arch::Wasm,
        ]
    }
}

impl Default for Platform {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let platform = match &self.inner {
            Some(platform) => platform,
            None => {
                write!(f, "{}", ANY_STR)?;
                return Ok(());
            }
        };

        write!(f, "{}/{}", platform.os, platform.architecture)?;
        if let Some(variant) = &platform.variant {
            write!(f, "/{}", variant)?;
        }
        if let Some(os_version) = &platform.os_version {
            write!(f, "/{}", os_version)?;
        }
        if let Some(features) = &platform.features {
            write!(f, " (features: {})", features.join(","))?;
        }

        Ok(())
    }
}

impl std::str::FromStr for Platform {
    type Err = crate::Error;

    fn from_str(value: &str) -> crate::Result<Self> {
        if value == ANY_STR {
            return Ok(Self::any());
        }

        let parts: Vec<&str> = value.split('/').collect();
        if parts.len() < 2 || parts.len() > 4 {
            return Err(crate::Error::PlatformInvalid(value.to_string()));
        }

        let os_str = parts[0];
        let arch_str = parts[1];
        if parts.len() == 2 && os_str == ANY_STR && arch_str == ANY_STR {
            return Ok(Self::any());
        }

        let os = os_str.into();
        if !Platform::os_variants().contains(&os) {
            return Err(crate::Error::PlatformInvalidOs(os.to_string()));
        }

        let architecture = arch_str.into();
        if !Platform::arch_variants().contains(&architecture) {
            return Err(crate::Error::PlatformInvalidArch(architecture.to_string()));
        }

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

        Ok(Self {
            inner: Some(native::Platform {
                os,
                architecture,
                variant,
                features: None,
                os_version,
                os_features: None,
            }),
        })
    }
}

impl TryFrom<native::Platform> for Platform {
    type Error = Error;

    fn try_from(platform: native::Platform) -> Result<Self> {
        if platform.features.is_some() || platform.os_features.is_some() {
            return Err(crate::Error::PlatformUnsupported(platform.to_string()));
        }

        if let (native::Os::Other(os), native::Arch::Other(arch)) = (&platform.os, &platform.architecture) {
            if os == ANY_STR
                && arch == ANY_STR
                && platform.variant.is_none()
                && platform.os_version.is_none()
                && platform.features.is_none()
                && platform.os_features.is_none()
            {
                return Ok(Platform::any());
            }
            return Err(crate::Error::PlatformUnsupported(platform.to_string()));
        }

        if !Platform::os_variants().contains(&platform.os) {
            return Err(crate::Error::PlatformUnsupported(platform.to_string()));
        }
        if !Platform::arch_variants().contains(&platform.architecture) {
            return Err(crate::Error::PlatformUnsupported(platform.to_string()));
        }

        Ok(Self { inner: Some(platform) })
    }
}

impl TryFrom<Option<native::Platform>> for Platform {
    type Error = Error;

    fn try_from(platform: Option<native::Platform>) -> Result<Self> {
        match platform {
            Some(p) => Self::try_from(p),
            None => Ok(Self::default()),
        }
    }
}

impl PartialEq<native::Platform> for Platform {
    fn eq(&self, other: &native::Platform) -> bool {
        match &self.inner {
            Some(platform) => platform == other,
            None => {
                if let (native::Os::Other(os), native::Arch::Other(arch)) = (&other.os, &other.architecture) {
                    os == ANY_STR
                        && arch == ANY_STR
                        && other.variant.is_none()
                        && other.os_version.is_none()
                        && other.features.is_none()
                        && other.os_features.is_none()
                } else {
                    false
                }
            }
        }
    }
}

impl From<Platform> for native::Platform {
    fn from(val: Platform) -> Self {
        match val.inner {
            Some(platform) => platform,
            None => native::Platform {
                os: native::Os::Other(ANY_STR.to_string()),
                architecture: native::Arch::Other(ANY_STR.to_string()),
                variant: None,
                features: None,
                os_version: None,
                os_features: None,
            },
        }
    }
}

fn oci_platform_is_any(platform: &native::Platform) -> bool {
    if let (native::Os::Other(os), native::Arch::Other(arch)) = (&platform.os, &platform.architecture) {
        os == ANY_STR
            && arch == ANY_STR
            && platform.variant.is_none()
            && platform.os_version.is_none()
            && platform.features.is_none()
            && platform.os_features.is_none()
    } else {
        false
    }
}
