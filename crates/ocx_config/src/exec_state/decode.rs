// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Bounded decode of an untrusted `__OCX_ENV_STATE` value.
//!
//! The value is inherited input: any process up the tree, or a file an IDE captured, may have written it.
//! Every arm therefore fails closed into [`Inherited::Unreadable`], never into an error and never a panic,
//! and the plaintext header is read first so a bad payload cannot hide a narrowing name.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io::Read as _;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use flate2::bufread::ZlibDecoder;

use super::header;
use super::types::{
    AppliedValue, ExecState, HeaderView, Inherited, MAJOR, MAX_ENCODED, MAX_INFLATED, MAX_INHERITED_SCOPES,
    MAX_RECORDED_TIERS, MAX_RECORDS, Narrowing, Pinned, PinnedTier, Policy, ReplayValue, Scope, Selection, Truncation,
    UnreadableReason, ValueKind,
};

/// Longest group name a project accepts (`ocx_package::metadata::slug::SLUG_MAX_LEN`).
const GROUP_NAME_MAX_LEN: usize = 64;

/// The last value decoded and its result. A process reads its own state at several seams (argv
/// folding, the activation entry, the reconcile, `Context` init) and a cap-sized value costs ~0.16 ms
/// to inflate and parse each time; decoding is a pure function of the bytes, so one entry makes the
/// repeats free without changing what any caller sees.
static LAST: Mutex<Option<(OsString, Inherited)>> = Mutex::new(None);

/// Decodes a raw value; never an error, an unreadable value is `Inherited::Unreadable`.
pub fn decode(raw: Option<&OsStr>) -> Inherited {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Inherited::Absent;
    };
    if let Some((key, decoded)) = LAST.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        && key == raw
    {
        return decoded.clone();
    }
    let decoded = decode_uncached(Some(raw));
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Some((raw.to_os_string(), decoded.clone()));
    decoded
}

fn decode_uncached(raw: Option<&OsStr>) -> Inherited {
    // Step 1.
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Inherited::Absent;
    };
    let bytes = raw_bytes(raw);

    // Step 2: header first, before any check on the whole value.
    let parsed = header::parse(&bytes);
    let unreadable = |reason| Inherited::Unreadable {
        header: parsed.view.clone(),
        reason,
    };
    let Some(payload_start) = parsed.payload_start.filter(|_| parsed.valid) else {
        return unreadable(UnreadableReason::HeaderInvalid);
    };
    // Step 3.
    if parsed.view.major != Some(MAJOR) {
        return unreadable(UnreadableReason::OtherMajor);
    }
    // Step 4.
    if bytes.len() > MAX_ENCODED {
        return unreadable(UnreadableReason::TooLong);
    }
    if raw.to_str().is_none() {
        return unreadable(UnreadableReason::NotUtf8);
    }

    let payload = &bytes[payload_start + 1..];
    // Step 5: the rung-3 marker is a readable state with the payload lost, never a stand-down.
    if payload.is_empty() {
        return Inherited::Readable(ExecState {
            narrowing: parsed.view.narrowing,
            home: parsed.view.home,
            truncated: Truncation::PayloadDropped,
            ..ExecState::default()
        });
    }

    match decode_payload(payload) {
        Ok(mut state) => {
            state.narrowing = parsed.view.narrowing;
            state.home = parsed.view.home;
            Inherited::Readable(state)
        }
        Err(reason) => unreadable(reason),
    }
}

impl Inherited {
    /// Decodes the process's own `__OCX_ENV_STATE` through the `ocx_env` read seam.
    pub fn from_process_env() -> Self {
        decode(ocx_env::__OCX_ENV_STATE.get_raw().as_deref())
    }

    /// The narrowing the header carries; honoured for `Readable` and `Unreadable` alike.
    ///
    /// `credential_helper` implies `offline`: a helper child must never reach the network.
    pub fn narrowing(&self) -> Narrowing {
        let mut narrowing = match self {
            Self::Absent => Narrowing::default(),
            Self::Readable(state) => state.narrowing,
            Self::Unreadable { header, .. } => header.narrowing,
        };
        narrowing.offline |= narrowing.credential_helper;
        narrowing
    }

    /// The home the header carries; same honouring as [`Inherited::narrowing`].
    pub fn home(&self) -> Option<&Path> {
        match self {
            Self::Absent => None,
            Self::Readable(state) => state.home.as_deref(),
            Self::Unreadable {
                header: HeaderView { home, .. },
                ..
            } => home.as_deref(),
        }
    }
}

#[cfg(unix)]
fn raw_bytes(raw: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt as _;
    Cow::Borrowed(raw.as_bytes())
}

