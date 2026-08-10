// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The one recogniser for OCX interpolation tokens.
//!
//! OCX claims **every** `${…}` sequence in an env value or an entrypoint arg
//! (`adr_interpolation_token_grammar.md` D3). There is no foreign-token
//! concept and no pass-through path: a `${…}` either parses into one of four
//! recognised bodies or the scan returns an error, and `$${` is the only way
//! to emit a literal `${…}`.
//!
//! ```text
//! installPath   self.installPath   self.env.KEY   deps.NAME.installPath
//! ```
//!
//! The walk is single-pass and left to right. At each position the **first**
//! rule that matches wins, and the bytes it consumes are appended to the
//! output and never re-examined:
//!
//! - **R1 — escape.** `$${` emits the two bytes `${` and advances by 3.
//! - **R2 — token.** `${` with a `}` after it: the whole raw body is parsed
//!   against the anchored body grammar *and* the closed set of four bodies.
//!   Failing either is an error returned from the scan — never a
//!   [`Segment::Literal`].
//! - **R3 — literal.** The residue: emit one character, advance by its UTF-8
//!   length. A `${` with no `}` before end of input lands here (Axis D).
//!
//! Why the three are ordered and bounded the way they are is recorded at each
//! rule, in [`scan`]'s body.
//!
//! Single-pass is a correctness property, not a performance one: output bytes
//! are never re-read, so bytes that came from the filesystem can never be
//! re-interpreted as a publisher token. That is what makes the install-path
//! `${` injection defence structurally unnecessary (D12, C-009).
//!
//! Byte indexing on `$`, `{`, `}`, `.` and `:` is safe on arbitrary UTF-8:
//! every byte of a multi-byte sequence has the high bit set, so an ASCII byte
//! can never occur inside one (C-035).

use super::render::RenderModifier;
use super::{TemplateError, UnknownTokenHint};
use crate::package::metadata::dependency::DependencyName;

/// One classified piece of a scanned template string.
///
/// Literal text is borrowed from the scanned input throughout — a fired escape
/// yields `Literal` over the input's own `${` bytes, so ordinary text costs no
/// allocation. Only a `${deps.NAME.installPath}` token allocates, for the
/// owned [`DependencyName`] its `NAME` segment converts into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment<'a> {
    /// Bytes to emit verbatim: ordinary text, and the `${` a fired escape
    /// produced.
    Literal {
        /// The bytes themselves.
        text: &'a str,
        /// Where `text` starts in the scanned input.
        ///
        /// Carried rather than recoverable from the borrow: a fired escape
        /// yields two bytes out of three, so the pieces' lengths do not sum to
        /// the input's and no cursor can reconstruct this. A consumer that
        /// needs to map a literal back onto what the publisher wrote — the libc
        /// lint's `PATH` split, the one there is — would otherwise reach for
        /// pointer arithmetic against the input's base address, which is an
        /// invariant about *where every literal borrows from* that nothing
        /// enforces and a scanner change could silently break.
        at: usize,
    },
    /// A recognised token, fully parsed.
    Token(Token<'a>),
}

/// A recognised `${…}` token.
///
/// A `${…}` that does not parse into one of the four bodies never reaches
/// this type — it is an error returned from [`scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    /// Which of the four recognised bodies this is.
    pub shape: TokenShape<'a>,
    /// The optional `:native` / `:posix` suffix. `None` means the modifier
    /// was omitted, which renders identically to `:native` (D5).
    pub modifier: Option<RenderModifier>,
    /// The raw `${…}` text exactly as the publisher wrote it, including the
    /// delimiters and any modifier — so an error can name the token verbatim.
    pub source: &'a str,
}

/// The closed set of recognised token bodies.
///
/// `installPath` and `self.installPath` are the *same* referent (D4) and
/// therefore the same variant: a gate that told them apart would make an
/// alias observably not an alias.
///
/// Every variant is fully validated by the time it exists: a body that fails
/// any of the constraints below is an error out of [`scan`], never a `Token`
/// a later stage has to re-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenShape<'a> {
    /// `${installPath}` or its exact alias `${self.installPath}` — the
    /// consuming package's `content/` directory.
    InstallPath,
    /// `${self.env.KEY}` — the resolved value of this package's
    /// earlier-declared var `KEY` (D6). Legal in env values only; the
    /// capability gate refuses it elsewhere.
    ///
    /// `key` satisfies [`crate::env::is_valid_env_key`], the same validator
    /// the shell emitters and the CI flavors gate their keys through, so the
    /// accepted character class cannot drift from what OCX will actually
    /// emit. The body grammar alone is looser — it would admit `1ABC` and
    /// `A-B`, neither of which is a settable env-var key — so a `KEY` the
    /// validator rejects is a [`TemplateError::UnknownToken`], not a
    /// recognised token that fails later.
    SelfEnv { key: &'a str },
    /// `${deps.NAME.installPath}` — a declared direct dependency's `content/`
    /// directory.
    ///
    /// `name` is the owned, validated [`DependencyName`] — not the raw text —
    /// because the pattern the scanner matches `NAME` against is only half of
    /// that type's contract: `DependencyName::try_from` also enforces
    /// `SLUG_MAX_LEN`, so a 65-byte name passes a pattern check and fails the
    /// conversion. Doing the conversion here means both consumers (the
    /// substitution's `HashMap<DependencyName, _>` lookup and the publish
    /// gate's declared-name check) get the key they need without a fallible
    /// step of their own.
    ///
    /// There is no `field`: `installPath` is the only leaf, so a `Dep` token
    /// with any other field could not exist. `${deps.cmake.version}` is a
    /// [`TemplateError::UnknownField`] raised from the raw body during the
    /// scan, which needs no `Token` to name it.
    Dep { name: DependencyName },
}

impl TokenShape<'_> {
    /// Whether a `:native` / `:posix` suffix is legal on this shape (D5).
    ///
    /// True exactly for the shapes whose value OCX *knows* is a path. The
    /// modifier is a slash-direction flip over the whole resolved value, so on
    /// a value that is not a path it is meaningless at best and corrupting at
    /// worst — a `${self.env.KEY}` naming a regex, a compiler flag, or a `list`
    /// var would have its legitimate backslashes rewritten on Windows.
    ///
    /// The narrow rule is the reversible one. This grammar is permanent on
    /// published metadata: widening it later breaks nothing, while narrowing it
    /// later would refuse documents already in registries.
    ///
    /// A `self.env` value composed *from* a path keeps the render axis — the
    /// modifier goes on the token in the declaring var (`${installPath:posix}/sdk`)
    /// and the reference inherits the rendered form. Only *re-rendering* an
    /// already-rendered value is lost, which was never coherent.
    const fn takes_modifier(&self) -> bool {
        match self {
            Self::InstallPath | Self::Dep { .. } => true,
            Self::SelfEnv { .. } => false,
        }
    }
}

