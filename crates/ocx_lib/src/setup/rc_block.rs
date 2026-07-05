// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Versioned, content-hashed RC-block state machine (Decision 3D).
//!
//! OCX writes a conda-style fenced block to user shell profiles:
//!
//! ```text
//! # >>> ocx v1 a1b2c3d4 >>>
//! if [ -f "${OCX_HOME:-$HOME/.ocx}/env.sh" ]; then
//!     . "${OCX_HOME:-$HOME/.ocx}/env.sh"
//! fi
//! # <<< ocx <<<
//! ```
//!
//! The opener carries a format version (`v1`) and an 8-hex content hash of the
//! block body. A three-hash state machine drives idempotent re-application,
//! format-upgrade rewrites, and dirty-edit detection:
//!
//! - **canonical** — hash of the body *this* binary would write (compile-time derivable).
//! - **marker** — the hash parsed from the on-disk opener line.
//! - **actual** — hash recomputed from the on-disk block body.
//!
//! | canonical | marker | actual | state | action |
//! |---|---|---|---|---|
//! | — | absent | — | `Fresh` | append fresh block |
//! | = | = | = | `Current` | no-op |
//! | = | absent | = | `Current` | no-op (hashless opener treated as v0-clean) |
//! | ≠ | = (or different `v\d+`) | = | `FormatUpgraded` | rewrite to canonical v1 |
//! | ≠ | absent | ≠ canonical | `FormatUpgraded` | silent overwrite — no marker means user edits undetectable |
//! | any | present | ≠ marker | `Dirty` | skip unless `--force` |
//!
//! Line scanning is manual (O(n), CRLF-tolerant, collapses duplicates); the
//! opener `regex` is used only to parse the version + hash off a single line.
//! See `.claude/artifacts/adr_self_setup.md` and the research artifact (Gap 1/2)
//! for the prior art this ports from.

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use crate::setup::error::Error;

/// The version this binary writes into the opener (`# >>> ocx v1 … >>>`).
const CURRENT_VERSION: u32 = 1;

/// Fence label used by the original `ocx self setup` shell-activation block.
/// Passed as the `label` argument by every existing caller so output stays
/// byte-identical; a second caller (e.g. managed config) passes its own
/// label (e.g. `"ocx managed"`) to get a distinctly fenced block.
pub const OCX_LABEL: &str = "ocx";

/// Fence label used by `ocx self setup --managed-config` (the second caller of
/// this fence machine) so its `[managed]` seed block is distinctly fenced from
/// the shell-activation block above.
pub const MANAGED_LABEL: &str = "ocx managed";

/// Closer line for the legacy (pre-v1) `# BEGIN ocx` / `# END ocx` migration
/// path — [`strip_block`] and [`has_legacy_artifacts`] are ocx-specific (no
/// second caller has a legacy footprint to migrate from), so they stay
/// hardcoded to the original label rather than taking a `label` parameter.
const CLOSER: &str = "# <<< ocx <<<";

/// A managed block as it appears on disk, with the spans needed to splice it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedBlock {
    /// Format version parsed from the opener (`v\d+`).
    pub version: u32,
    /// Content hash parsed from the opener, if the opener carried one.
    pub marker: Option<String>,
    /// Block body between the fences, normalized to `\n` line endings.
    pub body: String,
    /// Index (in normalized lines) of the opener line.
    opener_line: usize,
    /// Index (in normalized lines) of the closer line.
    closer_line: usize,
}

/// State of a profile relative to the block this binary would write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockState {
    /// No ocx-versioned fence present.
    Fresh,
    /// Block present, same version, body matches canonical and its own marker.
    Current,
    /// Block is ocx-authored (body matches its marker) but version or hash differs.
    FormatUpgraded,
    /// Block body hash differs from its marker — the user edited it.
    Dirty,
}

/// The payload a profile carries between the fences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcBlock {
    /// Body between the fences, e.g. `. "$OCX_HOME/env.sh"`.
    pub body: String,
}

impl RcBlock {
    /// Build the full fenced block text (LF-terminated, opener + body + closer)
    /// for the given fence `label` (e.g. [`OCX_LABEL`] or a second caller's own
    /// label).
    fn render(&self, label: &str) -> String {
        format!(
            "# >>> {label} v{CURRENT_VERSION} {hash} >>>\n{body}\n{closer}\n",
            hash = canonical_hash(&self.body),
            body = self.body,
            closer = closer_text(label),
        )
    }
}

/// Closer line for a v1-versioned fence with the given `label`.
fn closer_text(label: &str) -> String {
    format!("# <<< {label} <<<")
}

/// Version-agnostic opener for `label`: matches any `# >>> {label} v<N>
/// [<hash8>] >>>` line so a future v2 fence (or a malformed-hash opener) is
/// recognized and collapsed.
///
/// Rebuilt per call rather than cached — only a handful of distinct labels
/// exist (one per caller) and this runs at most a few times per CLI
/// invocation, so a `OnceLock`-per-label cache would be premature.
// ponytail: rebuilt per call; cache-by-label if this becomes a hot path.
fn opener_regex(label: &str) -> Regex {
    let escaped_label = regex::escape(label);
    // Tolerate leading whitespace and an optional hash group. `label` is
    // always a compile-time-constant caller label, so this never fails.
    Regex::new(&format!(
        r"^\s*# >>> {escaped_label} v(\d+)(?: ([0-9a-f]{{8}}))? >>>\s*$"
    ))
    .expect("opener regex is a valid pattern for any label")
}

