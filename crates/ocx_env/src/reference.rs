// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The value forms an OCX secret variable accepts: a literal, `file://<path>`, `env://<NAME>`, `plain://<text>`.

use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

/// The most a referenced file may hold; a larger file is refused rather than truncated.
pub const MAX_REFERENCED_SECRET_BYTES: usize = 64 * 1024;

/// A [`ReferenceError`] prints at most this many characters of a path or target, then `…`, so a literal
/// secret that happens to begin with a prefix is never printed whole.
const MAX_PRINTED_CHARS: usize = 128;

/// A variable's raw value read as a reference. The text after a prefix is kept verbatim.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SecretReference<'a> {
    /// The value itself.
    Literal(&'a str),
    /// `file://<path>`: the content of the file.
    File(&'a str),
    /// `env://<NAME>`: the value of another variable.
    Env(&'a str),
    /// `plain://<text>`: the text itself, for a literal that begins with a prefix.
    Plain(&'a str),
}

impl<'a> SecretReference<'a> {
    /// Reads `value` as a reference. Total: any value that carries no recognised prefix is a literal.
    ///
    /// Prefixes match ASCII-case-insensitively at the start of the value only.
    pub fn parse(value: &'a str) -> Self {
        let after = |prefix: &str| {
            value
                .get(..prefix.len())
                .filter(|head| head.eq_ignore_ascii_case(prefix))
                .map(|_| &value[prefix.len()..])
        };
        if let Some(path) = after("file://") {
            Self::File(path)
        } else if let Some(name) = after("env://") {
            Self::Env(name)
        } else if let Some(text) = after("plain://") {
            Self::Plain(text)
        } else {
            Self::Literal(value)
        }
    }
}

// `Literal` and `Plain` hold a secret, so no variant prints its payload.
impl fmt::Debug for SecretReference<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = match self {
            Self::Literal(_) => "Literal",
            Self::File(_) => "File",
            Self::Env(_) => "Env",
            Self::Plain(_) => "Plain",
        };
        write!(f, "{variant}(<redacted>)")
    }
}

/// A reference in a secret variable cannot be resolved.
///
/// Names the carrying variable and the path or target, never a literal value or file content.
#[derive(Clone, ocx_exit::Classify)]
pub enum ReferenceError {
    /// The raw value begins with a reference prefix but is not valid Unicode, so it cannot be parsed.
    #[exit(
        ConfigError,
        slug = "secret_reference_invalid",
        summary = "An OCX secret variable holds a file:// or env:// reference the grammar refuses"
    )]
    ValueNotUnicode { variable: String },
    /// The `env://` target is not a name any environment can hold.
    #[exit(
        ConfigError,
        slug = "secret_reference_invalid",
        summary = "An OCX secret variable holds a file:// or env:// reference the grammar refuses"
    )]
    EnvNameInvalid { variable: String },
    /// The `env://` target is the variable that carries the reference.
    #[exit(
        ConfigError,
        slug = "secret_reference_invalid",
        summary = "An OCX secret variable holds a file:// or env:// reference the grammar refuses"
    )]
    EnvSelfReference { variable: String },
    /// The `file://` path is empty, NUL-bearing or a device path.
    #[exit(
        ConfigError,
        slug = "secret_reference_invalid",
        summary = "An OCX secret variable holds a file:// or env:// reference the grammar refuses"
    )]
    FilePathInvalid { variable: String },
    /// The `file://` path is not absolute on this host.
    #[exit(
        ConfigError,
        slug = "secret_reference_invalid",
        summary = "An OCX secret variable holds a file:// or env:// reference the grammar refuses"
    )]
    FilePathRelative { variable: String, path: String },
    /// The `env://` target is unset or empty.
    #[exit(
        IoError,
        slug = "secret_reference_unreadable",
        summary = "A file:// or env:// reference in an OCX secret variable has nothing to read"
    )]
    EnvTargetUnset { variable: String, target: String },
    /// Opening or reading the file failed.
    #[exit(
        IoError,
        slug = "secret_reference_unreadable",
        summary = "A file:// or env:// reference in an OCX secret variable has nothing to read"
    )]
    FileUnreadable {
        variable: String,
        path: String,
        source: Arc<std::io::Error>,
    },
    /// The path is not a regular file.
    #[exit(
        IoError,
        slug = "secret_reference_unreadable",
        summary = "A file:// or env:// reference in an OCX secret variable has nothing to read"
    )]
    FileNotRegular { variable: String, path: String },
    /// The file holds nothing once trailing line endings are stripped.
    #[exit(
        IoError,
        slug = "secret_reference_unreadable",
        summary = "A file:// or env:// reference in an OCX secret variable has nothing to read"
    )]
    FileEmpty { variable: String, path: String },
    /// The file is larger than the cap.
    #[exit(
        DataError,
        slug = "secret_reference_malformed",
        summary = "A file referenced by an OCX secret variable is too large or not UTF-8"
    )]
    FileTooLarge { variable: String, path: String, cap: usize },
    /// The file is not UTF-8; `utf16` records a UTF-16 byte-order mark, so the message can name the fix.
    #[exit(
        DataError,
        slug = "secret_reference_malformed",
        summary = "A file referenced by an OCX secret variable is too large or not UTF-8"
    )]
    FileNotUtf8 {
        variable: String,
        path: String,
        utf16: bool,
    },
}

