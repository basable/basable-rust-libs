//! The config-type registry: every type's identity, its message, and the
//! binder that writes its subtype table, collected at boot and refused on
//! collision.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use basable_core::BoxError;
use basable_core::labels::Labels;
use basable_core::names::validate_public_id_prefix;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::{BinderError, RegistryError};

/// One configuration-object type: the `SMALLINT` its
/// `configuration_object_type` row carries, the proto message name seed
/// files and references use, and the prefix its public ids carry. Declared
/// as a `const` next to the binder that writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeInfo {
    /// The registered id (`configuration_object_type.id`); 1 is the
    /// namespace.
    pub id: i16,
    /// The type name: the proto message name (`PricingRuleConfiguration`),
    /// the last segment of a seed item's `@type`, the first token of a
    /// reference.
    pub name: &'static str,
    /// The public-id prefix.
    pub prefix: &'static str,
}

/// The namespace type — the root scope every other object is filed under.
/// Registered by the builder itself.
pub const NAMESPACE_TYPE: TypeInfo = TypeInfo {
    id: 1,
    name: "NamespaceConfiguration",
    prefix: "ns",
};

/// The load-time header every config message carries (`ConfigHeader` in
/// `basable/config/v1/config.proto`): where the object is filed, its name,
/// its labels. In a seed file `namespace` is a `#{NamespaceConfiguration:
/// <name>}` reference; the loader resolves it before the message decodes,
/// so a decoded message's header holds the namespace's id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigHeader {
    /// The namespace, as written.
    #[serde(default)]
    pub namespace: String,
    /// The object's name.
    #[serde(default)]
    pub name: String,
    /// The public id, read back only.
    #[serde(default, alias = "external_id")]
    pub external_id: String,
    /// The labels.
    #[serde(default)]
    pub labels: Labels,
}

/// A config message: the proto message a seed item decodes into (protobuf
/// JSON, through serde) and the binder writes and reads. Every serde type
/// qualifies; its header is read by its binder ([`TypedBinder::header`]),
/// not by a method on the message, because a buffa-generated message lives
/// in the tenant's proto crate, where a nanoservice cannot implement a
/// trait for it (the orphan rule).
pub trait ConfigMessage: Serialize + DeserializeOwned + Send + Sync + 'static {}

impl<M: Serialize + DeserializeOwned + Send + Sync + 'static> ConfigMessage for M {}

/// `NamespaceConfiguration`: the root scope object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceConfiguration {
    /// The header; `namespace` is empty for a namespace.
    #[serde(default)]
    pub header: ConfigHeader,
    /// A human-readable name.
    #[serde(default, alias = "display_name")]
    pub display_name: String,
}

/// Maps a config message onto its subtype table. One binder is registered
/// per config type; its methods run inside the loader's (or the
/// repository's) transaction, after the base row, with every reference
/// field in the message already resolved to an id.
pub trait TypedBinder: Send + Sync + 'static {
    /// The message type.
    type Msg: ConfigMessage;

    /// The type this binder writes.
    fn type_info(&self) -> TypeInfo;

    /// The message's header: its namespace, name and labels as written. A
    /// buffa message carries it as a message field, absent when unset.
    fn header(&self, msg: &Self::Msg) -> ConfigHeader;

    /// Writes (insert-or-update, `ON CONFLICT (id) DO UPDATE`) the subtype
    /// row(s) for `id` from `msg`, reconciling any nested rows.
    fn upsert(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
        msg: &Self::Msg,
    ) -> impl Future<Output = Result<(), BinderError>> + Send;

    /// Removes the subtype row(s) for `id`, nested rows first. The loader
    /// deletes the base row afterwards. A binder may refuse with
    /// [`BinderError::StillReferenced`] while another object still names
    /// the row; a prune retries it once the referrers are gone.
    fn delete(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
    ) -> impl Future<Output = Result<(), BinderError>> + Send;

    /// Reads the subtype row(s) for `id` back into a message, `None` when
    /// absent. The header need not be filled: the repository carries the
    /// base identity beside the message.
    fn read(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> impl Future<Output = Result<Option<Self::Msg>, BinderError>> + Send;
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The type-erased binder the loader and the repository drive.
pub(crate) trait Binder: Send + Sync {
    fn type_info(&self) -> TypeInfo;
    /// Decodes a resolved item body and returns its header.
    fn decode_header(&self, body: &Value) -> Result<ConfigHeader, BoxError>;
    /// Decodes a resolved item body and writes the subtype rows.
    fn apply<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
        body: Value,
    ) -> BoxFuture<'a, Result<(), BinderError>>;
    /// Removes the subtype rows.
    fn remove<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<(), BinderError>>;
    /// Reads the subtype rows as the message, erased.
    fn read<'a>(
        &'a self,
        conn: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Any + Send>>, BinderError>>;
}

/// A [`TypedBinder`] behind the erased interface.
struct Typed<B>(B);

impl<B: TypedBinder> Binder for Typed<B> {
    fn type_info(&self) -> TypeInfo {
        self.0.type_info()
    }