/// Normalize a body for hashing/matching: CRLF/CR → LF, strip a single trailing
/// newline so a body compares equal regardless of how it was spliced.
fn normalize(body: &str) -> String {
    let unix = body.replace("\r\n", "\n").replace('\r', "\n");
    unix.strip_suffix('\n').map(str::to_string).unwrap_or(unix)
}

/// Canonical content hash of a block body: the low 4 bytes of its `SHA-256`,
/// hex-encoded (8 hex chars). Line-ending agnostic via [`normalize`].
pub fn canonical_hash(body: &str) -> String {
    let digest = Sha256::digest(normalize(body).as_bytes());
    hex::encode(&digest[..4])
}

/// Detect whether `content` predominantly uses CRLF line endings.
fn dominant_is_crlf(content: &str) -> bool {
    let crlf = content.matches("\r\n").count();
    if crlf == 0 {
        return false;
    }
    // Count bare LF (LF not preceded by CR) to compare against CRLF runs.
    // Strict `>`: a file with *more* CRLF than bare LF is CRLF-dominant. An
    // equal-count mixed file defaults to LF (the else branch in
    // `apply_line_ending`), matching the spec's "more `\r\n` than bare `\n`".
    let total_lf = content.matches('\n').count();
    let bare_lf = total_lf.saturating_sub(crlf);
    crlf > bare_lf
}

/// Find the (first) versioned fence carrying `label` in `content`.
///
/// Matching is performed on a CRLF-normalized copy; the returned `body` is
/// normalized to `\n`. Returns `None` when no opener line parses.
pub fn find_block(content: &str, label: &str) -> Option<ParsedBlock> {
    find_all_blocks(content, label).into_iter().next()
}

/// Find every versioned fence carrying `label` (used to collapse duplicates /
/// forward versions).
fn find_all_blocks(content: &str, label: &str) -> Vec<ParsedBlock> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let regex = opener_regex(label);
    let closer = closer_text(label);

    let mut blocks = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let Some(captures) = regex.captures(lines[index]) else {
            index += 1;
            continue;
        };
        let opener_line = index;
        let version: u32 = captures
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(CURRENT_VERSION);
        let marker = captures.get(2).map(|m| m.as_str().to_string());

        // Scan forward for the closer. A missing closer (truncated/edited file)
        // means the opener is not a real block — skip past it.
        let mut closer_line = None;
        let mut scan = opener_line + 1;
        while scan < lines.len() {
            if lines[scan].trim() == closer {
                closer_line = Some(scan);
                break;
            }
            scan += 1;
        }
        let Some(closer_line) = closer_line else {
            index = opener_line + 1;
            continue;
        };

        let body = lines[opener_line + 1..closer_line].join("\n");
        blocks.push(ParsedBlock {
            version,
            marker,
            body,
            opener_line,
            closer_line,
        });
        index = closer_line + 1;
    }
    blocks
}

/// Classify `content` against the block body this binary would write for `label`.
pub fn classify(content: &str, body: &str, label: &str) -> BlockState {
    let Some(block) = find_block(content, label) else {
        return BlockState::Fresh;
    };
    let actual = canonical_hash(&block.body);
    let canonical = canonical_hash(body);

    match &block.marker {
        // A marker that disagrees with the on-disk body = user edited it.
        Some(marker) if *marker != actual => BlockState::Dirty,
        // Marker matches body (ocx-authored). Current iff same version and the
        // body already equals what this binary would write.
        Some(_) | None if block.version == CURRENT_VERSION && actual == canonical => BlockState::Current,
        // ocx-authored but version or content differs → safe rewrite.
        _ => BlockState::FormatUpgraded,
    }
}

/// Apply the state machine. Returns the new file content, or `None` when no
/// change is needed (`Current`, or `Dirty` without `force`).
///
/// CRLF handling: matching runs on a normalized copy, but the rewritten file
/// preserves the original dominant line ending (Decision item 9b). All
/// ocx-versioned fences are collapsed to a single v1 block (item 9a).
///
/// # Errors
///
/// Currently infallible, but returns [`Error`] so future write-side validation
/// can surface without changing the signature.
pub fn apply(content: &str, body: &str, force: bool, label: &str) -> Result<Option<String>, Error> {
    let state = classify(content, body, label);
    let blocks = find_all_blocks(content, label);

    match state {
        BlockState::Current => Ok(None),
        BlockState::Dirty if !force => Ok(None),
        BlockState::Fresh => Ok(Some(append_block(content, body, label))),
        // FormatUpgraded, or Dirty + force: collapse every fence to one v1 block.
        _ => Ok(Some(rewrite_blocks(content, body, &blocks, label))),
    }
}

/// Append a fresh fenced block to a file that has no fence carrying `label`.
fn append_block(content: &str, body: &str, label: &str) -> String {
    let crlf = dominant_is_crlf(content);
    let block = RcBlock { body: body.to_string() }.render(label);

    let mut normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    // Separate the appended block with one blank line, matching install.sh's
    // leading `\n` before the fence. A brand-new empty file gets no leading gap.
    if normalized.is_empty() {
        normalized = block;
    } else {
        if !normalized.ends_with('\n') {
            normalized.push('\n');
        }
        normalized.push('\n');
        normalized.push_str(&block);
    }
    apply_line_ending(&normalized, crlf)
}

