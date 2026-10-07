// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Parsing of one `#[exit(...)]` attribute into a [`Spec`].

use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Index, LitStr, Member, Path, Token, parenthesized};

/// One declared `error.detail` row: an exit code name, a slug and a summary.
#[derive(Clone)]
pub(crate) struct Row {
    pub code: Ident,
    /// `defer(Code)`: the arm's `classify` answers `None`, and `Code` is only the row's published code.
    pub defers: bool,
    pub slug: LitStr,
    pub summary: LitStr,
}

/// A field of the variant's payload, optionally followed by fields of that field: `0`, `source`, `0.kind`.
#[derive(Clone)]
pub(crate) struct FieldPath(pub Vec<Member>);

pub(crate) fn member_name(member: &Member) -> String {
    match member {
        Member::Named(ident) => ident.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    }
}

impl Parse for FieldPath {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut members = Vec::new();
        loop {
            if input.peek(syn::LitFloat) {
                // `0.0` lexes as one float literal: two tuple indices.
                let literal: syn::LitFloat = input.parse()?;
                for part in literal.base10_digits().split('.') {
                    let index = part
                        .parse::<u32>()
                        .map_err(|_| syn::Error::new(literal.span(), "expected a field index"))?;
                    members.push(Member::Unnamed(Index {
                        index,
                        span: literal.span(),
                    }));
                }
            } else if input.peek(syn::LitInt) {
                let literal: syn::LitInt = input.parse()?;
                members.push(Member::Unnamed(Index {
                    index: literal.base10_parse()?,
                    span: literal.span(),
                }));
            } else {
                members.push(Member::Named(input.parse()?));
            }
            if !input.peek(Token![.]) {
                break;
            }
            input.parse::<Token![.]>()?;
        }
        Ok(Self(members))
    }
}

/// What an arm says about its code and slug.
#[derive(Clone)]
pub(crate) enum Form {
    /// `Code, slug = "…", summary = "…"` (or `defer(Code), …`).
    Fixed(Row),
    /// `delegate` or `delegate = field`.
    Delegate(Option<FieldPath>),
    /// `chain, fallback(…)` or `chain = field, fallback(…)`.
    Chain { field: Option<FieldPath>, fallback: Row },
    /// `with = path, rows(…)`.
    With { path: Path, rows: Vec<Row> },
}

/// A parsed `#[exit(...)]`: the form, and the type-level `family` override and `reserve` rows.
pub(crate) struct Spec {
    pub form: Option<Form>,
    pub family: Option<LitStr>,
    pub reserved: Vec<Row>,
}

/// Stores `value` in `slot`, refusing a key given twice: the later one would silently replace the first.
fn set_once<T>(slot: &mut Option<T>, key: &Ident, value: T) -> syn::Result<()> {
    if slot.is_some() {
        return Err(syn::Error::new(key.span(), format!("`{key}` is given more than once")));
    }
    *slot = Some(value);
    Ok(())
}

#[derive(Default)]
struct RowParts {
    code: Option<(Ident, bool)>,
    slug: Option<LitStr>,
    summary: Option<LitStr>,
}

impl RowParts {
    /// Takes the row keys: `slug = …`, `summary = …`, `defer(Code)` and a bare `Code`.
    fn accept(&mut self, key: Ident, input: ParseStream) -> syn::Result<()> {
        match key.to_string().as_str() {
            "slug" => {
                input.parse::<Token![=]>()?;
                set_once(&mut self.slug, &key, input.parse()?)
            }
            "summary" => {
                input.parse::<Token![=]>()?;
                set_once(&mut self.summary, &key, input.parse()?)
            }
            "defer" => {
                let inner;
                parenthesized!(inner in input);
                self.set_code(inner.parse()?, true)
            }
            _ => self.set_code(key, false),
        }
    }

    fn set_code(&mut self, code: Ident, defers: bool) -> syn::Result<()> {
        if self.code.is_some() {
            return Err(syn::Error::new(code.span(), "a row names exactly one exit code"));
        }
        self.code = Some((code, defers));
        Ok(())
    }

    fn into_row(self, span: Span) -> syn::Result<Row> {
        let missing = |what: &str| syn::Error::new(span, format!("a row needs {what}"));
        let (code, defers) = self.code.ok_or_else(|| missing("an exit code name"))?;
        if code == "Success" {
            return Err(syn::Error::new(
                code.span(),
                "a row is an error outcome: `Success` is not a row's exit code",
            ));
        }
        Ok(Row {
            code,
            defers,
            slug: self.slug.ok_or_else(|| missing("`slug = \"…\"`"))?,
            summary: self.summary.ok_or_else(|| missing("`summary = \"…\"`"))?,
        })
    }
}