#[cfg(not(unix))]
fn raw_bytes(raw: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(raw.to_string_lossy().into_owned().into_bytes())
}

/// Steps 6 to 10 on a non-empty payload.
fn decode_payload(payload: &[u8]) -> Result<ExecState, UnreadableReason> {
    // Step 6: strict, unpadded base64url; the engine refuses padding and non-canonical trailing bits.
    let compressed = URL_SAFE_NO_PAD.decode(payload).map_err(|_| UnreadableReason::Base64)?;

    // Step 7: inflate through a bounded reader, so a bomb costs `MAX_INFLATED + 1` bytes, not its size.
    let mut input = compressed.as_slice();
    let mut inflated = Vec::new();
    let read = {
        let decoder = ZlibDecoder::new(&mut input);
        decoder.take(MAX_INFLATED as u64 + 1).read_to_end(&mut inflated)
    };
    if read.is_err() {
        return Err(UnreadableReason::Inflate);
    }
    if inflated.len() > MAX_INFLATED {
        return Err(UnreadableReason::InflateLimit);
    }
    if !input.is_empty() {
        return Err(UnreadableReason::TrailingBytes);
    }

    // Step 8: duplicate fields, an unknown enum variant and nesting past the recursion limit land here.
    let state: ExecState = serde_json::from_slice(&inflated).map_err(|_| UnreadableReason::Json)?;

    // Step 9.
    let records = state.applied.len() + state.replay.env.len() + state.replay.launch_identities.len();
    if records > MAX_RECORDS {
        return Err(UnreadableReason::RecordLimit);
    }
    if state.session.tiers.len() > MAX_RECORDED_TIERS {
        return Err(UnreadableReason::TierLimit);
    }
    if state.inherited_scopes.len() > MAX_INHERITED_SCOPES {
        return Err(UnreadableReason::ScopeLimit);
    }

    // Step 10.
    validate(&state)?;
    Ok(state)
}

fn invalid(field: &'static str) -> UnreadableReason {
    UnreadableReason::Invalid { field }
}

/// Step 10: fail closed on the whole payload.
fn validate(state: &ExecState) -> Result<(), UnreadableReason> {
    validate_policy(&state.policy)?;
    state.applied.iter().try_for_each(validate_applied)?;
    state.replay.env.iter().try_for_each(validate_replay_value)?;
    for identity in &state.replay.launch_identities {
        if !is_sha256_digest(&identity.digest) {
            return Err(invalid("replay.launch_identities.digest"));
        }
        let names_ok = identity.names.iter().all(|name| {
            !name.contains('@') && !name.chars().any(char::is_control) && name.parse::<ocx_oci::PackageRef>().is_ok()
        });
        if !names_ok {
            return Err(invalid("replay.launch_identities.names"));
        }
    }
    for scope in &state.inherited_scopes {
        if let Scope::Project { root } = scope
            && !root.is_absolute()
        {
            return Err(invalid("inherited_scopes.root"));
        }
    }
    if let Selection::Pinned(pinned) = &state.selection {
        validate_pinned(pinned)?;
    }
    validate_session(state)
}

fn validate_pinned(pinned: &Pinned) -> Result<(), UnreadableReason> {
    if let PinnedTier::Project { dir } = &pinned.tier
        && !dir.is_absolute()
    {
        return Err(invalid("selection.pinned.dir"));
    }
    if !pinned.groups.iter().all(|group| is_group_name(group)) {
        return Err(invalid("selection.pinned.groups"));
    }
    if !is_hex(&pinned.invocation_digest, 64) {
        return Err(invalid("selection.pinned.invocation_digest"));
    }
    Ok(())
}

fn validate_session(state: &ExecState) -> Result<(), UnreadableReason> {
    if !state.session.tiers.iter().all(|tier| tier.is_absolute()) {
        return Err(invalid("session.tiers"));
    }
    if let Some(hint) = &state.session.project {
        if !hint.dir.is_absolute() {
            return Err(invalid("session.project.dir"));
        }
        if !is_hex(&hint.key, 16) {
            return Err(invalid("session.project.key"));
        }
    }
    Ok(())
}

fn validate_applied(record: &AppliedValue) -> Result<(), UnreadableReason> {
    // Credential slots are reserved keys too: no writer records one, and reverting a forged record would drop the
    // user's credential.
    if !ocx_env::is_valid_env_key(&record.key) || ocx_env::is_reserved_ocx_key(&record.key) {
        return Err(invalid("applied.key"));
    }
    if record.kind == ValueKind::Constant && is_never_constant(&record.key) {
        return Err(invalid("applied.key"));
    }
    validate_separator(record.kind, record.separator.as_deref()).map_err(|()| invalid("applied.separator"))?;
    if record.prior.is_some() && record.kind != ValueKind::Constant {
        return Err(invalid("applied.prior"));
    }
    Ok(())
}