/// Collapse all fences carrying `label` to a single canonical v1 block written
/// in place of the first fence; later fences are removed entirely.
fn rewrite_blocks(content: &str, body: &str, blocks: &[ParsedBlock], label: &str) -> String {
    let crlf = dominant_is_crlf(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();
    let trailing_newline = normalized.ends_with('\n');

    let block_text = RcBlock { body: body.to_string() }.render(label);
    // `render()` always ends in `\n`; split into lines for re-interleaving.
    let block_lines: Vec<&str> = block_text.trim_end_matches('\n').lines().collect();

    let first_opener = blocks.first().map(|block| block.opener_line);
    let mut output: Vec<String> = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if let Some(block) = blocks.iter().find(|block| block.opener_line == index) {
            if Some(index) == first_opener {
                output.extend(block_lines.iter().map(|line| (*line).to_string()));
            }
            // Skip the entire fence span (opener..=closer) for every block.
            index = block.closer_line + 1;
            continue;
        }
        output.push(lines[index].to_string());
        index += 1;
    }

    let mut joined = output.join("\n");
    if trailing_newline {
        joined.push('\n');
    }
    apply_line_ending(&joined, crlf)
}

/// Convert an LF-normalized string to the requested dominant line ending.
fn apply_line_ending(normalized: &str, crlf: bool) -> String {
    if crlf {
        normalized.replace('\n', "\r\n")
    } else {
        normalized.to_string()
    }
}

/// Remove the ocx footprint from `content`: the v1 (and any `v\d+`) fence, the
/// legacy `# BEGIN ocx` / `# END ocx` block, `ocx shell init` dot-source lines,
/// and the extensionless `$OCX_HOME/env` references.
///
/// Ports the awk state machine at `install.sh:691-711` plus the legacy
/// block-strip at `install.sh:902` to a single Rust line-scan pass.
pub fn strip_block(content: &str) -> String {
    let crlf = dominant_is_crlf(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let trailing_newline = normalized.ends_with('\n');
    let lines: Vec<&str> = normalized.lines().collect();
    let regex = opener_regex(OCX_LABEL);

    let mut output: Vec<&str> = Vec::with_capacity(lines.len());
    let mut index = 0;
    // States for the legacy `# OCX` extensionless-env guard (install.sh:683).
    while index < lines.len() {
        let line = lines[index];

        // v1 / forward-version fence: drop opener..=closer.
        if regex.is_match(line) {
            let mut scan = index + 1;
            let mut closer = None;
            while scan < lines.len() {
                if lines[scan].trim() == CLOSER {
                    closer = Some(scan);
                    break;
                }
                scan += 1;
            }
            if let Some(closer) = closer {
                index = closer + 1;
                continue;
            }
        }

        // Legacy `# BEGIN ocx` / `# END ocx` block: drop BEGIN..=END.
        if line.trim() == "# BEGIN ocx" {
            let mut scan = index + 1;
            let mut end = None;
            while scan < lines.len() {
                if lines[scan].trim() == "# END ocx" {
                    end = Some(scan);
                    break;
                }
                scan += 1;
            }
            if let Some(end) = end {
                index = end + 1;
                continue;
            }
        }

        // Bare legacy dot-source lines: `. ".../.ocx/init.<shell>"` and
        // extensionless `. ".../.ocx/env"` (install.sh:707-708).
        if is_legacy_init_line(line) || is_legacy_env_line(line) {
            index += 1;
            continue;
        }

        output.push(line);
        index += 1;
    }

    let mut joined = output.join("\n");
    if trailing_newline && !joined.is_empty() {
        joined.push('\n');
    }
    apply_line_ending(&joined, crlf)
}

/// Removes the fence carrying `label` (if present) from `content`, leaving
/// everything else untouched. Returns `content` unchanged (byte-identical)
/// when no fence carrying `label` is found.
///
/// Unlike [`strip_block`] (hardcoded to [`OCX_LABEL`]'s legacy footprint),
/// this targets an arbitrary label's v1 (or forward-version) fence only.
/// Used by a second caller's clear/unadopt path (`ocx self setup
/// --managed-config ""`).
pub fn remove_block(content: &str, label: &str) -> String {
    let blocks = find_all_blocks(content, label);
    if blocks.is_empty() {
        return content.to_string();
    }

    let crlf = dominant_is_crlf(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let trailing_newline = normalized.ends_with('\n');
    let lines: Vec<&str> = normalized.lines().collect();

    let mut output: Vec<&str> = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if let Some(block) = blocks.iter().find(|block| block.opener_line == index) {
            index = block.closer_line + 1;
            continue;
        }
        output.push(lines[index]);
        index += 1;
    }

    let mut joined = output.join("\n");
    if trailing_newline && !joined.is_empty() {
        joined.push('\n');
    }
    apply_line_ending(&joined, crlf)
}

/// Whether `content` carries any pre-v1 (legacy) ocx footprint that
/// [`strip_block`] would remove: a `# BEGIN ocx` / `# END ocx` block, a legacy
/// `ocx shell init` dot-source line, or an extensionless `$OCX_HOME/env`
/// reference.
///
/// The orchestrator uses this to choose [`crate::setup::ProfileOutcome::Migrated`]
/// over `Completed`: a profile with a legacy footprint is stripped and rewritten
/// to the v1 fence (a migration), whereas a profile with only a v1 fence (or
/// none) takes the ordinary state-machine [`apply`] path. The v1-versioned fence
/// is intentionally NOT a legacy artifact — collapsing it is a format upgrade,
/// not a migration.
pub fn has_legacy_artifacts(content: &str) -> bool {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = normalized.lines().collect();

    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        // A `# BEGIN ocx` … `# END ocx` block is the canonical legacy marker.
        if line.trim() == "# BEGIN ocx" {
            return true;
        }
        if is_legacy_init_line(line) || is_legacy_env_line(line) {
            return true;
        }
        index += 1;
    }
    false
}

