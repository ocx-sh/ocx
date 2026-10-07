// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! What the generated types and commands share: the unknown-value walker, the argv builder and the stdin secret.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::runtime::Error;

/// Finds every enum value and union variant the SDK did not know at generation time.
pub trait Unknowns {
    /// Appends the JSON pointer of each unknown value below `self`; `pointer` addresses `self`.
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>);

    /// The sorted JSON pointers of every value that landed in an `Unknown` arm. A union's pointer addresses its whole
    /// object; an empty list means everything on the wire was known.
    fn unknown_pointers(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_unknowns(&mut String::new(), &mut out);
        out.sort();
        out
    }
}

/// Collects the unknown values below `value`, which sits at `name` under `pointer`.
pub fn field<T: Unknowns>(pointer: &mut String, out: &mut Vec<String>, name: &str, value: &T) {
    descend(pointer, name, |pointer| value.collect_unknowns(pointer, out));
}

/// Runs `visit` with `segment` appended to `pointer`, escaped as RFC 6901 requires, and restores it after.
fn descend(pointer: &mut String, segment: &str, visit: impl FnOnce(&mut String)) {
    let length = pointer.len();
    pointer.push('/');
    for character in segment.chars() {
        match character {
            '~' => pointer.push_str("~0"),
            '/' => pointer.push_str("~1"),
            other => pointer.push(other),
        }
    }
    visit(pointer);
    pointer.truncate(length);
}

macro_rules! known_scalars {
    ($($scalar:ty),*) => {
        $(impl Unknowns for $scalar {
            fn collect_unknowns(&self, _pointer: &mut String, _out: &mut Vec<String>) {}
        })*
    };
}

known_scalars!(String, bool, i64, f64, Value);

impl<T: Unknowns> Unknowns for Option<T> {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        if let Some(value) = self {
            value.collect_unknowns(pointer, out);
        }
    }
}

impl<T: Unknowns> Unknowns for Box<T> {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        (**self).collect_unknowns(pointer, out);
    }
}

impl<T: Unknowns> Unknowns for Vec<T> {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        for (index, item) in self.iter().enumerate() {
            field(pointer, out, &index.to_string(), item);
        }
    }
}

impl<T: Unknowns> Unknowns for BTreeMap<String, T> {
    fn collect_unknowns(&self, pointer: &mut String, out: &mut Vec<String>) {
        for (key, item) in self {
            field(pointer, out, key, item);
        }
    }
}

/// Decodes the object of a known union variant `tag` into its payload fields.
///
/// # Errors
///
/// The payload does not have the fields the variant declares; the message names `tag`.
pub fn payload<T: DeserializeOwned, E: serde::de::Error>(tag: &str, value: Value) -> Result<T, E> {
    serde_json::from_value(value).map_err(|error| E::custom(format!("`{tag}`: {error}")))
}

/// A value `ocx` reads from stdin and never from argv; `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    /// The bytes to write to the child's stdin.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Whether leaving an argument out is an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Optional,
    Required,
}

/// Whether a positional value may start with `-`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hyphen {
    Refuse,
    Allow,
}

/// The argv and stdin a command needs, ready to spawn.
#[derive(Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Everything after the binary: one token per flag, never joined into a shell string.
    pub arguments: Vec<OsString>,
    pub stdin: Option<Secret>,
}

impl std::fmt::Debug for Invocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Invocation")
            .field("arguments", &self.arguments)
            .field("stdin", &self.stdin)
            .finish()
    }
}

/// Builds argv from typed values. The first refused argument is kept and surfaces from [`Argv::finish`], so a
/// command's argument list reads straight down without a `?` per line.
#[derive(Default)]
pub struct Argv {
    arguments: Vec<OsString>,
    positionals: Vec<OsString>,
    operands: Vec<OsString>,
    stdin: Option<Secret>,
    refusal: Option<Error>,
}

impl Argv {
    pub fn new() -> Self {
        Self::default()
    }

