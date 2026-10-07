// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The environment registry: every variable OCX reads, declared once, and the one seam that reads them.
//!
//! A declaration is a `static` emitted by [`env_vars!`]; a read is a method on it. Names that come from
//! data rather than from code read through [`dynamic`].

use std::ffi::OsString;
use std::path::PathBuf;

#[cfg(any(test, feature = "__testing"))]
pub mod overrides;
mod retired;
#[cfg(test)]
mod tests;
mod vars;

pub use retired::{Change, REMOVAL_RELEASE, RETIRED, Release, Retired, Status, retired_hits};
pub use vars::*;

/// One declared environment variable.
#[derive(Debug)]
pub struct EnvVar {
    /// The literal name, or a pattern with exactly one `{SLOT}` placeholder (`OCX_AUTH_{REGISTRY}_TOKEN`).
    pub name: &'static str,
    /// The declaration's doc comment, one line per `///` line; the first line is the summary.
    pub doc: &'static str,
    pub value: EnvValue,
    pub on_invalid: OnInvalid,
    pub visibility: Visibility,
    /// Never formatted and never named with its value; read only through [`SecretVar`].
    pub secret: bool,
    pub child: Child,
    pub reader: Reader,
}

/// The value grammar a variable accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvValue {
    Bool,
    /// Unset, true or false — three answers, so it is never read through [`EnvVar::bool_or`].
    TriBool,
    String,
    Integer,
    Path,
    PathList,
    /// A path, or inline PEM text when the value contains `-----BEGIN`.
    PathOrPem,
    /// A comma-separated list of `host[:port]` authorities.
    HostList,
    Choice(&'static [&'static str]),
    Json,
}

/// What a value outside the variable's grammar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnInvalid {
    /// Falls back to the reader's default.
    Default,
    /// Refuses with [`InvalidEnv`] naming the key: a hardening switch never falls open on a typo.
    Error,
}

/// Who the variable is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// User configuration: documented, in the SDK manifest and the compatibility gate. `OCX_*`.
    Public,
    /// Owned by another tool or platform (`HOME`, `GITHUB_ACTIONS`); documented, never renamed.
    Foreign,
    /// Written by OCX for a later OCX process; documented, not configuration. `__OCX_*`.
    Plumbing,
    /// A test seam, absent from release builds. `__OCX_TESTING_*`.
    Testing,
}

/// What a child process OCX spawns sees of the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Child {
    /// The ambient value passes through untouched.
    Inherit,
    /// Removed from a plugin-dispatched child and from every env `apply_ocx_config` writes; [`credential_keys`] lists them.
    Scrub,
    /// Passed on explicitly to a child that starts from a clean environment.
    Forward,
}

/// Whose code interprets the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reader {
    Ocx,
    /// A dependency reads it; OCX documents it because it changes OCX's behaviour.
    Dependency(&'static str),
}

/// A declared variable holds a value outside its grammar and the declaration refuses to fall back.
///
/// Names the key only: the value may be a credential.
#[derive(Debug, Clone, PartialEq, Eq, ocx_exit::Classify)]
#[exit(
    ConfigError,
    slug = "invalid_env",
    summary = "An OCX environment variable holds a value its declaration refuses"
)]
pub struct InvalidEnv {
    pub key: String,
}

impl std::fmt::Display for InvalidEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "environment variable '{}' has an invalid value", self.key)
    }
}

impl std::error::Error for InvalidEnv {}

impl EnvVar {
    /// The value; unset, empty or not UTF-8 is `None`. A `secret` declaration answers `None`: read it through [`SecretVar`].
    pub fn get(&'static self) -> Option<String> {
        self.get_os().and_then(|value| value.into_string().ok())
    }

    /// The value as the OS holds it; unset or empty is `None`.
    pub fn get_os(&'static self) -> Option<OsString> {
        self.get_raw().filter(|value| !value.is_empty())
    }

    /// The value verbatim, empty included, for a reader that validates it itself (PEM, odd path bytes).
    pub fn get_raw(&'static self) -> Option<OsString> {
        if self.secret {
            return None;
        }
        self.value()
    }

