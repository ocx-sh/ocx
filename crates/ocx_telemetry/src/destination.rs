// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Destination selection: endpoint, headers, timeout and protocol for an armed run.

mod endpoint;
mod headers;

use std::fmt;
use std::net::SocketAddr;
use std::ops::RangeInclusive;
use std::time::Duration;

use ocx_config::telemetry::TelemetryConfig;
use ocx_oci::ssrf::Route;
use secrecy::SecretString;
use url::{Host, Url};

use endpoint::EndpointPath;
pub(crate) use endpoint::is_refused_address;

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(500);
const TIMEOUT_MILLIS: RangeInclusive<u64> = 1..=10_000;
const SUPPORTED_PROTOCOL: &str = "http/protobuf";

/// Which input family named the destination, for the debug audit line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationFamily {
    /// The merged `[telemetry]` section and `OCX_TELEMETRY_HEADERS`.
    OcxConfig,
    /// `OTEL_EXPORTER_OTLP_LOGS_*`.
    OtelLogs,
    /// `OTEL_EXPORTER_OTLP_*`.
    Otel,
}

impl DestinationFamily {
    /// The spelling the debug audit line uses.
    fn label(self) -> &'static str {
        match self {
            Self::OcxConfig => "[telemetry]",
            Self::OtelLogs => "OTEL_EXPORTER_OTLP_LOGS_*",
            Self::Otel => "OTEL_EXPORTER_OTLP_*",
        }
    }
}

/// A usable collector destination.
#[derive(Clone)]
pub struct Destination {
    /// The full logs URL, `/v1/logs` already appended where the source asked for it.
    pub endpoint: Url,
    /// Header name and secret value, in send order.
    pub headers: Vec<(String, SecretString)>,
    /// Connect and total deadline of the export.
    pub timeout: Duration,
    /// The input family that named it.
    pub family: DestinationFamily,
}

impl Destination {
    /// The endpoint with its query and fragment dropped: the only form of it that reaches a log
    /// line or a `Debug` impl, since an `?api-key=` sits in the query.
    fn scrubbed_endpoint(&self) -> Url {
        let mut url = self.endpoint.clone();
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    /// The debug audit line: the scrubbed URL, the family that selected it and the header names.
    /// A header value never appears.
    pub(crate) fn audit_line(&self) -> String {
        let url = self.scrubbed_endpoint();
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        format!(
            "telemetry: destination {url} ({}), headers [{}]",
            self.family.label(),
            names.join(", ")
        )
    }
}

impl Destination {
    /// The warning for headers that would cross the network in clear text, decided without I/O.
    ///
    /// Raised for any header on an `http://` endpoint unless the host is spelled as loopback: a
    /// loopback IP literal or a `localhost` or `*.localhost` name. Whether those stay on this host
    /// depends on the route and, for a name, on what it resolves to, which only the client knows: see
    /// [`Destination::loopback_warning`]. The warning names the input family, never a value or the URL.
    /// Not an error: operators run plaintext collectors inside their own network.
    pub(crate) fn plaintext_headers_warning(&self) -> Option<String> {
        if !self.sends_headers_in_clear() {
            return None;
        }
        let stays_local = match self.endpoint.host()? {
            Host::Domain(name) => is_loopback_name(name),
            Host::Ipv4(address) => address.is_loopback(),
            Host::Ipv6(address) => address.is_loopback(),
        };
        (!stays_local).then(|| self.plaintext_warning())
    }

    /// The warning for a loopback-spelled endpoint, raised unless the dial is direct and, for a name,
    /// every address it resolved to is loopback. A proxied dial carries no addresses: the proxy
    /// resolves the name or reads the literal, so the headers leave this host.
    pub(crate) fn loopback_warning(&self, route: Route, resolved: &[SocketAddr]) -> Option<String> {
        if !self.sends_headers_in_clear() {
            return None;
        }
        let local = match self.endpoint.host()? {
            Host::Domain(name) if is_loopback_name(name) => {
                route == Route::Direct
                    && !resolved.is_empty()
                    && resolved.iter().all(|address| address.ip().is_loopback())
            }
            Host::Ipv4(address) if address.is_loopback() => route == Route::Direct,
            Host::Ipv6(address) if address.is_loopback() => route == Route::Direct,
            _ => return None,
        };
        (!local).then(|| self.plaintext_warning())
    }

