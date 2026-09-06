// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Rendering the merge-request push options a `git push` carries, and refusing
//! the values that must never reach them.
//!
//! Deliberately pure — no workspace, no subprocess, no network — because this is
//! the value that reaches an external, security-sensitive parser and it is worth
//! reading on its own. Exactly four option keys are ever sent
//! (`merge_request.create`, `.target`, `.title`, `.description`), asserted as a
//! sorted key **set** rather than a count: swapping `.description` for
//! `merge_request.merge_when_pipeline_succeeds` keeps the count at four while
//! auto-merging the request and defeating the one governance control this path
//! exists to respect.
//!
//! **ocx is the only gate for almost every byte class.** git's documented wire
//! grammar is `push-option = 1*( VCHAR | SP )`, but git 2.54.0 does not enforce
//! it: measured on this host, only a raw LF is refused
//! (`fatal: push options must not have new line characters`). CR, TAB, BEL, ESC,
//! DEL, every other C0/C1 control, all non-ASCII, U+202E, U+200B and outright
//! invalid UTF-8 travel to `GIT_PUSH_OPTION_<n>` byte-for-byte. NUL is stopped
//! one layer lower still, by `execve` rather than by git. So the guard below is
//! not a belt on git's braces — it is the whole belt.
//!
//! The rule is an **allowlist**: a rendered option string may contain only bytes
//! `0x20..=0x7E`, minus `;`. A denylist of "`;` plus control characters" is the
//! exact shape CVE-2026-3854 defeated, where a `;` injected `rails_env`,
//! `custom_hooks_dir` and `repo_pre_receive_hooks` through GHES's
//! semicolon-delimited internal `X-Stat` header and reached RCE. And
//! `char::is_control()` alone is Unicode `Cc`, which misses U+202E, U+200B and
//! every non-ASCII byte — the half of the class the CVE lives in.
//!
//! `=`, `:`, `/` and `,` are **allowed and must stay allowed**: GitLab's own
//! matcher takes everything after the first `=` as the value, and C-067's values
//! carry `:` and `/` in the physical repository (`oci://host/path`) and `:` and
//! `,` in the resolved `login:id` owner pairs. Refusing them breaks the feature.

use super::ForgeError;

/// The pkt-line ceiling for one whole `key=value` option string, in bytes.
///
/// `LARGE_PACKET_MAX` (65520) minus the 4-byte length header. Measured on git
/// 2.54.0: at 65516 the option arrives whole, at 65517 git dies with
/// `fatal: protocol error: impossibly long line` before the receive hook runs.
const WIRE_OPTION_CEILING: usize = 65516;

/// Render the four merge-request push options a claim or announce push carries.
///
/// Returns them **in send order** as `key` / `key=value` strings. Order is part
/// of the contract, not an implementation detail: git preserves send order
/// exactly (measured), and the fixture asserts the delivered options as an
/// ordered list, so an unordered collection would make the acceptance test
/// depend on a hash seed.
///
/// Every value is checked against the module's allowlist *before* it is rendered,
/// so nothing outside `0x20..=0x7E` minus `;` ever reaches a pkt-line.
///
/// # Errors
///
/// Returns [`ForgeError::PushOptionRefused`] when a value is empty or
/// whitespace-only, carries a byte outside the allowlist, or would push its
/// whole `key=value` string past the pkt-line ceiling.
pub fn render_push_options(target: &str, title: &str, description: &str) -> Result<Vec<String>, ForgeError> {
    // `merge_request.create` is rendered bare: git delivers a valueless option
    // as-is, and the wave-1 fixture asserts that exact spelling on the wire.
    Ok(vec![
        "merge_request.create".to_string(),
        render_option("merge_request.target", target)?,
        render_option("merge_request.title", title)?,
        render_option("merge_request.description", description)?,
    ])
}