/// `text` cut to [`MAX_PRINTED_CHARS`] characters, with `…` marking the cut.
fn printable(text: &str) -> String {
    match text.char_indices().nth(MAX_PRINTED_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

// Hand-written: the derived form would print a path uncut.
impl fmt::Debug for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReferenceError({self})")
    }
}

impl fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvNameInvalid { variable } => {
                write!(
                    f,
                    "'{variable}' references an environment variable by a name no environment can hold"
                )
            }
            Self::EnvSelfReference { variable } => write!(f, "'{variable}' references itself"),
            Self::ValueNotUnicode { variable } => {
                write!(f, "'{variable}' holds a reference that is not valid Unicode")
            }
            Self::FilePathInvalid { variable } => {
                write!(f, "'{variable}' references a file with an empty or NUL-bearing path")
            }
            Self::FilePathRelative { variable, path } => write!(
                f,
                "'{variable}' references the relative path '{}'; a file reference needs an absolute path",
                printable(path)
            ),
            Self::EnvTargetUnset { variable, target } => write!(
                f,
                "'{variable}' references '{}', which is unset or empty",
                printable(target)
            ),
            Self::FileUnreadable { variable, path, .. } => {
                write!(f, "cannot read '{}', referenced by '{variable}'", printable(path))
            }
            Self::FileNotRegular { variable, path } => write!(
                f,
                "'{}', referenced by '{variable}', is not a regular file",
                printable(path)
            ),
            Self::FileEmpty { variable, path } => {
                write!(f, "'{}', referenced by '{variable}', is empty", printable(path))
            }
            Self::FileTooLarge { variable, path, cap } => write!(
                f,
                "'{}', referenced by '{variable}', is larger than {cap} bytes",
                printable(path)
            ),
            Self::FileNotUtf8 {
                variable,
                path,
                utf16: true,
            } => write!(
                f,
                "'{}', referenced by '{variable}', is UTF-16; save it as UTF-8",
                printable(path)
            ),
            Self::FileNotUtf8 { variable, path, .. } => {
                write!(f, "'{}', referenced by '{variable}', is not UTF-8", printable(path))
            }
        }
    }
}

