// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Record filename templates.
//!
//! The placeholder set is closed: a silently unexpanded `{jobid}` would give every record the same name
//! (`adr_exec_resolution_record.md` § "Rationale from code: launch").

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

use chrono::{DateTime, Utc};
use p256::elliptic_curve::rand_core::{OsRng, RngCore};

use super::error::RecordsError;
use ocx_util::prelude::StringExt;

/// The default template when `[records] name` is unset; its shape is contract (chronological sort, owning pid).
pub const DEFAULT_TEMPLATE: &str = "{time}-{pid}-{rand}.json";

/// `{time}`'s expansion, e.g. `20260726T140311482Z`: basic form, so it is a valid filename everywhere.
const TIME_FORMAT: &str = "%Y%m%dT%H%M%S%3fZ";

/// Eight lowercase hex characters: the `{rand}` expansion and the sink's collision-retry suffix.
///
/// From the OS CSPRNG, or another principal can pre-create a shared sink's predictable names and stop a
/// `required = true` job; the hasher fallback exists because an unavailable OS source must not fail a launch.
pub fn random_component() -> String {
    let mut bytes = [0u8; 4];
    if OsRng.try_fill_bytes(&mut bytes).is_err() {
        let value = RandomState::new().build_hasher().finish();
        bytes = ((value ^ (value >> 32)) as u32).to_le_bytes();
    }
    format!("{:08x}", u32::from_le_bytes(bytes))
}

/// A validated record filename template.
#[derive(Debug, Clone)]
pub struct NameTemplate {
    template: String,
}

/// The per-invocation values a template expands against, drawn from the record so name and payload agree.
#[derive(Debug, Clone)]
pub struct NameContext {
    /// Expands `{time}`.
    pub recorded_at: DateTime<Utc>,

    /// Expands `{pid}`: the process that runs the tool, matching the record's `process.pid`.
    pub pid: u32,

    /// Expands `{host}`; `None` (hostname undeterminable) expands to the empty string.
    pub host: Option<String>,
}

impl NameTemplate {
    /// Validate a template.
    ///
    /// # Errors
    ///
    /// - [`RecordsError::NameNotAFilename`] — a path separator, or the template is `.` or `..`.
    /// - [`RecordsError::TemplateUnknownPlaceholder`] — a placeholder outside `{time}`, `{pid}`, `{rand}`, `{host}`,
    ///   or an unterminated `{`.
    /// - [`RecordsError::TemplateNotUnique`] — none of `{time}`, `{pid}` or `{rand}`.
    pub fn parse(template: &str) -> Result<Self, RecordsError> {
        // Not left to the write-time check, or under `warn` every publish fails and no record is ever written.
        if template.contains('/') || template.contains('\\') || matches!(template, "." | "..") {
            return Err(RecordsError::NameNotAFilename {
                name: template.to_string(),
            });
        }

        let mut rest = template;
        let mut varies = false;
        while let Some(open) = rest.find('{') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('}') else {
                return Err(RecordsError::TemplateUnknownPlaceholder {
                    placeholder: rest.to_string(),
                });
            };
            let (placeholder, remainder) = rest.split_at(close);
            match placeholder {
                "time" | "pid" | "rand" => varies = true,
                // Constant per host, so no contribution to uniqueness.
                "host" => {}
                _ => {
                    return Err(RecordsError::TemplateUnknownPlaceholder {
                        placeholder: placeholder.to_string(),
                    });
                }
            }
            rest = &remainder[1..];
        }
        if !varies {
            return Err(RecordsError::TemplateNotUnique);
        }
        Ok(Self {
            template: template.to_string(),
        })
    }

    /// The validated template text.
    pub fn as_str(&self) -> &str {
        &self.template
    }

    /// Whether the template expands `{rand}`; without it a collision retry must append one, or it re-renders forever.
    pub fn has_random(&self) -> bool {
        self.template.contains("{rand}")
    }

    /// Render a filename, drawing a fresh random component each call, which the collision retry relies on.
    pub fn render(&self, context: &NameContext) -> String {
        let mut rendered = self
            .template
            .replace("{time}", &context.recorded_at.format(TIME_FORMAT).to_string())
            .replace("{pid}", &context.pid.to_string());
        // Terminates because a drawn component is hex only.
        while rendered.contains("{rand}") {
            rendered = rendered.replacen("{rand}", &random_component(), 1);
        }
        // Last, or a hostname could inject a placeholder that an earlier pass expands.
        rendered.replace(
            "{host}",
            &context.host.as_deref().map(sanitize_host).unwrap_or_default(),
        )
    }
}

