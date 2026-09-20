//! Public, base62, type-prefixed ids from UUIDs — `proj_3kFz…` — for exposing
//! internal UUID primary keys in URLs and APIs without leaking raw UUID
//! formatting. A port of the basable monorepo's `golang/lib/publicid`; the
//! encoding is byte-identical (`tests/go_fixture.rs` pins twenty ids the Go
//! implementation produced).
//!
//! Two layers:
//!
//! - [`encode`] / [`decode_with_prefix`]: the bare format, for the type
//!   systems' own encoders that know their prefix.
//! - [`Registry`]: the decode-complete set of resource types a binary knows,
//!   built ONCE at boot from the config-type and processing-object-type
//!   lists ([`Registry::builder`]) — a duplicate name or prefix is a boot
//!   error, not an init panic (porting note 3). It offers the typed
//!   operations the Go package exposes as free functions:
//!   [`Registry::encode`], [`Registry::decode`], [`Registry::decode_typed`],
//!   [`Registry::is_type`].
//!
//! Ids are not canonical on decode: the base62 payload is length-lenient
//! (a short payload is left-padded), so `proj_abc` and `proj_0abc` name the
//! same UUID. [`encode`] only ever emits the full-width form.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod base62;

use std::collections::HashMap;
use std::fmt;

use basable_core::names::{InvalidName, validate_public_id_prefix};
pub use uuid::Uuid;

const UUID_BYTE_WIDTH: usize = 16;

