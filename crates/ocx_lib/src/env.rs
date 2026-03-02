use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::{log, utility};

#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// Case-normalizing wrapper for environment variable keys.
///
/// On Windows, environment variable names are case-insensitive, so we
/// normalize to uppercase using the native wide-char representation.
/// On other platforms, keys are left as-is.
///
/// Keys are stored as `OsString` to preserve non-UTF-8 environment
/// variable names (possible on Unix).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct EnvKey(OsString);

impl EnvKey {
    fn new(key: impl Into<OsString>) -> Self {
        let key = key.into();
        #[cfg(windows)]
        let key = {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let upper: Vec<u16> = key.encode_wide().map(|c| wide_to_upper(c)).collect();
            OsString::from_wide(&upper)
        };
        Self(key)
    }
}

/// Uppercase a single UTF-16 code unit in the ASCII range.
///
/// Full Unicode case-folding is not needed — Windows environment variable
/// names are conventionally ASCII. This matches the kernel behavior for
/// env var lookups (case-insensitive in the ASCII range).
#[cfg(windows)]
fn wide_to_upper(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) {
        c - 0x20
    } else {
        c
    }
}

pub struct Env {
    vars: HashMap<EnvKey, OsString>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            vars: std::env::vars_os()
                .map(|(k, v)| (EnvKey::new(k), v))
                .collect(),
        }
    }

    pub fn clean() -> Self {
        Self { vars: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&OsStr> {
        self.vars.get(&EnvKey::new(key)).map(|s| s.as_os_str())
    }

    pub fn set(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.vars.insert(EnvKey::new(key), value.into());
    }

    pub fn add_path(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        let key = EnvKey::new(key);
        let value = value.into();
        match self.vars.get(&key).filter(|v| !v.is_empty()) {
            Some(existing) => {
                let mut new_value = value;
                new_value.push(PATH_SEPARATOR);
                new_value.push(existing);
                self.vars.insert(key, new_value);
            }
            None => {
                self.vars.insert(key, value);
            }
        }
    }

    /// Resolve a command name to a full executable path using PATH
    /// (and PATHEXT on Windows).
    ///
    /// Falls back to the bare command name if resolution fails, letting
    /// the OS handle it — `CreateProcessW` can still find `.exe` files.
    pub fn resolve_command(&self, command: impl AsRef<OsStr>) -> PathBuf {
        let command = command.as_ref();
        // cwd is only used by `which` when the command contains a path
        // separator (e.g. `./hello`).  For bare names it is ignored.
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            log::debug!("Could not determine current directory: {}", e);
            PathBuf::new()
        });
        match which::which_in(command, self.get("PATH"), cwd) {
            Ok(path) => path,
            Err(_) => {
                #[cfg(windows)]
                if std::env::var_os("PATHEXT").is_none() {
                    log::warn!(
                        "Could not resolve '{}' via PATH, falling back to OS lookup. \
                         PATHEXT is not set — .bat/.cmd scripts will not be found.",
                        command.to_string_lossy()
                    );
                } else {
                    log::warn!(
                        "Could not resolve '{}' via PATH, falling back to OS lookup.",
                        command.to_string_lossy()
                    );
                }
                #[cfg(not(windows))]
                log::warn!(
                    "Could not resolve '{}' via PATH, falling back to OS lookup.",
                    command.to_string_lossy()
                );
                PathBuf::from(command)
            }
        }
    }
}

impl IntoIterator for Env {
    type Item = (OsString, OsString);
    type IntoIter = std::vec::IntoIter<(OsString, OsString)>;

    fn into_iter(self) -> Self::IntoIter {
        self.vars
            .into_iter()
            .map(|(k, v)| (k.0, v))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

pub fn var(key: impl AsRef<str>) -> Option<String> {
    #[cfg(test)]
    match crate::test::env::get_override(key.as_ref()) {
        Some(Some(val)) => return Some(val),
        Some(None) => return None,
        None => {}
    }
    match std::env::var(key.as_ref()) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(os_str)) => {
            log::warn!("Environment variable '{}' is not valid: {:?}", key.as_ref(), os_str);
            None
        }
    }
}

