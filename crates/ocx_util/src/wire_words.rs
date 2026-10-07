// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A fieldless enum whose variants are words on a wire: the serde spelling, `as_str()` and `ALL` come from one row.

/// Declares a fieldless enum with one `Variant = "word"` row per variant.
///
/// Each row emits the variant (doc comments and attributes carried over verbatim) with
/// `#[serde(rename = "word")]`, one `as_str()` arm and one `ALL` entry, so the three cannot
/// disagree. The caller states the derives, including `Serialize`.
///
/// Use it only where the plain-text word is the serde word; an enum that prints something else
/// keeps its own display.
#[macro_export]
macro_rules! wire_words {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident = $word:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                #[serde(rename = $word)]
                $variant,
            )+
        }

        impl $name {
            /// Every variant, in declaration order.
            $vis const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// The word the wire carries.
            #[must_use]
            $vis const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word,)+
                }
            }
        }
    };
}

#[cfg(test)]
mod tests {
    wire_words! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        enum SnakeWords {
            /// First.
            JobToken = "job_token",
            Token = "token",
            None = "none",
        }
    }

    wire_words! {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        enum KebabWords {
            Ok = "ok",
            DeployToken = "deploy-token",
        }
    }

    fn serde_word(value: impl serde::Serialize) -> String {
        serde_json::to_value(value)
            .expect("a fieldless enum serializes")
            .as_str()
            .expect("each variant renders as a JSON string")
            .to_string()
    }

    /// `as_str` is the serde word and `ALL` lists every variant, for both spellings.
    #[test]
    fn as_str_is_the_serde_word_and_all_lists_every_variant() {
        let snake: Vec<(&str, String)> = SnakeWords::ALL.iter().map(|w| (w.as_str(), serde_word(w))).collect();
        assert_eq!(
            snake,
            [
                ("job_token", "job_token".into()),
                ("token", "token".into()),
                ("none", "none".into())
            ]
        );
        let kebab: Vec<(&str, String)> = KebabWords::ALL.iter().map(|w| (w.as_str(), serde_word(w))).collect();
        assert_eq!(kebab, [("ok", "ok".into()), ("deploy-token", "deploy-token".into())]);
    }

    /// The generated rename also reads back, so a deserializing enum keeps its spelling.
    #[test]
    fn the_word_round_trips_through_serde() {
        let parsed: KebabWords = serde_json::from_str("\"deploy-token\"").expect("the word parses");
        assert_eq!(parsed, KebabWords::DeployToken);
    }
}