impl std::error::Error for ReferenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FileUnreadable { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

/// Whether `text` begins with a reference prefix, so a value that is not Unicode can still be told to be one.
pub(crate) fn begins_with_prefix(text: &str) -> bool {
    !matches!(SecretReference::parse(text), SecretReference::Literal(_))
}

/// Resolves the raw value of `variable` to the value it stands for; `None` when that is empty.
///
/// One step: file content and an `env://` target's value are taken literally, never parsed again.
pub(crate) fn resolve(variable: &str, raw: String) -> Result<Option<String>, ReferenceError> {
    let resolved = match SecretReference::parse(&raw) {
        SecretReference::Literal(_) => return Ok(Some(raw)),
        SecretReference::Plain(text) => text.to_owned(),
        SecretReference::Env(name) => resolve_env(variable, name)?,
        SecretReference::File(path) => read_file(variable, path)?,
    };
    Ok(Some(resolved).filter(|value| !value.is_empty()))
}

fn resolve_env(variable: &str, name: &str) -> Result<String, ReferenceError> {
    if name.is_empty() || name.contains(['=', '\0']) {
        return Err(ReferenceError::EnvNameInvalid {
            variable: variable.to_owned(),
        });
    }
    let same = if cfg!(windows) {
        name.eq_ignore_ascii_case(variable)
    } else {
        name == variable
    };
    if same {
        return Err(ReferenceError::EnvSelfReference {
            variable: variable.to_owned(),
        });
    }
    crate::read_named(name)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ReferenceError::EnvTargetUnset {
            variable: variable.to_owned(),
            target: name.to_owned(),
        })
}

/// `O_NONBLOCK`, so a FIFO swapped in after the pre-stat cannot block the open.
#[cfg(unix)]
fn open_for_read(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    use std::fs::OpenOptions;

    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)
}

#[cfg(not(unix))]
fn open_for_read(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

fn read_file(variable: &str, path: &str) -> Result<String, ReferenceError> {
    let owned = || (variable.to_owned(), path.to_owned());
    // Both separators spell a device namespace on Windows; refused on every host.
    let backslashed = path.replace('/', "\\");
    if path.is_empty() || path.contains('\0') || backslashed.starts_with(r"\\.\") || backslashed.starts_with(r"\\?\") {
        return Err(ReferenceError::FilePathInvalid {
            variable: variable.to_owned(),
        });
    }
    let file_path = Path::new(path);
    if !file_path.is_absolute() {
        let (variable, path) = owned();
        return Err(ReferenceError::FilePathRelative { variable, path });
    }
    let unreadable = |source| {
        let (variable, path) = owned();
        ReferenceError::FileUnreadable {
            variable,
            path,
            source: Arc::new(source),
        }
    };
    let not_regular = || {
        let (variable, path) = owned();
        ReferenceError::FileNotRegular { variable, path }
    };
    // The stat follows symbolic links and keeps a device or FIFO from ever being opened.
    if !std::fs::metadata(file_path).map_err(unreadable)?.is_file() {
        return Err(not_regular());
    }
    let file = open_for_read(file_path).map_err(unreadable)?;
    // Re-checked on the handle: the path can be swapped between the stat and the open.
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(not_regular());
    }
    // Synchronous std I/O: capped at 64 KiB and run once per variable per command.
    let mut bytes = Vec::new();
    file.take(MAX_REFERENCED_SECRET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > MAX_REFERENCED_SECRET_BYTES {
        let (variable, path) = owned();
        return Err(ReferenceError::FileTooLarge {
            variable,
            path,
            cap: MAX_REFERENCED_SECRET_BYTES,
        });
    }
    // UTF-16 is refused, never transcoded: a BOM is the only signal (0xFF and 0xFE never start valid UTF-8),
    // so a BOM-less file keeps the plain message.
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    // The error is dropped unformatted: it carries the bytes.
    let text = String::from_utf8(bytes).map_err(|_| {
        let (variable, path) = owned();
        ReferenceError::FileNotUtf8 { variable, path, utf16 }
    })?;
    // A leading UTF-8 BOM is editor residue, not part of the secret; one only.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    let value = text.trim_end_matches(['\r', '\n']);
    if value.is_empty() {
        let (variable, path) = owned();
        return Err(ReferenceError::FileEmpty { variable, path });
    }
    Ok(value.to_owned())
}