impl Default for NameTemplate {
    /// [`DEFAULT_TEMPLATE`] unparsed: a policy with no sink must not fail on a name it never renders.
    fn default() -> Self {
        Self {
            template: DEFAULT_TEMPLATE.to_string(),
        }
    }
}

/// The filename-safe form of a hostname: outside `[A-Za-z0-9._-]` becomes `_`, dots-only becomes empty.
///
/// Sanitized, not rejected, since a launch must not fail over it; a raw `/` or `..` would misplace every record.
fn sanitize_host(host: &str) -> String {
    let slug = host.to_relaxed_slug();
    if slug.chars().all(|c| c == '.') {
        return String::new();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `2026-07-26T14:03:11.482Z` — the ADR's worked example, whose `{time}`
    /// expansion is the literal `20260726T140311482Z`.
    fn recorded_at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-26T14:03:11.482Z")
            .expect("fixture timestamp is valid RFC 3339")
            .with_timezone(&Utc)
    }

    fn context() -> NameContext {
        NameContext {
            recorded_at: recorded_at(),
            pid: 4711,
            host: Some("build-07".to_string()),
        }
    }

    // ── parse: the closed placeholder set ────────────────────────────────────

    /// The shipped default must itself satisfy every rule the parser enforces.
    #[test]
    fn default_template_parses_and_carries_a_random_component() {
        let template = NameTemplate::parse(DEFAULT_TEMPLATE).expect("the default template must parse");
        assert_eq!(template.as_str(), DEFAULT_TEMPLATE);
        assert!(template.has_random(), "the default template expands {{rand}}");
    }

    /// The infallible default is the same template the parser accepts, so the
    /// two ways of reaching it cannot drift.
    #[test]
    fn the_default_matches_the_parsed_default() {
        assert_eq!(NameTemplate::default().as_str(), DEFAULT_TEMPLATE);
        assert!(NameTemplate::default().has_random());
    }

    /// All four placeholders are accepted, in any combination.
    #[test]
    fn every_known_placeholder_is_accepted() {
        NameTemplate::parse("{time}-{pid}-{rand}-{host}.json").expect("the closed set must parse");
        NameTemplate::parse("{pid}.json").expect("a single varying placeholder is enough");
        NameTemplate::parse("{rand}.json").expect("a single varying placeholder is enough");
        NameTemplate::parse("{time}.json").expect("a single varying placeholder is enough");
    }

    /// A placeholder outside the closed set is a configuration error, never a
    /// silent literal — the failure mode of an unexpanded `{jobid}` is a
    /// directory of identically named files, discovered during an audit.
    #[test]
    fn unknown_placeholder_is_an_error() {
        let error = NameTemplate::parse("{time}-{jobid}.json").expect_err("an unknown placeholder must be refused");
        assert!(
            matches!(&error, RecordsError::TemplateUnknownPlaceholder { placeholder } if placeholder == "jobid"),
            "the offending placeholder must be named, got: {error:?}"
        );
    }

    /// `{command}` was considered and rejected — it is the one candidate fed by
    /// user-controlled input. It must not have crept back in.
    #[test]
    fn command_placeholder_is_not_in_the_closed_set() {
        let error = NameTemplate::parse("{time}-{command}.json").expect_err("{command} must stay rejected");
        assert!(matches!(error, RecordsError::TemplateUnknownPlaceholder { .. }));
    }

    /// An unterminated `{` is the same class of typo as an unknown name and is
    /// refused rather than kept as a literal brace.
    #[test]
    fn unterminated_placeholder_is_an_error() {
        let error = NameTemplate::parse("{time}-{pid.json").expect_err("an unterminated placeholder must be refused");
        assert!(
            matches!(error, RecordsError::TemplateUnknownPlaceholder { .. }),
            "got: {error:?}"
        );
    }

    // ── parse: the template is a filename, not a path ────────────────────────

    /// A separator in the literal text is refused where the operator can still
    /// see their config. Left to the write-time backstop it would parse clean
    /// and then fail on every publish — under the default warn posture, one
    /// warning per invocation and not a single record, forever.
    #[test]
    fn a_separator_in_the_literal_text_is_refused_at_parse() {
        for template in ["sub/{pid}.json", "{pid}/record.json", "sub\\{pid}.json", "/{pid}.json"] {
            let error = NameTemplate::parse(template).expect_err("a separator must be refused at parse time");
            assert!(
                matches!(&error, RecordsError::NameNotAFilename { name } if name == template),
                "{template:?} must be refused by name, got: {error:?}"
            );
        }
    }

    /// A template that is nothing but dots would name the sink itself or its
    /// parent rather than a record in it.
    #[test]
    fn a_dot_template_is_refused_at_parse() {
        for template in [".", ".."] {
            let error = NameTemplate::parse(template).expect_err("a dot template must be refused");
            assert!(
                matches!(&error, RecordsError::NameNotAFilename { name } if name == template),
                "{template:?} must be refused by name, got: {error:?}"
            );
        }
    }

    /// The discriminator: dots inside an ordinary template are literal text and
    /// must survive, so the guard above refuses structure rather than periods.
    #[test]
    fn dots_inside_an_ordinary_template_still_parse() {
        NameTemplate::parse("ocx.{pid}.record.json").expect("dots are ordinary literal text");
        NameTemplate::parse("..{pid}.json").expect("a leading `..` in a longer name is still one filename");
    }

    // ── parse: uniqueness ────────────────────────────────────────────────────

    /// A template with no varying component resolves every record in the sink to
    /// one path — rejected at parse rather than discovered as data loss.
    #[test]
    fn template_without_a_varying_component_is_rejected() {
        let error = NameTemplate::parse("record.json").expect_err("a constant template must be refused");
        assert!(matches!(error, RecordsError::TemplateNotUnique), "got: {error:?}");
    }

    /// `{host}` is constant for every record on a host, so it does not satisfy
    /// the uniqueness rule on its own.
    #[test]
    fn host_alone_does_not_satisfy_uniqueness() {
        let error = NameTemplate::parse("{host}.json").expect_err("{host} alone must be refused");
        assert!(matches!(error, RecordsError::TemplateNotUnique), "got: {error:?}");
    }

    // ── render ───────────────────────────────────────────────────────────────

    /// `{time}` expands as ISO 8601 basic, UTC, millisecond — the exact form the
    /// ADR froze, which sorts lexicographically into chronological order.
    #[test]
    fn time_expands_as_basic_utc_millisecond() {
        let template = NameTemplate::parse("{time}.json").expect("valid");
        assert_eq!(template.render(&context()), "20260726T140311482Z.json");
    }

    /// Every placeholder expands, and nothing brace-shaped survives.
    #[test]
    fn every_placeholder_expands() {
        let template = NameTemplate::parse("{time}-{host}-{pid}-{rand}.json").expect("valid");
        let rendered = template.render(&context());
        assert!(
            rendered.starts_with("20260726T140311482Z-build-07-4711-"),
            "got: {rendered}"
        );
        assert!(rendered.ends_with(".json"), "got: {rendered}");
        assert!(
            !rendered.contains('{') && !rendered.contains('}'),
            "no placeholder may survive rendering, got: {rendered}"
        );
    }

    /// `{rand}` is eight hex characters.
    #[test]
    fn random_component_is_eight_hex_characters() {
        let template = NameTemplate::parse("{rand}").expect("valid");
        let rendered = template.render(&context());
        assert_eq!(rendered.len(), 8, "got: {rendered}");
        assert!(
            rendered
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "the random component must be lowercase hex, got: {rendered}"
        );
    }

    /// A publish collision re-renders, so every call must draw anew — otherwise
    /// the retry produces the same name and spins forever.
    #[test]
    fn render_draws_a_fresh_random_component_each_call() {
        let template = NameTemplate::parse("{rand}.json").expect("valid");
        let context = context();
        let drawn: std::collections::HashSet<String> = (0..64).map(|_| template.render(&context)).collect();
        assert!(
            drawn.len() > 32,
            "successive renders must differ; {} distinct of 64",
            drawn.len()
        );
    }

    /// Two `{rand}` occurrences draw independently rather than repeating one
    /// value, so a template asking for more entropy actually gets it.
    #[test]
    fn repeated_random_placeholders_draw_independently() {
        let template = NameTemplate::parse("{rand}-{rand}").expect("valid");
        let differ = (0..16).any(|_| {
            let rendered = template.render(&context());
            let (left, right) = rendered.split_once('-').expect("two components");
            left != right
        });
        assert!(differ, "repeated {{rand}} must not expand to one repeated value");
    }

    /// An unknown hostname expands to the empty string: an environmental detail
    /// must never fail an invocation, and the other components carry uniqueness.
    #[test]
    fn unknown_host_expands_to_the_empty_string() {
        let template = NameTemplate::parse("{host}-{pid}.json").expect("valid");
        let rendered = template.render(&NameContext {
            host: None,
            ..context()
        });
        assert_eq!(rendered, "-4711.json");
    }

    /// The kernel puts no charset on a UTS hostname, and this value is joined
    /// into a path component. A `/` would point every record at a parent
    /// directory that does not exist — ENOENT on every attempt, and exit 74 for
    /// every invocation on that host under a `required` policy — using the ADR's
    /// own example template.
    #[test]
    fn a_hostname_carrying_a_separator_stays_one_path_component() {
        let template = NameTemplate::parse("{time}-{host}-{pid}.json").expect("valid");
        let rendered = template.render(&NameContext {
            host: Some("build/07".to_string()),
            ..context()
        });

        assert_eq!(rendered, "20260726T140311482Z-build_07-4711.json");
        assert!(
            !rendered.contains('/') && !rendered.contains('\\'),
            "no separator may survive the host expansion, got: {rendered}"
        );
    }

    /// A `..` hostname would relocate the audit trail out of the sink the
    /// operator designated, so it expands to nothing at all — the same as an
    /// undeterminable hostname.
    #[test]
    fn a_dot_dot_hostname_expands_to_nothing() {
        let template = NameTemplate::parse("{host}-{pid}.json").expect("valid");

        for host in ["..", ".", "..."] {
            let rendered = template.render(&NameContext {
                host: Some(host.to_string()),
                ..context()
            });
            assert_eq!(
                rendered, "-4711.json",
                "host {host:?} must not survive as a path segment"
            );
        }
    }

    /// An ordinary hostname is untouched — the sanitizer must not mangle the
    /// common case it exists to leave alone.
    #[test]
    fn an_ordinary_hostname_survives_verbatim() {
        let template = NameTemplate::parse("{host}-{pid}.json").expect("valid");
        let rendered = template.render(&NameContext {
            host: Some("build-07.eu-west-1.internal".to_string()),
            ..context()
        });

        assert_eq!(rendered, "build-07.eu-west-1.internal-4711.json");
    }

    /// Literal text around the placeholders survives verbatim.
    #[test]
    fn literal_text_is_preserved() {
        let template = NameTemplate::parse("ocx.{pid}.record.json").expect("valid");
        assert_eq!(template.render(&context()), "ocx.4711.record.json");
    }

    /// `has_random` reports the template's own shape, which is what decides
    /// whether a collision retry can re-render or must append a component.
    #[test]
    fn has_random_reports_the_placeholder() {
        assert!(NameTemplate::parse("{rand}.json").expect("valid").has_random());
        assert!(!NameTemplate::parse("{time}-{pid}.json").expect("valid").has_random());
    }
}