    fn sends_headers_in_clear(&self) -> bool {
        self.endpoint.scheme() == "http" && !self.headers.is_empty()
    }

    fn plaintext_warning(&self) -> String {
        format!(
            "telemetry headers from {} may leave this host over plain http; use an https endpoint or remove the headers",
            self.family.label()
        )
    }
}

/// `localhost` and `*.localhost`, by spelling only: the name still has to resolve to loopback.
fn is_loopback_name(name: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    name == "localhost" || name.ends_with(".localhost")
}

// Names only: a header value is a credential and `Debug` output reaches logs and test failures.
impl fmt::Debug for Destination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<&str> = self.headers.iter().map(|(name, _)| name.as_str()).collect();
        formatter
            .debug_struct("Destination")
            .field("endpoint", &self.scrubbed_endpoint().as_str())
            .field("header_names", &names)
            .field("timeout", &self.timeout)
            .field("family", &self.family)
            .finish()
    }
}

/// The destination the armed run sends to, with the warnings its inputs raised.
///
/// Source order: the merged `[telemetry]` endpoint, else `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`, else
/// `OTEL_EXPORTER_OTLP_ENDPOINT`. A source's endpoint, headers, timeout and protocol never mix with
/// another's, and the `OTEL_EXPORTER_OTLP_*` variables are not read at all when the section names
/// the endpoint. An unusable input selected by the winning source yields no destination and one
/// warning, never a value.
pub(crate) fn select(section: Option<&TelemetryConfig>) -> (Option<Destination>, Vec<String>) {
    let mut warnings = Vec::new();
    let chosen = if let Some(section) = section
        && let Some(endpoint) = section.endpoint.as_deref()
    {
        Some(from_section(section, endpoint))
    } else if let Some(endpoint) = ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT.get() {
        warn_unused_ocx_headers(section, &mut warnings);
        warn_unused_generic_headers(OtelSource::Logs, &mut warnings);
        Some(from_otel(&endpoint, OtelSource::Logs, &mut warnings))
    } else if let Some(endpoint) = ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT.get() {
        warn_unused_ocx_headers(section, &mut warnings);
        warn_unused_generic_headers(OtelSource::Generic, &mut warnings);
        Some(from_otel(&endpoint, OtelSource::Generic, &mut warnings))
    } else {
        None
    };
    match chosen {
        Some(Ok(destination)) => {
            warnings.extend(destination.plaintext_headers_warning());
            (Some(destination), warnings)
        }
        Some(Err(warning)) => {
            warnings.push(warning);
            (None, warnings)
        }
        None => (None, warnings),
    }
}

/// Rank one: the merged `[telemetry]` section, with `OCX_TELEMETRY_HEADERS` merged over its headers.
fn from_section(section: &TelemetryConfig, endpoint: &str) -> Result<Destination, String> {
    let url = endpoint::parse(endpoint, EndpointPath::AppendLogs)
        .ok_or("telemetry sends nothing: [telemetry] endpoint is not usable")?;
    let mut merged = Vec::new();
    for (name, value) in section.header_entries() {
        let (name, value) = headers::checked(name, value.expose())
            .ok_or("telemetry sends nothing: [telemetry] headers has a name or value that cannot be sent")?;
        put(&mut merged, name, value);
    }
    if let Some(variable) = ocx_env::OCX_TELEMETRY_HEADERS.get() {
        let parsed = headers::parse(variable.expose()).ok_or_else(|| {
            format!(
                "telemetry is off: {} is malformed",
                ocx_env::OCX_TELEMETRY_HEADERS.declaration().name
            )
        })?;
        // A name the system tier set is the operator's; the user's variable may add names, not replace them.
        let locked: Vec<String> = if section.system_locked {
            merged.iter().map(|(name, _)| name.clone()).collect()
        } else {
            Vec::new()
        };
        for (name, value) in parsed {
            if !locked.contains(&name) {
                put(&mut merged, name, value);
            }
        }
    }
    Ok(Destination {
        endpoint: url,
        headers: merged,
        timeout: section
            .timeout
            .filter(|millis| TIMEOUT_MILLIS.contains(millis))
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis),
        family: DestinationFamily::OcxConfig,
    })
}