/// Classifies `input` into a sequence of literal and token segments.
///
/// Pure: no filesystem access, no capability gate, no substitution. The
/// capability gate (`AllowedTokens`) and the substitution both run over the
/// result, which is what makes gate-before-substitution a property of the
/// pipeline's shape rather than of one call's position (D9).
///
/// # Errors
///
/// [`TemplateError::UnknownToken`] for a `${…}` whose body fails the anchored
/// grammar, falls outside the closed set, names a `deps.NAME` that is not a
/// valid [`DependencyName`], or names a `self.env.KEY` that
/// [`crate::env::is_valid_env_key`] rejects;
/// [`TemplateError::UnknownField`] for a recognised namespace with exactly one
/// unknown leaf; [`TemplateError::UnknownModifier`] for a `:suffix` outside
/// `{ native, posix }`.
pub fn scan(input: &str) -> Result<Vec<Segment<'_>>, TemplateError> {
    let mut segments = Vec::new();
    // Start of the literal run currently open. Literal bytes are accumulated
    // lazily rather than pushed per character, so a token-free input costs one
    // borrowed segment and no allocation.
    let mut literal_start = 0usize;
    let mut index = 0usize;
    // R2's `find(CLOSE)` can only answer `Some` while a `}` remains ahead of the
    // cursor, and `index` only grows — so one `rfind` up front collapses the
    // "no terminator anywhere ahead" case from a full re-read per `$` to an O(1)
    // test. Without it a run of `${` is quadratic, and R3 makes that run legal,
    // publishable literal text: every `$` re-reads the whole remainder, finds
    // nothing, and advances one byte.
    let last_close = input.rfind(CLOSE);

    loop {
        let rest = &input[index..];
        let Some(character) = rest.chars().next() else { break };

        // R1 — escape. Checked before R2, which is what makes `$${installPath}`
        // unambiguous: the `${` one byte in is never reachable as a token start.
        // The emitted delimiter borrows the input's own bytes and is output, so
        // it is never rescanned.
        if rest.starts_with(ESCAPED_OPEN) {
            push_literal(&mut segments, input, literal_start, index);
            push_literal(&mut segments, input, index + 1, index + ESCAPED_OPEN.len());
            index += ESCAPED_OPEN.len();
            literal_start = index;
            continue;
        }

        // R2 — token. The terminator is the *first* `}` at or after the body's
        // start, so there is no nesting. A `${` with no `}` before end of input
        // is not a token at all and falls through to R3 one character at a time
        // (Axis D), which is why the publish accept-set only ever grows.
        if rest.starts_with(OPEN)
            && last_close.is_some_and(|at| at >= index + OPEN.len())
            && let Some(offset) = rest[OPEN.len()..].find(CLOSE)
        {
            let body_start = index + OPEN.len();
            let body_end = body_start + offset;
            let source = &input[index..body_end + CLOSE.len_utf8()];

            let token = parse_token(source, &input[body_start..body_end])?;
            push_literal(&mut segments, input, literal_start, index);
            segments.push(Segment::Token(token));

            index = body_end + CLOSE.len_utf8();
            literal_start = index;
            continue;
        }

        // R3 — literal residue. The character stays in the open run.
        index += character.len_utf8();
    }

    push_literal(&mut segments, input, literal_start, input.len());
    Ok(segments)
}

/// The escape, the token opener, and the terminator — the only byte sequences
/// the walk branches on.
const ESCAPED_OPEN: &str = "$${";
const OPEN: &str = "${";
const CLOSE: char = '}';

/// The separator between a body and its verbatim `:suffix`.
const MODIFIER_SEPARATOR: char = ':';

/// The one leaf under every namespace, and the whole of the bare body.
const INSTALL_PATH: &str = "installPath";

/// The closed set of four recognised bodies, spelled the way a publisher writes
/// them — `KEY` and `NAME` are the publisher's own placeholders.
///
/// The single source of truth for D13's diagnostics: the branch-3 message lists
/// these verbatim, and the recognised-root set is each entry's root run (see
/// `unknown_token_hint`), so there is no second root vocabulary to keep in step
/// and no root freeze to maintain.
///
/// It is *not* the source of truth for acceptance — that is `parse_shape`'s
/// match, which the two placeholders could not express. Adding a fifth body
/// means editing both, and only the parse decides what resolves.
pub const RECOGNISED_BODIES: &[&str] = &[
    INSTALL_PATH,
    "self.installPath",
    "self.env.KEY",
    "deps.NAME.installPath",
];

/// The closed render-modifier set, as one list: the parse and the message that
/// enumerates the alternatives cannot drift apart.
const MODIFIERS: &[(&str, RenderModifier)] = &[("native", RenderModifier::Native), ("posix", RenderModifier::Posix)];

/// Appends `input[at..end]` as a literal segment, skipping an empty run.
///
/// Takes the range rather than the slice so the recorded offset cannot disagree
/// with the bytes it points at — the one invariant [`Segment::Literal`]'s `at`
/// has to hold.
fn push_literal<'a>(segments: &mut Vec<Segment<'a>>, input: &'a str, at: usize, end: usize) {
    if end > at {
        segments.push(Segment::Literal {
            text: &input[at..end],
            at,
        });
    }
}

/// Parses one `${…}` whose extent is already known: `source` is the whole
/// token including delimiters, `body` is the raw text between them.
///
/// The `:suffix` is split off **verbatim** and judged only once the base has
/// been recognised. That order is what makes `${self.installPath:POSIX}` an
/// unknown *modifier* while `${localEnv:HOME}` stays an unknown *token* — the
/// publisher is told which half of a recognised token is wrong, and is not told
/// that a foreign token has a modifier problem.
///
/// Applicability is judged before the suffix is resolved against the modifier
/// set, so `${self.env.KEY:POSIX}` reports that the *modifier does not belong
/// there* rather than that `POSIX` is misspelled — the latter would invite the
/// publisher to write `:posix`, which is refused too.
fn parse_token<'a>(source: &'a str, body: &'a str) -> Result<Token<'a>, TemplateError> {
    let (base, suffix) = match body.find(MODIFIER_SEPARATOR) {
        Some(at) => (&body[..at], Some(&body[at + MODIFIER_SEPARATOR.len_utf8()..])),
        None => (body, None),
    };

    let shape = parse_shape(source, base)?;
    let modifier = match suffix {
        Some(suffix) if !shape.takes_modifier() => return Err(modifier_not_applicable(source, suffix)),
        Some(suffix) => Some(parse_modifier(suffix)?),
        None => None,
    };

    Ok(Token {
        shape,
        modifier,
        source,
    })
}

/// Matches a modifier-free body against the anchored grammar and then against
/// the closed set of four bodies. Passing the grammar is necessary and not
/// sufficient: `${installPath.foo}` and `${localEnv}` both derive cleanly and
/// are still errors.
fn parse_shape<'a>(source: &str, base: &'a str) -> Result<TokenShape<'a>, TemplateError> {
    let path: Vec<&str> = base.split('.').collect();
    if !path.iter().all(|segment| is_body_segment(segment)) {
        return Err(unknown_token(source, base));
    }

    match path.as_slice() {
        // `installPath` is the one root that is also a whole body, so it admits
        // no dotted continuation; `self.installPath` is its exact alias (D4) and
        // therefore the same shape.
        [INSTALL_PATH] | ["self", INSTALL_PATH] => Ok(TokenShape::InstallPath),
        // A second filter on top of the grammar: `segment` admits a leading
        // digit and `-`, neither of which is a settable env-var key, so a body
        // the validator rejects is an unknown token rather than a recognised one
        // that fails later (C-039).
        ["self", "env", key] if crate::env::is_valid_env_key(key) => Ok(TokenShape::SelfEnv { key }),
        ["self", "env", _] => Err(unknown_token(source, base)),
        ["self", field] => Err(unknown_field("self", field, &[INSTALL_PATH, "env.KEY"])),
        ["deps", name, field] => match DependencyName::try_from(*name) {
            // The name is validated before the leaf: an unusable name means OCX
            // cannot locate the mistake in a namespace it recognises, which is
            // what `UnknownField` reports.
            Ok(dependency) if *field == INSTALL_PATH => Ok(TokenShape::Dep { name: dependency }),
            Ok(_) => Err(unknown_field(&format!("deps.{name}"), field, &[INSTALL_PATH])),
            Err(_) => Err(unknown_token(source, base)),
        },
        _ => Err(unknown_token(source, base)),
    }
}

