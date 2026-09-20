# basable-rust-libs

The basable nanoservice frameworks for Rust — the `basable-*` crates a
[basable](https://basable.com) project is generated against: processing
objects with claims and reconcile passes, external effects with an
admission contract, declarative configuration, a statically dispatched
messenger generated from `routing.yaml`, Connect glue, Kratos auth, and the
app runtime that boots them.

Bazel + rules_rust is the build (`bazel test //...`); the crates are also
published to crates.io in lockstep. See `CLAUDE.md` for the rules and
`docs/` for the decisions and porting notes.
