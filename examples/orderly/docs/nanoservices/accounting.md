# accounting

Books orders and payments on the ledger.

Design: `docs/01-shop/02-accounting.md`. Code:
`nanoservices/accounting/` (read its `AGENTS.md` first).

## Owns

- The project's ledger: `order` (states `placed`, `paid`, `cancelled`; produced by `order`), `payment` (states `pending`, `settled`, `reversed`; produced by `order`).

## Messages

- Handles: `Book`.
- Sends: nothing.

## Directive sections that bind

§2 always; §7 for each message sent; §10 for the docs; §11 for the ledger.

## TODO, in order

- [ ] `handle_book` in `src/handlers.rs`.
- [ ] The snapshot fields in `crates/messages/src/ledger.rs`; the accounts in `src/ledger/accounts.rs`.
- [ ] `order`: `desired_state` per state in `src/ledger/order.rs`; its test in `tests/ledger.rs`.
- [ ] `payment`: `desired_state` per state in `src/ledger/payment.rs`; its test in `tests/ledger.rs`.
- [ ] The message structs in `crates/messages/src/lib.rs`.
- [ ] `flows.md` and this file kept true.
