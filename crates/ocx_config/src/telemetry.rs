// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `[telemetry]` configuration section.

use std::collections::BTreeMap;
use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// A header value sent with every telemetry export. Always a secret: it never appears in `Debug`
/// output, a log line or an error message.
#[derive(Clone, schemars::JsonSchema)]
#[schemars(with = "String")]
pub struct HeaderValue(SecretString);

impl HeaderValue {
    /// Wraps `value` as a secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(SecretString::from(value.into()))
    }

    /// The raw value, for the transport that sends it; nothing else should read it.
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for HeaderValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl<'de> Deserialize<'de> for HeaderValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
}

/// Why a layer's `[telemetry]` section cannot be used, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnusableSection {
    key: String,
    origin: String,
}

/// Where and how OpenTelemetry log records are exported.
// No `deny_unknown_fields`: a section written for a newer ocx must not brick an older binary.
// A known key with a wrong type or value marks the section unusable instead of failing the file, so
// every other section of that file still applies (see the `Deserialize` impl below).
#[derive(Default, Clone, schemars::JsonSchema)]
pub struct TelemetryConfig {
    /// Turns export on (`true`) or off (`false`). Absent → export stays off.
    pub enabled: Option<bool>,

    /// Base URL of the OTLP/HTTP collector, an absolute `http` or `https` URL with a host and no
    /// user information. `/v1/logs` is appended to its path.
    pub endpoint: Option<String>,

    /// HTTP headers sent with every export, typically an authorization token.
    ///
    /// Names must be HTTP tokens other than `host`, `content-length`, `content-type`,
    /// `transfer-encoding`, `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer` and
    /// `upgrade`; values must not contain CR, LF or NUL. Values are secrets. A layer that sets
    /// `endpoint` discards the headers inherited from earlier layers.
    pub headers: Option<BTreeMap<String, HeaderValue>>,

    /// Export deadline in milliseconds, `1` to `10000`. Defaults to `500`.
    #[schemars(range(min = 1, max = 10000))]
    pub timeout: Option<u64>,

    /// Wire protocol. Only `http/protobuf` is supported, which is also the default.
    pub protocol: Option<String>,

    /// Replaces the `service.name` resource attribute. Must not be empty.
    pub service_name: Option<String>,

    /// Extra resource attributes added to every export, keyed by a non-empty attribute name.
    pub resource_attributes: Option<BTreeMap<String, String>>,

    /// Set by the loader when declared in the system tier; no lower tier can redirect or disable it.
    #[serde(skip)]
    #[schemars(skip)]
    pub system_locked: bool,

    /// Set when this layer has a known key with an unusable value.
    #[serde(skip)]
    #[schemars(skip)]
    unusable: Option<UnusableSection>,

    /// Set when any layer had an `enabled` that is present but unreadable; kept apart from
    /// `unusable`, which a later layer's bad key replaces.
    #[serde(skip)]
    #[schemars(skip)]
    enabled_unreadable: bool,
}

// Hand-written so the endpoint prints without its query and fragment, where a key may have been put.
impl fmt::Debug for TelemetryConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoint = self.endpoint.as_deref().map(|text| match url::Url::parse(text) {
            Ok(mut url) => {
                url.set_query(None);
                url.set_fragment(None);
                url.to_string()
            }
            Err(_) => "<unparsable>".to_string(),
        });
        formatter
            .debug_struct("TelemetryConfig")
            .field("enabled", &self.enabled)
            .field("endpoint", &endpoint)
            .field("headers", &self.headers)
            .field("timeout", &self.timeout)
            .field("protocol", &self.protocol)
            .field("service_name", &self.service_name)
            .field("resource_attributes", &self.resource_attributes)
            .field("system_locked", &self.system_locked)
            .field("unusable", &self.unusable)
            .field("enabled_unreadable", &self.enabled_unreadable)
            .finish()
    }
}

/// Names the transport owns, so no source may set them.
pub const REFUSED_HEADER_NAMES: [&str; 12] = [
    "host",
    "content-length",
    "content-type",
    "content-encoding",
    "user-agent",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
];

impl<'de> Deserialize<'de> for TelemetryConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // `Value` accepts every TOML shape, so no `[telemetry]` value can fail the file or be echoed
        // by a serde type error.
        Ok(Self::from_value(&Value::deserialize(deserializer)?))
    }
}

