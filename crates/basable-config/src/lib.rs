//! The declarative configuration framework: catalogs a person edits as JSON
//! under `config/base/` and the app loads at boot. A port of the basable
//! monorepo's `golang/lib/config` — the same storage, the same files, the
//! same loader — into the tenant's `basable_config` schema.
//!
//! - **Storage** is temporal class-table inheritance: a base
//!   `configuration_object` row (identity, natural key, namespace, labels)
//!   plus one subtype table per type, each live table inheriting its
//!   `_history` table, with the `versioning()` trigger closing versions on
//!   every change and writing history — never application code. Re-running
//!   unchanged files writes zero history rows.
//! - **A type** is a [`TypeInfo`] (id, proto message name, public-id
//!   prefix) and a [`TypedBinder`] that writes and reads its subtype table,
//!   registered at boot on [`ConfigTypesBuilder`]; a duplicate id, name or
//!   prefix is a boot error. The message is the proto message (buffa,
//!   protobuf JSON through serde) carrying a [`ConfigHeader`].
//! - **Seed files** are `{configSetName, items: [{"@type": "<pkg>.<Message>",
//!   header: {namespace: "#{NamespaceConfiguration:x}", name, labels},
//!   …fields}]}`, scoped to environments by `<name>[.<env>…].json`; a string
//!   value anywhere may reference another object as `#{Type:namespace:name}`
//!   and arrives in the message as that object's id.
//! - [`Loader::load`] reads a directory for one [`Environment`], checks that
//!   every dependency is declared in the same file set, orders the items so
//!   dependencies come first, and applies them in ONE transaction under a
//!   table lock, matching by natural key; then it prunes every
//!   loader-managed object the files no longer declare. Removing an item is
//!   the delete; there is no delete marker.
//! - [`Repository`] serves runtime reads (`get`, `list`, `lookup_id`,
//!   `namespace_id`) and the programmatic write (`upsert`, `delete`),
//!   stamped `basable.com/managed-by=runtime` where the loader stamps
//!   `config`, so runtime objects survive every load and the loader adopts
//!   one the first time the files declare it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod binder;
mod error;
mod item;
mod loader;
mod reference;
mod repository;
mod store;
mod types;

pub use binder::{parse_json, parse_reference, parse_timestamp};
pub use error::{BinderError, ConfigError, RegistryError};
pub use item::{Environment, Item, ItemName, file_applies_to, parse_item, parse_seed};
pub use loader::{LoadResult, Loader, load_seed};
pub use repository::{Object, Repository};
pub use store::{MANAGED_BY_CONFIG, MANAGED_BY_LABEL, MANAGED_BY_RUNTIME};
pub use types::{
    ConfigHeader, ConfigMessage, ConfigTypes, ConfigTypesBuilder, NAMESPACE_TYPE,
    NamespaceConfiguration, TypeInfo, TypedBinder,
};
