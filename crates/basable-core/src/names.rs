//! Registry naming rules, shared by everything that turns a name into DDL.
//!
//! A processing-object type name is also a SQL identifier fragment (the
//! partition table name), so the framework, the testkit and the scaffolder
//! must apply the identical rule; it lives here so they can.

use std::fmt;

/// Why a name was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidName {
    /// The name is empty.
    Empty,
    /// The name is longer than the limit (in characters).
    TooLong {
        /// The offending name.
        name: String,
        /// The maximum length.
        max: usize,
    },
    /// The name carries a character outside the allowed set, or starts with
    /// one that may not start it.
    Charset {
        /// The offending name.
        name: String,
        /// What the rule allows, for the message.
        rule: &'static str,
    },
}

impl fmt::Display for InvalidName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidName::Empty => write!(f, "empty name"),
            InvalidName::TooLong { name, max } => {
                write!(f, "name {name:?} exceeds {max} characters")
            }
            InvalidName::Charset { name, rule } => write!(f, "name {name:?}: {rule}"),
        }
    }
}

impl std::error::Error for InvalidName {}

/// The maximum length of a processing-object type name.
pub const MAX_TYPE_NAME_LEN: usize = 64;
/// The maximum length of a public-id prefix.
pub const MAX_PUBLIC_ID_PREFIX_LEN: usize = 16;

/// Checks a processing-object type name: lowercase `snake_case`, starting
/// with a letter, at most 64 characters. Exported because the name is a SQL
/// identifier fragment (the partition table name), so everything that builds
/// DDL from it must apply exactly this rule.
pub fn validate_type_name(name: &str) -> Result<(), InvalidName> {
    if name.is_empty() {
        return Err(InvalidName::Empty);
    }
    if name.chars().count() > MAX_TYPE_NAME_LEN {
        return Err(InvalidName::TooLong {
            name: name.to_owned(),
            max: MAX_TYPE_NAME_LEN,
        });
    }
    for (i, c) in name.chars().enumerate() {
        let ok = match c {
            'a'..='z' => true,
            '_' | '0'..='9' => i > 0,
            _ => false,
        };
        if !ok {
            return Err(InvalidName::Charset {
                name: name.to_owned(),
                rule: "lowercase snake_case starting with a letter required",
            });
        }
    }
    Ok(())
}

/// Checks a public-id prefix (`proj` in `proj_3kFz…`): lowercase letters
/// only, since the underscore is the public-id separator, and at most 16
/// characters.
pub fn validate_public_id_prefix(prefix: &str) -> Result<(), InvalidName> {
    if prefix.is_empty() {
        return Err(InvalidName::Empty);
    }
    if prefix.chars().count() > MAX_PUBLIC_ID_PREFIX_LEN {
        return Err(InvalidName::TooLong {
            name: prefix.to_owned(),
            max: MAX_PUBLIC_ID_PREFIX_LEN,
        });
    }
    if !prefix.chars().all(|c| c.is_ascii_lowercase()) {
        return Err(InvalidName::Charset {
            name: prefix.to_owned(),
            rule: "lowercase letters only (underscore is the public-id separator)",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_names_follow_the_go_rule() {
        for ok in ["tenant", "tenant_cluster", "a", "x9", "server_2"] {
            assert_eq!(validate_type_name(ok), Ok(()), "{ok}");
        }
        assert_eq!(validate_type_name(""), Err(InvalidName::Empty));
        for bad in [
            "Tenant",
            "_tenant",
            "9tenant",
            "tenant-cluster",
            "tenant cluster",
            "ténant",
        ] {
            assert!(
                matches!(validate_type_name(bad), Err(InvalidName::Charset { .. })),
                "{bad}"
            );
        }
        let long = "a".repeat(65);
        assert!(matches!(
            validate_type_name(&long),
            Err(InvalidName::TooLong { max: 64, .. })
        ));
        assert_eq!(validate_type_name(&"a".repeat(64)), Ok(()));
    }

    #[test]
    fn prefixes_are_lowercase_letters() {
        assert_eq!(validate_public_id_prefix("proj"), Ok(()));
        assert_eq!(validate_public_id_prefix(""), Err(InvalidName::Empty));
        for bad in ["proj_", "Proj", "proj1", "pro-j"] {
            assert!(
                matches!(
                    validate_public_id_prefix(bad),
                    Err(InvalidName::Charset { .. })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            validate_public_id_prefix(&"a".repeat(17)),
            Err(InvalidName::TooLong { max: 16, .. })
        ));
    }
}
