//! The label charset shared by processing-object labels and worker label
//! selectors.
//!
//! The strictness is load-bearing, not taste: the charset excludes `'` and
//! `\`, which is what makes rendering a validated selector as an inline SQL
//! literal injection-safe by construction. `/` is admitted in KEYS only, so a
//! platform-prefixed key (`basable.com/infra-type`) is expressible; a slash
//! terminates nothing and escapes nothing.

use std::collections::BTreeMap;
use std::fmt;

/// The maximum number of label pairs on one object or selector.
pub const MAX_LABELS: usize = 8;
/// The maximum length of a label key or value, in characters.
pub const MAX_LABEL_LEN: usize = 63;

/// Labels are an ordered map so two equal label sets render identically.
pub type Labels = BTreeMap<String, String>;

/// Why a label set was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidLabel {
    /// More than [`MAX_LABELS`] pairs.
    TooMany(usize),
    /// A key or value is empty or longer than [`MAX_LABEL_LEN`].
    Length {
        /// `"key"` or `"value"`.
        what: &'static str,
        /// The offending string.
        s: String,
    },
    /// A key or value carries a character outside its charset.
    Charset {
        /// `"key"` or `"value"`.
        what: &'static str,
        /// The offending string.
        s: String,
        /// The offending character.
        c: char,
        /// The charset, for the message.
        allowed: &'static str,
    },
    /// A key does not start with a letter.
    KeyStart(String),
}

impl fmt::Display for InvalidLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidLabel::TooMany(n) => {
                write!(f, "{n} label pairs exceeds the maximum of {MAX_LABELS}")
            }
            InvalidLabel::Length { what, s } => {
                write!(f, "label {what} {s:?} must be 1–{MAX_LABEL_LEN} characters")
            }
            InvalidLabel::Charset {
                what,
                s,
                c,
                allowed,
            } => {
                write!(f, "label {what} {s:?}: {c:?} outside {allowed}")
            }
            InvalidLabel::KeyStart(k) => write!(f, "label key {k:?} must start with a letter"),
        }
    }
}

impl std::error::Error for InvalidLabel {}

/// Checks a label set: at most [`MAX_LABELS`] pairs; keys and values are
/// 1–[`MAX_LABEL_LEN`] characters of `[a-z0-9_.-]`, keys may also carry `/`
/// and must start with a letter.
pub fn validate_labels(labels: &Labels) -> Result<(), InvalidLabel> {
    if labels.is_empty() {
        return Ok(());
    }
    if labels.len() > MAX_LABELS {
        return Err(InvalidLabel::TooMany(labels.len()));
    }
    for (k, v) in labels {
        validate_label_key(k)?;
        validate_label_value(v)?;
    }
    Ok(())
}

/// Checks one label key: 1–63 characters of `[a-z0-9_.-/]`, starting with a
/// letter.
pub fn validate_label_key(k: &str) -> Result<(), InvalidLabel> {
    validate_label_string(k, "key", true)?;
    if !k.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(InvalidLabel::KeyStart(k.to_owned()));
    }
    Ok(())
}

/// Checks one label value: 1–63 characters of `[a-z0-9_.-]`.
pub fn validate_label_value(v: &str) -> Result<(), InvalidLabel> {
    validate_label_string(v, "value", false)
}

fn validate_label_string(
    s: &str,
    what: &'static str,
    allow_slash: bool,
) -> Result<(), InvalidLabel> {
    let n = s.chars().count();
    if n == 0 || n > MAX_LABEL_LEN {
        return Err(InvalidLabel::Length {
            what,
            s: s.to_owned(),
        });
    }
    for c in s.chars() {
        let ok = matches!(c, 'a'..='z' | '0'..='9' | '_' | '.' | '-') || (c == '/' && allow_slash);
        if !ok {
            return Err(InvalidLabel::Charset {
                what,
                s: s.to_owned(),
                c,
                allowed: if allow_slash {
                    "[a-z0-9_.-/]"
                } else {
                    "[a-z0-9_.-]"
                },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn accepts_the_platform_shapes() {
        assert_eq!(validate_labels(&Labels::new()), Ok(()));
        assert_eq!(
            validate_labels(&labels(&[
                ("basable.com/infra-type", "bare_metal"),
                ("pool", "x-1.2")
            ])),
            Ok(())
        );
    }

    #[test]
    fn refuses_what_would_break_a_sql_literal() {
        assert!(matches!(
            validate_label_value("it's"),
            Err(InvalidLabel::Charset { c: '\'', .. })
        ));
        assert!(matches!(
            validate_label_value("a\\b"),
            Err(InvalidLabel::Charset { c: '\\', .. })
        ));
        // A slash is a key privilege, never a value's.
        assert!(matches!(
            validate_label_value("a/b"),
            Err(InvalidLabel::Charset { c: '/', .. })
        ));
        assert_eq!(validate_label_key("a/b"), Ok(()));
    }

    #[test]
    fn keys_start_with_a_letter_and_sizes_are_bounded() {
        assert_eq!(
            validate_label_key("9abc"),
            Err(InvalidLabel::KeyStart("9abc".into()))
        );
        assert_eq!(
            validate_label_key("_abc"),
            Err(InvalidLabel::KeyStart("_abc".into()))
        );
        assert!(matches!(
            validate_label_key(""),
            Err(InvalidLabel::Length { what: "key", .. })
        ));
        assert!(matches!(
            validate_label_value(&"a".repeat(64)),
            Err(InvalidLabel::Length { what: "value", .. })
        ));
        assert_eq!(validate_label_value(&"a".repeat(63)), Ok(()));
        let many: Labels = (0..9).map(|i| (format!("k{i}"), "v".to_string())).collect();
        assert_eq!(validate_labels(&many), Err(InvalidLabel::TooMany(9)));
    }
}