/// `^\s*\. .*\.ocx/init\.` — legacy `ocx shell init` dot-source line.
fn is_legacy_init_line(line: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\s*\. .*\.ocx/init\.").expect("legacy init regex is compile-time-valid"))
        .is_match(line)
}

/// `^\s*\. .*\.ocx/env"?\s*$` — extensionless `$OCX_HOME/env` dot-source line.
fn is_legacy_env_line(line: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"^\s*\. .*\.ocx/env"?\s*$"#).expect("legacy env regex is compile-time-valid"))
        .is_match(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSIX_BODY: &str = r#"if [ -f "${OCX_HOME:-$HOME/.ocx}/env.sh" ]; then
    . "${OCX_HOME:-$HOME/.ocx}/env.sh"
fi"#;
    const ELVISH_BODY: &str = r#"var _ocx_home = (if (has-env OCX_HOME) { put $E:OCX_HOME } else { put $E:HOME/.ocx })
if ?(test -f $_ocx_home/env.elv) {
    eval (slurp < $_ocx_home/env.elv)
}"#;
    const POWERSHELL_BODY: &str = r#"$_ocxHome = if ($env:OCX_HOME) { $env:OCX_HOME } elseif ($env:USERPROFILE) { Join-Path $env:USERPROFILE '.ocx' } else { Join-Path $HOME '.ocx' }
$_ocxEnv = Join-Path $_ocxHome 'env.ps1'
if (Test-Path $_ocxEnv) { . $_ocxEnv }"#;

    // ── canonical_hash ──────────────────────────────────────────────

    #[test]
    fn canonical_hash_is_eight_hex_chars() {
        let hash = canonical_hash(POSIX_BODY);
        assert_eq!(hash.len(), 8);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonical_hash_is_line_ending_agnostic() {
        let lf = "line one\nline two";
        let crlf = "line one\r\nline two";
        assert_eq!(canonical_hash(lf), canonical_hash(crlf));
    }

    #[test]
    fn canonical_hash_ignores_trailing_newline() {
        assert_eq!(canonical_hash(POSIX_BODY), canonical_hash(&format!("{POSIX_BODY}\n")));
    }

    #[test]
    fn canonical_hash_round_trips_through_apply() {
        // canonical_hash(body) == the marker emitted by apply on an empty file.
        let content = apply("", POSIX_BODY, false, OCX_LABEL).unwrap().expect("fresh append");
        let parsed = find_block(&content, OCX_LABEL).expect("block present after append");
        assert_eq!(parsed.marker.as_deref(), Some(canonical_hash(POSIX_BODY).as_str()));
        assert_eq!(parsed.body, POSIX_BODY);
        assert_eq!(parsed.version, CURRENT_VERSION);
    }

    // ── state machine truth table ───────────────────────────────────

    #[test]
    fn fresh_when_no_fence() {
        assert_eq!(classify("export PATH=/bin\n", POSIX_BODY, OCX_LABEL), BlockState::Fresh);
        let result = apply("export PATH=/bin\n", POSIX_BODY, false, OCX_LABEL).unwrap();
        let content = result.expect("fresh append");
        assert!(content.contains("# >>> ocx v1"));
        assert!(content.contains(POSIX_BODY));
        assert!(content.contains(CLOSER));
        assert!(content.starts_with("export PATH=/bin\n"));
    }

    #[test]
    fn current_when_body_matches_canonical() {
        let content = apply("", POSIX_BODY, false, OCX_LABEL).unwrap().unwrap();
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::Current);
        assert_eq!(apply(&content, POSIX_BODY, false, OCX_LABEL).unwrap(), None);
    }

    #[test]
    fn format_upgraded_when_marker_matches_body_but_version_differs() {
        // A v2 fence authored by a newer binary; body hash matches its marker.
        let hash = canonical_hash(POSIX_BODY);
        let content = format!("# >>> ocx v2 {hash} >>>\n{POSIX_BODY}\n{CLOSER}\n");
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::FormatUpgraded);
        let rewritten = apply(&content, POSIX_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("downgrade rewrite");
        let parsed = find_block(&rewritten, OCX_LABEL).expect("rewritten block");
        assert_eq!(parsed.version, CURRENT_VERSION);
        assert_eq!(parsed.marker.as_deref(), Some(hash.as_str()));
    }

    #[test]
    fn format_upgraded_when_old_canonical_hash_differs() {
        // v1 fence, marker matches the on-disk body, but the body differs from
        // what this binary now writes (a prior-version payload).
        let old_body = ". \"$OCX_HOME/old-env.sh\"";
        let hash = canonical_hash(old_body);
        let content = format!("# >>> ocx v1 {hash} >>>\n{old_body}\n{CLOSER}\n");
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::FormatUpgraded);
        let rewritten = apply(&content, POSIX_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("upgrade rewrite");
        let parsed = find_block(&rewritten, OCX_LABEL).unwrap();
        assert_eq!(parsed.body, POSIX_BODY);
        assert_eq!(parsed.marker.as_deref(), Some(canonical_hash(POSIX_BODY).as_str()));
    }

    #[test]
    fn dirty_when_body_hash_disagrees_with_marker() {
        // Marker claims one hash; the body was edited so its actual hash differs.
        let marker = canonical_hash(POSIX_BODY);
        let edited_body = ". \"$OCX_HOME/env.sh\"\nexport HACKED=1";
        let content = format!("# >>> ocx v1 {marker} >>>\n{edited_body}\n{CLOSER}\n");
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::Dirty);
        // Without force: no change.
        assert_eq!(apply(&content, POSIX_BODY, false, OCX_LABEL).unwrap(), None);
        // With force: rewrite to canonical.
        let forced = apply(&content, POSIX_BODY, true, OCX_LABEL)
            .unwrap()
            .expect("force rewrite");
        let parsed = find_block(&forced, OCX_LABEL).unwrap();
        assert_eq!(parsed.body, POSIX_BODY);
        assert_eq!(parsed.marker.as_deref(), Some(canonical_hash(POSIX_BODY).as_str()));
    }

    #[test]
    fn no_marker_opener_with_canonical_body_is_current() {
        // A hashless opener (regex captures the hash as optional, so None marker)
        // whose body already matches what this binary writes → Current (no-op).
        // Corresponds to truth-table row: canonical=  marker=absent  actual= → Current.
        let content = format!("# >>> ocx v1 >>>\n{POSIX_BODY}\n{CLOSER}\n");
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::Current);
        // No write needed.
        assert_eq!(apply(&content, POSIX_BODY, false, OCX_LABEL).unwrap(), None);
    }

    #[test]
    fn no_marker_opener_with_different_body_is_format_upgraded() {
        // A hashless opener whose body does NOT match the canonical payload →
        // FormatUpgraded: body was not user-edited (no marker to compare against),
        // so silent rewrite is the safe action.
        // Corresponds to truth-table row: canonical≠  marker=absent  actual≠canonical → FormatUpgraded.
        let old_body = r#". "$OCX_HOME/old-env.sh""#;
        let content = format!("# >>> ocx v1 >>>\n{old_body}\n{CLOSER}\n");
        assert_eq!(classify(&content, POSIX_BODY, OCX_LABEL), BlockState::FormatUpgraded);
        let rewritten = apply(&content, POSIX_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("upgrade rewrite");
        let parsed = find_block(&rewritten, OCX_LABEL).unwrap();
        assert_eq!(parsed.body, POSIX_BODY);
        // After rewrite the opener carries the canonical hash.
        assert_eq!(parsed.marker.as_deref(), Some(canonical_hash(POSIX_BODY).as_str()));
    }

    // ── CRLF preservation (item 9b) ─────────────────────────────────

    #[test]
    fn crlf_file_stays_crlf_after_append() {
        let content = "Set-Item env:FOO bar\r\nWrite-Host hi\r\n";
        let result = apply(content, POWERSHELL_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("append");
        assert!(result.contains("\r\n"), "CRLF must be preserved");
        assert!(!result.contains("\n\n\n"), "no triple newline collapse artifacts");
        // No bare LF should remain (every newline is part of a CRLF).
        assert_eq!(result.matches('\n').count(), result.matches("\r\n").count());
        assert!(result.contains("# >>> ocx v1"));
    }

    #[test]
    fn lf_file_stays_lf_after_append() {
        let content = "export PATH=/bin\n";
        let result = apply(content, POSIX_BODY, false, OCX_LABEL).unwrap().expect("append");
        assert!(!result.contains('\r'), "LF file must not gain CR");
    }

    #[test]
    fn dominant_is_crlf_strict_tiebreak_on_equal_counts() {
        // Equal counts of `\r\n` and bare `\n` must NOT be classified CRLF: the
        // spec is "more `\r\n` than bare `\n`" (strict `>`), so an equal-count
        // mixed file defaults to LF on write-back.
        let content = "a\r\nb\nc\r\nd\n";
        assert_eq!(content.matches("\r\n").count(), 2);
        assert_eq!(content.matches('\n').count() - content.matches("\r\n").count(), 2);
        assert!(
            !dominant_is_crlf(content),
            "equal CRLF/LF counts must not be CRLF-dominant"
        );
    }

    #[test]
    fn dominant_is_crlf_true_when_crlf_outnumbers_bare_lf() {
        let content = "a\r\nb\r\nc\n";
        assert!(
            dominant_is_crlf(content),
            "more CRLF than bare LF must be CRLF-dominant"
        );
    }

    #[test]
    fn crlf_preserved_on_rewrite() {
        let hash = canonical_hash("old");
        let content = format!("a\r\n# >>> ocx v1 {hash} >>>\r\nold\r\n{CLOSER}\r\nb\r\n");
        let rewritten = apply(&content, POSIX_BODY, false, OCX_LABEL).unwrap().expect("rewrite");
        assert!(rewritten.contains("\r\n"));
        assert_eq!(rewritten.matches('\n').count(), rewritten.matches("\r\n").count());
        // The multi-line body is re-emitted with the file's dominant CRLF endings,
        // so its internal `\n` separators become `\r\n` — assert the CRLF form.
        assert!(rewritten.contains(&POSIX_BODY.replace('\n', "\r\n")));
    }

    // ── duplicate / forward-version collapse (item 9a) ──────────────

    #[test]
    fn duplicate_fences_collapse_to_one() {
        let hash = canonical_hash(POSIX_BODY);
        let content = format!(
            "header\n# >>> ocx v1 {hash} >>>\n{POSIX_BODY}\n{CLOSER}\nmiddle\n# >>> ocx v1 {hash} >>>\n{POSIX_BODY}\n{CLOSER}\nfooter\n"
        );
        // A different body forces a rewrite path so the collapse is exercised.
        let new_body = ". \"$OCX_HOME/env.sh\" # changed";
        let rewritten = apply(&content, new_body, false, OCX_LABEL)
            .unwrap()
            .expect("collapse rewrite");
        assert_eq!(
            find_all_blocks(&rewritten, OCX_LABEL).len(),
            1,
            "must collapse to a single block"
        );
        assert!(rewritten.contains("header"));
        assert!(rewritten.contains("middle"));
        assert!(rewritten.contains("footer"));
    }

    #[test]
    fn forward_version_and_v1_collapse_to_single_v1() {
        let v1_hash = canonical_hash("a");
        let v2_hash = canonical_hash("b");
        let content = format!("# >>> ocx v1 {v1_hash} >>>\na\n{CLOSER}\n# >>> ocx v2 {v2_hash} >>>\nb\n{CLOSER}\n");
        let rewritten = apply(&content, POSIX_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("collapse");
        let blocks = find_all_blocks(&rewritten, OCX_LABEL);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].version, CURRENT_VERSION);
        assert_eq!(blocks[0].body, POSIX_BODY);
    }

    // ── strip_block legacy cases ────────────────────────────────────

    #[test]
    fn strip_removes_legacy_begin_end_block() {
        let content = "before\n# BEGIN ocx\n. \"$HOME/.ocx/env.sh\"\n# END ocx\nafter\n";
        let stripped = strip_block(content);
        assert!(!stripped.contains("# BEGIN ocx"));
        assert!(!stripped.contains("# END ocx"));
        assert!(!stripped.contains("env.sh"));
        assert!(stripped.contains("before"));
        assert!(stripped.contains("after"));
    }

    #[test]
    fn strip_removes_shell_init_dot_source() {
        let content = "x\n. \"$HOME/.ocx/init.sh\"\ny\n";
        let stripped = strip_block(content);
        assert!(!stripped.contains(".ocx/init."));
        assert!(stripped.contains('x'));
        assert!(stripped.contains('y'));
    }

    #[test]
    fn strip_removes_extensionless_env_line() {
        let content = "a\n. \"$HOME/.ocx/env\"\nb\n";
        let stripped = strip_block(content);
        assert!(!stripped.contains(".ocx/env\""));
        assert!(stripped.contains('a'));
        assert!(stripped.contains('b'));
    }

    #[test]
    fn strip_removes_v1_fence() {
        let content = apply("keep\n", POSIX_BODY, false, OCX_LABEL).unwrap().unwrap();
        let stripped = strip_block(&content);
        assert!(!stripped.contains("# >>> ocx"));
        assert!(!stripped.contains(CLOSER));
        assert!(stripped.contains("keep"));
    }

    #[test]
    fn strip_handles_mixed_legacy_and_v1() {
        let fence_hash = canonical_hash(POSIX_BODY);
        let content = format!(
            "top\n# BEGIN ocx\n. \"$HOME/.ocx/env.sh\"\n# END ocx\nmid\n# >>> ocx v1 {fence_hash} >>>\n{POSIX_BODY}\n{CLOSER}\nbottom\n"
        );
        let stripped = strip_block(&content);
        assert!(!stripped.contains("# BEGIN ocx"));
        assert!(!stripped.contains("# >>> ocx"));
        assert!(stripped.contains("top"));
        assert!(stripped.contains("mid"));
        assert!(stripped.contains("bottom"));
    }

    #[test]
    fn strip_preserves_crlf() {
        let content = "a\r\n# BEGIN ocx\n. \"$HOME/.ocx/env.sh\"\r\n# END ocx\r\nb\r\n";
        let stripped = strip_block(content);
        assert!(stripped.contains("\r\n"));
        assert!(!stripped.contains("# BEGIN ocx"));
    }

    // ── has_legacy_artifacts (migration trigger) ────────────────────

    #[test]
    fn has_legacy_detects_begin_end_block() {
        let content = "before\n# BEGIN ocx\n. \"$HOME/.ocx/env.sh\"\n# END ocx\nafter\n";
        assert!(has_legacy_artifacts(content));
    }

    #[test]
    fn has_legacy_detects_shell_init_and_extensionless_env() {
        assert!(has_legacy_artifacts(". \"$HOME/.ocx/init.bash\"\n"));
        assert!(has_legacy_artifacts(". \"$HOME/.ocx/env\"\n"));
    }

    #[test]
    fn has_legacy_ignores_a_clean_v1_fence() {
        // A v1 fence is a format-upgrade surface, not a legacy artifact: it must
        // NOT trigger migration.
        let content = apply("", POSIX_BODY, false, OCX_LABEL).unwrap().unwrap();
        assert!(!has_legacy_artifacts(&content));
    }

    #[test]
    fn has_legacy_false_for_plain_profile() {
        assert!(!has_legacy_artifacts("export PATH=/usr/bin\n"));
    }

    #[test]
    fn guarded_posix_body_is_not_matched_by_legacy_strippers() {
        // Regression guard for the `$OCX_HOME`-unset fence fix: the new guarded
        // POSIX body sources `${OCX_HOME:-$HOME/.ocx}/env.sh`, which must NOT
        // collide with the legacy `\.ocx/env"?$` / `\.ocx/init\.` strippers — the
        // `}` between `.ocx` and `/env.sh` breaks the `\.ocx/env` literal. If it
        // did match, every clean re-run would be misclassified as a legacy
        // migration instead of a no-op.
        for line in POSIX_BODY.lines() {
            assert!(
                !is_legacy_env_line(line),
                "guarded POSIX body line must not look like a legacy env source: {line:?}"
            );
            assert!(
                !is_legacy_init_line(line),
                "guarded POSIX body line must not look like a legacy init source: {line:?}"
            );
        }
        // A profile carrying the freshly written guarded fence is not legacy.
        let content = apply("", POSIX_BODY, false, OCX_LABEL).unwrap().unwrap();
        assert!(
            !has_legacy_artifacts(&content),
            "the guarded v1 fence must not trigger legacy migration"
        );
    }

    // ── reflowed-opener degradation ─────────────────────────────────

    #[test]
    fn reflowed_opener_treated_as_no_block() {
        // A user-mangled opener that the regex cannot parse → no block found →
        // classify Fresh → apply appends a fresh block (documented degradation).
        let content = "# >>> ocx (v1) a1b2c3d4 >>>\n. \"$OCX_HOME/env.sh\"\n# <<< ocx <<<\n";
        assert_eq!(classify(content, POSIX_BODY, OCX_LABEL), BlockState::Fresh);
        let result = apply(content, POSIX_BODY, false, OCX_LABEL)
            .unwrap()
            .expect("fresh append");
        // The original (unparsed) lines survive; a new fence is appended.
        assert!(result.contains("# >>> ocx (v1) a1b2c3d4 >>>"));
        assert!(result.contains("# >>> ocx v1 "));
    }

    // ── elvish + PowerShell body shape ──────────────────────────────

    #[test]
    fn elvish_body_round_trips() {
        let content = apply("", ELVISH_BODY, false, OCX_LABEL).unwrap().unwrap();
        let parsed = find_block(&content, OCX_LABEL).unwrap();
        assert_eq!(parsed.body, ELVISH_BODY);
        assert!(content.contains("eval (slurp < $_ocx_home/env.elv)"));
        assert!(content.contains("has-env OCX_HOME"));
        assert_eq!(classify(&content, ELVISH_BODY, OCX_LABEL), BlockState::Current);
    }

    #[test]
    fn powershell_body_multiline_round_trips() {
        let content = apply("", POWERSHELL_BODY, false, OCX_LABEL).unwrap().unwrap();
        let parsed = find_block(&content, OCX_LABEL).unwrap();
        assert_eq!(parsed.body, POWERSHELL_BODY);
        // Honors $env:OCX_HOME with a USERPROFILE fallback (no hardcoded path).
        assert!(parsed.body.contains("$env:OCX_HOME"));
        assert!(parsed.body.contains("$env:USERPROFILE"));
        assert_eq!(classify(&content, POWERSHELL_BODY, OCX_LABEL), BlockState::Current);
    }

    #[test]
    fn powershell_body_dirty_detection_survives_multiline() {
        let content = apply("", POWERSHELL_BODY, false, OCX_LABEL).unwrap().unwrap();
        // Append a user edit inside the fence.
        let parsed = find_block(&content, OCX_LABEL).unwrap();
        let tampered = content.replace(&parsed.body, &format!("{POWERSHELL_BODY}\nWrite-Host injected"));
        assert_eq!(classify(&tampered, POWERSHELL_BODY, OCX_LABEL), BlockState::Dirty);
    }

    // ── Managed-config second caller (generalized fence label) ──────────────
    //
    // The Implement phase wires `ocx self setup --managed-config` onto this
    // same fence machine with its own label (e.g. "ocx managed"). These tests
    // exercise the ALREADY-implemented, label-parameterized primitives
    // (`apply`/`classify`/`find_block`) plus `ManagedConfig`'s already-derived
    // `Serialize` with a rendered body, proving the mechanism the Implement
    // phase's `apply_managed_config` will rely on. They pass today — they are
    // regression guards for the wiring, not stub probes.

    /// Criterion 24 (Block-tier CWE-74 fix): a hostile `[managed].source`
    /// value must never escape its quoted TOML string context when the fence
    /// body is produced by REAL TOML serialization of the typed
    /// `ManagedConfig` struct — never `format!` interpolation of the raw ref
    /// (ADR Decision C).
    #[test]
    fn managed_config_fence_body_is_toml_injection_safe() {
        use crate::config::managed::ManagedConfig;

        let hostile = "evil\"\n[patches]\nregistry = \"hijacked.example.com\"\n# ";
        let managed = ManagedConfig {
            source: Some(hostile.to_string()),
            required: Some(true),
            refresh: Some(crate::config::managed::RefreshPolicy::Notify),
            interval: Some("1d".to_string()),
            system_locked: false,
        };
        // Real TOML serialization, never `format!` interpolation of the ref.
        let body = format!(
            "[managed]\n{}",
            toml::to_string(&managed).expect("ManagedConfig must serialize to TOML")
        );

        let content = apply("", &body, false, MANAGED_LABEL)
            .expect("apply must not error")
            .expect("fresh append produces content");

        let parsed = find_block(&content, MANAGED_LABEL).expect("fenced block must be found");
        // Re-parse the fenced body as a Config: the hostile string must
        // round-trip EXACTLY as the `source` value, never break out into a
        // sibling `[patches]` section or a stray repeated `[managed]` table.
        let reparsed: crate::config::Config =
            toml::from_str(&parsed.body).expect("the fence body must remain valid, well-formed TOML");
        let reparsed_managed = reparsed.managed.expect("[managed] section must survive re-parse");
        assert_eq!(
            reparsed_managed.source.as_deref(),
            Some(hostile),
            "the hostile ref must round-trip byte-for-byte as an inert string value"
        );
        assert!(
            reparsed.patches.is_none(),
            "the embedded '[patches]' text must stay inert inside the quoted source string, \
             never parse as a live sibling section"
        );
    }

    /// Criterion 3 shape: re-running `self setup --managed-config <same-ref>`
    /// renders the identical body — the fence classifies Current and is not
    /// rewritten (no re-fetch, no-op).
    #[test]
    fn managed_config_fence_reapply_same_source_is_current_noop() {
        use crate::config::managed::ManagedConfig;

        let managed = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            required: Some(true),
            refresh: Some(crate::config::managed::RefreshPolicy::Notify),
            interval: Some("1d".to_string()),
            system_locked: false,
        };
        let body = format!("[managed]\n{}", toml::to_string(&managed).unwrap());

        let first = apply("", &body, false, MANAGED_LABEL).unwrap().unwrap();
        assert_eq!(
            classify(&first, &body, MANAGED_LABEL),
            BlockState::Current,
            "re-applying the identical rendered body must classify Current"
        );
        assert_eq!(
            apply(&first, &body, false, MANAGED_LABEL).unwrap(),
            None,
            "a Current fence must not be rewritten"
        );
    }

    /// Criterion 4 shape: a different `--managed-config` ref re-adopts —
    /// the fence is rewritten to the new rendered body.
    #[test]
    fn managed_config_fence_reapply_different_source_is_re_adopted() {
        use crate::config::managed::ManagedConfig;

        let old = ManagedConfig {
            source: Some("old.example.com/ocx-config:user".to_string()),
            required: Some(true),
            refresh: Some(crate::config::managed::RefreshPolicy::Notify),
            interval: Some("1d".to_string()),
            system_locked: false,
        };
        let new = ManagedConfig {
            source: Some("new.example.com/ocx-config:user".to_string()),
            required: Some(true),
            refresh: Some(crate::config::managed::RefreshPolicy::Notify),
            interval: Some("1d".to_string()),
            system_locked: false,
        };
        let old_body = format!("[managed]\n{}", toml::to_string(&old).unwrap());
        let new_body = format!("[managed]\n{}", toml::to_string(&new).unwrap());

        let first = apply("", &old_body, false, MANAGED_LABEL).unwrap().unwrap();
        let rewritten = apply(&first, &new_body, false, MANAGED_LABEL)
            .unwrap()
            .expect("a different source must rewrite the fence (re-adopt)");
        let parsed = find_block(&rewritten, MANAGED_LABEL).unwrap();
        assert!(parsed.body.contains("new.example.com"));
        assert!(!parsed.body.contains("old.example.com"));
    }

    /// Criterion 5 shape: a dirty fence (user-edited body) is left untouched
    /// without `--force`; `--force` overwrites to the canonical body. The CLI
    /// layer maps `SkippedDirty` to exit 82 — this test proves the underlying
    /// mechanism only.
    #[test]
    fn managed_config_fence_dirty_edit_is_skipped_unless_forced() {
        use crate::config::managed::ManagedConfig;

        let managed = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            ..ManagedConfig::default()
        };
        let body = format!("[managed]\n{}", toml::to_string(&managed).unwrap());
        let marker = canonical_hash(&body);
        let dirty =
            format!("# >>> {MANAGED_LABEL} v1 {marker} >>>\nsource = \"tampered\"\n# <<< {MANAGED_LABEL} <<<\n");

        assert_eq!(classify(&dirty, &body, MANAGED_LABEL), BlockState::Dirty);
        assert_eq!(apply(&dirty, &body, false, MANAGED_LABEL).unwrap(), None);
        let forced = apply(&dirty, &body, true, MANAGED_LABEL)
            .unwrap()
            .expect("--force rewrites a dirty fence");
        assert!(forced.contains("corp.example.com"));
        assert!(!forced.contains("tampered"));
    }

    /// `remove_block` (the `--managed-config ""` clear path) deletes the
    /// fence carrying `label` and leaves the rest of the file untouched.
    #[test]
    fn remove_block_deletes_labeled_fence_leaves_rest_untouched() {
        use crate::config::managed::ManagedConfig;

        let managed = ManagedConfig {
            source: Some("corp.example.com/ocx-config:user".to_string()),
            ..ManagedConfig::default()
        };
        let body = format!("[managed]\n{}", toml::to_string(&managed).unwrap());
        let with_block = apply("[registry]\ndefault = \"x\"\n", &body, false, MANAGED_LABEL)
            .unwrap()
            .unwrap();

        let removed = remove_block(&with_block, MANAGED_LABEL);
        assert!(!removed.contains("[managed]"), "the managed fence must be gone");
        assert!(!removed.contains(MANAGED_LABEL), "no trace of the label must remain");
        assert!(
            removed.contains("[registry]\ndefault = \"x\""),
            "unrelated content must survive untouched"
        );
    }

    /// `remove_block` on content with no fence for `label` is a byte-identical no-op.
    #[test]
    fn remove_block_absent_fence_is_noop() {
        let content = "[registry]\ndefault = \"x\"\n";
        assert_eq!(remove_block(content, MANAGED_LABEL), content);
    }
}
