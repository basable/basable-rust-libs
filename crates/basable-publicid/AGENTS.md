# basable-publicid — public, base62, type-prefixed ids

The port of the monorepo's `golang/lib/publicid`: a UUID primary key is
exposed in URLs and APIs as `proj_3kFz…`, a prefix naming the resource type
and a base62 payload, so a raw UUID never leaks its format. The encoding is
byte-identical to Go's — `tests/go_fixture.rs` pins twenty ids the Go
implementation produced — and the registry that makes ids decodable is the
one Go built, moved from `init()` to an explicit boot step
(`docs/porting-notes.md` 5–6).

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: a processing-object type and a config type each
declare their prefix, and this crate is where those prefixes meet.

## Two layers

| Layer | API | For whom |
|---|---|---|
| Bare format | `encode(prefix, id)`, `decode_with_prefix(public_id) -> (prefix, Uuid)` | the type systems' own encoders, which know their prefix (the processing-object store derives `external_id` with it at create) |
| Registry | `Registry::builder().register(name, prefix).register_all(..).build()`, then `encode(name, id)`, `decode(public_id) -> (&ResourceType, Uuid)`, `decode_typed(name, public_id) -> Uuid`, `is_type(name, public_id)`, `type_by_name`, `type_by_prefix`, `types()` | the boundary: a gateway that receives an id and must know which type it names |

`ResourceType { name, prefix }` is one registered type (`tenant` / `proj`,
`OrganisationConfiguration` / `org`). The registry is built ONCE at boot
from the config-type and processing-object-type lists and shared behind an
`Arc`.

## Rules

- **A collision is a boot error.** `build()` returns `RegistryError::{
  EmptyName, InvalidPrefix, DuplicateName, DuplicatePrefix}` against the
  full list; nothing panics at first use. Encoding or `decode_typed` on a
  NAME that was never registered still panics, as in Go: that is a
  programmer error, not data.
- **Prefixes follow the core rule.** `basable_core::names::validate_public_id_prefix`
  (lowercase letters, at most 16) is applied at `build()`; the scaffolder
  applies the same rule to a manifest before a crate exists.
- **Decode is lenient about width, strict about the alphabet.** The payload
  is left-padded, so `proj_abc` and `proj_0abc` name the same UUID; a
  character outside base62 is `DecodeError::InvalidCharacter`, a value over
  16 bytes is `Overflow`. `encode` only ever emits the full-width form. The
  other refusals: `Malformed` (no `_`, empty prefix or payload),
  `UnknownPrefix`, `WrongType { expected, got }`.
- **An id is derived once from a random UUID and never reassigned.** The
  envelope stores it as `external_id`; a type's prefix cannot change
  without every id of that type changing.

## What the tests pin

- `tests/go_fixture.rs`: the twenty Go ids (the zero and all-ones UUIDs,
  leading-zero-byte cases, high-bit cases, random values) encode
  identically and decode back through the registry and the bare layer.
- `src/lib.rs` unit tests: random and zero UUIDs round-trip, the wrong
  type is refused, `is_type` accepts only its own well-formed ids, short
  payloads alias their padded form, every malformed shape is named, the
  builder refuses collisions and bad prefixes, an unregistered name panics.
- `src/base62.rs`: leading zero bytes become leading zero characters,
  decode inverts encode for every width.

## Porting notes

`docs/porting-notes.md` 5 (the explicit boot-time builder), 6 (the
byte-identical encoding).

## File map

| File | Responsibility |
|---|---|
| `src/lib.rs` | `encode`, `decode_with_prefix`, `DecodeError`, `ResourceType`, `RegistryError`, `Registry`, `RegistryBuilder` |
| `src/base62.rs` | The alphabet, fixed-width encode, lenient decode |
| `tests/go_fixture.rs`, `tests/fixtures/go_ids.txt` | The Go parity fixture (generated 2026-09-20 from the monorepo at `a51e41e4`) |
