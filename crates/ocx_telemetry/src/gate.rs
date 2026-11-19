// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The arming gate: whether this run may export at all.

use std::ffi::OsString;

use ocx_config::telemetry::TelemetryConfig;
use ocx_env::EnvVar;
use ocx_util::boolean_string::BooleanString;

use crate::destination::{self, Destination};

/// What armed the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmedBy {
    /// `OCX_TELEMETRY` was truthy.
    Switch,
    /// The merged `[telemetry]` section had `enabled = true`.
    Config,
}

/// Why a run does not export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffReason {
    /// `DO_NOT_TRACK` was truthy.
    DoNotTrack,
    /// `OTEL_SDK_DISABLED` was truthy.
    SdkDisabled,
    /// `OTEL_LOGS_EXPORTER` was `none`.
    LogsExporterNone,
    /// The run is offline.
    Offline,
    /// `OCX_TELEMETRY` was falsy, unrecognised or not UTF-8.
    Switch,
    /// The merged `[telemetry]` section cannot be used. Its warning reaches the user only when the
    /// run asked for export.
    Unusable,
    /// A system-locked section sets `enabled = false`.
    Locked,
    /// Nothing asked for export.
    NotEnabled,
}

/// The gate's verdict.
#[derive(Debug)]
pub enum Decision {
    /// The run does not export.
    Off {
        /// Why not.
        reason: OffReason,
        /// One line per unusable switch or section the gate reached, never carrying a value; the
        /// CLI prints them.
        warnings: Vec<String>,
    },
    /// The run may export; with no destination nothing is built, sent or written.
    Armed {
        /// Where to send, `None` when no source named an endpoint.
        destination: Option<Destination>,
        /// What armed the run.
        armed_by: ArmedBy,
        /// One line per unusable or ignored destination input, never carrying a value; the CLI prints them.
        /// Plaintext headers to a `localhost` name are warned at export time instead, through
        /// `tracing`, once the route and the resolved addresses are known.
        warnings: Vec<String>,
    },
}

impl ArmedBy {
    /// The spelling the debug audit line uses.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Switch => "OCX_TELEMETRY",
            Self::Config => "[telemetry] enabled",
        }
    }
}

impl OffReason {
    /// The spelling the debug audit line uses: the input that turned the run off.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::DoNotTrack => "DO_NOT_TRACK",
            Self::SdkDisabled => "OTEL_SDK_DISABLED",
            Self::LogsExporterNone => "OTEL_LOGS_EXPORTER=none",
            Self::Offline => "offline",
            Self::Switch => "OCX_TELEMETRY",
            Self::Unusable => "unusable [telemetry]",
            Self::Locked => "locked [telemetry]",
            Self::NotEnabled => "not enabled",
        }
    }
}

impl Decision {
    fn off(reason: OffReason) -> Self {
        Self::Off {
            reason,
            warnings: Vec::new(),
        }
    }
}

/// What `OCX_TELEMETRY` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SwitchState {
    /// Unset or empty: defers to the section.
    Unset,
    On,
    Off,
    /// Not a boolean spelling, or not UTF-8: off, so a typo never defers to a file that could arm it.
    Invalid,
}

fn classify_switch(value: Option<OsString>) -> SwitchState {
    let Some(value) = value else {
        return SwitchState::Unset;
    };
    match value.to_str().and_then(|text| BooleanString::try_from(text).ok()) {
        Some(parsed) if bool::from(parsed) => SwitchState::On,
        Some(_) => SwitchState::Off,
        None => SwitchState::Invalid,
    }
}

fn read_switch() -> SwitchState {
    classify_switch(ocx_env::OCX_TELEMETRY.get_os())
}

/// Whether `var` holds one of ocx's truthy spellings; anything else, unrecognised included, is not.
// Wider than the OpenTelemetry spec's `true`-only opt-out on purpose: it errs toward off.
pub(crate) fn truthy(var: &'static EnvVar) -> bool {
    var.get_os()
        .and_then(|value| value.to_str().and_then(|text| BooleanString::try_from(text).ok()))
        .is_some_and(bool::from)
}

/// Whether `var` is set to something that is neither a truthy nor a falsy spelling, and so is ignored.
fn unrecognised(var: &'static EnvVar) -> bool {
    var.get_os().is_some_and(|value| {
        value
            .to_str()
            .and_then(|text| BooleanString::try_from(text).ok())
            .is_none()
    })
}

