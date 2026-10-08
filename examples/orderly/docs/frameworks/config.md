# basable-config (crates 0.3.0)

Declarative catalogs: a person edits JSON under `config/base/`, the app
loads it at boot. The storage, the files and the loader are the basable
platform's own configuration system.

**Storage** is temporal class-table inheritance in the `basable_config`
schema: a base `configuration_object` row per object (id, a type-prefixed
`external_id`, the natural key `(type, namespace, name)`, `labels`) plus one
subtype table per type holding the fields; every live table inherits its
`_history` table, and the `versioning()` trigger closes a version on each
change and writes history — application code never does. Re-applying
unchanged files writes zero history rows.

**A type** is a proto message in `proto/<nanoservice>/v1/config.proto`
embedding `basable.config.v1.ConfigHeader`, a `TypeInfo { id, name, prefix }`
(the id allocated by the scaffolder and registered by the migration that
creates the subtype tables), and a `TypedBinder` (`upsert(tx, id, msg)`,
`delete(tx, id)`, `read(conn, id)`) in the nanoservice's `config.rs`, all
rendered from `owns.configTypes`. Binders are registered at boot on
`ConfigTypesBuilder`; a duplicate id, name or prefix is a boot error. A
reference field is a `uuid` in the plan: a `string` in the message, a
nullable `UUID` peer FK (`ON DELETE SET NULL`) in the table.

**Seed files** are `{configSetName, items: [{"@type": "<nano>.v1.<Name>
Configuration", header: {namespace: "#{NamespaceConfiguration:x}", name,
labels}, …fields}]}` in protobuf JSON; `<name>.<env>.json` scopes a file to
environments; a string value may reference another object as
`#{Type:namespace:name}` and arrives in the message as that object's id.

`Loader::load(dir, env)`: read the files in scope, check every dependency
(each reference and the object's namespace) is declared in the same file
set, order the items so dependencies come first, then ONE transaction under
a table lock (replicas booting together serialise): apply every item by
natural key — the base row, then the binder — and prune every object the
loader manages (`basable.com/managed-by=config`) that the run did not apply,
namespaces last, retrying a binder's "still referenced" refusal until the
referrers are gone. Removing an item is the delete. `load_seed` is the boot
entry point (an absent directory is a no-op). `Repository::{get, list,
lookup_id, namespace_id}` serve runtime reads through the binders;
`Repository::{upsert, delete}` the rare programmatic write, stamped
`managed-by=runtime` — such objects survive every load, and the loader
adopts one the first time the files declare its name.

Who may write a catalog at runtime is the nanoservice's decision,
documented in its AGENTS.md.
