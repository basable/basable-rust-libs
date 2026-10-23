# basable-ledger and basable-ledger-postgres (crates 0.7.0)

A declarative double-entry ledger (the Directive §11). One nanoservice owns
it (`accounting`, `owns.ledger` in the manifest); every nanoservice
whose reconcile pass changes something money-relevant sends it a snapshot of
the object, and the ledger books the difference between what that object's
state asks for and what is booked. Nothing is written per event, and nothing
is ever edited: corrections, amount changes and rule changes all fall out of
the same comparison.

```rust
pub trait LedgerObject: Serialize + DeserializeOwned + Send + Sync + 'static {
    const TYPE: &'static str;                       // snake_case; stored with every diff
    type State: Copy + Eq + Hash + Debug;           // the accounting states
    fn process_id(&self) -> ProcessId;              // the consistency boundary; a root's own id
    fn object_id(&self) -> ObjectId;
    fn parent_id(&self) -> Option<ObjectId> { None }
    fn version(&self) -> u64;                       // strictly increasing per object
    fn state(&self) -> Self::State;
}
pub trait Accounting<O: LedgerObject>: Send + Sync + 'static {
    const VERSION: u32;                             // bump to restate every process booked under an older number
    fn validate_state_change(current: Option<O::State>) -> &'static [O::State];
    fn desired_state(state: O::State, object: &O) -> Desired;     // pure: reads the object, names batches
    fn validate_balances(state: O::State, object: &O, current: &Balances, desired: &Desired) -> Result<(), Rejection> { Ok(()) }
}
pub trait BatchAccount { fn batch_key(&self) -> BatchKey; fn ledger(&self) -> LedgerKey; fn batch_type(&self) -> BatchType; } // Debit | Credit | Memo
pub trait Unit { fn unit_key(&self) -> UnitKey; }  // implemented for u32 and UnitKey
pub fn balance(account: impl BatchAccount, unit: impl Unit, amount: i128) -> Position;  // desired![balance(..), ..]
impl<S: Store> Ledger<S> {
    pub async fn book(&self, request: Request) -> Result<Vec<Outcome>, LedgerError>;   // Booked | AlreadyBooked | Stale | Rejected(reason)
    pub async fn balances(&self, process: ProcessId) -> Result<Balances, LedgerError>;
    pub async fn restate(&self, process: ProcessId) -> Result<Restated, LedgerError>;
    pub async fn preview_restatement(&self, process: ProcessId) -> Result<RestatementPreview, LedgerError>;
    pub async fn restate_behind(&self) -> Result<RestatementReport, LedgerError>;     // once, at boot
}
```

**The snapshot** (`crates/messages/src/ledger.rs`, `<Type>Snapshot`) is the
whole object as its producer knows it: identity, a version, the accounting
state and the fields the rules read, amounts as `i128` minor units through
`basable_ledger::amount_string`. Its serde form is its stored form: the
first time a process sees an object the ledger stores it whole, afterwards
as a JSON merge patch against the folded object, so adding a field is free
and renaming one is a migration of the stored diffs. The producer bumps the
version in the same status write that changes a state or an amount, and
sends a `Book` from the reconcile pass (a direct messenger request, the
Directive §7, never an event). A higher version books, an equal one answers
`AlreadyBooked`, a lower one `Stale`: re-sending is always safe.

**The desired state** of a state is the positions that must exist while an
object is in it: each a batch, a unit and a signed amount in the column of
the batch's type. Debit (assets, expenses) must equal credit (liabilities,
equity, revenue) per ledger and unit in every state; memo batches (points,
quantities, anything not money) stand outside the check. The engine diffs it
against the object's booked positions and appends the difference as one
journal per process per call, so a payment of 50 on a 100 receivable is two
lines, not a re-booking. A state listing itself in
`validate_state_change` allows amount changes while in it; a state listing
nothing is terminal and a snapshot with changed amounts is rejected: the
correction is a new object (a credit note). `validate_balances` sees the
whole process's positions before anything books (a payment may not exceed
the open receivable).

**Batches** are the user's (`src/ledger/accounts.rs`). The framework knows
of a batch only its key, its ledger and its type; a receivable per order, a
revenue batch per month, a payout batch per merchant and week are all
`Account` variants, and everything else about them (a GL code, a settle
time) is a table of the owner's keyed by the batch key. A batch's balance
is the sum of its lines over every process (`Store::batch_sum`), and a batch
is closed when a booking brings it to zero: a VAT return is an object whose
desired state holds minus the month's VAT batch and plus what the tax
office is owed, a payout likewise. Nothing marks a batch; a line landing
afterwards is the next claim's remainder.

**Rules change by version.** Bump `Accounting::VERSION` when a desired state
or a state's classification changes. Every booking re-evaluates its whole
process under the running rules, so a touched process converges at once; the
owner's constructor calls `restate_behind()` at boot, which restates every
process the version index lists under an older number. Prospective, always:
history is never rewritten, the restatement is a journal with lines and no
diffs.

**Tables**, in the owner's schema (`db/app/migrations/*_accounting_ledger.sql`,
vendored from `basable-ledger-postgres`): `journal (process_id, sequence) PK`
with `booked_at`, `lines` (one array per line: batch key, ledger, batch
type, object id, unit key, amount) and `snapshot_diffs`, indexed on the lines' batch
keys for `batch_sum`; and `process_ledger_object_version`, the version each
object type of a process was last evaluated under, written by the commit's
own statement and read only by the restatement at boot. No batch table and
no balance table: a position is a sum of lines. A queryable `batch_balance`
is a projection of yours off `journal` (a statement-level trigger, with the
warning that it serializes bookings on hot batches, or ClickHouse through
change data capture), and `basable_ledger_testkit::projection::check_agrees`
is the check it must pass.

**Tests.** `tests/ledger.rs` has the tables check and one property test per
object type to un-ignore: on `basable_ledger::InMemoryStore`, book a sample
through every allowed change and assert the final positions equal the last
state's desired state. A desired state that does not balance is a
`Rejected` outcome there before it is one in production.
