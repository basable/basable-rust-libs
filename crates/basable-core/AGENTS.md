# basable-core — the types every framework crate shares

The leaf of the basable crates: the handful of types the frameworks and a
tenant's nanoservices all speak, and nothing else. It has no dependencies.
It ports the pieces of the monorepo that had no package of their own —
the naming rules of `golang/controller/lib/processingobject/type.go`, the
two-clock ownership proof of `claim.go`'s `requireProof`, the label charset
of `golang/lib/labels`, the application error of
`golang/controller/lib/messages/apperror`, and what Go passed around as
`context.Context`. Semantics are the Go ones except where the type system
makes a rule mechanical (`docs/porting-notes.md` 1–4).

The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves.

## The surface

| Item | What it is | Who uses it |
|---|---|---|
| `names::validate_type_name` | A processing-object type name: lowercase `snake_case`, starts with a letter, at most `MAX_TYPE_NAME_LEN` (64). It is a SQL identifier fragment (the partition table), so everything that builds DDL from it applies exactly this rule | the store, the scaffolder, `basable-db`'s `Nanoservice::NAME` |
| `names::validate_public_id_prefix` | A public-id prefix: lowercase letters, at most `MAX_PUBLIC_ID_PREFIX_LEN` (16) | `basable-publicid`'s registry |
| `labels::{Labels, validate_labels, validate_label_key, validate_label_value}` | `Labels` is a `BTreeMap<String, String>` (ordered, so equal sets render identically); at most `MAX_LABELS` (8) pairs of at most `MAX_LABEL_LEN` (63) characters. The charset excludes `'` and `\`, which is what makes a validated selector injection-safe as an inline SQL literal; `/` is admitted in keys only (`basable.com/infra-type`) | processing-object labels and worker selectors |
| `Deadline` | A horizon held against BOTH clocks: `after(d)`, `is_live`, `has_passed`, `remaining`, `max`, `mono`, `wall`. The monotonic clock freezes across suspend, the wall clock can be stepped; a live deadline requires both. A passed deadline cannot be extended, only replaced | the claim's local ownership proof, `Ctx`, the effect dispatch bound |
| `BoxError` | `Box<dyn Error + Send + Sync + 'static>`: the error a caller only displays or walks (`?` converts any error, `String` or `&str`) | reconciler retry causes, provider transport failures, `AppError`'s source |
| `AppError`, `Code` | The application error with the sixteen Connect codes (`Code::name`, `from_name`, `http_status`, `as_u8`); `new`, `wrap`, `with_source`, one constructor per code (`invalid_argument`, `not_found`, `unimplemented`, …), `code`, `message`, `code_of` and `find` (walk a `source()` chain for the first `AppError`), `payment_required` / `is_payment_required` (a `FailedPrecondition` whose message starts with `PAYMENT_REQUIRED_MESSAGE`) | the Connect boundary, every handler |
| `Ctx` | The request context: `new(request_id)`, `background()`, `request_id`, `deadline`, `with_timeout` / `with_deadline`, `with_value` / `value::<T>` (typed values: the identity `basable-auth` attaches), `child`, `cancel`, `is_done`, `is_cancelled`, `cancelled()` (a future) | every handler, reconciler, effect call |

## Rules a consumer follows

- **Errors are typed where a consumer decides, boxed where nobody does.**
  `InvalidName` and `InvalidLabel` are enums with their `Display` and
  `Error` impls written out; no `anyhow`, no `thiserror` anywhere in the
  crates or the templates. `AppError::Display` includes the source on
  purpose: the platform logs errors as `%err` and has no reporter that
  would print the chain.
- **`Ctx` is a value, not a timer.** A child narrows its parent (a shorter
  deadline, one more value) and is cancelled whenever the parent is;
  nothing in here spawns or sleeps. The runtime that owns the request
  sleeps for `remaining()`.
- **A `Deadline` is compared, never extended.** A passed one cannot be
  revived, only replaced by a new value (`max` keeps the later of two); the
  processing-object claim's proof follows that rule.
- **Codes map one to one onto Connect.** `basable-connect` converts
  `AppError` to a `ConnectError` by code name; a code this crate does not
  carry cannot cross the boundary.

## What the tests pin

Unit tests in each module. `deadline.rs`: a future deadline is live and a
past one is not, a stepped wall clock fences even when the monotonic clock
is fine, `max` keeps the later horizon. `error.rs`: sixteen codes with
unique names and wire values, `Display` and `source` follow the Go shape,
`code_of` walks the source chain and defaults to `Internal`,
`payment_required` is recognisable. `ctx.rs`: values are typed and
inherited, a child deadline never extends the parent, cancel flows down and
wakes waiters, a passed deadline is done without a cancel. `labels.rs`: the
platform shapes are accepted, what would break a SQL literal is refused,
keys start with a letter and sizes are bounded. `names.rs`: type names
follow the Go rule, prefixes are lowercase letters.

## Porting notes

`docs/porting-notes.md` 1 (sixteen codes, the Go-to-Connect mapping),
2 (typed enums or `BoxError`), 3 (`Deadline` as a two-reading value),
4 (`Ctx` as a value).

## File map

| File | Responsibility |
|---|---|
| `src/names.rs` | `InvalidName`, the two validators and their limits |
| `src/labels.rs` | `Labels`, `InvalidLabel`, the three validators and their limits |
| `src/deadline.rs` | `Deadline` |
| `src/error.rs` | `BoxError`, `Code`, `AppError`, `PAYMENT_REQUIRED_MESSAGE` |
| `src/ctx.rs` | `Ctx`, `Cancelled` |
