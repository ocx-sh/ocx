// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The wire registry: a bare `host[:port]` authority, never a URL.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A registry authority, kept as written once validated.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistryHost(String);

/// The input was not a bare `host[:port]`; it is not echoed, since a rejected value can hold `user:password@`.
#[derive(Debug, thiserror::Error)]
#[error("invalid registry host: expected `host[:port]`")]
pub struct RegistryHostError;

impl RegistryHost {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RegistryHost {
    type Err = RegistryHostError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // `url` judges host and port; these characters would let it read a path, query or userinfo instead.
        if s.is_empty()
            || s.ends_with(':')
            || s.contains(|c: char| matches!(c, '/' | '\\' | '?' | '#' | '@' | '%') || c.is_whitespace())
        {
            return Err(RegistryHostError);
        }
        match url::Url::parse(&format!("https://{s}")) {
            Ok(url) if url.host_str().is_some() => Ok(Self(s.to_owned())),
            _ => Err(RegistryHostError),
        }
    }
}

impl fmt::Display for RegistryHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RegistryHost {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RegistryHost {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for RegistryHost {
    fn schema_name() -> Cow<'static, str> {
        "RegistryHost".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "ocx::RegistryHost".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Registry authority, `host[:port]`."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_host_and_host_port_verbatim() {
        for input in [
            "ghcr.io",
            "localhost:5000",
            "ocx.sh:443",
            "127.0.0.1:8080",
            "[::1]:5000",
        ] {
            let host: RegistryHost = input.parse().unwrap_or_else(|_| panic!("{input} is a registry host"));
            assert_eq!(host.as_str(), input);
            assert_eq!(
                serde_json::to_string(&host).expect("serialises"),
                format!("\"{input}\"")
            );
        }
    }

    #[test]
    fn refuses_anything_but_an_authority() {
        for input in [
            "",
            "https://ghcr.io",
            "ghcr.io/ns",
            "user:secret@ghcr.io",
            "ghcr.io?x=1",
            "ghcr.io#f",
            r"ghcr.io\ns",
            "gh cr.io",
            "ghcr.io:notaport",
            "ghcr.io:",
            "gh%63r.io",
        ] {
            assert!(input.parse::<RegistryHost>().is_err(), "{input:?} must be refused");
            assert!(serde_json::from_str::<RegistryHost>(&format!("{input:?}")).is_err());
        }
    }

    #[test]
    fn error_never_echoes_the_input() {
        let err = "user:hunter2@ghcr.io".parse::<RegistryHost>().expect_err("refused");
        assert!(!err.to_string().contains("hunter2"));
    }

    #[test]
    fn schema_has_fixed_name_and_id() {
        let schema = serde_json::to_value(schemars::schema_for!(RegistryHost)).expect("schema serialises");
        assert_eq!(schema["type"], "string");
        assert_eq!(RegistryHost::schema_name(), "RegistryHost");
        assert_eq!(RegistryHost::schema_id(), "ocx::RegistryHost");
    }
}