pub fn flag(key: impl AsRef<str>, default: bool) -> bool {
    let key = key.as_ref();
    match var(key).map(|value| utility::boolean_string::BooleanString::try_from(value.as_str()))
        .transpose() {
        Ok(Some(boolean)) => boolean.into(),
        Ok(None) => default,
        Err(error) => {
            log::warn!("Environment variable '{}' has invalid boolean value: {}", key, error);
            default
        }
    }
}

pub fn string(key: impl AsRef<str>, default: String) -> String {
    if let Some(value) = var(key) {
        if value.is_empty() {
            default
        } else {
            value
        }
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_key_case_insensitive() {
        // On Windows: keys are normalized to uppercase, so "Path" == "PATH" == "path".
        // On Unix: keys are case-sensitive, so each is distinct.
        let lower = EnvKey::new("path");
        let upper = EnvKey::new("PATH");
        let mixed = EnvKey::new("Path");

        #[cfg(windows)]
        {
            assert_eq!(lower, upper);
            assert_eq!(lower, mixed);
            assert_eq!(upper, mixed);
        }

        #[cfg(not(windows))]
        {
            assert_ne!(lower, upper);
            assert_ne!(lower, mixed);
            assert_ne!(upper, mixed);
        }
    }

    #[test]
    fn env_get_set_roundtrip() {
        let mut env = Env::clean();
        env.set("MY_VAR", "hello");
        assert_eq!(env.get("MY_VAR").unwrap(), "hello");
    }

    #[test]
    fn env_add_path_prepends() {
        let mut env = Env::clean();
        env.set("PATH", "/usr/bin");
        env.add_path("PATH", "/opt/bin");
        let path = env.get("PATH").unwrap().to_str().unwrap();
        assert!(path.starts_with("/opt/bin"));
        assert!(path.ends_with("/usr/bin"));
        assert!(path.contains(PATH_SEPARATOR));
    }

    #[test]
    fn env_add_path_to_empty() {
        let mut env = Env::clean();
        env.add_path("PATH", "/opt/bin");
        assert_eq!(env.get("PATH").unwrap(), "/opt/bin");
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use std::os::windows::ffi::OsStringExt;

        #[test]
        fn env_key_ascii_uppercase_only() {
            // Non-ASCII characters are NOT uppercased — only a-z.
            // German ü (U+00FC) should stay as-is, not become Ü.
            let key = EnvKey::new("myVar_ü");
            let expected = EnvKey::new("MYVAR_ü");
            assert_eq!(key, expected);
        }

        #[test]
        fn env_key_preserves_unpaired_surrogates() {
            // Construct an OsString with an unpaired high surrogate (0xD800).
            // This is invalid Unicode but valid WTF-16, which Windows allows.
            let wide: Vec<u16> = vec![0xD800, b'a' as u16, b'b' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // The unpaired surrogate must survive; ASCII a/b become A/B.
            let expected_wide: Vec<u16> = vec![0xD800, b'A' as u16, b'B' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_preserves_surrogate_pair() {
            // U+1F600 (😀) encoded as a surrogate pair: 0xD83D 0xDE00
            let wide: Vec<u16> = vec![b'h' as u16, 0xD83D, 0xDE00, b'i' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // Surrogate pair must survive intact; h→H, i→I.
            let expected_wide: Vec<u16> = vec![b'H' as u16, 0xD83D, 0xDE00, b'I' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_key_bmp_non_ascii() {
            // CJK character U+4E16 (世) — single BMP code unit, not ASCII.
            let wide: Vec<u16> = vec![0x4E16, b'x' as u16];
            let key_os = OsString::from_wide(&wide);

            let env_key = EnvKey::new(key_os);

            // 世 stays as-is, x→X.
            let expected_wide: Vec<u16> = vec![0x4E16, b'X' as u16];
            let expected = EnvKey(OsString::from_wide(&expected_wide));
            assert_eq!(env_key, expected);
        }

        #[test]
        fn env_get_case_insensitive() {
            let mut env = Env::clean();
            env.set("Path", "C:\\Windows");
            assert_eq!(env.get("PATH").unwrap(), "C:\\Windows");
            assert_eq!(env.get("path").unwrap(), "C:\\Windows");
        }
    }
}
