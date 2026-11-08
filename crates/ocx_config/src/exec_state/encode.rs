// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Encode with the overflow ladder.
//!
//! The value is `header "." payload`, the payload being base64url-nopad of zlib of compact JSON. The
//! alphabet is digits, ASCII letters and `; = , . - _`, so no `%`, `"`, LF or CR reaches a cmd.exe
//! line or `$GITHUB_ENV`. The cap is measured on the whole value, header included, because that is what
//! the emitted line carries.

use std::collections::HashMap;
use std::io::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::Compression;
use flate2::write::ZlibEncoder;

use super::header;
use super::types::{
    Encoded, ExecState, MAJOR, MAX_ENCODED, MAX_INFLATED, MAX_INHERITED_SCOPES, MAX_RECORDS, Owner, Truncation,
};

/// The zlib level of every encode: the re-encode runs on every changed prompt and the level is the
/// whole of its cost. `the_zlib_level_never_lands_a_state_on_a_worse_rung_than_level_nine` holds it to
/// level 9's rungs and, on realistic states, to a bounded length.
const ZLIB_LEVEL: Compression = Compression::new(6);

/// Encodes a state, applying the overflow ladder until the value fits `MAX_ENCODED`.
///
/// `Encoded::truncated` is the rung this call had to reach (`None` for a full encode). The payload itself
/// carries `max(state.truncated, rung)`, so a later reader still sees truncation an outer writer did.
/// `value` is always `Some`: the header is bounded by `MAX_HEADER`, well under `MAX_ENCODED`.
pub fn encode(state: &ExecState) -> Encoded {
    encode_at(state, ZLIB_LEVEL)
}

fn encode_at(state: &ExecState, level: Compression) -> Encoded {
    let mut state = state.clone();
    if cfg!(windows) {
        fold_key_spelling(&mut state);
    }
    let excess = state.inherited_scopes.len().saturating_sub(MAX_INHERITED_SCOPES);
    state.inherited_scopes.drain(..excess);

    let head = header::emit(MAJOR, &state.narrowing, state.home.as_deref());
    let sticky = state.truncated;

    let mut rung = Truncation::None;
    loop {
        if rung == Truncation::PayloadDropped {
            return header_only(&head);
        }
        let mut candidate = state.clone();
        match rung {
            Truncation::None | Truncation::PayloadDropped => {}
            Truncation::InheritedDropped => drop_inherited(&mut candidate),
            Truncation::AllAppliedDropped => {
                drop_inherited(&mut candidate);
                candidate.applied.clear();
            }
        }
        candidate.truncated = sticky.max(rung);
        if let Some(value) = assemble(&head, &candidate, level)
            && value.len() <= MAX_ENCODED
        {
            return Encoded {
                encoded_len: value.len(),
                value: Some(value),
                truncated: rung,
            };
        }
        rung = match rung {
            Truncation::None => Truncation::InheritedDropped,
            Truncation::InheritedDropped => Truncation::AllAppliedDropped,
            Truncation::AllAppliedDropped | Truncation::PayloadDropped => Truncation::PayloadDropped,
        };
    }
}

/// Rung 1: every inherited applied and identity record goes; `inherited_scopes` stays.
fn drop_inherited(state: &mut ExecState) {
    state.applied.retain(|record| record.owner != Owner::Inherited);
    state
        .replay
        .launch_identities
        .retain(|record| record.owner != Owner::Inherited);
}

/// Rung 3: the header and the empty payload marker.
fn header_only(head: &str) -> Encoded {
    let value = format!("{head}.");
    Encoded {
        encoded_len: value.len(),
        value: Some(value),
        truncated: Truncation::PayloadDropped,
    }
}

/// `None` when the payload cannot be built or would not survive the decoder's `MAX_INFLATED` and
/// `MAX_RECORDS` bounds: a state every child reads as unreadable is worse than a truncated one.
fn assemble(head: &str, state: &ExecState, level: Compression) -> Option<String> {
    let records = state.applied.len() + state.replay.env.len() + state.replay.launch_identities.len();
    if records > MAX_RECORDS {
        return None;
    }
    let json = serde_json::to_vec(state).ok()?;
    if json.len() > MAX_INFLATED {
        return None;
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), level);
    encoder.write_all(&json).ok()?;
    let compressed = encoder.finish().ok()?;
    Some(format!("{head}.{}", URL_SAFE_NO_PAD.encode(compressed)))
}

/// On Windows two spellings of one key are one variable. Every record key takes the spelling its
/// first occurrence used, so the payload never holds a variable under two names.
fn fold_key_spelling(state: &mut ExecState) {
    let mut first: HashMap<String, String> = HashMap::new();
    let keys = state
        .applied
        .iter_mut()
        .map(|record| &mut record.key)
        .chain(state.replay.env.iter_mut().map(|record| &mut record.key));
    for key in keys {
        let folded = key.to_ascii_uppercase();
        match first.get(&folded) {
            Some(spelling) => key.clone_from(spelling),
            None => {
                first.insert(folded, key.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests;