/// Which `OTEL_EXPORTER_OTLP_*` endpoint named the destination.
#[derive(Clone, Copy)]
enum OtelSource {
    /// `…_LOGS_ENDPOINT`, used as written.
    Logs,
    /// `…_ENDPOINT`, which gets `/v1/logs`.
    Generic,
}

/// Ranks two and three: the `OTEL_EXPORTER_OTLP_*` variables, and nothing from `[telemetry]`.
fn from_otel(endpoint: &str, source: OtelSource, warnings: &mut Vec<String>) -> Result<Destination, String> {
    let (endpoint_var, path, family) = match source {
        OtelSource::Logs => (
            &ocx_env::OTEL_EXPORTER_OTLP_LOGS_ENDPOINT,
            EndpointPath::AsGiven,
            DestinationFamily::OtelLogs,
        ),
        OtelSource::Generic => (
            &ocx_env::OTEL_EXPORTER_OTLP_ENDPOINT,
            EndpointPath::AppendLogs,
            DestinationFamily::Otel,
        ),
    };
    let url = endpoint::parse(endpoint, path)
        .ok_or_else(|| format!("telemetry sends nothing: {} is not usable", endpoint_var.name))?;

    let protocol = ocx_env::OTEL_EXPORTER_OTLP_LOGS_PROTOCOL
        .get()
        .map(|value| (value, ocx_env::OTEL_EXPORTER_OTLP_LOGS_PROTOCOL.name))
        .or_else(|| {
            ocx_env::OTEL_EXPORTER_OTLP_PROTOCOL
                .get()
                .map(|value| (value, ocx_env::OTEL_EXPORTER_OTLP_PROTOCOL.name))
        });
    if let Some((value, name)) = protocol
        && !value.eq_ignore_ascii_case(SUPPORTED_PROTOCOL)
    {
        return Err(format!("telemetry sends nothing: {name} must be http/protobuf"));
    }

    let headers_var = otel_headers_var(source);
    let mut merged = Vec::new();
    if let Some(variable) = headers_var.get() {
        let parsed = headers::parse(variable.expose()).ok_or_else(|| {
            format!(
                "telemetry sends nothing: {} is malformed",
                headers_var.declaration().name
            )
        })?;
        for (name, value) in parsed {
            put(&mut merged, name, value);
        }
    }

    let timeout = otel_timeout(warnings);

    Ok(Destination {
        endpoint: url,
        headers: merged,
        timeout,
        family,
    })
}

/// The headers variable that supplies a destination named by `source`: the generic one only when
/// the endpoint is the generic one and no logs-specific headers are set.
fn otel_headers_var(source: OtelSource) -> &'static ocx_env::SecretVar {
    match source {
        OtelSource::Generic if ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.get().is_none() => {
            &ocx_env::OTEL_EXPORTER_OTLP_HEADERS
        }
        OtelSource::Logs | OtelSource::Generic => &ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS,
    }
}