    /// A bare token: a command word or an option the SDK itself sets.
    pub fn word(&mut self, word: &str) {
        self.arguments.push(OsString::from(word));
    }

    pub fn switch(&mut self, long: &'static str, enabled: bool) {
        if enabled {
            self.arguments.push(OsString::from(format!("--{long}")));
        }
    }

    /// One `--long=value` token per value, so a value that starts with `-` is never read as an option; an empty value
    /// passes as `--long=`. A non-empty `choices` lists the only values `ocx` accepts.
    pub fn flag<T: AsRef<OsStr>>(
        &mut self,
        long: &'static str,
        values: impl IntoIterator<Item = T>,
        presence: Presence,
        choices: &'static [&'static str],
    ) {
        let mut seen = false;
        for value in values {
            let value = value.as_ref();
            seen = true;
            if !choices.is_empty() && !value.to_str().is_some_and(|text| choices.contains(&text)) {
                self.refuse(long, "not one of the accepted values");
                continue;
            }
            let mut token = OsString::from(format!("--{long}="));
            token.push(value);
            self.arguments.push(token);
        }
        self.require(long, presence, seen);
    }

    /// A flag whose value `ocx` reads from stdin: the flag itself goes in argv, the value never does.
    pub fn secret(&mut self, long: &'static str, value: Option<&Secret>) {
        let Some(value) = value else { return };
        if self.stdin.is_some() {
            self.refuse(long, "only one value can be sent on stdin");
            return;
        }
        self.arguments.push(OsString::from(format!("--{long}")));
        self.stdin = Some(value.clone());
    }

    /// Positional values, placed after every flag. One that starts with `-` is refused unless `hyphen` allows it.
    pub fn positional<T: AsRef<OsStr>>(
        &mut self,
        id: &'static str,
        values: impl IntoIterator<Item = T>,
        presence: Presence,
        hyphen: Hyphen,
    ) {
        let mut seen = false;
        for value in values {
            let value = value.as_ref();
            seen = true;
            // An empty identifier or name is never valid; a free-form value (a command word) passes unchanged.
            if hyphen == Hyphen::Refuse && value.is_empty() {
                self.refuse(id, "empty");
                continue;
            }
            if hyphen == Hyphen::Refuse && value.as_encoded_bytes().first() == Some(&b'-') {
                self.refuse(id, "starts with `-`");
                continue;
            }
            self.positionals.push(value.to_owned());
        }
        self.require(id, presence, seen);
    }

    /// Ends the positionals read so far with `--`, so the next ones are data and never swallowed by them.
    pub fn terminator(&mut self) {
        self.positionals.push(OsString::from("--"));
    }

    /// Operands after the `--` terminator, where a leading `-` is data.
    pub fn operand<T: AsRef<OsStr>>(
        &mut self,
        id: &'static str,
        values: impl IntoIterator<Item = T>,
        presence: Presence,
    ) {
        let mut seen = false;
        for value in values {
            seen = true;
            self.operands.push(value.as_ref().to_owned());
        }
        self.require(id, presence, seen);
    }

    /// # Errors
    ///
    /// The first argument that was refused.
    pub fn finish(self) -> Result<Invocation, Error> {
        if let Some(refusal) = self.refusal {
            return Err(refusal);
        }
        let mut arguments = self.arguments;
        arguments.extend(self.positionals);
        if !self.operands.is_empty() {
            arguments.push(OsString::from("--"));
            arguments.extend(self.operands);
        }
        Ok(Invocation {
            arguments,
            stdin: self.stdin,
        })
    }

    fn require(&mut self, arg: &'static str, presence: Presence, seen: bool) {
        if presence == Presence::Required && !seen {
            self.refuse(arg, "required");
        }
    }

    fn refuse(&mut self, arg: &'static str, reason: &'static str) {
        if self.refusal.is_none() {
            self.refusal = Some(Error::InvalidArgument { arg, reason });
        }
    }
}