    /// The value of the pattern's name with its `{SLOT}` replaced by `slot`; unset or empty is `None`.
    pub fn get_slot(&'static self, slot: &str) -> Option<String> {
        if self.secret {
            return None;
        }
        self.slot_value(slot)
    }

    /// The boolean value, `default` when unset or empty.
    ///
    /// # Errors
    ///
    /// [`InvalidEnv`] when the value is not a boolean spelling and the declaration says [`OnInvalid::Error`].
    pub fn bool_or(&'static self, default: bool) -> Result<bool, InvalidEnv> {
        let Some(value) = self.get_os() else {
            return Ok(default);
        };
        match value.to_str().and_then(parse_bool) {
            Some(parsed) => Ok(parsed),
            None => match self.on_invalid {
                OnInvalid::Default => Ok(default),
                OnInvalid::Error => Err(InvalidEnv {
                    key: self.name.to_owned(),
                }),
            },
        }
    }

    /// Whether [`EnvVar::name`] is a pattern rather than a literal name.
    pub fn is_pattern(&self) -> bool {
        self.name.contains('{')
    }

    /// Itself; the counterpart of [`SecretVar::declaration`], so a list can hold both.
    pub const fn declaration(&'static self) -> &'static EnvVar {
        self
    }

    /// The value regardless of `secret`; a pattern has none.
    fn value(&'static self) -> Option<OsString> {
        if self.is_pattern() {
            return None;
        }
        lookup(self, retired::table())
    }

    /// The non-empty UTF-8 value of the pattern's name for `slot`, regardless of `secret`.
    fn slot_value(&self, slot: &str) -> Option<String> {
        let (open, close) = (self.name.find('{')?, self.name.find('}')?);
        let name = format!("{}{slot}{}", self.name.get(..open)?, self.name.get(close + 1..)?);
        read_named(&name).filter(|value| !value.is_empty())
    }
}

/// A `secret` declaration: its value reads only as [`Sensitive`].
#[derive(Debug)]
pub struct SecretVar(EnvVar);

impl SecretVar {
    pub const fn new(var: EnvVar) -> Self {
        Self(var)
    }