/// The `OTEL_EXPORTER_OTLP_*` timeout; a value that is not whole milliseconds in range falls back
/// to the default and warns once, naming the variable and never its value.
fn otel_timeout(warnings: &mut Vec<String>) -> Duration {
    let Some((raw, name)) = ocx_env::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT
        .get()
        .map(|value| (value, ocx_env::OTEL_EXPORTER_OTLP_LOGS_TIMEOUT.name))
        .or_else(|| {
            ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT
                .get()
                .map(|value| (value, ocx_env::OTEL_EXPORTER_OTLP_TIMEOUT.name))
        })
    else {
        return DEFAULT_TIMEOUT;
    };
    match raw.parse::<u64>().ok().filter(|millis| TIMEOUT_MILLIS.contains(millis)) {
        Some(millis) => Duration::from_millis(millis),
        None => {
            warnings.push(format!(
                "telemetry uses the default timeout of 500 ms: {name} is not whole milliseconds from {} to {}; fix or unset it",
                TIMEOUT_MILLIS.start(),
                TIMEOUT_MILLIS.end()
            ));
            DEFAULT_TIMEOUT
        }
    }
}

/// `OTEL_EXPORTER_OTLP_HEADERS` is read only when the generic endpoint names the destination and no
/// logs headers are set; anywhere else it is ignored, and the operator is told once.
fn warn_unused_generic_headers(source: OtelSource, warnings: &mut Vec<String>) {
    if ocx_env::OTEL_EXPORTER_OTLP_HEADERS.get().is_some()
        && !std::ptr::eq(otel_headers_var(source), &ocx_env::OTEL_EXPORTER_OTLP_HEADERS)
    {
        warnings.push(format!(
            "telemetry ignores {} when OTEL_EXPORTER_OTLP_LOGS_* variables name the endpoint or the headers; move the headers to {}",
            ocx_env::OTEL_EXPORTER_OTLP_HEADERS.declaration().name,
            ocx_env::OTEL_EXPORTER_OTLP_LOGS_HEADERS.declaration().name,
        ));
    }
}

/// `[telemetry].headers` and `OCX_TELEMETRY_HEADERS` belong to a `[telemetry]` destination; when an
/// `OTEL_EXPORTER_OTLP_*` endpoint wins they are not sent, and the operator is told once.
fn warn_unused_ocx_headers(section: Option<&TelemetryConfig>, warnings: &mut Vec<String>) {
    let in_section = section.is_some_and(|section| section.header_names().next().is_some());
    if in_section || ocx_env::OCX_TELEMETRY_HEADERS.get().is_some() {
        warnings.push(format!(
            "telemetry headers from [telemetry] and {} are not sent to a destination named by OTEL_EXPORTER_OTLP_* variables; set a [telemetry] endpoint to send them, or remove them",
            ocx_env::OCX_TELEMETRY_HEADERS.declaration().name
        ));
    }
}

