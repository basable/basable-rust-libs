# basable-ledger-testkit — the fixture, the scenarios, the properties, the store suite

The test harness of `basable-ledger`, and the reference for what a
tenant's own ledger tests look like. No database: everything here runs over
`InMemoryStore`; `basable-ledger-postgres` runs the same suites against
Postgres in its own crate.

## The surface

| Item | What it is |
|---|---|
| `fixture::Account` | The fixture's `BatchAccount`: one variant per kind with what identifies the batch, an invoice (`Receivable`, `CashClearing`) or a period (`Revenue`, `Vat`, `TaxOffice`, `BadDebt`, `Points`, a memo account). `name()` is `<kind>/<invoice or period>` and the key is an FNV-1a hash of it: the fixture's stand-in for the batch table a user keeps, or the encoding they choose |
| `fixture::{Invoice, InvoiceState, InvoiceLedger, InvoiceLedgerV2}` | The plan's worked example: an invoice with net, VAT and paid amounts, a `Draft → Issued → PartiallyPaid → Paid / Credited / WrittenOff` lifecycle, batches per invoice (`receivable_batch`, `cash_batch`) and per month (`revenue_batch`, `vat_batch`, `bad_debt_batch`, `points_batch`). Version 2 of the rules groups VAT per quarter, which is how the suites exercise a rules bump |
| `fixture::{Payment, PaymentState, PaymentLedger}` | A child object in the invoice's process: settled money sits in the invoice's cash batch and clears its receivable batch, so the payment holds a negative position on a debit batch and `validate_balances` checks the process-wide receivable |
| `fixture::{VatReturn, VatReturnState, VatReturnLedger}` | A root object of its own: filed, it holds minus the claimed amount on the period's VAT batch and the same amount on the tax office's. How a batch closes: a booking to zero, not a status |
| `fixture::{Currency, PLATFORM_LEDGER}` | The fixture's `Unit` and its one ledger |
| `fixture::{new_invoice, new_payment, new_vat_return}`, `Invoice::next`, `Payment::next` | Snapshot builders: the next version with a state and amounts |
| `Scenario` | A ledger over a fresh in-memory store with the fixture registered: `book`, `booked`, `book_all`, `journals`, `balances`, `positions` (per batch key and unit), `batch_sum(&account)`; `upgraded()` is the same store under version 2 of the invoice rules |
| `walk::{Rng, random_history, desired_positions, check_history}` | The generator-driven properties: a deterministic xorshift, a random legal history of one invoice, and the check that books it and asserts path independence, balanced journals and an idempotent replay |
| `conformance::{run_all, ...}` | The store suite: reads an unknown process as empty; commits and reads back lines with their batches and diffs; the sequence is the compare-and-set; concurrent commits land once; a batch sums across processes and units, and an unwritten one to nothing; the versions a journal moves fold to the highest per type and the index the commit maintains agrees, never moving back; `processes_behind` lists the startup restatement's worklist within the limit |
| `projection::{expected_balances, check_agrees}` | The check any balance projection must pass: per (batch key, unit) it equals the journals' lines summed, over the processes given, zeros dropped. The framework ships no balance table; this is the testkit's projection, and a user's trigger or ClickHouse table answers to it. A zero balance is a closed batch and should have no row (the trigger deletes it, a `SummingMergeTree` drops it on merge); the check asks only for the nonzero ones |

## Rules

- **A suite books through `Ledger`, never through the store**, except the
  conformance suite, whose subject is the store.
- **Properties are generated, not enumerated.** `check_history` is the
  shape: a seed, a legal history, the three invariants. A desired state
  that does not balance for some amounts, or a state change that is not
  path independent, fails here before it fails on a tenant.
- **No dependency for randomness.** `Rng` is xorshift64*, enough for a walk
  and reproducible from its seed in a failure message.

## What the tests pin

See `crates/basable-ledger/AGENTS.md`: `tests/scenario.rs`,
`tests/walk.rs`, `tests/restatement.rs`, `tests/conformance_memory.rs`.