/// `segment = 1*( ALPHA / DIGIT / "_" / "-" )` — the anchored body grammar's
/// one production, applied to the root and to every dotted segment alike.
fn is_body_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Resolves a captured `:suffix` against the closed modifier set.
fn parse_modifier(suffix: &str) -> Result<RenderModifier, TemplateError> {
    MODIFIERS
        .iter()
        .find(|(name, _)| *name == suffix)
        .map(|(_, modifier)| *modifier)
        .ok_or_else(|| TemplateError::UnknownModifier {
            // Same echo class as `unknown_token`: the suffix is the rest of the
            // body, so it carries the same bytes at the same lengths.
            modifier: for_message(suffix),
            supported: MODIFIERS.iter().map(|(name, _)| (*name).to_string()).collect(),
        })
}

/// The refusal for a modifier on a shape that does not take one
/// ([`TokenShape::takes_modifier`]).
///
/// Both echoes are publisher-controlled and go through [`for_message`], the
/// same escaping every other echo site uses. No list of applicable bodies is
/// enumerated: `self.env.KEY` is the only shape this refuses, so "install-path
/// tokens" names the complement exactly, and a second body vocabulary here
/// could drift from [`RECOGNISED_BODIES`].
fn modifier_not_applicable(source: &str, suffix: &str) -> TemplateError {
    TemplateError::ModifierNotApplicable {
        token: for_message(source),
        modifier: for_message(suffix),
    }
}

/// The catch-all refusal, naming the token verbatim.
///
/// `source` is the whole `${…}` as authored — what the message quotes — and
/// `rejected` is the text the parse refused, from which the message branch is
/// chosen. Both, rather than re-deriving one from the other at the render site:
/// R2.2 is a scanner rule, and a second reading of the token elsewhere would be
/// a second recogniser.
///
/// Every call site passes the *modifier-stripped* base, not the raw body. The
/// two agree on the root run either way — `:` is outside the root's character
/// class, so it ends the run exactly as the end of the base does.
fn unknown_token(source: &str, rejected: &str) -> TemplateError {
    TemplateError::UnknownToken {
        token: for_message(source),
        hint: unknown_token_hint(rejected),
    }
}

/// The byte ceiling on publisher text quoted back in a refusal message.
///
/// Comfortably above every token a real tool writes — the longest in the
/// rejection corpus is `${containerWorkspaceFolder}` at 27 — so truncation is
/// reachable only by input authored to reach it.
const MAX_ECHOED_BYTES: usize = 120;

/// Marks a quoted run the message cut short, so a truncated echo does not read
/// as the whole of what the publisher wrote.
///
/// Public because the escape hint has to know: advice that spells out a
/// truncated token would tell the publisher to write a literal that is not the
/// one they wrote.
pub const TRUNCATION_MARKER: &str = "…";

/// Renders publisher-controlled text safe to put in a refusal message.
///
/// A token body admits every byte but `}`, so without this a newline or an ANSI
/// escape sequence reaches stderr raw and a forged multi-line diagnostic is
/// constructible (CWE-117/150), at whatever length the metadata field allows.
/// The repo already holds this line — `RelativePath::parse` refuses control
/// characters citing the same class.
///
/// Truncate first, escape second: escaping expands, so bounding its input is
/// what bounds the message. `str::escape_debug` is the escaping — the same
/// rendering `{:?}` uses, so an escaped run is unambiguous and stays readable
/// for the ordinary token that needs no escaping at all.
///
/// Public because the token bodies this module rejects are not the only
/// publisher-controlled text a refusal echoes: [`SelfEnvScope::lookup`] names
/// declared env keys, which reach it straight off `metadata.json` with no
/// grammar applied. One escaping helper, so the two sites cannot drift.
///
/// [`SelfEnvScope::lookup`]: super::SelfEnvScope::lookup
pub fn for_message(text: &str) -> String {
    let mut end = MAX_ECHOED_BYTES.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }

    let escaped = text[..end].escape_debug().to_string();
    if end == text.len() {
        escaped
    } else {
        escaped + TRUNCATION_MARKER
    }
}

/// Picks D13's message branch for a rejected body.
///
/// The branch is chosen from **R2.2's root run** — the maximal
/// `[A-Za-z0-9_-]` prefix of the body, possibly empty — which is also how each
/// recognised root is derived from [`RECOGNISED_BODIES`], so there is no second
/// root vocabulary to drift from the closed set.
///
/// Ordered, and the order is the whole rule:
///
/// 1. **Root recognised** → [`UnknownTokenHint::SupportedBodies`]. Deciding
///    this first is what keeps a typo in a *name* — `${deps.Python.installPath}`,
///    where `deps` is recognised and `Python` fails the slug class — away from
///    the suggester, which has nothing useful to say about a name.
/// 2. **Near miss on a recognised root** → [`UnknownTokenHint::SuggestedRoot`].
///    Within `max(recognised_root.len(), 3) / 3` edits, the length-scaled
///    threshold `find_best_match_for_name` uses
///    ([rustc_span/src/edit_distance.rs](https://github.com/rust-lang/rust/blob/master/compiler/rustc_span/src/edit_distance.rs)).
///    Under a flat 1, `${instalPatch}` — two edits from `installPath` — falls to
///    the escape branch and the publisher is advised to ship their own typo as
///    literal text (C-033).
/// 3. Otherwise → [`UnknownTokenHint::Escape`].
///
/// The distance is [`strsim::osa_distance`], **not** `strsim::levenshtein`:
/// rustc's metric counts an adjacent transposition as one edit, and plain
/// Levenshtein scores C-033's `slef` → `self` leg as 2 against a threshold of
/// 1 — losing the very suggestion the contract requires.
fn unknown_token_hint(body: &str) -> UnknownTokenHint {
    let root = root_run(body);

    if recognised_roots().any(|recognised| recognised == root) {
        return UnknownTokenHint::SupportedBodies;
    }

    recognised_roots()
        .map(|recognised| (strsim::osa_distance(root, recognised), recognised))
        .filter(|(distance, recognised)| *distance <= recognised.len().max(3) / 3)
        // `min_by_key` keeps the first of an equal-distance pair, so the winner
        // is a function of `RECOGNISED_BODIES`' order and not of iteration luck.
        .min_by_key(|(distance, _)| *distance)
        .map_or(UnknownTokenHint::Escape, |(_, recognised)| {
            UnknownTokenHint::SuggestedRoot(recognised.to_string())
        })
}

/// The recognised roots, derived from [`RECOGNISED_BODIES`] rather than listed
/// again — `self` appears twice and is left duplicated, which no consumer above
/// can observe.
fn recognised_roots() -> impl Iterator<Item = &'static str> {
    RECOGNISED_BODIES.iter().copied().map(root_run)
}

