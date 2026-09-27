//! The declarative configuration framework: catalogs a person edits as JSON
//! under `config/base/` and the app loads at boot. A port of the basable
//! monorepo's `golang/lib/config` onto the tenant's `basable_config` schema
//! (`configuration_type`, `configuration_object` with a JSONB `spec` and a
//! version, `configuration_object_history`).
//!
//! - A config type is a [`TypeInfo`] (id, name, public-id prefix) and a
//!   message type that is `Serialize + Deserialize`, registered at boot on
//!   [`ConfigTypesBuilder`]; a duplicate id, name or prefix is a boot error.
//!   A type whose objects also live in the nanoservice's own tables registers
//!   a [`TypedBinder`] instead, which the loader and the repository call
//!   inside their transaction.
//! - A seed file is `{apiVersion, kind, items: [{metadata: {namespace, name,
//!   labels}, spec, operation}]}`; `kind` names the type, `<name>[.<env>…]
//!   .json` scopes the file to environments, and a string value anywhere in
//!   `spec` may reference another object as `#{type:namespace:name}` (or
//!   `#{namespace:name}` for a namespace), which the loader replaces with
//!   that object's id.
//! - [`Loader::load`] reads a directory for one [`Environment`], checks that
//!   every dependency is declared in the same file set, orders the items so
//!   dependencies come first, and applies them in ONE transaction under a
//!   table lock, matching by natural key: an unchanged object writes
//!   nothing, a changed one gets a history row and a new version, an
//!   `operation: delete` item removes the object. The loader never prunes.
//! - [`Repository`] serves runtime reads (`get`, `list`, `lookup_id`,
//!   `namespace_id`) and the rare programmatic write (`upsert`, `delete`),
//!   stamped `basable.com/managed-by=runtime` where the loader stamps
//!   `config`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod item;
mod loader;
mod reference;
mod repository;
mod store;
mod types;

pub use error::{ConfigError, RegistryError};
pub use item::{Environment, Item, ItemName, Operation, file_applies_to, parse_seed};
pub use loader::{LoadResult, Loader, canonical_spec, load_seed};
pub use repository::{Object, Repository};
pub use store::{MANAGED_BY_CONFIG, MANAGED_BY_LABEL, MANAGED_BY_RUNTIME};
pub use types::{
    ConfigMessage, ConfigTypes, ConfigTypesBuilder, NAMESPACE_TYPE, Namespace, TypeInfo,
    TypedBinder,
};