    /// The declaration's metadata. Its own readers answer `None`.
    pub const fn declaration(&'static self) -> &'static EnvVar {
        &self.0
    }

    /// The value; unset or empty is `None`.
    pub fn get(&'static self) -> Option<Sensitive> {
        let value = self.0.value()?.into_string().ok()?;
        (!value.is_empty()).then_some(Sensitive(value))
    }

    /// The value of the pattern's name with its `{SLOT}` replaced by `slot`; unset or empty is `None`.
    pub fn get_slot(&'static self, slot: &str) -> Option<Sensitive> {
        self.0.slot_value(slot).map(Sensitive)
    }
}

/// A secret value: no `Display`, and `Debug` prints `<redacted>`.
///
/// ```compile_fail
/// let secret = ocx_env::Sensitive::new("hunter2".into());
/// println!("{secret}");
/// ```
#[derive(Clone)]
pub struct Sensitive(String);

impl Sensitive {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The plaintext, for the one call that needs it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The plaintext, moved out: for a caller that wraps it in its own zeroizing type.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Reads a variable whose name comes from data, not code. Verbatim, empty included; not UTF-8 is `None`.
///
/// A declared secret's name answers `None`, so a credential never reads as a plain `String`: use
/// [`dynamic_secret`]. There is no `&str`-taking read for a declared name:
///
/// ```compile_fail
/// let _ = ocx_env::var("OCX_HOME");
/// ```
///
/// ```compile_fail
/// let _ = ocx_env::flag("OCX_OFFLINE", false);
/// ```
///
/// ```compile_fail
/// let _ = ocx_env::string("OCX_LOG_LEVEL", String::new());
/// ```
pub fn dynamic(name: &str) -> Option<String> {
    if is_declared_secret(name) {
        return None;
    }
    read_named(name)
}

/// Whether `name` is a `secret` declaration's, or fills a secret pattern's slot.
///
/// ASCII-case-insensitive, as Windows resolves names: there `ocx_signing_key` reads `OCX_SIGNING_KEY`.
fn is_declared_secret(name: &str) -> bool {
    let fits = |declared: &str| match (declared.find('{'), declared.find('}')) {
        (Some(open), Some(close)) => {
            let (prefix, suffix) = (&declared[..open], &declared[close + 1..]);
            name.len() > prefix.len() + suffix.len()
                && name
                    .get(..prefix.len())
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                && name
                    .get(name.len() - suffix.len()..)
                    .is_some_and(|tail| tail.eq_ignore_ascii_case(suffix))
        }
        _ => declared.eq_ignore_ascii_case(name),
    };
    all().any(|var| var.secret && fits(var.name))
}

/// The value of `name`, unrefused; a name no environment block can hold is `None`.
fn read_named(name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['=', '\0']) {
        return None;
    }
    raw(name)?.into_string().ok()
}

/// [`dynamic`] for a name whose value may be a credential, declared secrets included.
pub fn dynamic_secret(name: &str) -> Option<Sensitive> {
    read_named(name).map(Sensitive)
}

/// Every variable of the process environment, with the test overrides applied.
#[expect(clippy::disallowed_methods, reason = "the seam")]
pub fn snapshot() -> Vec<(OsString, OsString)> {
    #[cfg(any(test, feature = "__testing"))]
    let mut variables: std::collections::BTreeMap<OsString, OsString> = if overrides::is_hermetic() {
        std::collections::BTreeMap::new()
    } else {
        std::env::vars_os().collect()
    };
    #[cfg(not(any(test, feature = "__testing")))]
    let variables: std::collections::BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    #[cfg(any(test, feature = "__testing"))]
    for (key, value) in overrides::entries() {
        match value {
            Some(value) => variables.insert(key.into(), value.into()),
            None => variables.remove(std::ffi::OsStr::new(&key)),
        };
    }
    variables.into_iter().collect()
}

/// The value of `var`, honouring the in-window [`Retired`] entries of `table` that replace it.
fn lookup(var: &'static EnvVar, table: &'static [Retired]) -> Option<OsString> {
    let in_window =
        |entry: &&Retired| std::ptr::eq(entry.replacement, var) && matches!(entry.status, Status::Window { .. });
    let current = raw(var.name);
    let value = match current {
        Some(value) if !value.is_empty() => value,
        _ => table
            .iter()
            .filter(in_window)
            .filter(|entry| entry.change == Change::Rename)
            .find_map(|entry| raw(entry.name).filter(|value| !value.is_empty()))
            .or(current)?,
    };
    let renamed = table.iter().filter(in_window).find_map(|entry| match entry.change {
        Change::ValueRename { old, new } if value == old => Some(OsString::from(new)),
        _ => None,
    });
    Some(renamed.unwrap_or(value))
}

/// The boolean spellings `ocx_util::boolean_string::BooleanString` accepts, case-insensitively.
fn parse_bool(value: &str) -> Option<bool> {
    const TRUE: [&str; 5] = ["1", "y", "yes", "on", "true"];
    const FALSE: [&str; 5] = ["0", "n", "no", "off", "false"];
    let matches = |spellings: [&str; 5]| spellings.iter().any(|spelling| spelling.eq_ignore_ascii_case(value));
    if matches(TRUE) {
        Some(true)
    } else if matches(FALSE) {
        Some(false)
    } else {
        None
    }
}

/// The one read of the process environment, through the test overrides; verbatim.
#[expect(clippy::disallowed_methods, reason = "the seam")]
fn raw(name: &str) -> Option<OsString> {
    #[cfg(any(test, feature = "__testing"))]
    if let Some(value) = overrides::get(name) {
        return value.map(OsString::from);
    }
    std::env::var_os(name)
}

/// Every declaration of this build; `Testing` ones only under `test` or `__testing`.
pub fn all() -> impl Iterator<Item = &'static EnvVar> {
    DECLARED.iter().copied()
}

/// The names a spawned child must not inherit: every [`Child::Scrub`] declaration.
///
/// A name belongs here when holding its value is enough to act as the operator (a token, a
/// passphrase, a private key); configuration and policy do not. `Command::envs` only adds, so a
/// spawn site inheriting the ambient environment must remove each name itself.
///
/// Adding one, in the same change: declare it `secret, child = Scrub` in `vars.rs`; if a sibling
/// stays inherited, say why on both declarations; add its row to the credential table in
/// `.claude/rules/subsystem-cli.md`; and state in `website/src/docs/reference/environment.md`
/// that it is never forwarded to child processes.
pub fn credential_keys() -> impl Iterator<Item = &'static str> {
    all().filter(|var| var.child == Child::Scrub).map(|var| var.name)
}