/// Escape a push-option value's newlines into the two-character `\n` sequence
/// the server converts back.
///
/// GitLab documents it: *"To include newlines in push option values (for
/// example, in a merge request description), use the `\n` escape sequence
/// instead of a literal newline"* (`docs.gitlab.com/topics/git/commit/`,
/// implemented by `gitlab-org/gitlab!87020`), and performs the conversion at
/// parse time, before the description is stored. So a multi-line markdown body
/// travels as printable ASCII and no literal LF ever reaches a pkt-line — which
/// is the only reason a claim body can be sent at all: git itself dies with
/// `fatal: push options must not have new line characters`, and
/// [`render_option`] refuses the byte one layer earlier.
///
/// Applied at the construction site rather than inside [`render_push_options`],
/// which stays a pure refusing function: every byte this produces still goes
/// through the allowlist, and a CR, a TAB or a `;` in the same value is refused
/// exactly as before.
///
/// **Version floor, so nobody re-derives it.** An instance predating the
/// !87020 release (2022-05) stores the literal `\n` rather than converting it —
/// a cosmetic degradation, never a failure. That floor sits far below the
/// GitLab version the job-token preflight already requires
/// (`ci_push_repository_for_job_token_allowed`, GitLab 17), so it is a note and
/// not a gate.
///
/// ponytail: `\n` only — the backslash is deliberately **not** doubled. Every
/// value that reaches here is a fixed template over structured values (C-067):
/// the logical name and the physical repository from the identifier grammar,
/// the branch from `claim_branch`, `login:id` pairs from a charset-guarded
/// login plus a numeric id, and an enum word. None of them can carry a
/// backslash, so there is nothing to double. Interpolating an operator-supplied
/// string into a title or a body breaks that premise — a `\` the operator wrote
/// before an `n` would then be read by GitLab as a newline they did not write —
/// and the fix at that point is to escape the backslash here first, not to drop
/// the escape.
pub fn escape_newlines(value: &str) -> String {
    value.replace('\n', "\\n")
}

