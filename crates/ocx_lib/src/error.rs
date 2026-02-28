use crate::{auth, log, oci, shell, utility};

#[derive(Debug)]
pub enum Error {
    OfflineMode,

    AuthInvalidType(String),
    AuthMissingEnv(auth::AuthType, String),
    AuthDockerCredentialRetrieval(oci::native::DockerCredentialRetrievalError),

    ConfigInvalidBooleanString(String),

    PlatformInvalid(String),
    PlatformInvalidOs(String),
    PlatformInvalidArch(String),
    PlatformUnsupported(String),

    PackageVersionInvalid(String),
    PackageDigestInvalid(String),
    PackageNotFound(oci::Identifier),
    PackageInstallFailed(Vec<oci::Identifier>),
    PackageSelectionAmbiguous(Vec<oci::Identifier>),
    /// A symlink-based path was requested but the identifier carries a digest,
    /// which already uniquely addresses content and cannot be indirected through a symlink.
    PackageSymlinkRequiresTag(oci::Identifier),
    /// The requested symlink does not exist for the given package.
    PackageSymlinkNotFound(oci::Identifier, crate::file_structure::SymlinkKind),

    InternalFile(std::path::PathBuf, std::io::Error),
    InternalPathInvalid(std::path::PathBuf),

    UnsupportedArchive(String),
    UnsupportedClapShell(shell::Shell),
    UnsupportedMediaType(String, &'static [&'static str]),
    Undefined,
    UndefinedWithMessage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::OfflineMode => write!(f, "A network operation was attempted while in offline mode."),

            Error::AuthInvalidType(auth_type) => write!(
                f,
                "Invalid authentication type '{}', valid types are: {}",
                auth_type,
                auth::AuthType::valid_strings().join(", ")
            ),
            Error::AuthMissingEnv(auth_type, env_var) => write!(
                f,
                "Authentication type '{}' requires environment variable '{}' to be set",
                auth_type, env_var
            ),
            Error::AuthDockerCredentialRetrieval(error) => {
                write!(f, "Failed to retrieve Docker credentials: {}", error)
            }

            Error::ConfigInvalidBooleanString(value) => write!(
                f,
                "Invalid boolean string '{}', possible values are: {}",
                value,
                <utility::boolean_string::BooleanString as clap_builder::ValueEnum>::value_variants()
                    .iter()
                    .map(|v| clap_builder::ValueEnum::to_possible_value(&v.clone())
                        .unwrap()
                        .get_name()
                        .to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),

            Error::PlatformInvalid(platform) => write!(f, "Invalid platform: {}", platform),
            Error::PlatformInvalidOs(os) => write!(
                f,
                "Invalid platform OS '{}'. Possible values are: {}",
                os,
                oci::Platform::os_variants()
                    .into_iter()
                    .map(|os| os.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Error::PlatformInvalidArch(arch) => write!(
                f,
                "Invalid platform architecture '{}'. Possible values are: {}",
                arch,
                oci::Platform::arch_variants()
                    .into_iter()
                    .map(|arch| arch.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Error::PlatformUnsupported(detail) => write!(f, "Unsupported platform oci platform: {}", detail),

            Error::PackageVersionInvalid(version) => write!(f, "Invalid package version: {}", version),
            Error::PackageDigestInvalid(digest) => write!(f, "Invalid package digest: {}", digest),
            Error::PackageNotFound(identifier) => write!(f, "Package not found: {}", identifier),
            Error::PackageInstallFailed(references) => write!(
                f,
                "Failed to install package(s): {}",
                references.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", "),
            ),
            Error::PackageSelectionAmbiguous(candidates) => write!(
                f,
                "Multiple candidates found for package selection: {}",
                candidates
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Error::PackageSymlinkRequiresTag(identifier) => write!(
                f,
                "Symlink path resolution requires a tag identifier, but '{}' contains a digest component.",
                identifier,
            ),
            Error::PackageSymlinkNotFound(identifier, kind) => match kind {
                crate::file_structure::SymlinkKind::Candidate => write!(
                    f,
                    "Package '{}' has no installed candidate — the package must be installed first.",
                    identifier,
                ),
                crate::file_structure::SymlinkKind::Current => write!(
                    f,
                    "Package '{}' has no selected version — a version of the package must be selected first.",
                    identifier,
                ),
            },

            Error::InternalFile(path, error) => write!(f, "Internal file error for '{}': {}", path.display(), error),
            Error::InternalPathInvalid(path) => write!(f, "Path '{}' has an unexpected structure", path.display()),

            Error::UnsupportedArchive(file) => write!(f, "Unsupported archive format: {}", file),
            Error::UnsupportedClapShell(shell) => write!(f, "Shell '{}' is not supported for clap completions", shell),
            Error::UnsupportedMediaType(media_type, supported) => write!(
                f,
                "Unsupported media type '{}'. Expected media types are: {}",
                media_type,
                supported.join(", ")
            ),
            Error::Undefined => write!(f, "An undefined error occurred"),
            Error::UndefinedWithMessage(message) => write!(f, "An undefined error occurred: {}", message),
        }
    }
}

impl std::error::Error for Error {
    /* */
}

impl From<oci::ParseError> for Error {
    fn from(_value: oci::ParseError) -> Self {
        Error::Undefined
    }
}

pub trait ErrorExt<T> {
    // TODO: Remove this when we have more specific error handling in the library.
    #[deprecated]
    fn map_to_undefined_error(self) -> Result<T>;
}

impl<T, E: std::error::Error> ErrorExt<T> for std::result::Result<T, E> {
    fn map_to_undefined_error(self) -> Result<T> {
        if let Err(error) = &self {
            log::error!("Error: {}", error);
        }
        self.map_err(|error| Error::UndefinedWithMessage(error.to_string()))
    }
}

impl<T> ErrorExt<T> for Option<T> {
    fn map_to_undefined_error(self) -> Result<T> {
        self.ok_or(Error::Undefined)
    }
}

pub fn file_error(path: impl AsRef<std::path::Path>, error: std::io::Error) -> Error {
    Error::InternalFile(path.as_ref().to_path_buf(), error)
}
