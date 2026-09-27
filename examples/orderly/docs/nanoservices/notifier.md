# notifier

Sends one templated email per event.

Design: `docs/01-shop/02-notifier.md`. Code:
`nanoservices/notifier/` (read its `AGENTS.md` first).

## Owns

- External call `send_email` on email: `keyed_replay`, irreversible.

## Messages

- Handles: `OrderEvent`.
- Sends: nothing.

## Directive sections that bind

§2 always; §6 for each adapter; §7 for each message sent; §10 for the docs.

## TODO, in order

- [ ] `handle_order_event` in `src/handlers.rs`.
- [ ] `send` for `send_email` in `effects.rs`, the real client in `provider.rs`; the audit stays green.
- [ ] The message structs in `crates/messages/src/lib.rs`.
- [ ] `flows.md` and this file kept true.