impl TelemetryConfig {
    /// Reads the raw table; every wrong type or value marks the section unusable instead of erroring.
    /// Unknown keys are dropped.
    fn from_value(value: &Value) -> Self {
        let mut config = TelemetryConfig::default();
        let Some(table) = value.as_object() else {
            // The loader, which alone knows the layer's path, fills the origin in.
            config.mark_unusable("telemetry", "");
            return config;
        };
        // Declared order decides which bad key is named when several are wrong.
        let mut bad_key = None;
        let mut take = |key: &'static str, valid: bool| {
            if !valid && bad_key.is_none() {
                bad_key = Some(key);
            }
        };
        if let Some(value) = table.get("enabled") {
            config.enabled = value.as_bool();
            take("enabled", config.enabled.is_some());
        }
        if let Some(value) = table.get("endpoint") {
            config.endpoint = valid_endpoint(value);
            take("endpoint", config.endpoint.is_some());
        }
        if let Some(value) = table.get("headers") {
            config.headers = valid_headers(value);
            take("headers", config.headers.is_some());
        }
        if let Some(value) = table.get("timeout") {
            config.timeout = value.as_u64().filter(|ms| (1..=10_000).contains(ms));
            take("timeout", config.timeout.is_some());
        }
        if let Some(value) = table.get("protocol") {
            config.protocol = value
                .as_str()
                .filter(|protocol| protocol.eq_ignore_ascii_case("http/protobuf"))
                .map(str::to_string);
            take("protocol", config.protocol.is_some());
        }
        if let Some(value) = table.get("service_name") {
            config.service_name = value.as_str().filter(|name| !name.is_empty()).map(str::to_string);
            take("service_name", config.service_name.is_some());
        }
        if let Some(value) = table.get("resource_attributes") {
            config.resource_attributes = valid_resource_attributes(value);
            take("resource_attributes", config.resource_attributes.is_some());
        }
        if let Some(key) = bad_key {
            config.mark_unusable(key, "");
        }
        config
    }
}