/// R2.2's root run: the maximal leading run of `segment`-class characters,
/// possibly empty (`${}`, `${.foo}`).
///
/// The class is [`is_body_segment`]'s, spelled per byte because this needs the
/// position where the run ends rather than a verdict on a whole segment. Byte
/// indexing is safe on arbitrary UTF-8: every byte of a multi-byte sequence has
/// the high bit set, so the first byte outside the class is never inside a
/// character (C-035).
fn root_run(body: &str) -> &str {
    let end = body
        .bytes()
        .position(|byte| !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
        .unwrap_or(body.len());
    &body[..end]
}

/// The located refusal: a recognised namespace shape with exactly one unknown
/// leaf. Everything else is [`unknown_token`].
fn unknown_field(namespace: &str, field: &str, supported: &[&str]) -> TemplateError {
    TemplateError::UnknownField {
        namespace: namespace.to_string(),
        field: field.to_string(),
        supported: supported.iter().map(|name| (*name).to_string()).collect(),
    }
}

/// Rewrites `input` so [`scan`] reproduces it verbatim.
///
/// The publisher-facing inverse of the scanner's R1 rule, and the only
/// supported way to put a literal `${` into a metadata string: every `${`
/// becomes `$${`, and nothing else changes. `scan(&escape(s))` classifies to
/// exactly the text of `s` for every `s`, including strings OCX would
/// otherwise refuse (C-034).
///
/// Lives beside [`scan`] so the round-trip has one authored inverse. A caller
/// — a test included — that spells the escaping itself is asserting the rule
/// twice and can get it wrong twice.
#[must_use]
pub fn escape(input: &str) -> String {
    input.replace(OPEN, ESCAPED_OPEN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{ClassifyExitCode, ExitCode};

    /// The text a scan classified as literal, asserting on the way that it
    /// classified *nothing* as a token. Every escape and pass-through contract
    /// below is a statement about literal bytes, so a token appearing in one of
    /// them is the failure the assertion exists to catch.
    fn literals_only(segments: &[Segment<'_>]) -> String {
        let mut text = String::new();
        for segment in segments {
            match segment {
                Segment::Literal { text: literal, .. } => text.push_str(literal),
                Segment::Token(token) => panic!("expected literal text throughout, got token {:?}", token.source),
            }
        }
        text
    }

    /// Re-renders a scan as text with every token standing for its own source.
    /// For an input carrying no escape this must reproduce the input exactly,
    /// which is how the non-ASCII contract asserts that literal bytes around a
    /// token survive in place.
    fn rendered(segments: &[Segment<'_>]) -> String {
        let mut text = String::new();
        for segment in segments {
            match segment {
                Segment::Literal { text: literal, .. } => text.push_str(literal),
                Segment::Token(token) => text.push_str(token.source),
            }
        }
        text
    }

    /// The single token `input` must scan to.
    fn single_token(input: &str) -> Token<'_> {
        let segments = scan(input).unwrap_or_else(|error| panic!("{input:?} must scan: {error}"));
        match segments.as_slice() {
            [Segment::Token(token)] => token.clone(),
            other => panic!("{input:?} must scan to exactly one token, got {other:?}"),
        }
    }

    fn dep_shape(name: &str) -> TokenShape<'static> {
        TokenShape::Dep {
            name: DependencyName::try_from(name).unwrap(),
        }
    }

    /// The error shape a rejected `${…}` must produce, with the text the
    /// publisher has to be told about.
    ///
    /// Carrying the offending text in the variant is what makes the rejection
    /// tables below assertions rather than "some error happened": the variant
    /// alone would go green for a scanner that refused every token, and the
    /// text alone would not tell `UnknownToken` from `UnknownField`.
    ///
    /// Every arm asserts the *rendered* message carries that text as well: a
    /// struct field no `#[error]` string interpolates is a fact the publisher
    /// never gets told, and only the rendered form can catch that.
    // The shared `Unknown` prefix is the point: each variant names the
    // `TemplateError` variant it asserts, so renaming them would make the
    // mirror harder to read, not easier.
    #[allow(clippy::enum_variant_names)]
    #[derive(Debug)]
    enum Rejection<'a> {
        /// The whole `${…}` extent, verbatim.
        UnknownToken(&'a str),
        /// A recognised namespace with one unknown leaf.
        UnknownField { namespace: &'a str, field: &'a str },
        /// A `:suffix` outside `{ native, posix }`, verbatim.
        UnknownModifier(&'a str),
    }

    fn assert_rejects(input: &str, expected: &Rejection<'_>) {
        let error = scan(input)
            .err()
            .unwrap_or_else(|| panic!("{input:?} must be refused, not scanned"));

        match (&error, expected) {
            (TemplateError::UnknownToken { token, .. }, Rejection::UnknownToken(offender)) => {
                assert_eq!(token.as_str(), *offender, "{input:?} must name the offending token");
                assert!(
                    error.to_string().contains(*offender),
                    "the message for {input:?} must carry the token verbatim: {error}"
                );
            }
            (
                TemplateError::UnknownField { namespace, field, .. },
                Rejection::UnknownField {
                    namespace: expected_namespace,
                    field: expected_field,
                },
            ) => {
                assert_eq!(
                    namespace.as_str(),
                    *expected_namespace,
                    "{input:?} must name the namespace"
                );
                assert_eq!(field.as_str(), *expected_field, "{input:?} must name the unknown leaf");
                assert!(
                    error.to_string().contains(*expected_field),
                    "the message for {input:?} must carry the unknown leaf verbatim: {error}"
                );
            }
            (TemplateError::UnknownModifier { modifier, .. }, Rejection::UnknownModifier(offender)) => {
                assert_eq!(
                    modifier.as_str(),
                    *offender,
                    "{input:?} must name the offending modifier"
                );
                assert!(
                    error.to_string().contains(*offender),
                    "the message for {input:?} must carry the modifier verbatim: {error}"
                );
            }
            (actual, _) => panic!("{input:?} must be refused as {expected:?}, got {actual:?}"),
        }

        assert_eq!(
            error.classify(),
            Some(ExitCode::DataError),
            "every grammar refusal is malformed input (65), not a fault of its own: {input:?}"
        );
    }

    // ── Escape (D2) ───────────────────────────────────────────────────────────

    // C-001 / S-005 / S-020 — `$${` emits the two bytes `${` and the token that
    // follows is never seen. No leading `$` remains, and the escape works for a
    // body OCX would otherwise refuse, which is the whole point of D3's exit.
    #[test]
    fn an_escaped_token_emits_the_literal_delimiter() {
        assert_eq!(literals_only(&scan("$${installPath}").unwrap()), "${installPath}");
        assert_eq!(
            literals_only(&scan("$${workspaceFolder}").unwrap()),
            "${workspaceFolder}"
        );
        assert_eq!(literals_only(&scan("a$${b}c").unwrap()), "a${b}c");
    }

    // C-002 / S-006 / S-021 — a `$$` not followed by `{` is ordinary text. OCX
    // deliberately diverges from Make/Bazel/Compose, which collapse `$$` → `$`
    // unconditionally: OCX values routinely carry a `$` that means nothing to
    // OCX (a shell fragment, a regex, a price, a `$`-bearing password) and there
    // is no author present to notice the corruption.
    #[test]
    fn a_double_dollar_not_followed_by_a_brace_is_ordinary_text() {
        for text in ["$$", "$$foo", "price: $$5", "trailing $", "$", "mkdir /tmp/$$"] {
            assert_eq!(
                literals_only(&scan(text).unwrap()),
                text,
                "{text:?} must pass through byte-identical"
            );
        }
    }

    // C-003 — stacked dollars resolve left to right, one escape at a time.
    #[test]
    fn stacked_dollars_escape_once_left_to_right() {
        assert_eq!(literals_only(&scan("$$${installPath}").unwrap()), "$${installPath}");
        assert_eq!(literals_only(&scan("$$$${installPath}").unwrap()), "$$${installPath}");
    }

    // C-034 (leg 2 of the inverse pair) — `escape` is the authored inverse of
    // R1: every `${` becomes `$${`, and nothing else moves. Pinned directly as
    // well as through the round trip, because an `escape` that returned its
    // input unchanged would satisfy the round trip for every `${`-free string.
    #[test]
    fn escape_rewrites_every_open_delimiter_and_nothing_else() {
        assert_eq!(escape("${installPath}"), "$${installPath}");
        assert_eq!(escape("a${b}c${d}"), "a$${b}c$${d}");
        assert_eq!(escape("$${installPath}"), "$$${installPath}");
        assert_eq!(escape("$$"), "$$");
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape(""), "");
    }

    // ── Unterminated `${` (Axis D) ────────────────────────────────────────────

    // C-005 / S-025 — a `${` with no `}` before end of input is not a token, so
    // the error path is never entered and the bytes are exactly what the
    // publisher wrote. This is the one shape OCX's claim over `${…}` does not
    // reach, and the reason the publish accept-set only ever grows.
    #[test]
    fn an_unterminated_open_delimiter_is_literal_text() {
        for text in ["${self.installPath", "prefix ${", "${", "${installPath", "${a${b"] {
            assert_eq!(
                literals_only(&scan(text).unwrap()),
                text,
                "{text:?} carries no token, so it must pass through byte-identical"
            );
        }
    }

    // C-005 (complexity leg) — the same shape the contract above pins as legal
    // literal text, at a size a publisher can actually push: every `$` opens a
    // `${` with no `}` anywhere after it. Asking "is there a terminator?" once
    // per `$` re-reads the whole remainder each time, which is O(n²) — 1 MiB
    // cost 9.1 s of single-threaded CPU, and `MAX_METADATA_BLOB_BYTES` admits
    // 4 MiB. That is a victim's CPU, burnt on every `ocx run` through a
    // transitive dependency they never named, so the bound belongs in the test
    // suite and not in a comment. It sits two orders of magnitude above the
    // linear scan's cost, which is what makes it discriminating and not flaky.
    #[test]
    fn a_long_unterminated_delimiter_run_scans_in_linear_time() {
        let input = OPEN.repeat(512 * 1024);
        let started = std::time::Instant::now();
        let segments = scan(&input).expect("an unterminated `${` run is literal text, not a refusal");
        let elapsed = started.elapsed();

        assert_eq!(
            literals_only(&segments),
            input,
            "the run must pass through byte-identical, exactly as C-005 requires"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "scanning {} bytes of unterminated `${{` must be linear, took {elapsed:?}",
            input.len()
        );
    }

    // ── Recognised bodies ─────────────────────────────────────────────────────

    // The closed set of four, and the alias. `${installPath}` and
    // `${self.installPath}` are the same referent (D4) and therefore the same
    // shape — a scanner that told them apart would make the alias observably
    // not an alias.
    #[test]
    fn the_four_recognised_bodies_scan_to_their_shapes() {
        assert_eq!(single_token("${installPath}").shape, TokenShape::InstallPath);
        assert_eq!(single_token("${self.installPath}").shape, TokenShape::InstallPath);
        assert_eq!(
            single_token("${self.env.TOOL_HOME}").shape,
            TokenShape::SelfEnv { key: "TOOL_HOME" }
        );
        assert_eq!(single_token("${deps.cmake.installPath}").shape, dep_shape("cmake"));
        assert_eq!(
            single_token("${deps.my-cmake.installPath}").shape,
            dep_shape("my-cmake"),
            "the dep-name class is SLUG_PATTERN_STR's, which admits '-'"
        );
    }

    // S-010 / S-011 / D5 — an optional `:native` / `:posix` suffix is parsed off
    // every **install-path** body, and `source` keeps the token verbatim so an
    // error can name what the publisher wrote.
    #[test]
    fn a_render_modifier_suffix_is_parsed_off_every_install_path_body() {
        assert_eq!(single_token("${installPath}").modifier, None, "omitted means None");
        assert_eq!(
            single_token("${installPath:native}").modifier,
            Some(RenderModifier::Native)
        );
        assert_eq!(
            single_token("${self.installPath:posix}").modifier,
            Some(RenderModifier::Posix)
        );
        assert_eq!(
            single_token("${deps.cmake.installPath:posix}").modifier,
            Some(RenderModifier::Posix)
        );
        assert_eq!(
            single_token("${self.installPath:posix}").source,
            "${self.installPath:posix}"
        );
    }

    // D5 — the modifier is refused on `${self.env.KEY}`, whose value OCX cannot
    // know is a path.
    //
    // The positive leg above and this one are both required: without the
    // positive, `takes_modifier` could return `false` unconditionally and the
    // modifier would be gone entirely; without this, it could return `true`
    // unconditionally and the gate would be dead code.
    #[test]
    fn a_render_modifier_is_refused_on_self_env() {
        assert!(
            matches!(
                scan("${self.env.TOOL_HOME:posix}"),
                Err(TemplateError::ModifierNotApplicable { .. })
            ),
            "a slash flip over a value of unknown shape corrupts it"
        );
        assert!(
            matches!(
                scan("${self.env.TOOL_HOME:native}"),
                Err(TemplateError::ModifierNotApplicable { .. })
            ),
            "the explicit default is refused too — the axis does not apply, not the value"
        );
        // Applicability is decided before the suffix is resolved: a misspelling
        // on a shape that takes no modifier must not be reported as a typo the
        // publisher can fix by spelling it right.
        assert!(
            matches!(
                scan("${self.env.TOOL_HOME:POSIX}"),
                Err(TemplateError::ModifierNotApplicable { .. })
            ),
            "not UnknownModifier, which would invite ':posix' — also refused"
        );
        assert_eq!(
            single_token("${self.env.TOOL_HOME}").modifier,
            None,
            "premise: the body itself is still recognised"
        );
    }

    // C-039 — the `self.env.KEY` recogniser applies `env::is_valid_env_key` as a
    // **second** filter after the body grammar matches.
    //
    // Both legs are required and neither substitutes for the other: without the
    // positive, deleting the whole recogniser still passes; without the
    // negatives, the second filter can be deleted and every key the body grammar
    // admits becomes a `SelfEnv` token OCX could never set.
    #[test]
    fn self_env_applies_the_env_key_validator_as_a_second_filter() {
        for key in ["A_B1", "_UNDERSCORE", "TOOL_HOME"] {
            assert!(crate::env::is_valid_env_key(key), "premise: {key:?} is a settable key");
            assert_eq!(
                single_token(&format!("${{self.env.{key}}}")).shape,
                TokenShape::SelfEnv { key },
                "a valid env key must be recognised as a self-env token"
            );
        }

        // Each of these satisfies `segment = 1*( ALPHA / DIGIT / "_" / "-" )`
        // and is still not a settable env-var key, so it must never become a
        // recognised token. The empty key is here because it is the one case
        // the body grammar and the validator refuse for *different* reasons:
        // a `body` parser that started filtering empty segments would turn
        // `${self.env.}` into `["self", "env"]` and report an unknown *field*
        // `env`, which is a token refusal quietly downgraded to a leaf typo.
        for body in ["1ABC", "A-B", "9", ""] {
            assert!(
                !crate::env::is_valid_env_key(body),
                "premise: {body:?} is not a settable key"
            );
            let input = format!("${{self.env.{body}}}");
            assert_rejects(&input, &Rejection::UnknownToken(&input));
        }
    }

    // ── Claim-all rejection (D3) ──────────────────────────────────────────────

    // C-004 — the inverted golden corpus. Every one of these is a hard error
    // (65) naming the token verbatim; the withdrawn design asserted
    // pass-through on the same inputs. Table-driven so a new rejection case is a
    // row, not a function.
    #[test]
    fn every_unrecognised_token_is_refused_naming_it_verbatim() {
        const CORPUS: &[(&str, Rejection<'static>)] = &[
            ("${workspaceFolder}", Rejection::UnknownToken("${workspaceFolder}")),
            (
                "${containerWorkspaceFolder}",
                Rejection::UnknownToken("${containerWorkspaceFolder}"),
            ),
            ("${localEnv:HOME}", Rejection::UnknownToken("${localEnv:HOME}")),
            ("${env:HOME}", Rejection::UnknownToken("${env:HOME}")),
            (
                "${localEnv:PATH:default}",
                Rejection::UnknownToken("${localEnv:PATH:default}"),
            ),
            ("${1}", Rejection::UnknownToken("${1}")),
            ("${}", Rejection::UnknownToken("${}")),
            ("${a b}", Rejection::UnknownToken("${a b}")),
            ("${installpath}", Rejection::UnknownToken("${installpath}")),
            // R2.0 takes the terminator at the *first* `}`, so the extent is
            // `${a${b}` and its body `a${b` fails the grammar on `$`. There is
            // no nesting, and the trailing `}` is never reached.
            ("${a${b}}", Rejection::UnknownToken("${a${b}")),
            ("${ocx.version}", Rejection::UnknownToken("${ocx.version}")),
            // A body that is entirely multi-byte: `root_run` walks bytes and
            // slices at the first one outside the class, which here is the
            // body's first byte — the case its C-035 safety argument covers
            // and no other row exercises.
            ("${日本語}", Rejection::UnknownToken("${日本語}")),
            // Mixed: the recognised token resolves nothing, because the scan
            // errors before any substitution can happen.
            (
                "${installPath}/x:${workspaceFolder}/y",
                Rejection::UnknownToken("${workspaceFolder}"),
            ),
        ];

        for (input, expected) in CORPUS {
            assert_rejects(input, expected);
        }
    }

    // C-008 / S-007 / S-012 — the grammar error table. Each row produces the
    // variant the ADR names, and each message carries the offending text.
    // `UnknownField` fires only where OCX can *locate* the mistake: a
    // recognised namespace shape with exactly one unknown leaf. Everything else
    // is `UnknownToken`, including a recognised root whose body is outside the
    // closed set.
    #[test]
    fn the_grammar_error_table_produces_the_variant_it_names() {
        const TABLE: &[(&str, Rejection<'static>)] = &[
            (
                "${self.foo}",
                Rejection::UnknownField {
                    namespace: "self",
                    field: "foo",
                },
            ),
            (
                "${self.instalPath}",
                Rejection::UnknownField {
                    namespace: "self",
                    field: "instalPath",
                },
            ),
            (
                "${deps.x.version}",
                Rejection::UnknownField {
                    namespace: "deps.x",
                    field: "version",
                },
            ),
            ("${installPath:frobnicate}", Rejection::UnknownModifier("frobnicate")),
            // The modifier class is lowercase, and the suffix is captured off
            // the body before it is judged — so the publisher is told which
            // half of a recognised token is wrong.
            ("${self.installPath:POSIX}", Rejection::UnknownModifier("POSIX")),
            // Recognised root, body outside the closed set.
            ("${self}", Rejection::UnknownToken("${self}")),
            ("${deps}", Rejection::UnknownToken("${deps}")),
            ("${self.}", Rejection::UnknownToken("${self.}")),
            ("${deps.}", Rejection::UnknownToken("${deps.}")),
            ("${deps.x}", Rejection::UnknownToken("${deps.x}")),
            // Body fails the anchored grammar on a character outside
            // `segment` / `modifier`.
            ("${self!}", Rejection::UnknownToken("${self!}")),
            ("${self.env.A B}", Rejection::UnknownToken("${self.env.A B}")),
            ("${installPath }", Rejection::UnknownToken("${installPath }")),
            (
                "${deps.x.installPath!}",
                Rejection::UnknownToken("${deps.x.installPath!}"),
            ),
            // `installPath` is a complete token, not a namespace — it admits no
            // dotted continuation, so this is not an unknown *field*.
            ("${installPath.foo}", Rejection::UnknownToken("${installPath.foo}")),
            (
                "${installPath.foo:posix}",
                Rejection::UnknownToken("${installPath.foo:posix}"),
            ),
            // Uppercase fails the dep-name slug class.
            (
                "${deps.Python.installPath}",
                Rejection::UnknownToken("${deps.Python.installPath}"),
            ),
        ];

        for (input, expected) in TABLE {
            assert_rejects(input, expected);
        }
    }

    // The echoed token is bounded and control-free (CWE-117/150). The body
    // admits every byte but `}`, and every refusal quotes it: raw, a newline or
    // an ANSI erase-line sequence forges a second diagnostic line on the
    // publisher's terminal; unbounded, the escape branch rendered a 1 MB token
    // as a multi-megabyte message.
    #[test]
    fn a_refused_token_is_echoed_escaped_and_truncated() {
        let forged = "${\u{1b}[2Kocx: everything is fine\nnothing was refused}";
        let message = scan(forged)
            .err()
            .unwrap_or_else(|| panic!("{forged:?} must be refused"))
            .to_string();
        assert!(
            !message.contains('\n') && !message.contains('\u{1b}'),
            "no control byte may reach the message raw: {message:?}"
        );
        assert!(
            message.contains("\\u{1b}") && message.contains("\\n"),
            "the control bytes must still be visible, escaped: {message:?}"
        );

        let oversize = format!("${{{}}}", "a".repeat(1_000_000));
        let message = scan(&oversize)
            .err()
            .unwrap_or_else(|| panic!("the oversize token must be refused"))
            .to_string();
        assert!(
            message.len() < 1024,
            "a 1 MB token must not produce a message of its size: {} bytes",
            message.len()
        );
        assert!(
            message.contains(TRUNCATION_MARKER),
            "a cut-short echo must say so: {message:?}"
        );

        let oversize_modifier = format!("${{installPath:{}}}", "b".repeat(1_000_000));
        let message = scan(&oversize_modifier)
            .err()
            .unwrap_or_else(|| panic!("the oversize modifier must be refused"))
            .to_string();
        assert!(
            message.len() < 1024,
            "the modifier echo is the same class and the same bound: {} bytes",
            message.len()
        );
    }

    // C-008 (length leg) — a `NAME` that satisfies the slug *pattern* and fails
    // `DependencyName`'s length ceiling is an unknown token, not a recognised
    // one that fails somewhere later.
    //
    // Both premises are asserted, because together they are the whole reason
    // the scanner does the conversion rather than a pattern match: swap the
    // `DependencyName::try_from` in the `["deps", name, field]` arm for a
    // `SLUG_PATTERN` check and this is the only case that reds.
    #[test]
    fn a_dependency_name_over_the_length_ceiling_is_an_unknown_token() {
        use crate::package::metadata::slug::{SLUG_MAX_LEN, SLUG_PATTERN};

        let name = "a".repeat(SLUG_MAX_LEN + 1);
        assert!(
            SLUG_PATTERN.is_match(&name),
            "premise: the name satisfies the pattern, so a pattern check would admit it"
        );
        assert!(
            DependencyName::try_from(name.as_str()).is_err(),
            "premise: the conversion refuses it on length, which is the half a pattern check drops"
        );

        let input = format!("${{deps.{name}.installPath}}");
        assert_rejects(&input, &Rejection::UnknownToken(&input));
    }

    // C-008 (empty-modifier leg) — a `:` with nothing after it is a modifier
    // outside the closed set, not a token carrying none.
    //
    // Pinned here rather than as a table row: the table's verbatim-echo leg
    // asserts the message contains the offending text, and every message
    // contains the empty string. What a publisher who wrote a bare `:` needs is
    // the set, so that is what this asserts.
    #[test]
    fn an_empty_render_modifier_is_refused_and_names_the_supported_set() {
        let error = scan("${installPath:}")
            .err()
            .unwrap_or_else(|| panic!("an empty modifier must be refused, not scanned"));

        let TemplateError::UnknownModifier { modifier, supported } = &error else {
            panic!("expected UnknownModifier, got {error:?}");
        };
        assert_eq!(modifier, "", "the empty suffix is the modifier that was refused");
        assert_eq!(
            supported.as_slice(),
            ["native", "posix"],
            "the message must name what the publisher could have written instead"
        );
    }

    // ── Unknown-token diagnostics (D13 / C-033) ───────────────────────────────

    /// The D13 branch a refused `${…}` routed to.
    ///
    /// Asserted as the hint *value* rather than through the rendered message:
    /// the three branches are three different pieces of advice, and only an
    /// exact match excludes the other two. What each branch then says is
    /// `UnknownTokenHint::advice_for`'s contract, pinned beside it in
    /// `template.rs`.
    fn hint_of(input: &str) -> UnknownTokenHint {
        let error = scan(input)
            .err()
            .unwrap_or_else(|| panic!("{input:?} must be refused, not scanned"));
        match error {
            TemplateError::UnknownToken { hint, .. } => hint,
            other => panic!("{input:?} must be refused as UnknownToken, got {other:?}"),
        }
    }

    fn suggesting(root: &str) -> UnknownTokenHint {
        UnknownTokenHint::SuggestedRoot(root.to_string())
    }

    // C-033 leg 1 — a transposed root suggests the root it transposes.
    //
    // The metric is load-bearing and the premises below say why. `self` is 4
    // characters, so its threshold is `max(4, 3) / 3` = 1; plain Levenshtein has
    // no transposition operation and scores `slef` → `self` as **2**, which
    // exceeds it. Under Levenshtein this leg emits no suggestion, falls through
    // to the escape branch, and the publisher is advised to escape their own
    // typo — "fixing" the error by shipping it as literal text into a
    // digest-pinned artifact, the precise outcome D13 exists to prevent.
    //
    // Two roots, not one: a single guard on the metric can be deleted without
    // anything else noticing.
    #[test]
    fn a_transposed_root_suggests_the_root_it_transposes() {
        assert_eq!(
            strsim::osa_distance("slef", "self"),
            1,
            "premise: optimal string alignment counts one adjacent transposition as a single edit"
        );
        assert_eq!(
            strsim::levenshtein("slef", "self"),
            2,
            "premise: plain Levenshtein scores the same pair as two substitutions"
        );
        assert_eq!(
            std::cmp::max("self".len(), 3) / 3,
            1,
            "premise: the scaled threshold for a 4-character root is 1, so the two metrics disagree here"
        );

        assert_rejects("${slef.env.HOME}", &Rejection::UnknownToken("${slef.env.HOME}"));
        assert_eq!(hint_of("${slef.env.HOME}"), suggesting("self"));
        assert_rejects(
            "${dpes.cmake.installPath}",
            &Rejection::UnknownToken("${dpes.cmake.installPath}"),
        );
        assert_eq!(hint_of("${dpes.cmake.installPath}"), suggesting("deps"));
    }

    // C-033 leg 2 — the threshold is length-scaled, not a flat 1. `instalPatch`
    // is two edits from `installPath`, whose length 11 gives `max(11, 3) / 3` =
    // 3. Under a flat 1 this falls to the escape branch, which is the same
    // escape-your-own-typo failure leg 1 guards from the other side.
    #[test]
    fn a_two_edit_typo_in_a_long_root_still_suggests_it() {
        assert_eq!(
            strsim::osa_distance("instalPatch", "installPath"),
            2,
            "premise: the pair is two edits apart under the chosen metric"
        );
        assert_eq!(
            std::cmp::max("installPath".len(), 3) / 3,
            3,
            "premise: an 11-character root scales to a threshold of 3, which admits those two edits"
        );

        assert_rejects("${instalPatch}", &Rejection::UnknownToken("${instalPatch}"));
        assert_eq!(hint_of("${instalPatch}"), suggesting("installPath"));
    }

    // S-009 — the near miss a publisher actually writes: the right word in the
    // wrong case. `${installpath}` is one substitution from `${installPath}`,
    // and the root match is case-sensitive, so it must route to the suggester
    // rather than to the escape branch — which would advise shipping a
    // lowercase `${installpath}` as literal text into a digest-pinned artifact.
    //
    // The token is in the C-004 rejection corpus, so its refusal is pinned;
    // only its *hint* was not, and the two branches are indistinguishable there.
    #[test]
    fn a_case_only_near_miss_suggests_the_canonical_root() {
        assert_eq!(hint_of("${installpath}"), suggesting("installPath"));
    }

    // C-033 leg 3 — an unrecognised root with no near miss is most likely
    // another tool's vocabulary, and under D3 the escape is its only legal
    // spelling. No suggestion: there is nothing OCX could plausibly claim the
    // publisher meant.
    #[test]
    fn a_foreign_token_with_no_near_miss_is_told_to_escape() {
        assert_eq!(hint_of("${workspaceFolder}"), UnknownTokenHint::Escape);
        assert_eq!(hint_of("${localEnv:HOME}"), UnknownTokenHint::Escape);
        assert_eq!(hint_of("${containerWorkspaceFolder}"), UnknownTokenHint::Escape);
    }

    // C-033 leg 4 — a recognised root carrying a body outside the closed set.
    // The publisher was writing an OCX token, so the message enumerates the
    // bodies that exist; offering the escape here would advise publishing
    // `${self.env.A B}` as literal text.
    #[test]
    fn a_recognised_root_with_an_illegal_body_gets_the_supported_bodies() {
        for input in ["${self.env.A B}", "${self}", "${deps}", "${installPath.foo}"] {
            assert_eq!(
                hint_of(input),
                UnknownTokenHint::SupportedBodies,
                "{input:?} carries a recognised root, so it must get the body list"
            );
        }
    }

    // C-033 leg 5 (added at execution time — the ADR lacks it): the branch
    // *order* is the rule, not an implementation detail.
    //
    // `deps` is a recognised root and `Python` is what fails the slug class, so
    // branch 3 must be decided **before** the suggester is consulted. A
    // suggester consulted first finds `deps` at distance 0 from itself and
    // answers "did you mean root 'deps'" to a publisher who already spelled
    // `deps` correctly — advice about the one part of the token that is right.
    //
    // Without this leg an implementation that consults the suggester first
    // passes every other leg of C-033.
    #[test]
    fn a_recognised_root_with_a_bad_name_never_reaches_the_suggester() {
        assert_eq!(
            hint_of("${deps.Python.installPath}"),
            UnknownTokenHint::SupportedBodies,
            "the fault is in NAME, and the suggester has nothing useful to say about a name"
        );
    }

    // ── Deviation-1 mitigations (C-034, C-035) ────────────────────────────────

    /// The alphabet the round trip is generated over: every byte the grammar
    /// branches on (`$`, `{`, `}`, `:`), the two separators a path value carries
    /// (`\`, `/`), and one ordinary letter to make position matter.
    ///
    /// `:` is in the alphabet because the body/modifier split reads it: without
    /// it the round trip never generates a modifier separator, and the escape's
    /// inverse is never exercised against the one byte that decides where a
    /// body ends.
    const ROUND_TRIP_ALPHABET: &[char] = &['$', '{', '}', '\\', '/', ':', 'a'];

    /// Enumerated to length 4: long enough to contain `$${` with a byte on
    /// either side, which is where an off-by-one in the escape branch lives.
    const ROUND_TRIP_MAX_LEN: usize = 4;

    /// Bytes and clusters an index-arithmetic bug would split, drop or
    /// duplicate: the ASCII/high-bit boundary either side of 0x7F, a CJK run,
    /// an emoji ZWJ sequence, and a combining-mark cluster.
    const NON_ASCII_TEXTS: &[&str] = &[
        "~",
        "\u{7F}",
        "\u{80}",
        "日本語",
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
        "e\u{0301}",
    ];

    /// Every string of length `0..=max_len` over `alphabet`.
    fn enumerate_strings(alphabet: &[char], max_len: usize) -> Vec<String> {
        let mut all = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..max_len {
            let mut next = Vec::with_capacity(frontier.len() * alphabet.len());
            for prefix in &frontier {
                for character in alphabet {
                    let mut candidate = prefix.clone();
                    candidate.push(*character);
                    next.push(candidate);
                }
            }
            all.extend_from_slice(&next);
            frontier = next;
        }
        all
    }

    /// The generated corpus: the exhaustive ASCII enumeration plus each
    /// non-ASCII cluster placed bare, inside a token body, inside an escaped
    /// token, between two recognised tokens, and against a trailing `$${`.
    fn generated_corpus() -> Vec<String> {
        let mut corpus = enumerate_strings(ROUND_TRIP_ALPHABET, ROUND_TRIP_MAX_LEN);
        for text in NON_ASCII_TEXTS {
            corpus.push((*text).to_string());
            corpus.push(format!("${{{text}}}"));
            corpus.push(format!("$${{{text}}}"));
            corpus.push(format!("${{installPath}}{text}${{self.installPath}}"));
            corpus.push(format!("{text}$${{"));
        }
        corpus
    }

    // C-034 (leg 1) — `scan(escape(s))` classifies to exactly the text of `s`,
    // for generated `s` rather than a hand-picked corpus. An enumerated fixture
    // set structurally cannot catch the failure mode `quality-core.md`'s worked
    // example describes: a wrong escape boundary affirmed identically by the
    // test and by the doc comment, with no fixture containing the offending
    // byte. Both halves of the inverse are exercised, so a wrong rule in either
    // one reds unless the *other* is wrong in exactly the mirror way.
    #[test]
    fn escaping_any_input_makes_the_scan_reproduce_it_verbatim() {
        for original in generated_corpus() {
            let escaped = escape(&original);
            let segments =
                scan(&escaped).unwrap_or_else(|error| panic!("scan(escape({original:?})) must not fail: {error}"));
            assert_eq!(
                literals_only(&segments),
                original,
                "scan(escape(s)) must reproduce s verbatim; s={original:?} escaped={escaped:?}"
            );
        }
    }

    // C-034 (byte conservation) — an input carrying no `${` carries no token
    // and no escape either, so the scan must return its bytes unchanged: none
    // dropped, none duplicated, none reordered. Asserted as string equality
    // rather than a length check, which reordering would survive.
    #[test]
    fn a_scan_of_delimiter_free_input_conserves_every_byte() {
        let corpus = generated_corpus();
        let subjects: Vec<&String> = corpus.iter().filter(|text| !text.contains("${")).collect();
        assert!(
            subjects.len() > 100,
            "the filtered corpus must still be a corpus, got {} inputs",
            subjects.len()
        );

        for original in subjects {
            let segments = scan(original).unwrap_or_else(|error| panic!("{original:?} carries no token: {error}"));
            assert_eq!(
                literals_only(&segments),
                *original,
                "{original:?} must survive the scan byte-identical"
            );
        }
    }

    // C-035 — non-ASCII text survives byte-identical before, after and between
    // recognised tokens, and adjacent to `$`, `$$` and `$${`.
    //
    // The reasoning the scanner rests on, recorded because it is the whole
    // safety argument for byte indexing: in UTF-8 every byte of a multi-byte
    // sequence has the high bit set, so an ASCII byte — `$`, `{`, `}`, `.`, `:`
    // — can never occur inside one, and index arithmetic on those five bytes
    // cannot split a character.
    #[test]
    fn non_ascii_text_survives_around_tokens_and_dollars() {
        for text in NON_ASCII_TEXTS {
            for input in [
                format!("{text}${{installPath}}"),
                format!("${{installPath}}{text}"),
                format!("${{installPath}}{text}${{self.installPath}}"),
                // Adjacent with nothing between them: the one arrangement in
                // which a token's extent ending one byte early or late is
                // absorbed by its neighbour instead of by a literal run.
                format!("{text}${{installPath}}${{self.installPath}}{text}"),
                format!("{text}${{deps.cmake.installPath:posix}}{text}"),
                format!("{text}$"),
            ] {
                let segments = scan(&input).unwrap_or_else(|error| panic!("{input:?} must scan: {error}"));
                assert_eq!(
                    rendered(&segments),
                    input,
                    "every literal byte around a token must survive in place"
                );
            }

            // Adjacent to the escape forms, where the expected output differs
            // from the input by exactly the collapsed `$$`.
            assert_eq!(literals_only(&scan(&format!("{text}$$")).unwrap()), format!("{text}$$"));
            assert_eq!(
                literals_only(&scan(&format!("{text}$${{installPath}}{text}")).unwrap()),
                format!("{text}${{installPath}}{text}")
            );
        }
    }
}
