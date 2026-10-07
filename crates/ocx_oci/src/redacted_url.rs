// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A URL safe to put in an error: credentials are gone before the value exists.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

/// A URL without userinfo, query or fragment.
///
/// The whole query goes, not a list of credential names: presigned blob redirects carry
/// their secret under names no list keeps up with (`X-Amz-Signature`, `sig`, `verify`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RedactedUrl(Url);

impl RedactedUrl {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<Url> for RedactedUrl {
    fn from(mut url: Url) -> Self {
        // The setters fail only on URLs that cannot carry userinfo at all.
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        Self(url)
    }
}

impl fmt::Display for RedactedUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

impl Serialize for RedactedUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for RedactedUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Url::deserialize(deserializer).map(Self::from)
    }
}

impl JsonSchema for RedactedUrl {
    fn schema_name() -> Cow<'static, str> {
        "RedactedUrl".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "ocx::RedactedUrl".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "uri",
            "description": "URL with userinfo, query and fragment removed."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(raw: &str) -> String {
        RedactedUrl::from(Url::parse(raw).expect("valid URL")).to_string()
    }

    #[test]
    fn strips_userinfo() {
        assert_eq!(redact("https://user:hunter2@ghcr.io/v2/"), "https://ghcr.io/v2/");
        assert_eq!(redact("https://tokenonly@ghcr.io/v2/"), "https://ghcr.io/v2/");
    }

    #[test]
    fn strips_credential_query_params_and_fragment() {
        let redacted = redact(
            "https://bucket.s3.amazonaws.com/blob?X-Amz-Credential=AKIA&X-Amz-Signature=abc&access_token=t#frag",
        );
        assert_eq!(redacted, "https://bucket.s3.amazonaws.com/blob");
    }

    #[test]
    fn keeps_a_credential_free_url() {
        assert_eq!(
            redact("http://localhost:5000/v2/ns/pkg/blobs/sha256:00"),
            "http://localhost:5000/v2/ns/pkg/blobs/sha256:00"
        );
    }

    #[test]
    fn deserialising_redacts_too() {
        let value: RedactedUrl = serde_json::from_str(r#""https://u:p@ghcr.io/token?scope=x""#).expect("parses");
        assert_eq!(
            serde_json::to_string(&value).expect("serialises"),
            r#""https://ghcr.io/token""#
        );
        assert!(serde_json::from_str::<RedactedUrl>(r#""not a url""#).is_err());
    }

    #[test]
    fn schema_has_fixed_name_and_id() {
        let schema = serde_json::to_value(schemars::schema_for!(RedactedUrl)).expect("schema serialises");
        assert_eq!(schema["type"], "string");
        assert_eq!(RedactedUrl::schema_name(), "RedactedUrl");
        assert_eq!(RedactedUrl::schema_id(), "ocx::RedactedUrl");
    }
}
