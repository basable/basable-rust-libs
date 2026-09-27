# Adding a nanoservice, a type, a table, a message

Never hand-write a skeleton. The plan's `docs/01-shop/manifest.yaml`
declares what each nanoservice owns; the scaffolder renders it.

1. **Change the plan first.** Add the nanoservice, the processing-object
   type, the table, the external call, the schedule, the webhook or the
   message to `manifest.yaml` (and the prose beside it). Append new types
   at the END of their list: type keys are allocated by position and never
   move.
2. **Call `scaffold_nanoservice`** with the plan directory and the
   nanoservice name. It renders only what is missing — a new crate, a new
   `types/<type>/` module with its migration, a new table migration, a new
   adapter in `effects.rs` — merges the nanoservice's messages into
   `routing.yaml`, and wires the crate into `Cargo.toml`, `MODULE.bazel`,
   the messenger and `app/src/main.rs` between the `basable:` markers.
3. **Fill the bodies.** `handlers.rs` (one `handle_<snake_message>` per
   handled message), the reconcile pass, the queries, `send` per adapter.
   The build names every method the generated traits expect.
4. **Declare the message structs** in `crates/messages/src/lib.rs`.
5. **Keep the docs true**: `nanoservices/<name>/AGENTS.md`, `flows.md`,
   `docs/nanoservices/<name>.md`.

A message is an edit to `routing.yaml` (the sender's `sends`, the handler's
`handles`, same `response` on both) plus its struct in `crates/messages`; the
generated sender then has `send_<snake_message>` and the handler trait
`handle_<snake_message>`. A sent message nobody handles, two handlers for a
request, or a response mismatch is a build error naming the YAML line.