    fn decode_header(&self, body: &Value) -> Result<ConfigHeader, BoxError> {
        let msg: B::Msg = serde_json::from_value(body.clone())?;
        Ok(self.0.header(&msg))
    }

    fn apply<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
        body: Value,
    ) -> BoxFuture<'a, Result<(), BinderError>> {
        Box::pin(async move {
            let msg: B::Msg = serde_json::from_value(body)?;
            self.0.upsert(tx, id, &msg).await
        })
    }

    fn remove<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<(), BinderError>> {
        Box::pin(self.0.delete(tx, id))
    }

    fn read<'a>(
        &'a self,
        conn: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<Option<Box<dyn Any + Send>>, BinderError>> {
        Box::pin(async move {
            Ok(self
                .0
                .read(conn, id)
                .await?
                .map(|m| Box::new(m) as Box<dyn Any + Send>))
        })
    }
}

/// The namespace binder, over `basable_config.namespace_configuration`.
struct NamespaceBinder;

impl TypedBinder for NamespaceBinder {
    type Msg = NamespaceConfiguration;

    fn type_info(&self) -> TypeInfo {
        NAMESPACE_TYPE
    }

    fn header(&self, msg: &NamespaceConfiguration) -> ConfigHeader {
        msg.header.clone()
    }

    async fn upsert(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
        msg: &NamespaceConfiguration,
    ) -> Result<(), BinderError> {
        sqlx::query(
            "INSERT INTO basable_config.namespace_configuration (id, display_name) VALUES ($1, $2)
             ON CONFLICT (id) DO UPDATE SET display_name = EXCLUDED.display_name",
        )
        .bind(id)
        .bind(&msg.display_name)
        .execute(tx)
        .await?;
        Ok(())
    }

    async fn delete(&self, tx: &mut PgConnection, id: Uuid) -> Result<(), BinderError> {
        sqlx::query("DELETE FROM basable_config.namespace_configuration WHERE id = $1")
            .bind(id)
            .execute(tx)
            .await?;
        Ok(())
    }

    async fn read(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<Option<NamespaceConfiguration>, BinderError> {
        let row: Option<(Option<String>,)> = sqlx::query_as(
            "SELECT display_name FROM basable_config.namespace_configuration WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(conn)
        .await?;
        Ok(row.map(|(display_name,)| NamespaceConfiguration {
            header: ConfigHeader::default(),
            display_name: display_name.unwrap_or_default(),
        }))
    }
}

/// Collects the config types at boot. The namespace type is registered
/// from the start.
pub struct ConfigTypesBuilder {
    binders: Vec<Box<dyn Binder>>,
}

impl Default for ConfigTypesBuilder {
    fn default() -> Self {
        ConfigTypesBuilder::new()
    }
}

impl ConfigTypesBuilder {
    /// A builder holding the namespace type.
    pub fn new() -> ConfigTypesBuilder {
        let mut b = ConfigTypesBuilder {
            binders: Vec::new(),
        };
        b.register(NamespaceBinder);
        b
    }

    /// Registers a type through its binder.
    pub fn register<B: TypedBinder>(&mut self, binder: B) -> &mut Self {
        self.binders.push(Box::new(Typed(binder)));
        self
    }

    /// Validates the collected types: message-shaped names, prefixes by the
    /// public-id rule, and no two types sharing an id, a name or a prefix.
    /// The first problem is the error.
    pub fn build(self) -> Result<ConfigTypes, RegistryError> {
        let mut by_id: HashMap<i16, usize> = HashMap::new();
        let mut by_name: HashMap<&'static str, usize> = HashMap::new();
        let mut by_prefix: HashMap<&'static str, usize> = HashMap::new();
        let mut names: Vec<String> = Vec::with_capacity(self.binders.len());
        for (i, b) in self.binders.iter().enumerate() {
            let t = b.type_info();
            if !is_message_name(t.name) {
                return Err(RegistryError::InvalidTypeName {
                    name: t.name.to_owned(),
                });
            }
            if let Err(cause) = validate_public_id_prefix(t.prefix) {
                return Err(RegistryError::InvalidPrefix {
                    name: t.name.to_owned(),
                    cause,
                });
            }
            if let Some(prior) = by_id.insert(t.id, i) {
                return Err(RegistryError::DuplicateId {
                    id: t.id,
                    names: (names[prior].clone(), t.name.to_owned()),
                });
            }
            if let Some(prior) = by_name.insert(t.name, i) {
                return Err(RegistryError::DuplicateName {
                    name: t.name.to_owned(),
                    ids: (self.binders[prior].type_info().id, t.id),
                });
            }
            if let Some(prior) = by_prefix.insert(t.prefix, i) {
                return Err(RegistryError::DuplicatePrefix {
                    prefix: t.prefix.to_owned(),
                    names: (names[prior].clone(), t.name.to_owned()),
                });
            }
            names.push(t.name.to_owned());
        }
        Ok(ConfigTypes {
            binders: self.binders,
            by_id,
            by_name,
        })
    }
}

/// A proto message name: an upper-case letter followed by letters and
/// digits.
fn is_message_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
        && name.len() <= 128
}

/// The validated registry, shared behind an [`Arc`] by the loader, the
/// repository and every reader.
pub struct ConfigTypes {
    binders: Vec<Box<dyn Binder>>,
    by_id: HashMap<i16, usize>,
    by_name: HashMap<&'static str, usize>,
}

impl ConfigTypes {
    /// The type named `name`.
    pub fn type_by_name(&self, name: &str) -> Option<TypeInfo> {
        self.by_name.get(name).map(|&i| self.binders[i].type_info())
    }