fn validate_replay_value(record: &ReplayValue) -> Result<(), UnreadableReason> {
    // The carrier is untrusted input: a replayed `OCX_*` key would let it set the tool's own configuration
    // and credentials in the launcher's environment, so only ordinary variable names replay.
    if !ocx_env::is_valid_env_key(&record.key) || ocx_env::is_reserved_ocx_key(&record.key) {
        return Err(invalid("replay.env.key"));
    }
    // A launcher applies replay values: a `PATH` constant would replace the whole search path.
    if record.kind == ValueKind::Constant && is_never_constant(&record.key) {
        return Err(invalid("replay.env.key"));
    }
    validate_separator(record.kind, record.separator.as_deref()).map_err(|()| invalid("replay.env.separator"))?;
    Ok(())
}

/// A `List` has a non-empty separator; nothing else has one.
fn validate_separator(kind: ValueKind, separator: Option<&str>) -> Result<(), ()> {
    match (kind, separator) {
        (ValueKind::List, Some(separator)) if !separator.is_empty() => Ok(()),
        (ValueKind::List, _) => Err(()),
        (_, None) => Ok(()),
        (_, Some(_)) => Err(()),
    }
}

/// Every value goes through the parser of the variable it replaces; a refusal there refuses the payload.
fn validate_policy(policy: &Policy) -> Result<(), UnreadableReason> {
    let paths = [
        ("policy.config_path", &policy.config_path),
        ("policy.index_dir", &policy.index_dir),
        ("policy.toolchain_dir", &policy.toolchain_dir),
        ("policy.patch_snapshot_path", &policy.patch_snapshot_path),
        ("policy.records_dir", &policy.records_dir),
    ];
    for (field, path) in paths {
        if path.as_deref().is_some_and(|path| !path.is_absolute()) {
            return Err(invalid(field));
        }
    }
    if let Some(mirrors) = &policy.mirrors {
        mirrors_parse(mirrors).map_err(|()| invalid("policy.mirrors"))?;
    }
    if let Some(patches) = &policy.patches {
        patches_parse(patches).map_err(|()| invalid("policy.patches"))?;
    }
    if let Some(source) = &policy.managed_config
        && ocx_oci::PackageRef::parse_with_default_registry(source, ocx_oci::DEFAULT_REGISTRY).is_err()
    {
        return Err(invalid("policy.managed_config"));
    }
    if let Some(name) = &policy.records_name
        && !is_records_filename(name)
    {
        return Err(invalid("policy.records_name"));
    }
    if let Some(registry) = &policy.default_registry
        && !is_host_like(registry)
    {
        return Err(invalid("policy.default_registry"));
    }
    if let Some(list) = &policy.insecure_registries
        && !list
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .all(is_host_like)
    {
        return Err(invalid("policy.insecure_registries"));
    }
    Ok(())
}

/// `OCX_MIRRORS`: a JSON object of per-host values, each role URL through `parse_url` (which refuses userinfo).
fn mirrors_parse(raw: &str) -> Result<(), ()> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw).map_err(|_| ())?;
    for (host, value) in &map {
        let Some(config) = crate::mirror::parse_mirror_value(host, value).map_err(|_| ())? else {
            continue;
        };
        for url in [&config.registry, &config.index].into_iter().flatten() {
            crate::mirror::parse_url(url).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// `OCX_PATCHES`: a JSON object with a string `registry` (as `patch::patches_from_env` reads it).
fn patches_parse(raw: &str) -> Result<(), ()> {
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw).map_err(|_| ())?;
    match map.get("registry") {
        Some(serde_json::Value::String(registry)) if is_host_like(registry) => Ok(()),
        _ => Err(()),
    }
}

/// A registry host (`host[:port]`): non-empty, no whitespace, control character, userinfo or path.
fn is_host_like(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '@' | '/' | '\\'))
}

/// The filename half of the records sink's template check: no path separator, not `.` or `..`, no control
/// character. Placeholder validation stays with the sink.
fn is_records_filename(name: &str) -> bool {
    !matches!(name, "." | "..") && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_sha256_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| is_hex(hex, 64))
}

/// A project group name: `[A-Za-z0-9][A-Za-z0-9._-]*`, at most 64 bytes.
fn is_group_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= GROUP_NAME_MAX_LEN
        && bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// `PATH` and `PATHEXT` are never constants; keys fold ASCII case on Windows.
fn is_never_constant(key: &str) -> bool {
    ["PATH", "PATHEXT"].iter().any(|reserved| key_eq(key, reserved))
}

fn key_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests;