/// Process working directory, through the seam tests override.
pub fn current_dir() -> std::io::Result<PathBuf> {
    #[cfg(any(test, feature = "__testing"))]
    if let Some(cwd) = overrides::cwd() {
        return Ok(cwd);
    }
    std::env::current_dir()
}

/// The current user's home directory.
///
/// `std::env::home_dir`, never `dirs::home_dir`, which ignores `%USERPROFILE%` and hands one invocation two homes.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(any(test, feature = "__testing"))]
    if overrides::is_hermetic() {
        let key = if cfg!(windows) { USERPROFILE.name } else { HOME.name };
        return raw(key).filter(|home| !home.is_empty()).map(PathBuf::from);
    }
    std::env::home_dir()
}

/// Validates that `key` matches the POSIX environment-variable name grammar (`[A-Za-z_][A-Za-z0-9_]*`).
///
/// Every shell and CI emitter gates keys here, since value escaping leaves the key slot open to injection (CWE-77).
/// It also gates `${self.env.KEY}` tokens: narrowing it refuses tokens published packages carry.
pub fn is_valid_env_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    let Some(first) = bytes.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Returns `true` when `key` falls in the reserved `OCX_*` / `__OCX_*` namespace.
///
/// The one gate stopping every env surface from setting `OCX_*`, or a checked-in file could reconfigure resolution.
pub fn is_reserved_ocx_key(key: &str) -> bool {
    // Case-insensitive: on Windows `ocx_offline` lands in the same slot as `OCX_OFFLINE`.
    let upper = key.to_ascii_uppercase();
    upper.starts_with("OCX_") || upper.starts_with("__OCX_")
}

