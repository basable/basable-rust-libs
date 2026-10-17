# basable-ledger-testkit — the fixture, the scenarios, the properties, the store suite

The test harness of `basable-ledger`, and the reference for what a
tenant's own ledger tests look like. No database: everything here runs over
`InMemoryStore`; `basable-ledger-postgres` runs the same suites against
Postgres in its own crate.

## The surface

| Item | What it is |
|---|---|
| `fixture::{Invoice, InvoiceState, InvoiceLedger, InvoiceLedgerV2}` | The plan's worked example: an invoice with net, VAT and paid amounts, a `Draft → Issued → PartiallyPaid → Paid / Credited / WrittenOff` lifecycle, one batch per account and invoice (`receivable/<id>`, `cash/<id>`) and per account and month (`revenue/<month>`, `vat/<month>`, `bad_debt/<month>`, `points/<month>`). Version 2 of the rules groups VAT per quarter, which is how the suites exercise a rules bump |
| `fixture::{Payment, PaymentState, PaymentLedger}` | A child object in the invoice's process: settled money sits in the invoice's cash batch and clears its receivable batch, so the payment holds a negative position on a debit batch and `validate_balances` checks the process-wide receivable |
| `fixture::{Account, Currency, PLATFORM_LEDGER}` | The fixture's `BatchAccount` (one variant per kind, `Points` a memo account) and `Unit` |
| `fixture::{new_invoice, new_payment}`, `Invoice::next`, `Payment::next` | Snapshot builders: the next version with a state and amounts |
| `Scenario` | A ledger over a fresh in-memory store with the fixture registered: `book`, `booked`, `book_all`, `journals`, `balances`, `positions`, `batch`, `batches_of`; `upgraded()` is the same store under version 2 of the invoice rules |
| `walk::{Rng, random_history, desired_positions, check_history}` | The generator-driven properties: a deterministic xorshift, a random legal history of one invoice, and the check that books it and asserts path independence, balanced journals and an idempotent replay |
| `conformance::{run_all, ...}` | The store suite: reads an unknown process as empty; commits and reads back lines, diffs and batch rows; the sequence is the compare-and-set; concurrent commits land once; batch ids resolve to their highest epoch and a lost race on (id, epoch) reuses the winner's row; a batch sums across processes; versions record monotonically; `processes_behind` lists the sweep's worklist within the limit |
| `projection::{expected_balances, check_agrees}` | The check any balance projection must pass: per (batch key, unit) it equals the journals' lines summed, over the processes given |

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
`tests/walk.rs`, `tests/sweep.rs`, `tests/conformance_memory.rs`.