/// An absolute `http`/`https` URL with a host and no user information, written without control
/// characters or surrounding whitespace (the URL parser would silently drop them, and the stored
/// text is what a log line later names).
fn valid_endpoint(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if text.chars().any(|c| c.is_ascii_control()) || text != text.trim() {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let ok = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    ok.then(|| text.to_string())
}

/// A table of header names (HTTP tokens, none reserved) to values `http` accepts.
fn valid_headers(value: &Value) -> Option<BTreeMap<String, HeaderValue>> {
    let table = value.as_object()?;
    let mut headers = BTreeMap::new();
    let mut seen = std::collections::BTreeSet::new();
    for (name, value) in table {
        // Two spellings of one header would both reach the transport.
        if !seen.insert(name.to_ascii_lowercase()) {
            return None;
        }
        http::HeaderName::from_bytes(name.as_bytes()).ok()?;
        if REFUSED_HEADER_NAMES
            .iter()
            .any(|reserved| reserved.eq_ignore_ascii_case(name))
        {
            return None;
        }
        let text = value.as_str()?;
        http::HeaderValue::from_bytes(text.as_bytes()).ok()?;
        headers.insert(name.clone(), HeaderValue::new(text));
    }
    Some(headers)
}

/// A table of non-empty attribute names to string values.
fn valid_resource_attributes(value: &Value) -> Option<BTreeMap<String, String>> {
    let table = value.as_object()?;
    let mut attributes = BTreeMap::new();
    for (key, value) in table {
        if key.is_empty() {
            return None;
        }
        attributes.insert(key.clone(), value.as_str()?.to_string());
    }
    Some(attributes)
}

impl TelemetryConfig {
    /// Merge `other` into `self`; `other` wins unless `self` is SYSTEM-locked.
    ///
    /// A layer that sets `endpoint` drops every header inherited so far, so a token is never sent to
    /// a destination its author did not name. Otherwise headers merge per name (ASCII
    /// case-insensitive) and resource attributes per exact key.
    pub fn merge(&mut self, other: TelemetryConfig) {
        // The system tier is the fold's accumulator base, so this one return clamps every file tier.
        if self.system_locked {
            return;
        }
        if other.unusable.is_some() {
            self.unusable = other.unusable;
        }
        self.enabled_unreadable |= other.enabled_unreadable;
        if other.enabled.is_some() {
            self.enabled = other.enabled;
        }
        if other.endpoint.is_some() {
            self.endpoint = other.endpoint;
            self.headers = None;
        }
        if let Some(incoming) = other.headers {
            let headers = self.headers.get_or_insert_with(BTreeMap::new);
            for (name, value) in incoming {
                headers.retain(|existing, _| !existing.eq_ignore_ascii_case(&name));
                headers.insert(name, value);
            }
        }
        if other.timeout.is_some() {
            self.timeout = other.timeout;
        }
        if other.protocol.is_some() {
            self.protocol = other.protocol;
        }
        if other.service_name.is_some() {
            self.service_name = other.service_name;
        }
        if let Some(incoming) = other.resource_attributes {
            self.resource_attributes
                .get_or_insert_with(BTreeMap::new)
                .extend(incoming);
        }
        // `other.system_locked` is not adopted: only the loader locks, and only the system file.
    }

    /// Mark this section as declared in the system tier.
    pub fn lock_as_system(&mut self) {
        self.system_locked = true;
    }

    /// Mark this section unusable because `key` holds a value of the wrong type or range, declared
    /// by the layer named `origin` (a path, or the managed config's source).
    pub fn mark_unusable(&mut self, key: impl Into<String>, origin: impl Into<String>) {
        let key = key.into();
        self.enabled_unreadable |= key == "enabled";
        self.unusable = Some(UnusableSection {
            key,
            origin: origin.into(),
        });
    }

    /// Names the layer this section was read from, for a section already marked unusable; a usable
    /// section carries no origin.
    pub fn stamp_origin(&mut self, origin: impl Into<String>) {
        if let Some(unusable) = self.unusable.as_mut() {
            unusable.origin = origin.into();
        }
    }

    /// `false` once any contributing layer marked the section unusable.
    pub fn is_usable(&self) -> bool {
        self.unusable.is_none()
    }

    /// The key that made the section unusable.
    pub fn unusable_key(&self) -> Option<&str> {
        self.unusable.as_ref().map(|unusable| unusable.key.as_str())
    }

    /// Whether any contributing layer wrote an `enabled` too broken to read, so whether the run
    /// wanted telemetry is unknown even when a later layer's bad key is the one named.
    pub fn enabled_unreadable(&self) -> bool {
        self.enabled_unreadable
    }

    /// The layer that made the section unusable.
    pub fn unusable_origin(&self) -> Option<&str> {
        self.unusable.as_ref().map(|unusable| unusable.origin.as_str())
    }

    /// Every header name, in name order, without the values.
    pub fn header_names(&self) -> impl Iterator<Item = &str> {
        self.headers
            .iter()
            .flat_map(|headers| headers.keys().map(String::as_str))
    }

    /// Every header with its secret value, in name order, for the transport.
    pub fn header_entries(&self) -> impl Iterator<Item = (&str, &HeaderValue)> {
        self.headers
            .iter()
            .flat_map(|headers| headers.iter().map(|(name, value)| (name.as_str(), value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_headers(endpoint: Option<&str>, headers: &[(&str, &str)]) -> TelemetryConfig {
        TelemetryConfig {
            endpoint: endpoint.map(str::to_string),
            headers: Some(
                headers
                    .iter()
                    .map(|(n, v)| (n.to_string(), HeaderValue::new(*v)))
                    .collect(),
            ),
            ..Default::default()
        }
    }

    fn parse(body: &str) -> crate::Config {
        toml::from_str(body).expect("the file parses whatever [telemetry] holds")
    }

    fn section(body: &str) -> TelemetryConfig {
        parse(&format!("[telemetry]\n{body}"))
            .telemetry
            .expect("the section is present")
    }

    #[test]
    fn telemetry_unknown_keys_are_ignored() {
        let parsed = section("enabled = true\nrotation = \"daily\"\n[telemetry.future]\nx = 1\n");
        assert!(parsed.is_usable());
        assert_eq!(parsed.enabled, Some(true));
    }

    #[test]
    fn telemetry_bad_value_marks_the_section_unusable() {
        let rows = [
            ("enabled", "enabled = \"yes\""),
            ("endpoint", "endpoint = 5"),
            ("endpoint", "endpoint = \"collector.example:4318\""),
            ("endpoint", "endpoint = \"ftp://collector.example\""),
            ("endpoint", "endpoint = \"https://\""),
            ("endpoint", "endpoint = \"/relative/path\""),
            ("endpoint", "endpoint = \"https://user@collector.example\""),
            ("endpoint", "endpoint = \"https://user:pw@collector.example\""),
            ("endpoint", "endpoint = \"https://collector.example/\\r\\nX-Evil: 1\""),
            ("endpoint", "endpoint = \"https://collector.example/\\tx\""),
            ("endpoint", "endpoint = \" https://collector.example\""),
            ("endpoint", "endpoint = \"https://collector.example \""),
            ("headers", "headers = \"authorization\""),
            ("headers", "[telemetry.headers]\nauthorization = 1"),
            ("headers", "[telemetry.headers]\n\"bad name\" = \"v\""),
            ("headers", "[telemetry.headers]\n\"\" = \"v\""),
            ("headers", "[telemetry.headers]\nx-a = \"line\\nbreak\""),
            ("headers", "[telemetry.headers]\nx-a = \"nul\\u0000byte\""),
            ("headers", "[telemetry.headers]\nHost = \"evil.example\""),
            ("headers", "[telemetry.headers]\nContent-Length = \"0\""),
            ("headers", "[telemetry.headers]\ncontent-type = \"text/plain\""),
            ("headers", "[telemetry.headers]\nTransfer-Encoding = \"chunked\""),
            ("headers", "[telemetry.headers]\nconnection = \"close\""),
            ("headers", "[telemetry.headers]\nkeep-alive = \"1\""),
            ("headers", "[telemetry.headers]\nProxy-Connection = \"x\""),
            ("headers", "[telemetry.headers]\nTE = \"trailers\""),
            ("headers", "[telemetry.headers]\ntrailer = \"x\""),
            ("headers", "[telemetry.headers]\nUpgrade = \"h2c\""),
            ("headers", "[telemetry.headers]\nUser-Agent = \"x\""),
            ("headers", "[telemetry.headers]\ncontent-encoding = \"gzip\""),
            (
                "headers",
                "[telemetry.headers]\nAuthorization = \"a\"\nauthorization = \"b\"",
            ),
            ("timeout", "timeout = \"500\""),
            ("timeout", "timeout = 0"),
            ("timeout", "timeout = -5"),
            ("timeout", "timeout = 10001"),
            ("timeout", "timeout = 1.5"),
            ("protocol", "protocol = 3"),
            ("protocol", "protocol = \"grpc\""),
            ("protocol", "protocol = \"http/json\""),
            ("service_name", "service_name = \"\""),
            ("service_name", "service_name = 7"),
            ("resource_attributes", "resource_attributes = \"team=build\""),
            ("resource_attributes", "[telemetry.resource_attributes]\n\"\" = \"v\""),
            ("resource_attributes", "[telemetry.resource_attributes]\nteam = 3"),
        ];
        for (key, body) in rows {
            // Another section of the same file must still apply.
            let config = parse(&format!("[update]\ninterval = \"2d\"\n[telemetry]\n{body}"));
            let mut telemetry = config.telemetry.expect("the section is present");
            assert!(!telemetry.is_usable(), "{body}");
            assert_eq!(telemetry.unusable_key(), Some(key), "{body}");
            assert!(config.update.is_some(), "{body}: [update] still loads");
            telemetry.stamp_origin("/etc/ocx/config.toml");
            assert_eq!(telemetry.unusable_origin(), Some("/etc/ocx/config.toml"), "{body}");
        }
    }

    #[test]
    fn a_non_table_telemetry_marks_the_section_unusable_without_echoing_it() {
        for body in [
            "telemetry = false",
            "telemetry = \"Bearer hunter2\"",
            "telemetry = 5",
            "[[telemetry]]\nenabled = true",
        ] {
            let config = parse(&format!("{body}\n[update]\ninterval = \"2d\"\n"));
            let telemetry = config.telemetry.expect("the section is kept as unusable");
            assert_eq!(telemetry.unusable_key(), Some("telemetry"), "{body}");
            assert!(config.update.is_some(), "{body}: [update] still loads");
            assert!(!format!("{telemetry:?}").contains("hunter2"), "{body}");
        }
    }

    #[test]
    fn telemetry_debug_omits_the_endpoint_query_and_fragment() {
        let parsed = section("endpoint = \"https://collector.example/base?api_key=hunter2#frag\"");
        assert!(parsed.is_usable());
        let shown = format!("{parsed:?}");
        assert!(shown.contains("https://collector.example/base"), "{shown}");
        assert!(!shown.contains("hunter2") && !shown.contains("frag"), "{shown}");
    }

    #[test]
    fn telemetry_accepts_the_boundary_values() {
        let parsed = section(
            "timeout = 10000\nprotocol = \"HTTP/Protobuf\"\nendpoint = \"http://127.0.0.1:4318/base\"\n\
             [telemetry.headers]\nAuthorization = \"Bearer x\"\n",
        );
        assert!(parsed.is_usable());
        assert_eq!(parsed.timeout, Some(10_000));
        assert_eq!(parsed.header_names().collect::<Vec<_>>(), ["Authorization"]);
        assert!(section("timeout = 1").is_usable());
    }

    #[test]
    fn telemetry_debug_never_prints_a_header_value() {
        let config = parse("[telemetry]\n[telemetry.headers]\nauthorization = \"Bearer hunter2\"\n");
        let shown = format!("{config:?}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn telemetry_cannot_set_system_locked() {
        let parsed = section("enabled = true\nsystem_locked = true\nunusable = \"x\"\n");
        assert!(!parsed.system_locked);
        assert!(parsed.is_usable());
    }

    #[test]
    fn telemetry_schema_pins_the_timeout_range() {
        let schema = serde_json::to_value(schemars::schema_for!(TelemetryConfig)).expect("schema serialises");
        let timeout = &schema["properties"]["timeout"];
        assert_eq!(timeout["minimum"], 1, "{timeout}");
        assert_eq!(timeout["maximum"], 10_000, "{timeout}");
        assert_eq!(schema["properties"].as_object().map(|keys| keys.len()), Some(7));
    }

    #[test]
    fn merge_higher_tier_wins_field_by_field() {
        let mut lower = TelemetryConfig {
            enabled: Some(false),
            timeout: Some(100),
            protocol: Some("http/protobuf".into()),
            service_name: Some("low".into()),
            resource_attributes: Some(BTreeMap::from([("a".into(), "1".into()), ("b".into(), "1".into())])),
            ..Default::default()
        };
        lower.merge(TelemetryConfig {
            enabled: Some(true),
            timeout: Some(900),
            service_name: Some("high".into()),
            resource_attributes: Some(BTreeMap::from([("b".into(), "2".into()), ("c".into(), "2".into())])),
            ..Default::default()
        });
        assert_eq!(lower.enabled, Some(true));
        assert_eq!(lower.timeout, Some(900));
        assert_eq!(lower.protocol.as_deref(), Some("http/protobuf"));
        assert_eq!(lower.service_name.as_deref(), Some("high"));
        let attributes = lower.resource_attributes.expect("merged");
        assert_eq!(
            attributes.into_iter().collect::<Vec<_>>(),
            [
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "2".into())
            ]
        );
    }

    #[test]
    fn merge_none_never_clobbers() {
        let mut lower = with_headers(Some("https://a.example"), &[("x-team", "a")]);
        lower.enabled = Some(true);
        lower.timeout = Some(300);
        lower.merge(TelemetryConfig::default());
        assert_eq!(lower.enabled, Some(true));
        assert_eq!(lower.endpoint.as_deref(), Some("https://a.example"));
        assert_eq!(lower.timeout, Some(300));
        assert_eq!(lower.header_names().collect::<Vec<_>>(), ["x-team"]);
    }

    #[test]
    fn a_later_endpoint_drops_earlier_headers() {
        let mut lower = with_headers(Some("https://a.example"), &[("authorization", "token-a")]);
        lower.merge(TelemetryConfig {
            endpoint: Some("https://b.example".into()),
            ..Default::default()
        });
        assert_eq!(lower.endpoint.as_deref(), Some("https://b.example"));
        assert_eq!(lower.header_names().count(), 0);
        // The same layer's own headers survive: only inherited ones go.
        lower.merge(with_headers(Some("https://c.example"), &[("authorization", "token-c")]));
        let entries: Vec<_> = lower.header_entries().map(|(n, v)| (n, v.expose())).collect();
        assert_eq!(entries, [("authorization", "token-c")]);
    }

    #[test]
    fn headers_merge_case_insensitive() {
        let mut lower = with_headers(None, &[("Authorization", "old"), ("x-team", "a")]);
        lower.merge(with_headers(None, &[("authorization", "new")]));
        let entries: Vec<_> = lower.header_entries().map(|(n, v)| (n, v.expose())).collect();
        assert_eq!(entries, [("authorization", "new"), ("x-team", "a")]);
    }

    #[test]
    fn merge_system_locked_ignores_every_field() {
        let mut system = with_headers(Some("https://a.example"), &[("authorization", "t")]);
        system.lock_as_system();
        let mut unusable = TelemetryConfig::default();
        unusable.mark_unusable("timeout", "/home/u/config.toml");
        system.merge(TelemetryConfig {
            enabled: Some(false),
            endpoint: Some("https://evil.example".into()),
            timeout: Some(5),
            ..Default::default()
        });
        system.merge(unusable);
        assert_eq!(system.endpoint.as_deref(), Some("https://a.example"));
        assert_eq!(system.enabled, None);
        assert_eq!(system.timeout, None);
        assert_eq!(system.header_names().collect::<Vec<_>>(), ["authorization"]);
        assert!(system.is_usable());
    }

    #[test]
    fn an_unreadable_enabled_survives_a_later_layers_other_bad_key() {
        let mut earlier = TelemetryConfig::default();
        earlier.mark_unusable("enabled", "/etc/ocx/config.toml");
        let mut later = TelemetryConfig::default();
        later.mark_unusable("timeout", "/home/u/config.toml");
        let mut usable = TelemetryConfig {
            enabled: Some(false),
            ..Default::default()
        };

        // Layers fold into an empty accumulator, as the loader does.
        let mut merged = TelemetryConfig::default();
        merged.merge(earlier);
        merged.merge(later);
        assert_eq!(merged.unusable_key(), Some("timeout"), "the later layer names the key");
        assert!(
            merged.enabled_unreadable(),
            "the earlier layer's enabled is not forgotten"
        );

        // A later usable layer, or a locked accumulator, changes neither answer.
        merged.merge(usable.clone());
        assert!(merged.enabled_unreadable());
        let mut locked = TelemetryConfig::default();
        locked.lock_as_system();
        let mut bad_enabled = TelemetryConfig::default();
        bad_enabled.mark_unusable("enabled", "/home/u/config.toml");
        locked.merge(bad_enabled);
        assert!(!locked.enabled_unreadable(), "a system lock ignores every later layer");

        // Control: with no layer unreadable on `enabled` the flag stays down.
        usable.mark_unusable("timeout", "/home/u/config.toml");
        let mut plain = TelemetryConfig::default();
        plain.merge(usable);
        assert!(!plain.enabled_unreadable());
    }

    #[test]
    fn only_a_bad_enabled_value_is_an_unreadable_enabled() {
        assert!(section("enabled = \"yes\"").enabled_unreadable());
        assert!(!section("timeout = 0").enabled_unreadable());
        assert!(!section("enabled = false").enabled_unreadable());
    }

    #[test]
    fn merge_does_not_inherit_a_lock() {
        let mut base = TelemetryConfig::default();
        let mut locked = TelemetryConfig {
            enabled: Some(true),
            ..Default::default()
        };
        locked.lock_as_system();
        base.merge(locked);
        assert_eq!(base.enabled, Some(true));
        assert!(!base.system_locked);
    }

    #[test]
    fn unusable_layer_poisons_the_merge() {
        let mut lower = TelemetryConfig {
            enabled: Some(true),
            ..Default::default()
        };
        let mut higher = TelemetryConfig::default();
        higher.mark_unusable("timeout", "/etc/ocx/config.toml");
        lower.merge(higher);
        assert_eq!(lower.unusable_key(), Some("timeout"));
        assert_eq!(lower.unusable_origin(), Some("/etc/ocx/config.toml"));
        assert!(!lower.is_usable());
        // A later usable layer does not clear it.
        lower.merge(TelemetryConfig {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(!lower.is_usable());
        assert_eq!(lower.unusable_origin(), Some("/etc/ocx/config.toml"));
    }
}