/// Adds a header, replacing an earlier one of the same name.
fn put(headers: &mut Vec<(String, SecretString)>, name: String, value: SecretString) {
    headers.retain(|(existing, _)| *existing != name);
    headers.push((name, value));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_prints_header_names_and_never_their_values() {
        let destination = Destination {
            endpoint: Url::parse("https://collector.example:4318/v1/logs").expect("literal URL"),
            headers: vec![("authorization".to_owned(), SecretString::from("hunter2-token"))],
            timeout: Duration::from_millis(500),
            family: DestinationFamily::OcxConfig,
        };

        let printed = format!("{destination:?}");

        assert!(printed.contains("authorization"), "{printed}");
        assert!(!printed.contains("hunter2-token"), "{printed}");
    }

    #[test]
    fn audit_line_drops_the_query_and_every_header_value() {
        let destination = Destination {
            endpoint: Url::parse("https://collector.example:4318/v1/logs?api-key=query-secret").expect("literal URL"),
            headers: vec![
                ("authorization".to_owned(), SecretString::from("header-secret")),
                ("x-team".to_owned(), SecretString::from("blue")),
            ],
            timeout: Duration::from_millis(500),
            family: DestinationFamily::Otel,
        };

        let line = destination.audit_line();

        assert_eq!(
            line,
            "telemetry: destination https://collector.example:4318/v1/logs (OTEL_EXPORTER_OTLP_*), headers [authorization, x-team]"
        );
        for secret in ["api-key", "query-secret", "header-secret", "blue"] {
            assert!(!line.contains(secret), "{line}");
        }
    }

    fn plaintext_case(endpoint: &str, headers: &[&str]) -> Destination {
        Destination {
            endpoint: Url::parse(endpoint).expect("literal URL"),
            headers: headers
                .iter()
                .map(|name| ((*name).to_owned(), SecretString::from("value-never-printed")))
                .collect(),
            timeout: Duration::from_millis(500),
            family: DestinationFamily::OcxConfig,
        }
    }

    #[test]
    fn plaintext_warning_names_the_source_and_never_a_value() {
        let warning = plaintext_case(
            "http://collector.internal:4318/v1/logs?k=query-secret",
            &["authorization"],
        )
        .plaintext_headers_warning()
        .expect("headers over plain http to a named host warn");
        assert!(warning.contains("[telemetry]"), "{warning}");
        for secret in ["value-never-printed", "query-secret", "collector.internal"] {
            assert!(!warning.contains(secret), "{warning}");
        }
    }

    #[test]
    fn the_gate_warns_for_every_endpoint_not_spelled_as_loopback() {
        for (endpoint, headers, warns) in [
            ("http://collector.internal:4318", &["authorization"][..], true),
            ("http://10.0.0.7:4318", &["authorization"], true),
            ("https://collector.internal:4318", &["authorization"], false),
            ("http://collector.internal:4318", &[], false),
            // Loopback spellings are judged by the client, once the route and the addresses are known.
            ("http://127.0.0.1:4318", &["authorization"], false),
            ("http://[::1]:4318", &["authorization"], false),
            ("http://localhost:4318", &["authorization"], false),
            ("http://app.localhost:4318", &["authorization"], false),
        ] {
            let got = plaintext_case(endpoint, headers).plaintext_headers_warning().is_some();
            assert_eq!(got, warns, "{endpoint} with {headers:?}");
        }
    }

    #[test]
    fn a_loopback_spelling_warns_unless_direct_and_resolved_only_to_loopback() {
        let loopback_v4: SocketAddr = "127.0.0.1:4318".parse().expect("literal");
        let loopback_v6: SocketAddr = "[::1]:4318".parse().expect("literal");
        let outside: SocketAddr = "10.0.0.9:4318".parse().expect("literal");
        for (endpoint, headers, route, resolved, warns) in [
            (
                "http://localhost:4318",
                &["authorization"][..],
                Route::Direct,
                &[loopback_v4][..],
                false,
            ),
            (
                "http://app.localhost:4318",
                &["authorization"],
                Route::Direct,
                &[loopback_v6, loopback_v4],
                false,
            ),
            (
                "http://app.localhost:4318",
                &["authorization"],
                Route::Direct,
                &[loopback_v4, outside],
                true,
            ),
            (
                "http://app.localhost:4318",
                &["authorization"],
                Route::Direct,
                &[outside],
                true,
            ),
            ("http://localhost:4318", &["authorization"], Route::Proxied, &[], true),
            ("http://localhost:4318", &["authorization"], Route::Direct, &[], true),
            ("http://localhost:4318", &[], Route::Proxied, &[], false),
            ("https://localhost:4318", &["authorization"], Route::Proxied, &[], false),
            // A loopback literal has no answer to judge: the route alone decides.
            ("http://127.0.0.1:4318", &["authorization"], Route::Direct, &[], false),
            ("http://[::1]:4318", &["authorization"], Route::Direct, &[], false),
            ("http://127.0.0.1:4318", &["authorization"], Route::Proxied, &[], true),
            ("http://[::1]:4318", &["authorization"], Route::Proxied, &[], true),
            ("http://127.0.0.1:4318", &[], Route::Proxied, &[], false),
            // Not a loopback spelling: the gate already judged it.
            (
                "http://collector.internal:4318",
                &["authorization"],
                Route::Direct,
                &[outside],
                false,
            ),
        ] {
            let got = plaintext_case(endpoint, headers)
                .loopback_warning(route, resolved)
                .is_some();
            assert_eq!(got, warns, "{endpoint} {route:?} {resolved:?}");
        }
    }
}
