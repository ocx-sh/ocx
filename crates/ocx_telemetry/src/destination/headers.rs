// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `name=value[,name=value…]` header grammar and the names no source may set.

use http::{HeaderName, HeaderValue};
use percent_encoding::percent_decode_str;
use secrecy::SecretString;

/// One header ready to send: the lower-cased name and its secret value, or `None` when the name
/// is not an HTTP token, is refused, or the value is not a legal header value. Never carries
/// any part of the input back.
pub(super) fn checked(name: &str, value: &str) -> Option<(String, SecretString)> {
    let name = HeaderName::from_bytes(name.trim().as_bytes()).ok()?;
    if ocx_config::telemetry::REFUSED_HEADER_NAMES.contains(&name.as_str()) {
        return None;
    }
    HeaderValue::from_bytes(value.as_bytes()).ok()?;
    Some((name.as_str().to_owned(), SecretString::from(value)))
}

/// Parses the OTel header grammar, percent-decoding each value as UTF-8; one bad pair refuses the
/// whole list, so a half-parsed credential is never sent.
pub(super) fn parse(list: &str) -> Option<Vec<(String, SecretString)>> {
    list.split(',')
        .map(|pair| {
            let (name, value) = pair.split_once('=')?;
            checked(name, &percent_decode_str(value).decode_utf8().ok()?)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::*;

    fn pairs(list: &str) -> Option<Vec<(String, String)>> {
        parse(list).map(|headers| {
            headers
                .into_iter()
                .map(|(name, value)| (name, value.expose_secret().to_owned()))
                .collect()
        })
    }

    #[test]
    fn header_variable_grammar() {
        let expected = |entries: &[(&str, &str)]| {
            Some(
                entries
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                    .collect::<Vec<_>>(),
            )
        };
        // Valid: names trimmed and lower-cased, values percent-decoded, `=` kept inside a value.
        assert_eq!(pairs("a=1"), expected(&[("a", "1")]));
        assert_eq!(
            pairs(" X-Team = blue ,authorization=Bearer%20abc%3D%3D"),
            expected(&[("x-team", " blue "), ("authorization", "Bearer abc==")])
        );
        assert_eq!(pairs("k=a=b"), expected(&[("k", "a=b")]));
        assert_eq!(pairs("empty="), expected(&[("empty", "")]));
        assert_eq!(pairs("x-utf8=caf%C3%A9"), expected(&[("x-utf8", "café")]));

        // Refused: the whole variable is invalid, never a partial list.
        for bad in [
            "",
            ",",
            "a=1,",
            "a=1,,b=2",
            "novalue",
            "a=1,novalue",
            "=value",
            " =value",
            "bad name=1",
            "bad:name=1",
            "na\u{e9}me=1",
            "a=%FF",
            "a=%00",
            "a=%0D%0Ax-injected%3A1",
            "a=%0A",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} must be refused");
        }
        for name in [
            "host",
            "Host",
            "content-length",
            "CONTENT-TYPE",
            "content-encoding",
            "Content-Encoding",
            "user-agent",
            "USER-AGENT",
            "transfer-encoding",
            "connection",
            "keep-alive",
            "proxy-connection",
            "te",
            "trailer",
            "upgrade",
        ] {
            assert!(parse(&format!("a=1,{name}=x")).is_none(), "{name} must be refused");
        }
    }
}
