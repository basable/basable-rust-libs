//! The config-type registry: every type's identity, its message type, and
//! the binder that writes it, collected at boot and refused on collision.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use basable_core::BoxError;
use basable_core::names::{validate_public_id_prefix, validate_type_name};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::error::RegistryError;

/// One configuration-object type: the `SMALLINT` its `configuration_type`
/// row carries, the name seed files and references use, and the prefix its
/// public ids carry. Declared as a `const` next to the message type it
/// describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeInfo {
    /// The registered id (`configuration_type.id`); 1 is the namespace.
    pub id: i16,
    /// The type name: lowercase `snake_case`, the `kind` of its seed files
    /// (compared ignoring case and separators) and the first token of a
    /// reference.
    pub name: &'static str,
    /// The public-id prefix.
    pub prefix: &'static str,
}

/// The namespace type — the root scope every other object is filed under.
/// Registered by the builder itself; a seed declares namespaces with
/// `kind: Namespace` and no `metadata.namespace`.
pub const NAMESPACE_TYPE: TypeInfo = TypeInfo {
    id: 1,
    name: "namespace",
    prefix: "ns",
};

/// The namespace message. Config messages spell their fields in camelCase
/// on the wire (the protobuf JSON convention the seed files follow).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
    /// A human-readable name, empty by default.
    #[serde(default)]
    pub display_name: String,
}

/// A config message: the type a seed item's `spec` decodes into and the
/// repository reads back. Any `Serialize + Deserialize` type qualifies.
pub trait ConfigMessage: Serialize + DeserializeOwned + Send + Sync + 'static {}

impl<T: Serialize + DeserializeOwned + Send + Sync + 'static> ConfigMessage for T {}

/// A binder for a type whose objects also live in the nanoservice's own
/// tables: `upsert` writes the subtype rows for the object id inside the
/// loader's (or the repository's) transaction, after the base row and
/// with every reference in `msg` already resolved to an id; `delete`
/// removes them before the base row goes. A type that lives in the
/// `spec` column alone needs no binder: [`ConfigTypesBuilder::register`].
pub trait TypedBinder: Send + Sync + 'static {
    /// The message type.
    type Msg: ConfigMessage;

    /// The type this binder writes.
    fn type_info(&self) -> TypeInfo;

    /// Writes (insert-or-update) the subtype rows for `id` from `msg`.
    fn upsert(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
        msg: &Self::Msg,
    ) -> impl Future<Output = Result<(), BoxError>> + Send;

    /// Removes the subtype rows for `id`.
    fn delete(
        &self,
        tx: &mut PgConnection,
        id: Uuid,
    ) -> impl Future<Output = Result<(), BoxError>> + Send;
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The type-erased binder the loader and the repository drive.
pub(crate) trait Binder: Send + Sync {
    fn type_info(&self) -> TypeInfo;
    /// Decodes `spec` as the message and re-encodes it: the canonical form
    /// the base row stores, and the proof the spec is well-formed.
    fn canonicalize(&self, spec: Value) -> Result<Value, BoxError>;
    /// Decodes `spec` and writes the subtype rows.
    fn apply<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
        spec: Value,
    ) -> BoxFuture<'a, Result<(), BoxError>>;
    /// Removes the subtype rows.
    fn remove<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<(), BoxError>>;
}

/// The binder of a type that lives in the `spec` column alone.
struct SpecOnly<M> {
    info: TypeInfo,
    _msg: std::marker::PhantomData<fn() -> M>,
}

impl<M: ConfigMessage> Binder for SpecOnly<M> {
    fn type_info(&self) -> TypeInfo {
        self.info
    }

    fn canonicalize(&self, spec: Value) -> Result<Value, BoxError> {
        let msg: M = serde_json::from_value(spec)?;
        Ok(serde_json::to_value(&msg)?)
    }

    fn apply<'a>(
        &'a self,
        _tx: &'a mut PgConnection,
        _id: Uuid,
        spec: Value,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        let decoded = serde_json::from_value::<M>(spec)
            .map(|_| ())
            .map_err(BoxError::from);
        Box::pin(async move { decoded })
    }

    fn remove<'a>(
        &'a self,
        _tx: &'a mut PgConnection,
        _id: Uuid,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A [`TypedBinder`] behind the erased interface.
struct Typed<B>(B);

impl<B: TypedBinder> Binder for Typed<B> {
    fn type_info(&self) -> TypeInfo {
        self.0.type_info()
    }

    fn canonicalize(&self, spec: Value) -> Result<Value, BoxError> {
        let msg: B::Msg = serde_json::from_value(spec)?;
        Ok(serde_json::to_value(&msg)?)
    }