/// Render one `key=value` option, refusing any value the git wire must not carry.
fn render_option(key: &'static str, value: &str) -> Result<String, ForgeError> {
    let refuse = |reason: String| ForgeError::PushOptionRefused { key, reason };

    if value.trim().is_empty() {
        // git accepts `merge_request.title=` and GitLab then titles the request
        // from the commit subject instead of from the caller's template — a
        // silent loss of the contract with no error raised anywhere.
        return Err(refuse("the value is empty or whitespace only".to_string()));
    }

    // An allowlist, never a denylist: `;` is excluded because CVE-2026-3854
    // injected `rails_env`, `custom_hooks_dir` and `repo_pre_receive_hooks`
    // through a semicolon-delimited internal header and reached RCE, and a
    // denylist of "`;` plus control characters" is the shape that CVE defeated.
    if let Some((offset, character)) = value
        .char_indices()
        .find(|&(_, character)| !matches!(character, ' '..='~') || character == ';')
    {
        return Err(refuse(format!(
            "byte {offset} is U+{:04X}, outside the printable ASCII the wire carries",
            u32::from(character)
        )));
    }

    let option = format!("{key}={value}");
    if option.len() > WIRE_OPTION_CEILING {
        return Err(refuse(format!(
            "the option is {} bytes, past the {WIRE_OPTION_CEILING}-byte pkt-line ceiling",
            option.len()
        )));
    }
    Ok(option)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The values the wave-1 fixture pushes, so this module and
    /// `test/tests/test_git_http_fixture.py` agree on one rendering byte for byte.
    const TARGET: &str = "main";
    const TITLE: &str = "Claim acme/widget";
    const DESCRIPTION: &str = "Claim opened by the ocx test fixture.";

    /// The option key that asks the server to open a merge request.
    ///
    /// Rendered **bare, with no `=`** — the wave-1 fixture sends it that way
    /// (`test/tests/test_git_http_fixture.py`, `CLAIM_PUSH_OPTIONS`) and git
    /// delivers it bare to the receive hook. One of the four options therefore
    /// carries no value at all, which every per-value guard must tolerate rather
    /// than assume a `key=value` split.
    const CREATE_KEY: &str = "merge_request.create";
    /// The option key naming the branch the request targets.
    const TARGET_KEY: &str = "merge_request.target";
    /// The option key carrying the request title.
    const TITLE_KEY: &str = "merge_request.title";
    /// The option key carrying the request body.
    const DESCRIPTION_KEY: &str = "merge_request.description";

    /// The exact option list, in send order, that `CLAIM_PUSH_OPTIONS` asserts on
    /// the wire. Written out rather than assembled from the key constants: an
    /// expectation built by the same concatenation the renderer uses would agree
    /// with any concatenation bug.
    const EXPECTED_OPTIONS: [&str; 4] = [
        "merge_request.create",
        "merge_request.target=main",
        "merge_request.title=Claim acme/widget",
        "merge_request.description=Claim opened by the ocx test fixture.",
    ];

    /// The two spellings C-039 forbids by name. `merge_when_pipeline_succeeds`
    /// auto-merges the request, defeating G-04 — the single governance control
    /// this command exists to respect; `remove_source_branch` destroys the branch
    /// a reviewer would need to re-read.
    const FORBIDDEN_SPELLINGS: [&str; 2] = [
        "merge_request.merge_when_pipeline_succeeds",
        "merge_request.remove_source_branch",
    ];

    /// The three option keys that carry a value.
    ///
    /// Every per-value guard is driven through all three: a guard written for
    /// `.title` alone leaves the target branch and the body ungated, and a
    /// single-key test cannot tell the two apart.
    const VALUED_KEYS: [&str; 3] = [TARGET_KEY, TITLE_KEY, DESCRIPTION_KEY];

    /// The pkt-line ceiling for one whole `key=value` string, in bytes.
    ///
    /// `LARGE_PACKET_MAX` (65520) minus the 4-byte length header. Measured, not
    /// derived: at 65516 the option arrives whole, and at 65517 git dies with
    /// `fatal: protocol error: impossibly long line` before the hook runs.
    /// Deliberately a literal here rather than a constant read from the renderer
    /// — an expectation that reads the implementation's own number agrees with it
    /// in every state.
    const WIRE_OPTION_CEILING: usize = 65516;

    /// Render with `value` in the slot `key` names, leaving the other two at their
    /// fixture defaults so exactly one thing varies per case.
    fn render_with(key: &str, value: &str) -> Result<Vec<String>, ForgeError> {
        match key {
            TARGET_KEY => render_push_options(value, TITLE, DESCRIPTION),
            TITLE_KEY => render_push_options(TARGET, value, DESCRIPTION),
            DESCRIPTION_KEY => render_push_options(TARGET, TITLE, value),
            other => panic!("no renderer argument corresponds to {other}"),
        }
    }

    /// Assert `value` is refused in `key`'s slot, with `case` naming the byte
    /// class so a table row that fails identifies itself.
    fn assert_refused(key: &str, value: &str, case: &str) {
        match render_with(key, value) {
            Err(ForgeError::PushOptionRefused { key: refused, .. }) => {
                assert_eq!(refused, key, "{case}: the refusal must name the option it refused");
            }
            Err(other) => panic!("{case}: expected PushOptionRefused for {key}, got {other}"),
            Ok(options) => panic!("{case}: {key} accepted a value it must refuse, rendering {options:?}"),
        }
    }

    /// Assert `value` is accepted in `key`'s slot and survives byte for byte.
    ///
    /// The permissive half of the guard. Without it an implementation that
    /// refuses everything passes every refusal test in this module.
    fn assert_accepted(key: &str, value: &str, case: &str) {
        let options = match render_with(key, value) {
            Ok(options) => options,
            Err(error) => panic!("{case}: {key} refused a value it must accept: {error}"),
        };
        let expected = format!("{key}={value}");
        assert!(
            options.contains(&expected),
            "{case}: {key} must render {expected:?} unaltered, got {options:?}"
        );
    }

    /// C-039's key set, its send order, the bare-key form and the absence of
    /// duplicates, all pinned by one assertion on the exact `Vec`.
    ///
    /// The `Vec` comes first and the sorted key set is derived **from that same
    /// vec**, so the two halves cannot cover different renderings. A set-only
    /// assertion would survive four separate defects: a reordering (which the
    /// fixture asserts positionally), `merge_request.create` rendered as
    /// `merge_request.create=true` instead of bare, a duplicated key (git
    /// delivers both, and the server's handling of a duplicate is the server's
    /// choice), and a value mangled in place.
    ///
    /// Reds on two independent mutations, which must both be run:
    /// - **Addition** — push a fifth option (`merge_request.squash=true`). The
    ///   `assert_eq!` on the set reds; an `is_subset` assertion would not, which
    ///   is why this is an equality.
    /// - **Substitution** — render `merge_request.merge_when_pipeline_succeeds`
    ///   in place of the `.description` literal. The count stays four and every
    ///   forbidden-key assertion elsewhere stays green; only the set equality
    ///   catches it.
    ///
    /// A third, a **dropped** key, reds here too and nowhere else: a degenerate
    /// renderer returning `vec![]` satisfies every "forbidden spelling absent"
    /// assertion in this module.
    #[test]
    fn push_options_render_exactly_the_four_keys() {
        let options = render_push_options(TARGET, TITLE, DESCRIPTION).expect("the fixture's values must render");

        assert_eq!(
            options, EXPECTED_OPTIONS,
            "the rendered options must match the fixture's CLAIM_PUSH_OPTIONS in order and byte for byte"
        );

        let keys: BTreeSet<&str> = options
            .iter()
            .map(|option| option.split_once('=').map_or(option.as_str(), |(key, _)| key))
            .collect();
        let expected_keys: BTreeSet<&str> = BTreeSet::from([CREATE_KEY, TARGET_KEY, TITLE_KEY, DESCRIPTION_KEY]);
        assert_eq!(
            keys, expected_keys,
            "C-039 names these four keys and no others; a count of four is not the assertion"
        );
    }

    /// Neither forbidden spelling appears anywhere in the rendered bytes — key
    /// **or** value.
    ///
    /// The value half is what makes this more than a restatement of the key-set
    /// test. A forbidden key cannot become a second option (each option is its
    /// own pkt-line), but a *value* carrying the literal text
    /// `merge_request.merge_when_pipeline_succeeds=true` is precisely what a
    /// downstream delimiter split would promote into one — CVE-2026-3854's own
    /// mechanism. A key-prefix assertion cannot see it.
    ///
    /// Reds on: interpolating either spelling into the `.description` value. A
    /// prefix or `starts_with` assertion stays green on that mutation; only a
    /// `contains` over the whole option string catches it.
    #[test]
    fn forbidden_option_spellings_are_absent_from_every_rendered_byte() {
        let options = render_push_options(TARGET, TITLE, DESCRIPTION).expect("the fixture's values must render");
        let rendered = options.join("\n");

        for spelling in FORBIDDEN_SPELLINGS {
            assert!(
                !rendered.contains(spelling),
                "{spelling} must not appear in any rendered byte, key or value: {rendered:?}"
            );
        }
    }

    /// Every control-character class, refused in every valued option.
    ///
    /// Table-driven because one character is a habit, not a check: a predicate of
    /// `c != '\n'` passes a single-character version of this test while leaving
    /// CR, TAB, BEL, ESC, DEL and C1 to travel to the hook verbatim, which is
    /// measurably what git does with them.
    ///
    /// **NUL is provable only here, and that is why it lives in this module.**
    /// `std::process::Command` refuses an argument containing a NUL byte before
    /// `execve` — measured: `InvalidInput`, "nul byte found in provided data" —
    /// so no acceptance test downstream can ever red the NUL case, and one
    /// written to try would be an unfalsifiable green.
    ///
    /// Reds on: replacing the allowlist with `c != '\n'` (every row but LF), and
    /// on widening its low bound from `0x20` to `0x09` (the TAB row alone, which
    /// proves the rows are independent rather than one check counted eight
    /// times).
    #[test]
    fn control_character_in_option_value_is_refused() {
        let cases = [
            ('\u{0}', "NUL (U+0000) — refused before Command can refuse it"),
            ('\u{7}', "BEL (U+0007)"),
            ('\t', "TAB (U+0009) — git carries it through untouched"),
            ('\n', "LF (U+000A) — the one class git refuses on its own"),
            ('\r', "CR (U+000D) — the other half of a CRLF header split"),
            ('\u{1b}', "ESC (U+001B) — a terminal-escape injection into any CI log"),
            ('\u{7f}', "DEL (U+007F)"),
            ('\u{85}', "NEL (U+0085) — a C1 control, above the ASCII range"),
        ];

        for (character, description) in cases {
            let value = format!("Claim acme{character}widget");
            for key in VALUED_KEYS {
                assert_refused(key, &value, description);
            }
        }
    }

    /// The printable classes a downstream parser — or a human reader — splits on,
    /// refused in every valued option.
    ///
    /// Not Unicode `Cc`, and that is the point: `char::is_control()` admits every
    /// row below. `;` is the one character with a disclosed incident
    /// (CVE-2026-3854, a semicolon-delimited internal header reaching RCE); the
    /// non-ASCII rows are the class a control-character filter misses by
    /// construction, and U+202E and U+200B are separately named because each is
    /// invisible in the merge-request title a G-04 reviewer approves.
    ///
    /// Reds on: dropping the `c == ';'` clause (the `;` row alone), and on
    /// raising the allowlist's upper bound from `'~'` to `char::MAX` (the four
    /// non-ASCII rows). Two mutations, two disjoint failure sets — a
    /// single-character version driven by `;` is passed by a predicate of
    /// `c != ';'`, which lets every remaining row through.
    #[test]
    fn delimiter_character_in_option_value_is_refused() {
        let cases = [
            (';', "SEMICOLON (U+003B) — CVE-2026-3854's injected delimiter"),
            ('ü', "U+00FC — ordinary non-ASCII, carried verbatim by git"),
            (
                '\u{202e}',
                "U+202E RIGHT-TO-LEFT OVERRIDE — reorders what a reviewer reads",
            ),
            ('\u{200b}', "U+200B ZERO WIDTH SPACE — two claims that look identical"),
            ('\u{301}', "U+0301 COMBINING ACUTE — benign, and refused all the same"),
        ];

        for (character, description) in cases {
            let value = format!("Claim acme{character}widget");
            for key in VALUED_KEYS {
                assert_refused(key, &value, description);
            }
        }
    }

    /// Every byte the allowlist admits survives, in one value.
    ///
    /// The range is written out here from DX-28's rule rather than read from the
    /// renderer, so the two are independent statements of the same contract and a
    /// narrowed implementation reds instead of agreeing with itself. Any
    /// tightening — `0x21..=0x7D`, an added metacharacter, a `char::is_control`
    /// substitution that also catches `~` — fails this test.
    ///
    /// Reds on: narrowing either bound of the allowlist, or adding any printable
    /// character other than `;` to the refused set.
    #[test]
    fn every_allowed_printable_byte_is_accepted() {
        let every_allowed: String = (0x20u8..=0x7e)
            .map(char::from)
            .filter(|character| *character != ';')
            .collect();

        assert_accepted(
            DESCRIPTION_KEY,
            &every_allowed,
            "the whole allowlist, 0x20..=0x7E minus the one character with a CVE",
        );
    }

    /// The punctuation C-067's own values carry, which a guard must not refuse.
    ///
    /// This is the row that proves the guard is not over-broad, and it is not
    /// theoretical: the description names the physical repository
    /// (`oci://host/namespace/name`) and the resolved owners render as bare
    /// `login:id` pairs, plural, so `:`, `/` and `,` all reach a value by design.
    /// GitLab's matcher takes everything after the *first* `=` as the value, so a
    /// later `=` is data, not a delimiter.
    ///
    /// Reds on: adding `=`, `:`, `/`, `,`, `&`, `|` or `\` to the refused set, or
    /// on trimming or rewriting an accepted value instead of passing it through.
    #[test]
    fn legitimate_punctuation_is_accepted() {
        let cases = [
            ("oci://ghcr.io/acme/widget", "the physical repository — ':' and '/'"),
            ("alice:7,bob:9", "C-067's resolved login:id pairs, plural — ':' and ','"),
            ("see https://example.test/x?a=b&c=d", "a URL in the body — '=' and '&'"),
            ("a|b", "a pipe, which no incident implicates"),
            (
                "Claim opened.\\nOwners: alice:7",
                "GitLab's two-character newline escape — a literal backslash and n",
            ),
            (
                " Claim acme/widget ",
                "leading and trailing spaces, passed through rather than trimmed",
            ),
        ];

        for (value, description) in cases {
            assert_accepted(DESCRIPTION_KEY, value, description);
        }
    }

    /// An empty value is refused in every valued option.
    ///
    /// git accepts `merge_request.title=` and GitLab then titles the request from
    /// the commit subject instead of from C-067's template — a silent, invisible
    /// loss of the contract, with no error raised anywhere. The empty case is the
    /// highest-value row no plan-named test reaches.
    ///
    /// Reds on: deleting the non-empty guard. Distinct from its whitespace
    /// sibling below: a guard of `value.is_empty()` passes that one and this one,
    /// which is why they are two tests and not one.
    #[test]
    fn empty_option_value_is_refused() {
        for key in VALUED_KEYS {
            assert_refused(key, "", "an empty value");
        }
    }

    /// A whitespace-only value is refused in every valued option.
    ///
    /// git preserves it verbatim (measured), so the forge sees a request whose
    /// title is three spaces. Same failure mode as the empty value, different
    /// guard: `value.is_empty()` lets it through.
    ///
    /// Reds on: weakening the guard from `value.trim().is_empty()` to
    /// `value.is_empty()`. That mutation leaves `empty_option_value_is_refused`
    /// green, which is the proof the two tests are not one check counted twice.
    #[test]
    fn whitespace_only_option_value_is_refused() {
        for key in VALUED_KEYS {
            assert_refused(key, "   ", "a whitespace-only value");
        }
    }

    /// An option sized to exactly the pkt-line ceiling renders.
    ///
    /// The positive control for the length cap, and it is not optional: a cap
    /// tested only from above is satisfied by a gate that refuses everything,
    /// while every refusal test in this module stays green. This test and its
    /// over-boundary sibling only mean something together.
    ///
    /// Reds on: an off-by-one in the comparison (`<` where `<=` belongs), or a
    /// cap set to any value below 65516.
    #[test]
    fn option_at_the_wire_length_boundary_renders() {
        let prefix_bytes = format!("{TITLE_KEY}=").len();
        let title = "a".repeat(WIRE_OPTION_CEILING - prefix_bytes);

        let options = render_push_options(TARGET, &title, DESCRIPTION)
            .expect("an option of exactly 65516 bytes is what the pkt-line carries");
        let rendered = options
            .iter()
            .find(|option| option.starts_with(TITLE_KEY))
            .expect("the title option must be rendered");
        assert_eq!(
            rendered.len(),
            WIRE_OPTION_CEILING,
            "the whole key=value string is what the 65516-byte ceiling measures"
        );
    }

    /// One byte past the ceiling is refused, before git can fail on it.
    ///
    /// At 65517 git dies with `fatal: protocol error: impossibly long line` and
    /// exit 128, which the stderr classifier can only report as an opaque
    /// `GitPushFailed`. Refusing here turns that into a named error naming the
    /// option.
    ///
    /// Reds on: deleting the length check.
    #[test]
    fn option_over_the_wire_length_boundary_is_refused() {
        let prefix_bytes = format!("{TITLE_KEY}=").len();
        let title = "a".repeat(WIRE_OPTION_CEILING - prefix_bytes + 1);

        assert_refused(TITLE_KEY, &title, "one byte past the 65516-byte pkt-line ceiling");
    }

    /// The refusal names the option it refused and never reproduces the value.
    ///
    /// The guard fires exactly when C-067's structured-values-only rule has
    /// already been broken upstream, so at that moment the value is
    /// operator-influenced text — a `--upstream-disclaimer` that found a path it
    /// should not have. Echoing it into a `ForgeError` message re-injects it into
    /// the CI log: a second sink, and for the ESC case an actively hostile one.
    /// The over-length row carries the same argument at a different scale — a
    /// 65517-byte message floods the log whatever it contains.
    ///
    /// Reds on: adding `{value}` to the variant's `#[error(...)]` format string,
    /// or building `reason` from the offending value rather than from the key and
    /// the single offending codepoint.
    #[test]
    fn refusal_names_the_key_and_never_echoes_the_value() {
        const SENTINEL: &str = "ocx-sentinel-must-not-be-echoed";

        let cases = [
            (format!("{SENTINEL}\u{1b}"), "a value carrying a forbidden byte"),
            (
                format!("{SENTINEL}{}", "a".repeat(WIRE_OPTION_CEILING)),
                "an over-length value",
            ),
        ];

        for (value, description) in cases {
            let error = match render_with(TITLE_KEY, &value) {
                Err(error) => error,
                Ok(options) => panic!("{description}: expected a refusal, got {options:?}"),
            };
            assert!(
                matches!(&error, ForgeError::PushOptionRefused { key, .. } if *key == TITLE_KEY),
                "{description}: expected PushOptionRefused naming {TITLE_KEY}, got {error:?}"
            );
            let message = error.to_string();
            assert!(
                !message.contains(SENTINEL),
                "{description}: the offending value reached the error message: {message}"
            );
            assert!(
                message.contains(TITLE_KEY),
                "{description}: the message must name the option so the operator knows which one broke: {message}"
            );
        }
    }

    /// A multi-line body survives the allowlist only because the newlines are
    /// escaped first — and the escape leaves the guard's teeth in.
    ///
    /// The claim body is the real input: `claim/request.rs::request_body`
    /// renders `"Namespace claim for `X`.\n\n- name: …"`, whose first LF is at
    /// byte 41 — the exact byte that made every `ocx package claim
    /// --transport git` exit 1 with `{"kind":"internal"}`.
    ///
    /// Reds on: deleting the escape (the render refuses U+000A); on escaping
    /// something the wire already accepts (the round-trip assertion below);
    /// and on escaping *into* the value rather than replacing (the length
    /// assertion).
    #[test]
    fn a_multi_line_body_is_escaped_into_the_wire_alphabet() {
        let body = "Namespace claim for `ocx.sh/acme/widget`.\n\n- name: ocx.sh/acme/widget\n";
        assert_eq!(
            body.as_bytes()[41],
            b'\n',
            "the fixture body must reproduce the reported offset, or this row measures a different string"
        );

        render_with(DESCRIPTION_KEY, body).expect_err("a raw newline is refused, which is the defect");

        let escaped = escape_newlines(body);
        assert!(!escaped.contains('\n'), "no literal newline may survive: {escaped:?}");
        assert_eq!(
            escaped.matches("\\n").count(),
            body.matches('\n').count(),
            "every newline becomes exactly one two-character sequence: {escaped:?}"
        );
        let options = render_with(DESCRIPTION_KEY, &escaped).expect("the escaped body renders");
        assert!(
            options
                .iter()
                .any(|option| option == &format!("{DESCRIPTION_KEY}={escaped}")),
            "the escaped body reaches the wire verbatim: {options:?}"
        );
    }

    /// The escape is not a laundering step: a byte the wire forbids for any
    /// other reason is still refused after it runs.
    ///
    /// Reds on: widening the allowlist to admit control characters "because
    /// they are escaped now", or moving the escape inside `render_option` and
    /// applying it to every byte class.
    #[test]
    fn the_escape_launders_nothing_but_the_newline() {
        for (value, case) in [
            ("carriage\rreturn", "CR"),
            ("tab\tseparated", "TAB"),
            ("semicolon;injected", "a delimiter"),
            ("escape\u{1b}sequence", "ESC"),
        ] {
            let escaped = escape_newlines(value);
            assert_eq!(escaped, value, "{case}: the escape must leave it untouched");
            for key in VALUED_KEYS {
                assert_refused(key, &escaped, case);
            }
        }
    }

    /// A value with no newline is returned unchanged, so announce — whose body
    /// is one line — sends exactly the bytes it sent before.
    #[test]
    fn a_single_line_value_is_unchanged_by_the_escape() {
        for value in [TARGET, TITLE, DESCRIPTION] {
            assert_eq!(escape_newlines(value), value, "{value:?} carries no newline to escape");
        }
    }
}