/// `OTEL_LOGS_EXPORTER=none`. Any other value is ignored, which is worth one debug line: an operator
/// who set `otlp` expects ocx to follow it.
fn logs_exporter_is_none() -> bool {
    match ocx_env::OTEL_LOGS_EXPORTER.get() {
        Some(exporter) if exporter.eq_ignore_ascii_case("none") => true,
        Some(exporter) => {
            tracing::debug!(target: "ocx_telemetry", %exporter, "OTEL_LOGS_EXPORTER is not none: ignored");
            false
        }
        None => false,
    }
}

/// The first opt-out that applies; nothing after it is read.
fn opt_out(offline: bool) -> Option<OffReason> {
    if truthy(&ocx_env::DO_NOT_TRACK) {
        Some(OffReason::DoNotTrack)
    } else if truthy(&ocx_env::OTEL_SDK_DISABLED) {
        Some(OffReason::SdkDisabled)
    } else if logs_exporter_is_none() {
        Some(OffReason::LogsExporterNone)
    } else if offline {
        Some(OffReason::Offline)
    } else {
        None
    }
}

/// Decides whether the run exports, reading only `ocx_env` and constructing nothing: first
/// matching row wins, opt-outs before the switch before the section.
///
/// After an opt-out or an explicit off nothing more is read and nothing warns. An invalid switch
/// warns whenever it is reached. An unusable section always turns the run off, but warns only when
/// the run asked for export: `OCX_TELEMETRY` truthy, `enabled = true` in the merged section, or an
/// `enabled` value too broken to read, where the intent is unknown. Anyone else gets a debug line,
/// so a stray key in a shared config never prints on a run that never wanted telemetry.
pub fn decide(section: Option<&TelemetryConfig>, offline: bool) -> Decision {
    if let Some(reason) = opt_out(offline) {
        return Decision::off(reason);
    }
    let switch = read_switch();
    match switch {
        SwitchState::Off => return Decision::off(OffReason::Switch),
        SwitchState::Invalid => {
            return Decision::Off {
                reason: OffReason::Switch,
                warnings: vec![format!(
                    "telemetry is off: {} is not a boolean; set it to true or false, or unset it",
                    ocx_env::OCX_TELEMETRY.name
                )],
            };
        }
        SwitchState::On | SwitchState::Unset => {}
    }
    if let Some(section) = section {
        if !section.is_usable() {
            let message = format!(
                "telemetry is off: [telemetry] in {} has an invalid {}",
                section.unusable_origin().unwrap_or("a config layer"),
                section.unusable_key().unwrap_or("value"),
            );
            let asked = switch == SwitchState::On || section.enabled == Some(true) || section.enabled_unreadable();
            let warnings = if asked {
                vec![format!("{message}; fix or remove it")]
            } else {
                tracing::debug!(target: "ocx_telemetry", "{message}");
                Vec::new()
            };
            return Decision::Off {
                reason: OffReason::Unusable,
                warnings,
            };
        }
        if section.system_locked && section.enabled == Some(false) {
            return Decision::off(OffReason::Locked);
        }
    }
    let armed_by = match switch {
        SwitchState::On => ArmedBy::Switch,
        _ if section.is_some_and(|section| section.enabled == Some(true)) => ArmedBy::Config,
        _ => return Decision::off(OffReason::NotEnabled),
    };
    let (destination, mut warnings) = destination::select(section);
    if unrecognised(&ocx_env::OTEL_SDK_DISABLED) {
        warnings.push(format!(
            "telemetry ignores {}: it is not a boolean; set it to true to turn telemetry off, or unset it",
            ocx_env::OTEL_SDK_DISABLED.name
        ));
    }
    Decision::Armed {
        destination,
        armed_by,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use ocx_config::telemetry::HeaderValue;
    use ocx_env::overrides::{EnvLock, lock};
    use secrecy::ExposeSecret;

    use super::*;
    use crate::destination::DestinationFamily;

    /// Every variable the gate and the destination read, so a case starts from a known-empty
    /// environment instead of the developer's.
    fn clear(env: &EnvLock) {
        env.hermetic("/");
        for var in [
            &ocx_env::OCX_TELEMETRY,
            &ocx_env::DO_NOT_TRACK,
            &ocx_env::OTEL_SDK_DISABLED,
            &ocx_env::OTEL_LOGS_EXPORTER,
            &ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT,
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
            &ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT,
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT,
            &ocx_env::OTEL_EXPORTER_OTLP_PROTOCOL,
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_PROTOCOL,
        ] {
            env.remove(var);
        }
        for var in [
            &ocx_env::OCX_TELEMETRY_HEADERS,
            &ocx_env::OTEL_EXPORTER_OTLP_HEADERS,
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS,
        ] {
            env.remove(var.declaration());
        }
    }

    // ── the arming table ─────────────────────────────────────────────────────

    #[derive(Debug, PartialEq, Eq)]
    enum Verdict {
        Off(OffReason),
        Armed(ArmedBy),
    }

    fn verdict(decision: &Decision) -> Verdict {
        match decision {
            Decision::Off { reason, .. } => Verdict::Off(*reason),
            Decision::Armed { armed_by, .. } => Verdict::Armed(*armed_by),
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Switch {
        Unset,
        On,
        Off,
        Invalid,
    }

    #[derive(Clone, Copy, Debug)]
    enum OptOut {
        None,
        DoNotTrack,
        SdkDisabled,
        LogsExporterNone,
        Offline,
    }

    #[derive(Clone, Copy, Debug)]
    enum Section {
        Absent,
        EnabledTrue,
        EnabledFalse,
        LockedFalse,
        LockedTrue,
        LockedUnset,
        Unusable,
    }

    /// A default section with `set` applied; `unusable` is private to `ocx_config`, so a struct
    /// update cannot build one here.
    fn section_with(set: impl FnOnce(&mut TelemetryConfig)) -> TelemetryConfig {
        let mut section = TelemetryConfig::default();
        set(&mut section);
        section
    }

    fn section(kind: Section) -> Option<TelemetryConfig> {
        if matches!(kind, Section::Absent) {
            return None;
        }
        Some(section_with(|section| match kind {
            Section::Absent => {}
            Section::EnabledTrue => section.enabled = Some(true),
            Section::EnabledFalse => section.enabled = Some(false),
            Section::LockedFalse => {
                section.enabled = Some(false);
                section.lock_as_system();
            }
            Section::LockedTrue => {
                section.enabled = Some(true);
                section.lock_as_system();
            }
            Section::LockedUnset => section.lock_as_system(),
            Section::Unusable => {
                section.enabled = Some(true);
                section.mark_unusable("timeout", "/etc/ocx/config.toml");
            }
        }))
    }

    /// The first matching row of the arming table, written from the table and not from the gate.
    fn expected(switch: Switch, opt_out: OptOut, kind: Section) -> Verdict {
        match opt_out {
            OptOut::DoNotTrack => return Verdict::Off(OffReason::DoNotTrack),
            OptOut::SdkDisabled => return Verdict::Off(OffReason::SdkDisabled),
            OptOut::LogsExporterNone => return Verdict::Off(OffReason::LogsExporterNone),
            OptOut::Offline => return Verdict::Off(OffReason::Offline),
            OptOut::None => {}
        }
        match switch {
            Switch::Off | Switch::Invalid => return Verdict::Off(OffReason::Switch),
            Switch::On | Switch::Unset => {}
        }
        match kind {
            Section::Unusable => return Verdict::Off(OffReason::Unusable),
            Section::LockedFalse => return Verdict::Off(OffReason::Locked),
            _ => {}
        }
        match (switch, kind) {
            (Switch::On, _) => Verdict::Armed(ArmedBy::Switch),
            (_, Section::EnabledTrue | Section::LockedTrue) => Verdict::Armed(ArmedBy::Config),
            _ => Verdict::Off(OffReason::NotEnabled),
        }
    }

    #[test]
    fn gate_truth_table() {
        let switches = [Switch::Unset, Switch::On, Switch::Off, Switch::Invalid];
        let opt_outs = [
            OptOut::None,
            OptOut::DoNotTrack,
            OptOut::SdkDisabled,
            OptOut::LogsExporterNone,
            OptOut::Offline,
        ];
        let sections = [
            Section::Absent,
            Section::EnabledTrue,
            Section::EnabledFalse,
            Section::LockedFalse,
            Section::LockedTrue,
            Section::LockedUnset,
            Section::Unusable,
        ];
        let mut cases = 0;
        for switch in switches {
            for opt_out in opt_outs {
                for kind in sections {
                    let env = lock();
                    clear(&env);
                    match switch {
                        Switch::Unset => {}
                        Switch::On => env.set(&ocx_env::OCX_TELEMETRY, "1"),
                        Switch::Off => env.set(&ocx_env::OCX_TELEMETRY, "off"),
                        Switch::Invalid => env.set(&ocx_env::OCX_TELEMETRY, "maybe"),
                    }
                    match opt_out {
                        OptOut::DoNotTrack => env.set(&ocx_env::DO_NOT_TRACK, "1"),
                        OptOut::SdkDisabled => env.set(&ocx_env::OTEL_SDK_DISABLED, "true"),
                        OptOut::LogsExporterNone => env.set(&ocx_env::OTEL_LOGS_EXPORTER, "NONE"),
                        OptOut::None | OptOut::Offline => {}
                    }
                    let decision = decide(section(kind).as_ref(), matches!(opt_out, OptOut::Offline));
                    assert_eq!(
                        verdict(&decision),
                        expected(switch, opt_out, kind),
                        "switch {switch:?}, opt-out {opt_out:?}, section {kind:?}"
                    );
                    cases += 1;
                }
            }
        }
        assert_eq!(cases, 140, "the table must cover every combination");
    }

    // ── the destination table ────────────────────────────────────────────────

    fn headers(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, HeaderValue>> {
        Some(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), HeaderValue::new(*value)))
                .collect(),
        )
    }

    /// Armed by the switch with `section` merged, returning the destination and the warnings.
    fn armed(section: Option<TelemetryConfig>) -> (Option<Destination>, Vec<String>) {
        match decide(section.as_ref(), false) {
            Decision::Armed {
                destination, warnings, ..
            } => (destination, warnings),
            Decision::Off { reason, .. } => panic!("the run must be armed, was off: {reason:?}"),
        }
    }

    fn header_names(destination: &Destination) -> Vec<&str> {
        let mut names: Vec<&str> = destination.headers.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Whether the header `name` carries `expected`; a mismatch must never print a value.
    fn header_is(destination: &Destination, name: &str, expected: &str) -> bool {
        destination
            .headers
            .iter()
            .any(|(sent, value)| sent == name && value.expose_secret() == expected)
    }

    #[test]
    fn plaintext_header_warning_reaches_the_decision() {
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        let plain = section_with(|file| {
            file.endpoint = Some("http://collector.internal:4318".to_owned());
            file.headers = headers(&[("authorization", "never-printed-token")]);
        });
        let (destination, warnings) = armed(Some(plain));
        assert!(destination.is_some());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("plain http"), "{warnings:?}");
        assert!(
            warnings[0].ends_with("use an https endpoint or remove the headers"),
            "{warnings:?}"
        );
        assert!(!warnings[0].contains("never-printed-token"), "{warnings:?}");

        let tls = section_with(|file| {
            file.endpoint = Some("https://collector.internal:4318".to_owned());
            file.headers = headers(&[("authorization", "never-printed-token")]);
        });
        let (_, warnings) = armed(Some(tls));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn destination_table() {
        // D1: the section names the endpoint; no OTEL header is sent, none is read.
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://other.example");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT, "9000");
        env.set(ocx_env::OTEL_EXPORTER_OTLP_HEADERS.declaration(), "x-otel=generic");
        env.set(ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.declaration(), "x-otel=logs");
        env.set(
            ocx_env::OCX_TELEMETRY_HEADERS.declaration(),
            "x-team=blue,authorization=from%20env",
        );
        let file = section_with(|file| {
            file.endpoint = Some("https://c.example:4318".to_owned());
            file.headers = headers(&[("authorization", "from-file"), ("x-file", "1")]);
            file.timeout = Some(800);
            file.protocol = Some("HTTP/Protobuf".to_owned());
        });
        let (destination, warnings) = armed(Some(file));
        let destination = destination.expect("D1 selects the section's endpoint");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(destination.endpoint.as_str(), "https://c.example:4318/v1/logs");
        assert_eq!(destination.family, DestinationFamily::OcxConfig);
        assert_eq!(destination.timeout, Duration::from_millis(800));
        assert_eq!(header_names(&destination), ["authorization", "x-file", "x-team"]);
        assert!(
            header_is(&destination, "authorization", "from env"),
            "the variable wins per name"
        );
        assert!(header_is(&destination, "x-file", "1"));

        // D1: a system-locked section's header name beats the variable.
        let locked = section_with(|locked| {
            locked.endpoint = Some("https://c.example/otlp/".to_owned());
            locked.headers = headers(&[("authorization", "locked")]);
            locked.lock_as_system();
        });
        let (destination, _) = armed(Some(locked));
        let destination = destination.expect("a locked section still selects its endpoint");
        assert_eq!(destination.endpoint.as_str(), "https://c.example/otlp/v1/logs");
        assert_eq!(destination.timeout, Duration::from_millis(500));
        assert!(header_is(&destination, "authorization", "locked"), "locked names win");
        assert!(header_is(&destination, "x-team", "blue"), "other names still add");
        drop(env);

        // D2: the logs endpoint is used verbatim with the logs headers and timeout only.
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        env.set(
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
            "https://logs.example:4318/custom",
        );
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://generic.example");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT, "2000");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT, "1500");
        env.set(ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.declaration(), "x-logs=1");
        env.set(ocx_env::OTEL_EXPORTER_OTLP_HEADERS.declaration(), "x-generic=1");
        let (destination, warnings) = armed(None);
        let destination = destination.expect("D2 selects the logs endpoint");
        assert_eq!(warnings.len(), 1, "the ignored generic headers warn once: {warnings:?}");
        assert!(warnings[0].contains("OTEL_EXPORTER_OTLP_HEADERS"), "{warnings:?}");
        assert!(!warnings[0].contains("x-generic"), "{warnings:?}");
        assert_eq!(destination.endpoint.as_str(), "https://logs.example:4318/custom");
        assert_eq!(destination.family, DestinationFamily::OtelLogs);
        assert_eq!(destination.timeout, Duration::from_millis(2000));
        assert_eq!(header_names(&destination), ["x-logs"]);

        // D2: OCX header sources are not sent and one warning names both.
        env.set(ocx_env::OCX_TELEMETRY_HEADERS.declaration(), "x-ocx=secret-token");
        let file = section_with(|file| file.headers = headers(&[("x-file", "file-token")]));
        let (destination, warnings) = armed(Some(file));
        let destination = destination.expect("D2 still selects the logs endpoint");
        assert_eq!(header_names(&destination), ["x-logs"]);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("OCX_TELEMETRY_HEADERS"), "{warnings:?}");
        assert!(warnings[0].contains("[telemetry]"), "{warnings:?}");
        assert!(warnings[0].ends_with("or remove them"), "{warnings:?}");
        assert!(warnings[1].contains("OTEL_EXPORTER_OTLP_HEADERS"), "{warnings:?}");
        assert!(!warnings.concat().contains("secret-token") && !warnings.concat().contains("file-token"));
        drop(env);

        // D3: the generic endpoint gets `/v1/logs`; logs headers win over generic ones when set.
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://c.example:4318/otlp/");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT, "1500");
        env.set(ocx_env::OTEL_EXPORTER_OTLP_HEADERS.declaration(), "x-generic=1");
        let (destination, _) = armed(None);
        let destination = destination.expect("D3 selects the generic endpoint");
        assert_eq!(destination.endpoint.as_str(), "https://c.example:4318/otlp/v1/logs");
        assert_eq!(destination.family, DestinationFamily::Otel);
        assert_eq!(destination.timeout, Duration::from_millis(1500));
        assert_eq!(header_names(&destination), ["x-generic"]);
        env.set(ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.declaration(), "x-logs=1");
        let (destination, _) = armed(None);
        assert_eq!(header_names(&destination.expect("D3")), ["x-logs"]);
        drop(env);

        // D4: nothing names a destination.
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        let (destination, warnings) = armed(None);
        assert!(destination.is_none(), "{destination:?}");
        assert!(warnings.is_empty(), "{warnings:?}");
        drop(env);

        // A selected protocol other than http/protobuf sends nothing and warns once.
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://c.example");
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_PROTOCOL, "grpc");
        let (destination, warnings) = armed(None);
        assert!(destination.is_none(), "{destination:?}");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("OTEL_EXPORTER_OTLP_PROTOCOL"), "{warnings:?}");
        drop(env);

        // A bad OTEL timeout falls back to 500 ms; the bounds 1 and 10000 are accepted.
        for (value, expected) in [("0", 500), ("10001", 500), ("abc", 500), ("1", 1), ("10000", 10_000)] {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://c.example");
            env.set(&ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT, value);
            let (destination, warnings) = armed(None);
            let destination = destination.unwrap_or_else(|| panic!("timeout {value:?} must keep the destination"));
            assert_eq!(
                destination.timeout,
                Duration::from_millis(expected),
                "timeout {value:?}"
            );
            let bad = matches!(value, "0" | "10001" | "abc");
            assert_eq!(warnings.len(), usize::from(bad), "timeout {value:?}: {warnings:?}");
        }
    }

    // ── the switch and the warning rule ──────────────────────────────────────

    #[test]
    fn switch_grammar() {
        let table = [
            ("", SwitchState::Unset),
            ("1", SwitchState::On),
            ("TRUE", SwitchState::On),
            ("Yes", SwitchState::On),
            ("on", SwitchState::On),
            ("0", SwitchState::Off),
            ("False", SwitchState::Off),
            ("no", SwitchState::Off),
            ("off", SwitchState::Off),
            ("2", SwitchState::Invalid),
            ("enable", SwitchState::Invalid),
        ];
        for (value, expected) in table {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, value);
            assert_eq!(read_switch(), expected, "OCX_TELEMETRY={value:?}");
        }
        let env = lock();
        clear(&env);
        assert_eq!(read_switch(), SwitchState::Unset, "unset");
        drop(env);

        // The override seam carries `String`, so a value that is not UTF-8 reaches the classifier directly.
        #[cfg(unix)]
        let not_utf8 = std::os::unix::ffi::OsStringExt::from_vec(vec![0xff, 0xfe]);
        #[cfg(windows)]
        let not_utf8 = std::os::windows::ffi::OsStringExt::from_wide(&[0xd800]);
        assert_eq!(classify_switch(Some(not_utf8)), SwitchState::Invalid);
    }

    fn warnings_of(decision: &Decision) -> &[String] {
        match decision {
            Decision::Off { warnings, .. } | Decision::Armed { warnings, .. } => warnings,
        }
    }

    #[test]
    fn a_warning_names_its_input_and_only_when_the_gate_reaches_it() {
        let env = lock();
        clear(&env);

        // An invalid switch warns by name and never shows the value.
        env.set(&ocx_env::OCX_TELEMETRY, "maybe");
        let decision = decide(None, false);
        assert!(matches!(
            decision,
            Decision::Off {
                reason: OffReason::Switch,
                ..
            }
        ));
        assert_eq!(warnings_of(&decision).len(), 1);
        assert!(warnings_of(&decision)[0].contains("OCX_TELEMETRY"));
        assert!(!warnings_of(&decision)[0].contains("maybe"));
        assert_eq!(
            warnings_of(&decision)[0],
            "telemetry is off: OCX_TELEMETRY is not a boolean; set it to true or false, or unset it"
        );

        // An opt-out ahead of the switch reads nothing further, so nothing warns.
        env.set(&ocx_env::DO_NOT_TRACK, "1");
        assert!(warnings_of(&unusable_section_decision()).is_empty());
        env.remove(&ocx_env::DO_NOT_TRACK);

        // An explicit off is not a typo.
        env.set(&ocx_env::OCX_TELEMETRY, "off");
        assert!(warnings_of(&unusable_section_decision()).is_empty());

        // An unusable section that asked for export (`enabled = true`) warns with its origin and
        // key, with or without the switch.
        for switch in [None, Some("1")] {
            match switch {
                Some(value) => env.set(&ocx_env::OCX_TELEMETRY, value),
                None => env.remove(&ocx_env::OCX_TELEMETRY),
            }
            let decision = decide(section(Section::Unusable).as_ref(), false);
            assert!(matches!(
                decision,
                Decision::Off {
                    reason: OffReason::Unusable,
                    ..
                }
            ));
            assert_eq!(warnings_of(&decision).len(), 1, "switch {switch:?}");
            assert_eq!(
                warnings_of(&decision)[0],
                "telemetry is off: [telemetry] in /etc/ocx/config.toml has an invalid timeout; fix or remove it"
            );
        }
    }

    fn unusable_section_decision() -> Decision {
        decide(section(Section::Unusable).as_ref(), false)
    }

    #[test]
    fn opt_outs_are_wider_than_the_opentelemetry_spelling_and_exact_about_the_rest() {
        for value in ["1", "y", "YES", "On", "true"] {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            env.set(&ocx_env::OTEL_SDK_DISABLED, value);
            assert!(
                matches!(
                    decide(None, false),
                    Decision::Off {
                        reason: OffReason::SdkDisabled,
                        ..
                    }
                ),
                "{value}"
            );
        }
        // Not a boolean, or a falsy spelling: not an opt-out, and `OTEL_LOGS_EXPORTER` other than `none` is ignored.
        for value in ["", "0", "no", "maybe", "2"] {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            env.set(&ocx_env::DO_NOT_TRACK, value);
            env.set(&ocx_env::OTEL_LOGS_EXPORTER, "otlp");
            assert!(matches!(decide(None, false), Decision::Armed { .. }), "{value:?}");
        }
    }

    #[test]
    fn an_unusable_section_warns_only_when_the_run_asked_for_export() {
        // (OCX_TELEMETRY, `enabled`, the key that is broken, warns)
        let table = [
            (None, None, "timeout", false),
            (None, Some(false), "timeout", false),
            (Some("1"), None, "timeout", true),
            (Some("1"), Some(false), "timeout", true),
            (None, Some(true), "headers", true),
            (None, None, "enabled", true),
            (Some("1"), None, "enabled", true),
        ];
        for (switch, enabled, key, warns) in table {
            let env = lock();
            clear(&env);
            if let Some(value) = switch {
                env.set(&ocx_env::OCX_TELEMETRY, value);
            }
            let broken = section_with(|section| {
                section.enabled = enabled;
                section.mark_unusable(key, "/etc/ocx/config.toml");
            });

            let decision = decide(Some(&broken), false);

            let case = format!("switch {switch:?}, enabled {enabled:?}, broken {key}");
            assert_eq!(verdict(&decision), Verdict::Off(OffReason::Unusable), "{case}");
            assert_eq!(warnings_of(&decision).len(), usize::from(warns), "{case}");
            if warns {
                assert_eq!(
                    warnings_of(&decision)[0],
                    format!(
                        "telemetry is off: [telemetry] in /etc/ocx/config.toml has an invalid {key}; fix or remove it"
                    ),
                    "{case}"
                );
            }
        }
    }

    // Two layers: the first has an unreadable `enabled`, the later one a different bad key. The
    // merged section names the later key, but whether the run wanted telemetry is still unknown.
    #[test]
    fn an_unreadable_enabled_in_an_earlier_layer_still_warns() {
        let env = lock();
        clear(&env);
        let mut merged = TelemetryConfig::default();
        let mut earlier = TelemetryConfig::default();
        earlier.mark_unusable("enabled", "/etc/ocx/config.toml");
        let mut later = TelemetryConfig::default();
        later.mark_unusable("timeout", "/home/u/config.toml");
        merged.merge(earlier);
        merged.merge(later);

        let decision = decide(Some(&merged), false);

        assert_eq!(verdict(&decision), Verdict::Off(OffReason::Unusable));
        assert_eq!(
            warnings_of(&decision),
            ["telemetry is off: [telemetry] in /home/u/config.toml has an invalid timeout; fix or remove it"]
        );
    }

    #[test]
    fn an_unrecognised_sdk_disabled_warns_once_on_an_armed_run() {
        // (OTEL_SDK_DISABLED, OCX_TELEMETRY, warns)
        let table = [
            ("maybe", "1", true),
            ("2", "1", true),
            ("false", "1", false),
            ("0", "1", false),
            ("no", "1", false),
            ("maybe", "off", false),
        ];
        for (disabled, switch, warns) in table {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, switch);
            env.set(&ocx_env::OTEL_SDK_DISABLED, disabled);

            let decision = decide(None, false);

            let case = format!("OTEL_SDK_DISABLED={disabled:?}, OCX_TELEMETRY={switch:?}");
            assert_eq!(warnings_of(&decision).len(), usize::from(warns), "{case}");
            if warns {
                assert!(matches!(decision, Decision::Armed { .. }), "{case}");
                assert!(warnings_of(&decision)[0].starts_with("telemetry ignores OTEL_SDK_DISABLED"));
                assert!(!warnings_of(&decision)[0].contains(disabled), "{case}");
            }
        }
    }

    #[test]
    fn a_bad_otel_timeout_falls_back_and_warns_once_naming_the_variable() {
        for (generic, logs, named) in [
            (Some("abc"), None, "OTEL_EXPORTER_OTLP_TIMEOUT"),
            (None, Some("10001"), "OTEL_EXPORTER_OTLP_LOGS_TIMEOUT"),
            (Some("1500"), Some("0"), "OTEL_EXPORTER_OTLP_LOGS_TIMEOUT"),
        ] {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://c.example");
            if let Some(value) = generic {
                env.set(&ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT, value);
            }
            if let Some(value) = logs {
                env.set(&ocx_env::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT, value);
            }

            let (destination, warnings) = armed(None);

            let case = format!("generic {generic:?}, logs {logs:?}");
            assert_eq!(
                destination.expect("the destination survives").timeout,
                Duration::from_millis(500)
            );
            assert_eq!(warnings.len(), 1, "{case}: {warnings:?}");
            assert!(warnings[0].contains(named), "{case}: {warnings:?}");
            assert!(
                !warnings[0].contains("abc") && !warnings[0].contains("10001"),
                "{warnings:?}"
            );
        }
    }

    #[test]
    fn ignored_generic_otel_headers_warn_once_and_only_when_ignored() {
        // (logs endpoint, generic endpoint, logs headers, generic headers, warns)
        let table = [
            (true, false, false, true, true),
            (false, true, true, true, true),
            (false, true, false, true, false),
            (true, false, true, false, false),
            (true, false, true, true, true),
        ];
        for (logs_endpoint, generic_endpoint, logs_headers, generic_headers, warns) in table {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            if logs_endpoint {
                env.set(
                    &ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
                    "https://logs.example/v1/logs",
                );
            }
            if generic_endpoint {
                env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "https://c.example");
            }
            if logs_headers {
                env.set(ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.declaration(), "x-logs=1");
            }
            if generic_headers {
                env.set(
                    ocx_env::OTEL_EXPORTER_OTLP_HEADERS.declaration(),
                    "x-generic=never-printed",
                );
            }

            let (_, warnings) = armed(None);

            let case = format!("{logs_endpoint} {generic_endpoint} {logs_headers} {generic_headers}");
            assert_eq!(warnings.len(), usize::from(warns), "{case}: {warnings:?}");
            if warns {
                assert!(
                    warnings[0].starts_with("telemetry ignores OTEL_EXPORTER_OTLP_HEADERS"),
                    "{warnings:?}"
                );
                assert!(!warnings[0].contains("never-printed"), "{warnings:?}");
            }
        }
    }

    // ── destination inputs ───────────────────────────────────────────────────

    #[test]
    fn secret_variable_is_never_readable_through_get() {
        let env = lock();
        clear(&env);
        env.set(ocx_env::OCX_TELEMETRY_HEADERS.declaration(), "authorization=hunter2");
        let declaration = ocx_env::OCX_TELEMETRY_HEADERS.declaration();
        assert!(declaration.get().is_none());
        assert!(declaration.get_raw().is_none());
        assert!(declaration.get_os().is_none());
        let sensitive = ocx_env::OCX_TELEMETRY_HEADERS.get().expect("the secret reader sees it");
        assert!(!format!("{sensitive:?}").contains("hunter2"));
    }

    #[test]
    fn link_local_endpoint_is_unusable() {
        for (variable, endpoint) in [
            (&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "http://169.254.169.254/latest"),
            (&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "http://[fe80::1]:4318"),
            (
                &ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
                "http://169.254.169.254/v1/logs",
            ),
        ] {
            let env = lock();
            clear(&env);
            env.set(&ocx_env::OCX_TELEMETRY, "1");
            env.set(variable, endpoint);
            let (destination, warnings) = armed(None);
            assert!(destination.is_none(), "{endpoint}");
            assert_eq!(warnings.len(), 1, "{endpoint}: {warnings:?}");
            assert!(warnings[0].contains(variable.name), "{warnings:?}");
            assert!(!warnings[0].contains("169.254") && !warnings[0].contains("fe80"));
        }
        let env = lock();
        clear(&env);
        env.set(&ocx_env::OCX_TELEMETRY, "1");
        let file = section_with(|file| file.endpoint = Some("http://169.254.169.254".to_owned()));
        let (destination, warnings) = armed(Some(file));
        assert!(destination.is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("[telemetry]"), "{warnings:?}");

        // A routable literal stays usable.
        env.set(&ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT, "http://10.0.0.5:4318");
        let (destination, _) = armed(None);
        assert_eq!(
            destination.expect("10.0.0.5 is not link-local").endpoint.as_str(),
            "http://10.0.0.5:4318/v1/logs"
        );
    }
}