    /// The type with id `id`.
    pub fn type_by_id(&self, id: i16) -> Option<TypeInfo> {
        self.by_id.get(&id).map(|&i| self.binders[i].type_info())
    }

    /// Every registered type, in registration order.
    pub fn types(&self) -> impl Iterator<Item = TypeInfo> + '_ {
        self.binders.iter().map(|b| b.type_info())
    }

    /// The `(name, prefix)` pairs for the public-id registry.
    pub fn public_ids(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.types().map(|t| (t.name, t.prefix))
    }

    /// Shares the registry.
    pub fn shared(self) -> Arc<ConfigTypes> {
        Arc::new(self)
    }

    pub(crate) fn binder(&self, id: i16) -> Option<&dyn Binder> {
        self.by_id.get(&id).map(|&i| &*self.binders[i])
    }
}

impl fmt::Debug for ConfigTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.types()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rule(TypeInfo);

    impl TypedBinder for Rule {
        type Msg = NamespaceConfiguration;

        fn type_info(&self) -> TypeInfo {
            self.0
        }

        fn header(&self, msg: &NamespaceConfiguration) -> ConfigHeader {
            msg.header.clone()
        }

        async fn upsert(
            &self,
            _tx: &mut PgConnection,
            _id: Uuid,
            _msg: &NamespaceConfiguration,
        ) -> Result<(), BinderError> {
            Ok(())
        }

        async fn delete(&self, _tx: &mut PgConnection, _id: Uuid) -> Result<(), BinderError> {
            Ok(())
        }

        async fn read(
            &self,
            _conn: &mut PgConnection,
            _id: Uuid,
        ) -> Result<Option<NamespaceConfiguration>, BinderError> {
            Ok(None)
        }
    }

    const RULE: TypeInfo = TypeInfo {
        id: 100,
        name: "PricingRuleConfiguration",
        prefix: "prule",
    };

    #[test]
    fn the_registry_resolves_by_name_and_id() {
        let mut b = ConfigTypesBuilder::new();
        b.register(Rule(RULE));
        let types = b.build().unwrap();
        assert_eq!(types.type_by_name("PricingRuleConfiguration"), Some(RULE));
        assert_eq!(types.type_by_id(100), Some(RULE));
        assert_eq!(
            types.type_by_name("NamespaceConfiguration"),
            Some(NAMESPACE_TYPE)
        );
        assert_eq!(types.type_by_name("pricing_rule"), None);
        assert_eq!(
            types.public_ids().collect::<Vec<_>>(),
            vec![
                ("NamespaceConfiguration", "ns"),
                ("PricingRuleConfiguration", "prule")
            ]
        );
        let header = types
            .binder(1)
            .unwrap()
            .decode_header(&serde_json::json!({
                "header": {"name": "billing", "labels": {"tier": "a"}},
                "display_name": "Billing"
            }))
            .unwrap();
        assert_eq!(header.name, "billing");
        assert_eq!(header.labels.get("tier").map(String::as_str), Some("a"));
    }

    #[test]
    fn the_registry_refuses_collisions_and_bad_names() {
        type Is = fn(&RegistryError) -> bool;
        let cases: Vec<(TypeInfo, Is)> = vec![
            (
                TypeInfo {
                    id: 1,
                    name: "Other",
                    prefix: "oth",
                },
                |e| matches!(e, RegistryError::DuplicateId { id: 1, .. }),
            ),
            (
                TypeInfo {
                    id: 101,
                    name: "NamespaceConfiguration",
                    prefix: "oth",
                },
                |e| matches!(e, RegistryError::DuplicateName { .. }),
            ),
            (
                TypeInfo {
                    id: 101,
                    name: "Other",
                    prefix: "ns",
                },
                |e| matches!(e, RegistryError::DuplicatePrefix { .. }),
            ),
            (
                TypeInfo {
                    id: 101,
                    name: "pricing_rule",
                    prefix: "pr",
                },
                |e| matches!(e, RegistryError::InvalidTypeName { .. }),
            ),
            (
                TypeInfo {
                    id: 101,
                    name: "PricingRule",
                    prefix: "p_r",
                },
                |e| matches!(e, RegistryError::InvalidPrefix { .. }),
            ),
        ];
        for (info, is) in cases {
            let mut b = ConfigTypesBuilder::new();
            b.register(Rule(info));
            let err = b.build().unwrap_err();
            assert!(is(&err), "{info:?}: {err}");
        }
    }
}
