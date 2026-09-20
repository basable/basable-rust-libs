# Porting notes

Every place the Rust crates deviate from the Go originals in the basable
monorepo, with the reason. The rule is: identical semantics unless the type
system makes an invariant mechanical, and then the deviation is written down
here.

## basable-core

1. **`AppError` carries the sixteen Connect codes, not Go's nine.** Go's
   `apperror.Code` is a local enum the server framework maps onto gRPC
   statuses; a tenant's boundary is connect-rust, so the port carries the
   wire vocabulary directly. The mapping: `InvalidInput` → `InvalidArgument`,
   `Unauthorized` → `Unauthenticated`, `NotImplemented` → `Unimplemented`,
   `Conflict` → `FailedPrecondition` (which is also what Go's server mapped
   it to), `PaymentRequired` → `FailedPrecondition` with the
   `PAYMENT_REQUIRED_MESSAGE` discriminator the frontend matches on, kept
   because the reason for the magic string (no structured detail in the
   status) still holds.
2. **`Cause` is a box, not `std::error::Error`.** Like `anyhow::Error`: the
   reflexive `From<Cause> for Cause` would otherwise collide with the blanket
   conversion that makes `?` work. It derefs to the boxed error, and
   `chain()` / `find::<T>()` are `errors.Is` / `errors.As`.
3. **`Deadline` is a value with both clock readings.** Go's `requireProof`
   checks `time.Now()` against a `time.Time` twice (with and without the
   monotonic reading). The port stores both readings and `is_live` applies
   the same two-clock rule; a passed deadline cannot be extended, only
   replaced (`max`).
4. **`Ctx` is a value, not an interface.** Request id, deadline, typed values
   and cancellation in one cheap-to-clone struct; a child narrows its parent
   and cancels with it. Deadlines are not timers: the runtime that owns the
   request sleeps for `remaining()`.

## basable-publicid

5. **The registry is an explicit boot-time builder, not `init()`.** Go's
   `publicid` imports both type systems and builds its registry under a
   `sync.Once`, panicking on a collision at the first call. The port's
   `Registry::builder().register_all(..).build()` returns the collision as an
   error at boot (`RegistryError`); `encode`/`decode_typed` on an unregistered
   NAME still panic, as in Go, because that is a programmer error.
6. **The encoding is byte-identical.** `tests/go_fixture.rs` pins twenty ids
   the Go implementation produced, including the leading-zero-byte cases the
   format is lenient about on decode (`proj_abc` = `proj_0abc`).