    fn apply<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
        spec: Value,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(async move {
            let msg: B::Msg = serde_json::from_value(spec)?;
            self.0.upsert(tx, id, &msg).await
        })
    }

    fn remove<'a>(
        &'a self,
        tx: &'a mut PgConnection,
        id: Uuid,
    ) -> BoxFuture<'a, Result<(), BoxError>> {
        Box::pin(self.0.delete(tx, id))
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
        b.register::<Namespace>(NAMESPACE_TYPE);
        b
    }

    /// Registers a type whose objects live in the `spec` column alone,
    /// decoded as `M`.
    pub fn register<M: ConfigMessage>(&mut self, info: TypeInfo) -> &mut Self {
        self.binders.push(Box::new(SpecOnly::<M> {
            info,
            _msg: std::marker::PhantomData,
        }));
        self
    }

    /// Registers a type with a binder that also writes the nanoservice's
    /// own tables.
    pub fn register_binder<B: TypedBinder>(&mut self, binder: B) -> &mut Self {
        self.binders.push(Box::new(Typed(binder)));
        self
    }

    /// Validates the collected types: names and prefixes by the registry
    /// rules, and no two types sharing an id, a name or a prefix. The first
    /// problem is the error.
    pub fn build(self) -> Result<ConfigTypes, RegistryError> {
        let mut by_id: HashMap<i16, usize> = HashMap::new();
        let mut by_name: HashMap<&'static str, usize> = HashMap::new();
        let mut by_prefix: HashMap<&'static str, usize> = HashMap::new();
        let mut types: Vec<String> = Vec::with_capacity(self.binders.len());
        for (i, b) in self.binders.iter().enumerate() {
            let t = b.type_info();
            if let Err(cause) = validate_type_name(t.name) {
                return Err(RegistryError::InvalidTypeName {
                    name: t.name.to_owned(),
                    cause,
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
                    names: (types[prior].clone(), t.name.to_owned()),
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
                    names: (types[prior].clone(), t.name.to_owned()),
                });
            }
            types.push(t.name.to_owned());
        }
        Ok(ConfigTypes {
            binders: self.binders,
            by_id,
            by_name,
        })
    }
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

    /// The type a seed file's `kind` names: `kind` and the type name are
    /// compared ignoring case and separators, so `PricingRule`,
    /// `pricing_rule` and `pricing-rule` all name `pricing_rule`.
    pub fn type_by_kind(&self, kind: &str) -> Option<TypeInfo> {
        let wanted = normalize(kind);
        self.binders
            .iter()
            .map(|b| b.type_info())
            .find(|t| normalize(t.name) == wanted)
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

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize, serde::Deserialize)]
    struct Rule {
        rate: i64,
    }

    const RULE: TypeInfo = TypeInfo {
        id: 100,
        name: "pricing_rule",
        prefix: "prule",
    };

    #[test]
    fn the_registry_resolves_by_name_id_and_kind() {
        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(RULE);
        let types = b.build().unwrap();
        assert_eq!(types.type_by_name("pricing_rule"), Some(RULE));
        assert_eq!(types.type_by_id(100), Some(RULE));
        assert_eq!(types.type_by_kind("PricingRule"), Some(RULE));
        assert_eq!(types.type_by_kind("pricing-rule"), Some(RULE));
        assert_eq!(types.type_by_kind("Namespace"), Some(NAMESPACE_TYPE));
        assert_eq!(types.type_by_kind("Order"), None);
        assert_eq!(
            types.public_ids().collect::<Vec<_>>(),
            vec![("namespace", "ns"), ("pricing_rule", "prule")]
        );
        let canonical = types
            .binder(100)
            .unwrap()
            .canonicalize(serde_json::json!({"rate": 3, "extra": true}))
            .unwrap();
        assert_eq!(
            canonical,
            serde_json::json!({"rate": 3}),
            "unknown keys drop"
        );
        assert!(
            types
                .binder(100)
                .unwrap()
                .canonicalize(serde_json::json!({"rate": "x"}))
                .is_err()
        );
    }

    #[test]
    fn the_registry_refuses_collisions_and_bad_names() {
        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(TypeInfo {
            id: 1,
            name: "other",
            prefix: "oth",
        });
        assert!(matches!(
            b.build(),
            Err(RegistryError::DuplicateId { id: 1, .. })
        ));

        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(RULE).register::<Rule>(TypeInfo {
            id: 101,
            name: "pricing_rule",
            prefix: "other",
        });
        assert!(matches!(
            b.build(),
            Err(RegistryError::DuplicateName { .. })
        ));

        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(TypeInfo {
            id: 101,
            name: "other",
            prefix: "ns",
        });
        assert!(matches!(
            b.build(),
            Err(RegistryError::DuplicatePrefix { .. })
        ));

        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(TypeInfo {
            id: 101,
            name: "PricingRule",
            prefix: "pr",
        });
        assert!(matches!(
            b.build(),
            Err(RegistryError::InvalidTypeName { .. })
        ));

        let mut b = ConfigTypesBuilder::new();
        b.register::<Rule>(TypeInfo {
            id: 101,
            name: "pricing_rule",
            prefix: "p_r",
        });
        assert!(matches!(
            b.build(),
            Err(RegistryError::InvalidPrefix { .. })
        ));
    }
}
