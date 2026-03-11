// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

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

    InternalFile(std::path::PathBuf, std::io::Error),
    InternalPathInvalid(std::path::PathBuf),

    UnsupportedArchive(String),
    UnsupportedClapShell(shell::Shell),
    UnsupportedMediaType(String, &'static [&'static str]),
    /// A package manager operation failed.
    PackageManager(crate::package_manager::error::Error),
    /// An OCI client operation failed.
    OciClient(crate::oci::client::error::ClientError),

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
            Error::PackageManager(error) => write!(f, "{error}"),
            Error::OciClient(error) => write!(f, "{error}"),
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

impl From<crate::package_manager::error::Error> for Error {
    fn from(error: crate::package_manager::error::Error) -> Self {
        Error::PackageManager(error)
    }
}

impl From<crate::oci::client::error::ClientError> for Error {
    fn from(error: crate::oci::client::error::ClientError) -> Self {
        Error::OciClient(error)
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
