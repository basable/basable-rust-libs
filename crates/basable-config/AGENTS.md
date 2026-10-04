# basable-config — the declarative configuration framework

The port of the monorepo's `golang/lib/config` into a tenant's
`basable_config` schema: the same storage (temporal class-table
inheritance, the `versioning()` trigger), the same seed files, the same
loader with its prune, the same repository for runtime reads and writes.
What the Go package carried beyond the framework — the platform's eighteen
types, slug allocation, contacts, the pricing cache — stays in the
monorepo; the port is the registry, the loader and the repository over
any registered type (`docs/porting-notes.md` 48–54 and 77 list what Rust
changed). The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: a config type is declarative state a person
edits, loaded at boot, never written by a reconciler.

## The model

| Piece | What it is |
|---|---|
| Storage | A base `configuration_object` row (identity, natural key `(namespace, name)`, labels) plus one subtype table per type; every live table inherits its `_history` table and the `versioning()` trigger closes versions on change. Re-applying unchanged files writes zero history rows |
| `TypeInfo` | `{ id: i16, name: "<Pascal>Configuration", prefix }`: the registry row, the proto message name, the public-id prefix. `NAMESPACE_TYPE` is built in |
| `TypedBinder` | One per type, registered on `ConfigTypesBuilder`: `type Msg`, `type_info`, `header(&Msg) -> ConfigHeader`, `upsert(tx, id, &Msg)` (`ON CONFLICT (id) DO UPDATE`), `delete(tx, id)` (may refuse with `BinderError::StillReferenced`), `read(conn, id) -> Option<Msg>` |
| `ConfigMessage` | A blanket marker over `Serialize + DeserializeOwned + Send + Sync + 'static`: the buffa proto message, through protobuf JSON. The header is the BINDER's to read (`TypedBinder::header`), because a generated message lives in the tenant's `proto` crate where a nanoservice cannot implement a trait for it (porting note 77) |
| `ConfigHeader` | The crate's own copy of the proto's four fields: `namespace`, `name`, `external_id` (read back only), `labels` |
| `ConfigTypesBuilder::build()` | Refuses a duplicate id, name or prefix (`RegistryError`); the result is `ConfigTypes` (`type_by_name`, `type_by_id`, `types`, `public_ids`, `shared()` → `Arc`) |

## Seed files and the loader

A seed file is `{configSetName, items: [{"@type": "<pkg>.<Message>",
header: {namespace: "#{NamespaceConfiguration:x}", name, labels}, …}]}`,
scoped by `<name>[.<env>…].json` (`Environment`, `file_applies_to`);
`header.namespace` MUST be a namespace reference, a bare name is rejected.
Any string value may reference another object as `#{Type:namespace:name}`
and arrives in the message as that object's id; references are found by
walking the decoded JSON values, not by a regex over bytes (note 53), and
`parse_reference` / `parse_timestamp` / `parse_json` are the binder-side
decoders for reference, timestamp and JSON columns.

`Loader::load(dir, env)` is ONE transaction: `validate_deps` (every
reference must be declared in the same file set, a dangling one fails
before any write), `topo_sort`, `LOCK TABLE basable_config.configuration_object
IN SHARE ROW EXCLUSIVE MODE` (the table lock replaces Go's advisory lock,
note 52), apply in dependency order matching by natural key, then prune:
every object labelled `basable.com/managed-by=config` the run did not apply
is deleted, namespaces last, a `StillReferenced` refusal retried until a
pass frees nothing. `LoadResult` counts created, updated and deleted.
`load_seed` is the boot-time call over the project's `config/base`.

## The repository

`Repository::new(pool, types)` serves runtime reads and the programmatic
write: `get` / `get_by_id` / `list` return `Object<M>` (the base identity
beside the message, hydrated through the binder's `read`), `lookup_id` and
`namespace_id` resolve natural keys, `upsert` and `delete` write stamped
`basable.com/managed-by=runtime`. A runtime object survives every load; the
loader adopts it the first time the files declare it (and then owns it).
Removing an item from the files is the delete; there is no delete marker.

## Rules for a consumer

- Register every type's binder before `load_seed`; the loader refuses an
  item whose `@type` is unknown.
- Config reads go through `Repository`, never through SQL on
  `basable_config` tables from a nanoservice schema (table ownership, the
  Directive §2).
- A binder's `delete` refuses while another object still names the row;
  do not cascade from inside a binder.
- `upsert` is idempotent by construction (`ON CONFLICT (id)`); a binder
  must write every column it owns.

## Tests

`tests/loader.rs`, over a real database (`TEST_DATABASE_URL`) with the
`PricingRuleConfiguration` fixture binder rendered as the scaffolder would:
a load creates, updates and prunes across the seed directories under
`tests/seed/` (`v1` → `v2` → `v3`), environment-scoped files and the
directory are checked, a dangling reference fails before any write, the
repository writes by natural key and the loader adopts, and concurrent
seed loads serialise. `basable_config_test` covers the registry refusals
and the item parser.

## File map

| File | Responsibility |
|---|---|
| `src/types.rs` | `TypeInfo`, `NAMESPACE_TYPE`, `ConfigHeader`, `ConfigMessage`, `NamespaceConfiguration`, `TypedBinder`, the erased `Binder`, `ConfigTypesBuilder`, `ConfigTypes` |
| `src/item.rs` | `Environment`, `file_applies_to`, `ItemName`, `Item`, `parse_item`, `parse_seed` |
| `src/reference.rs` | `#{…}` discovery, `validate_deps`, `topo_sort`, `resolve_refs` |
| `src/loader.rs` | `Loader`, `LoadResult`, `load_seed`, the prune |
| `src/repository.rs` | `Repository`, `Object<M>` |
| `src/store.rs` | The base-row SQL, `MANAGED_BY_LABEL` / `MANAGED_BY_CONFIG` / `MANAGED_BY_RUNTIME`, `stamped` |
| `src/binder.rs` | `parse_reference`, `parse_timestamp`, `parse_json` |
| `src/error.rs` | `RegistryError`, `BinderError` (`is_still_referenced`), `ConfigError` |
