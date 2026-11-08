// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The plaintext header grammar: `major *( ";" segment )`, frozen, ASCII, at most `MAX_HEADER` bytes.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::types::{HeaderView, MAX_HEADER, Narrowing};

/// A header split off the front of a raw value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHeader {
    pub view: HeaderView,
    /// Index of the first `.`, or `None` when none lies within `MAX_HEADER` bytes or the major is invalid
    /// (the view still carries the narrowing names found).
    pub payload_start: Option<usize>,
    pub valid: bool,
}

/// Parses the header out of the raw bytes. Every `n=` name found is honoured whatever else fails.
pub fn parse(raw: &[u8]) -> ParsedHeader {
    // A header is at most `MAX_HEADER` bytes, so its terminating dot sits at index `MAX_HEADER` at most.
    let window = &raw[..raw.len().min(MAX_HEADER + 1)];
    let dot = window.iter().position(|&b| b == b'.');
    let header = match dot {
        Some(index) => &raw[..index],
        None => &raw[..raw.len().min(MAX_HEADER)],
    };

    // The window cut the last segment: its `h` would decode to a prefix of the written path.
    let cut = dot.is_none() && raw.len() > MAX_HEADER;

    let mut segments = header.split(|&b| b == b';').peekable();
    let major = segments.next().and_then(parse_major);
    let mut view = HeaderView {
        major,
        ..HeaderView::default()
    };
    while let Some(segment) = segments.next() {
        let complete = !(cut && segments.peek().is_none());
        // A non-ASCII byte ends only the segment it sits in.
        let ascii_len = segment.iter().position(|b| !b.is_ascii()).unwrap_or(segment.len());
        let (name, text) = match segment[..ascii_len].iter().position(|&b| b == b'=') {
            Some(eq) => (&segment[..eq], &segment[eq + 1..ascii_len]),
            None => continue,
        };
        match name {
            b"n" => apply_narrowing(&mut view.narrowing, text),
            b"h" if complete && ascii_len == segment.len() && view.home.is_none() => view.home = parse_home(text),
            _ => {}
        }
    }

    let valid = major.is_some() && dot.is_some();
    ParsedHeader {
        view,
        payload_start: if valid { dot } else { None },
        valid,
    }
}

/// Emits `major[;n=...][;h=...]`, never longer than `MAX_HEADER`: a home that would pass it is not written.
pub fn emit(major: u32, narrowing: &Narrowing, home: Option<&Path>) -> String {
    let mut header = major.to_string();
    let names: Vec<&str> = [
        (narrowing.offline, "offline"),
        (narrowing.frozen, "frozen"),
        (narrowing.no_consent, "no_consent"),
        (narrowing.credential_helper, "credential_helper"),
    ]
    .into_iter()
    .filter_map(|(set, name)| set.then_some(name))
    .collect();
    if !names.is_empty() {
        header.push_str(";n=");
        header.push_str(&names.join(","));
    }
    // A home the reader would drop (relative, non-UTF-8) is not written.
    if let Some(text) = home.filter(|path| path.is_absolute()).and_then(Path::to_str) {
        let segment = format!(";h={}", URL_SAFE_NO_PAD.encode(text));
        // Past `MAX_HEADER` a reader finds no `.` and the whole value is unreadable.
        if header.len() + segment.len() <= MAX_HEADER {
            header.push_str(&segment);
        }
    }
    header
}

fn parse_major(segment: &[u8]) -> Option<u32> {
    if segment.is_empty() || segment.len() > MAX_MAJOR_DIGITS || !segment.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(segment).ok()?.parse().ok()
}

fn apply_narrowing(narrowing: &mut Narrowing, names: &[u8]) {
    for name in names.split(|&b| b == b',') {
        match name {
            b"offline" => narrowing.offline = true,
            b"frozen" => narrowing.frozen = true,
            b"no_consent" => narrowing.no_consent = true,
            b"credential_helper" => narrowing.credential_helper = true,
            _ => {}
        }
    }
}

fn parse_home(text: &[u8]) -> Option<PathBuf> {
    let decoded = URL_SAFE_NO_PAD.decode(text).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    // A control character (NUL included) would reach a terminal, a log or a C string as written.
    if text.chars().any(char::is_control) {
        return None;
    }
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

const MAX_MAJOR_DIGITS: usize = 10;

#[cfg(test)]
mod tests;