/// Declares environment variables: one `static` per entry plus one `DECLARED` list per invocation.
///
/// ```ignore
/// env_vars! {
///     /// Disables network access when truthy.
///     pub OCX_OFFLINE: Bool, Public, on_invalid = Error;
///     /// Registry token for one registry; `{REGISTRY}` is the registry slug.
///     pub OCX_AUTH_TOKEN = "OCX_AUTH_{REGISTRY}_TOKEN": String, Public, secret;
///     /// Lazy materialisation mode.
///     pub OCX_LAZY_MODE: Choice["never", "always"], Public;
///     /// Docker credential-store directory.
///     pub DOCKER_CONFIG: Path, Foreign, reader = Dependency("docker_credential");
/// }
/// ```
///
/// Attributes follow the visibility in any order: `secret`, `on_invalid = Default|Error`,
/// `child = Inherit|Scrub|Forward`, `reader = Ocx|Dependency("crate")`. A `Testing` entry is compiled only under
/// `cfg(any(test, feature = "__testing"))`, resolved in the invoking crate, which must declare that feature.
#[macro_export]
macro_rules! env_vars {
    // One entry, its attributes not yet folded into the slots.
    (@entry $cfg:tt [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] | secret $($rest:tt)*) => {
        $crate::env_vars!(@entry $cfg [$($doc)*] $name [$($pattern)?] [$($value)*] $vis
            [true] [$on_invalid] [$child] [$($reader)*] | $($rest)*);
    };
    (@entry $cfg:tt [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] | on_invalid = $set:ident $($rest:tt)*) => {
        $crate::env_vars!(@entry $cfg [$($doc)*] $name [$($pattern)?] [$($value)*] $vis
            [$secret] [$set] [$child] [$($reader)*] | $($rest)*);
    };
    (@entry $cfg:tt [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] | child = $set:ident $($rest:tt)*) => {
        $crate::env_vars!(@entry $cfg [$($doc)*] $name [$($pattern)?] [$($value)*] $vis
            [$secret] [$on_invalid] [$set] [$($reader)*] | $($rest)*);
    };
    (@entry $cfg:tt [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*]
        | reader = $set:ident ($dependency:literal) $($rest:tt)*) => {
        $crate::env_vars!(@entry $cfg [$($doc)*] $name [$($pattern)?] [$($value)*] $vis
            [$secret] [$on_invalid] [$child] [$set ($dependency)] | $($rest)*);
    };
    (@entry $cfg:tt [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] | reader = $set:ident $($rest:tt)*) => {
        $crate::env_vars!(@entry $cfg [$($doc)*] $name [$($pattern)?] [$($value)*] $vis
            [$secret] [$on_invalid] [$child] [$set] | $($rest)*);
    };
    // Every attribute folded: emit the static.
    (@entry [$($cfg:tt)*] [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [false] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] |) => {
        $(#[doc = $doc])*
        #[cfg($($cfg)*)]
        pub static $name: $crate::EnvVar =
            $crate::env_vars!(@var [$($doc)*] $name [$($pattern)?] [$($value)*] $vis [false] [$on_invalid] [$child] [$($reader)*]);
    };
    (@entry [$($cfg:tt)*] [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [true] [$on_invalid:ident] [$child:ident] [$($reader:tt)*] |) => {
        $(#[doc = $doc])*
        #[cfg($($cfg)*)]
        pub static $name: $crate::SecretVar = $crate::SecretVar::new(
            $crate::env_vars!(@var [$($doc)*] $name [$($pattern)?] [$($value)*] $vis [true] [$on_invalid] [$child] [$($reader)*]),
        );
    };
    (@var [$($doc:literal)*] $name:ident [$($pattern:literal)?] [$($value:tt)*] $vis:ident
        [$secret:ident] [$on_invalid:ident] [$child:ident] [$($reader:tt)*]) => {
        $crate::EnvVar {
            name: $crate::env_vars!(@name $name $($pattern)?),
            doc: $crate::env_vars!(@doc $($doc)*),
            value: $crate::EnvValue::$($value)*,
            on_invalid: $crate::OnInvalid::$on_invalid,
            visibility: $crate::Visibility::$vis,
            secret: $secret,
            child: $crate::Child::$child,
            reader: $crate::Reader::$($reader)*,
        }
    };
    (@name $name:ident) => { stringify!($name) };
    (@name $name:ident $pattern:literal) => { $pattern };
    (@doc) => { "" };
    (@doc $first:literal $($rest:literal)*) => { concat!($first $(, "\n", $rest)*) };
    (@slot Testing $name:ident) => {{
        #[cfg(any(test, feature = "__testing"))]
        let slot = ::std::option::Option::Some($name.declaration());
        #[cfg(not(any(test, feature = "__testing")))]
        let slot = ::std::option::Option::None;
        slot
    }};
    (@slot $vis:ident $name:ident) => {
        ::std::option::Option::Some($name.declaration())
    };
    (@gate Testing $($entry:tt)*) => { $crate::env_vars!(@entry [any(test, feature = "__testing")] $($entry)*); };
    (@gate $vis:ident $($entry:tt)*) => { $crate::env_vars!(@entry [all()] $($entry)*); };
    (
        $(
            $(#[doc = $doc:literal])*
            pub $name:ident $(= $pattern:literal)? : $value:ident $([$($choice:literal),* $(,)?])?, $vis:ident
            $(, $key:ident $(= $set:ident $(($dependency:literal))?)?)*;
        )*
    ) => {
        $(
            $crate::env_vars!(@gate $vis [$($doc)*] $name [$($pattern)?] [$value $((&[$($choice),*]))?] $vis
                [false] [Default] [Inherit] [Ocx] | $($key $(= $set $(($dependency))?)?)*);
        )*

        /// Every declaration of this invocation; `Testing` ones only under `test` or `__testing`.
        pub static DECLARED: ::std::sync::LazyLock<::std::vec::Vec<&'static $crate::EnvVar>> =
            ::std::sync::LazyLock::new(|| {
                let slots: ::std::vec::Vec<::std::option::Option<&'static $crate::EnvVar>> =
                    ::std::vec![$( $crate::env_vars!(@slot $vis $name) ),*];
                slots.into_iter().flatten().collect()
            });
    };
}