fn parse_row(input: ParseStream, span: Span) -> syn::Result<Row> {
    let mut parts = RowParts::default();
    while !input.is_empty() {
        let key: Ident = input.parse()?;
        parts.accept(key, input)?;
        if input.is_empty() {
            break;
        }
        input.parse::<Token![,]>()?;
    }
    parts.into_row(span)
}

/// `= field` after a keyword, or nothing.
fn optional_field(input: ParseStream) -> syn::Result<Option<FieldPath>> {
    if input.peek(Token![=]) {
        input.parse::<Token![=]>()?;
        return Ok(Some(input.parse()?));
    }
    Ok(None)
}

impl Parse for Spec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let span = input.span();
        let mut row = RowParts::default();
        let mut delegate = None;
        let mut chain = None;
        let mut fallback = None;
        let mut with = None;
        let mut rows = None;
        let mut family = None;
        let mut reserved = Vec::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "delegate" => set_once(&mut delegate, &key, optional_field(input)?)?,
                "chain" => set_once(&mut chain, &key, optional_field(input)?)?,
                "fallback" => {
                    let inner;
                    parenthesized!(inner in input);
                    set_once(&mut fallback, &key, parse_row(&inner, key.span())?)?;
                }
                "with" => {
                    input.parse::<Token![=]>()?;
                    set_once(&mut with, &key, input.parse::<Path>()?)?;
                }
                "rows" => {
                    let inner;
                    parenthesized!(inner in input);
                    let mut parsed = Vec::new();
                    while !inner.is_empty() {
                        let one;
                        parenthesized!(one in inner);
                        parsed.push(parse_row(&one, key.span())?);
                        if inner.is_empty() {
                            break;
                        }
                        inner.parse::<Token![,]>()?;
                    }
                    set_once(&mut rows, &key, parsed)?;
                }
                "reserve" => {
                    let inner;
                    parenthesized!(inner in input);
                    reserved.push(parse_row(&inner, key.span())?);
                }
                "family" => {
                    input.parse::<Token![=]>()?;
                    set_once(&mut family, &key, input.parse::<LitStr>()?)?;
                }
                _ => row.accept(key, input)?,
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        let has_row = row.code.is_some() || row.slug.is_some() || row.summary.is_some();
        let forms = [has_row, delegate.is_some(), chain.is_some(), with.is_some()];
        let form = match forms.iter().filter(|present| **present).count() {
            0 if fallback.is_none() && rows.is_none() => None,
            1 => Some(if has_row {
                Form::Fixed(row.into_row(span)?)
            } else if let Some(field) = delegate {
                Form::Delegate(field)
            } else if let Some(field) = chain {
                let fallback = fallback.take().ok_or_else(|| {
                    syn::Error::new(span, "`chain` needs `fallback(Code, slug = \"…\", summary = \"…\")`")
                })?;
                if fallback.defers {
                    return Err(syn::Error::new(
                        fallback.code.span(),
                        "`defer` does not belong in `fallback(…)`: a chain already defers to its causes",
                    ));
                }
                // Plain `chain` answers `None`, so the fallback code is only advertised in `DETAILS`.
                if field.is_none() && fallback.code != "Failure" {
                    return Err(syn::Error::new(
                        fallback.code.span(),
                        "plain `chain` answers no exit code, so its fallback must be `Failure`; use `chain = field` to decide one",
                    ));
                }
                Form::Chain { field, fallback }
            } else {
                Form::With {
                    path: with
                        .take()
                        .ok_or_else(|| syn::Error::new(span, "`with` needs a path"))?,
                    rows: rows.take().unwrap_or_default(),
                }
            }),
            _ => {
                return Err(syn::Error::new(
                    span,
                    "an `#[exit(...)]` is exactly one of: `Code, slug, summary`, `delegate`, `chain, fallback(…)`, `with = path, rows(…)`",
                ));
            }
        };
        if fallback.is_some() {
            return Err(syn::Error::new(span, "`fallback(…)` belongs to `chain`"));
        }
        if rows.is_some() {
            return Err(syn::Error::new(span, "`rows(…)` belongs to `with`"));
        }
        Ok(Self { form, family, reserved })
    }
}