/// Why a public id failed to decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// No `_`, or an empty prefix or payload.
    Malformed(String),
    /// The prefix is not a registered resource type.
    UnknownPrefix(String),
    /// The payload carries a character outside the base62 alphabet.
    InvalidCharacter {
        /// The offending character.
        c: char,
        /// The payload.
        payload: String,
    },
    /// The payload's value does not fit in 16 bytes.
    Overflow(String),
    /// The id decodes but belongs to another resource type.
    WrongType {
        /// The type the caller expected.
        expected: String,
        /// The prefix the id carried.
        got: String,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Malformed(id) => write!(f, "publicid: malformed id {id:?}"),
            DecodeError::UnknownPrefix(p) => {
                write!(f, "publicid: unknown resource type prefix {p:?}")
            }
            DecodeError::InvalidCharacter { c, payload } => {
                write!(f, "publicid: invalid character {c:?} in id {payload:?}")
            }
            DecodeError::Overflow(p) => {
                write!(
                    f,
                    "publicid: id {p:?} decodes to more than {UUID_BYTE_WIDTH} bytes"
                )
            }
            DecodeError::WrongType { expected, got } => {
                write!(f, "publicid: expected a {expected} id, got prefix {got:?}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Builds a public id from a prefix and a UUID: `<prefix>_<base62 of the 16
/// bytes>`, each leading zero BYTE kept as a leading `0` character (so the
/// nil UUID is sixteen zeros and a random one is 21 or 22 characters). The
/// prefix is not checked against a registry; the type systems' own encoders
/// pass theirs.
pub fn encode(prefix: &str, id: Uuid) -> String {
    let mut out = String::with_capacity(prefix.len() + 1 + 22);
    out.push_str(prefix);
    out.push('_');
    out.push_str(&base62::encode(id.as_bytes()));
    out
}

/// Splits a public id into its prefix and UUID without consulting a registry.
/// The prefix is returned as it appeared.
pub fn decode_with_prefix(public_id: &str) -> Result<(&str, Uuid), DecodeError> {
    let (prefix, payload) = split(public_id)?;
    Ok((prefix, decode_payload(payload)?))
}

fn split(public_id: &str) -> Result<(&str, &str), DecodeError> {
    match public_id.split_once('_') {
        Some((prefix, payload)) if !prefix.is_empty() && !payload.is_empty() => {
            Ok((prefix, payload))
        }
        _ => Err(DecodeError::Malformed(public_id.to_owned())),
    }
}

fn decode_payload(payload: &str) -> Result<Uuid, DecodeError> {
    let bytes = base62::decode(payload, UUID_BYTE_WIDTH)?;
    Ok(Uuid::from_slice(&bytes).expect("decode yields exactly 16 bytes"))
}

/// One registered public-id type: a name (`tenant`,
/// `OrganisationConfiguration`) and the prefix its ids carry (`proj`, `org`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResourceType {
    /// The system name.
    pub name: String,
    /// The public-id prefix.
    pub prefix: String,
}

/// Why a registry could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A type has an empty name.
    EmptyName,
    /// A prefix fails [`validate_public_id_prefix`].
    InvalidPrefix {
        /// The type being registered.
        name: String,
        /// The rule it broke.
        cause: InvalidName,
    },
    /// Two types share a name.
    DuplicateName {
        /// The name.
        name: String,
        /// The two prefixes.
        prefixes: (String, String),
    },
    /// Two types share a prefix.
    DuplicatePrefix {
        /// The prefix.
        prefix: String,
        /// The two names.
        names: (String, String),
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegistryError::EmptyName => write!(f, "publicid: resource type with an empty name"),
            RegistryError::InvalidPrefix { name, cause } => {
                write!(f, "publicid: type {name:?}: PublicIDPrefix {cause}")
            }
            RegistryError::DuplicateName { name, prefixes } => write!(
                f,
                "publicid: duplicate resource type name {name:?} (prefixes {:?} and {:?})",
                prefixes.0, prefixes.1
            ),
            RegistryError::DuplicatePrefix { prefix, names } => write!(
                f,
                "publicid: duplicate public-id prefix {prefix:?} (types {:?} and {:?})",
                names.0, names.1
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

/// The decode-complete set of resource types. Built once at boot; cheap to
/// share behind an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    by_name: HashMap<String, ResourceType>,
    by_prefix: HashMap<String, ResourceType>,
}

/// Collects resource types and refuses duplicates when built.
#[derive(Debug, Default)]
pub struct RegistryBuilder {
    types: Vec<ResourceType>,
}

impl RegistryBuilder {
    /// Adds one type. Validation happens in [`RegistryBuilder::build`] so
    /// every problem is reported against the full list.
    pub fn register(mut self, name: impl Into<String>, prefix: impl Into<String>) -> Self {
        self.types.push(ResourceType {
            name: name.into(),
            prefix: prefix.into(),
        });
        self
    }

    /// Adds every type of a list — the config-type and processing-object
    /// registries hand their leaf lists over like this.
    pub fn register_all<I, N, P>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = (N, P)>,
        N: Into<String>,
        P: Into<String>,
    {
        for (n, p) in types {
            self.types.push(ResourceType {
                name: n.into(),
                prefix: p.into(),
            });
        }
        self
    }

    /// Builds the registry; the first duplicate or invalid entry is the error.
    pub fn build(self) -> Result<Registry, RegistryError> {
        let mut reg = Registry::default();
        for t in self.types {
            if t.name.is_empty() {
                return Err(RegistryError::EmptyName);
            }
            if let Err(cause) = validate_public_id_prefix(&t.prefix) {
                return Err(RegistryError::InvalidPrefix {
                    name: t.name,
                    cause,
                });
            }
            if let Some(prior) = reg.by_name.get(&t.name) {
                return Err(RegistryError::DuplicateName {
                    name: t.name.clone(),
                    prefixes: (prior.prefix.clone(), t.prefix),
                });
            }
            if let Some(prior) = reg.by_prefix.get(&t.prefix) {
                return Err(RegistryError::DuplicatePrefix {
                    prefix: t.prefix.clone(),
                    names: (prior.name.clone(), t.name),
                });
            }
            reg.by_name.insert(t.name.clone(), t.clone());
            reg.by_prefix.insert(t.prefix.clone(), t);
        }
        Ok(reg)
    }
}

impl Registry {
    /// Starts a registry.
    pub fn builder() -> RegistryBuilder {
        RegistryBuilder::default()
    }

    /// The type for a system name.
    pub fn type_by_name(&self, name: &str) -> Option<&ResourceType> {
        self.by_name.get(name)
    }

    /// The type for a prefix.
    pub fn type_by_prefix(&self, prefix: &str) -> Option<&ResourceType> {
        self.by_prefix.get(prefix)
    }

    /// Every registered type, in no particular order.
    pub fn types(&self) -> impl Iterator<Item = &ResourceType> {
        self.by_name.values()
    }

    /// The public id of a UUID of the named type — `EncodeTyped`. An
    /// unregistered name is a programmer error and panics, as in Go: the
    /// registry is a build-graph fact, not a runtime condition.
    pub fn encode(&self, name: &str, id: Uuid) -> String {
        let t = self
            .type_by_name(name)
            .unwrap_or_else(|| panic!("publicid: resource type {name:?} not registered"));
        encode(&t.prefix, id)
    }

    /// Splits a public id into its type and UUID — `Decode`. Fails when the
    /// id is malformed, its prefix is unregistered, or the payload does not
    /// decode.
    pub fn decode(&self, public_id: &str) -> Result<(&ResourceType, Uuid), DecodeError> {
        let (prefix, payload) = split(public_id)?;
        let t = self
            .type_by_prefix(prefix)
            .ok_or_else(|| DecodeError::UnknownPrefix(prefix.to_owned()))?;
        Ok((t, decode_payload(payload)?))
    }

    /// Decodes a public id and checks it belongs to the named type —
    /// `DecodeTyped`. Catches an organisation id passed where a tenant id was
    /// expected, and vice versa. An unregistered `name` panics like
    /// [`Registry::encode`].
    pub fn decode_typed(&self, name: &str, public_id: &str) -> Result<Uuid, DecodeError> {
        let want = self
            .type_by_name(name)
            .unwrap_or_else(|| panic!("publicid: resource type {name:?} not registered"));
        let (t, id) = self.decode(public_id)?;
        if t.prefix != want.prefix {
            return Err(DecodeError::WrongType {
                expected: name.to_owned(),
                got: t.prefix.clone(),
            });
        }
        Ok(id)
    }

    /// Whether `public_id` is a well-formed public id of the named type: true
    /// exactly when [`Registry::decode_typed`] succeeds, so it can never drift
    /// from what decode accepts. A prefix test alone is NOT equivalent:
    /// `proj_!!!` has the right prefix and no valid payload.
    pub fn is_type(&self, name: &str, public_id: &str) -> bool {
        self.decode_typed(name, public_id).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() -> Registry {
        Registry::builder()
            .register("tenant", "proj")
            .register("OrganisationConfiguration", "org")
            .build()
            .unwrap()
    }

    #[test]
    fn round_trips_random_ids() {
        let reg = platform();
        for _ in 0..1000 {
            let id = Uuid::new_v4();
            let public = reg.encode("tenant", id);
            assert!(public.starts_with("proj_"));
            assert_eq!(reg.decode_typed("tenant", &public), Ok(id), "{public}");
        }
    }

    #[test]
    fn the_zero_uuid_round_trips() {
        let reg = platform();
        let public = reg.encode("OrganisationConfiguration", Uuid::nil());
        assert_eq!(public, "org_0000000000000000");
        assert_eq!(
            reg.decode_typed("OrganisationConfiguration", &public),
            Ok(Uuid::nil())
        );
    }

    #[test]
    fn decode_rejects_the_wrong_type() {
        let reg = platform();
        let public = reg.encode("tenant", Uuid::new_v4());
        assert!(matches!(
            reg.decode_typed("OrganisationConfiguration", &public),
            Err(DecodeError::WrongType { .. })
        ));
    }

    #[test]
    fn is_type_accepts_only_its_own_well_formed_ids() {
        let reg = platform();
        let tenant = reg.encode("tenant", Uuid::new_v4());
        let org = reg.encode("OrganisationConfiguration", Uuid::new_v4());
        assert!(reg.is_type("tenant", &tenant));
        assert!(reg.is_type("OrganisationConfiguration", &org));
        assert!(!reg.is_type("tenant", &org));
        assert!(!reg.is_type("OrganisationConfiguration", &tenant));
        // A bare UUID is not the public wire format.
        assert!(!reg.is_type("tenant", &Uuid::new_v4().to_string()));
        // The right prefix with no decodable payload is not an id either.
        for bad in ["proj_", "proj_!!!invalid", "not-a-real-id", ""] {
            assert!(!reg.is_type("tenant", bad), "{bad}");
        }
    }

    #[test]
    fn short_payloads_are_valid_and_alias_their_zero_padded_form() {
        let reg = platform();
        assert!(reg.is_type("tenant", "proj_abc"));
        let short = reg.decode_typed("tenant", "proj_abc").unwrap();
        let padded = reg.decode_typed("tenant", "proj_0abc").unwrap();
        assert_eq!(short, padded);
    }

    #[test]
    fn decode_rejects_malformed_ids() {
        let reg = platform();
        assert_eq!(reg.decode(""), Err(DecodeError::Malformed("".into())));
        assert_eq!(
            reg.decode("noPrefix"),
            Err(DecodeError::Malformed("noPrefix".into()))
        );
        assert_eq!(
            reg.decode("proj_"),
            Err(DecodeError::Malformed("proj_".into()))
        );
        assert_eq!(
            reg.decode("_abc"),
            Err(DecodeError::Malformed("_abc".into()))
        );
        assert!(matches!(
            reg.decode("proj_!!!invalid"),
            Err(DecodeError::InvalidCharacter { c: '!', .. })
        ));
        assert_eq!(
            reg.decode("unknown_abc123"),
            Err(DecodeError::UnknownPrefix("unknown".into()))
        );
        // 23 'z's exceed 16 bytes.
        assert!(matches!(
            reg.decode(&format!("proj_{}", "z".repeat(23))),
            Err(DecodeError::Overflow(_))
        ));
    }

    #[test]
    fn the_builder_refuses_collisions_and_bad_prefixes() {
        let dup_name = Registry::builder()
            .register("tenant", "proj")
            .register("tenant", "ten")
            .build();
        assert!(matches!(dup_name, Err(RegistryError::DuplicateName { .. })));
        let dup_prefix = Registry::builder()
            .register("tenant", "proj")
            .register("project", "proj")
            .build();
        assert!(matches!(
            dup_prefix,
            Err(RegistryError::DuplicatePrefix { .. })
        ));
        assert!(matches!(
            Registry::builder().register("tenant", "Proj").build(),
            Err(RegistryError::InvalidPrefix { .. })
        ));
        assert_eq!(
            Registry::builder().register("", "x").build().err(),
            Some(RegistryError::EmptyName)
        );
    }

    #[test]
    #[should_panic(expected = "not registered")]
    fn encoding_an_unregistered_type_is_a_programmer_error() {
        platform().encode("volume", Uuid::nil());
    }
}
