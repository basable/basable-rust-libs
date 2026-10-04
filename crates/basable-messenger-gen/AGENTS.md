# basable-messenger-gen — the generator's command line

The binary a tenant's build runs: the port of invoking the monorepo's
`messenger-gen-v2` and `interface-gen-v2` from a genrule, over the one
library `basable-messenger-codegen` (its `AGENTS.md` is the specification
of what gets generated; `docs/porting-notes.md` 55–62 list what Rust
changed). The Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: `routing.yaml` is the durable topology, and a
send nobody handles or a response two declarations disagree on fails the
build, not the request.

## Commands

| Command | What it does |
|---|---|
| `generate --crate interfaces\|messenger --spec routing.yaml --output FILE` | Emits one of the two generated crates' `src/lib.rs` |
| `validate --spec routing.yaml` | Runs the schema and the coded rules, prints the summary (`ok (N nanoservices, N messages, N routes, N boxed)`) |
| `docs --spec routing.yaml [--output FILE]` | The topology page (tables plus a mermaid graph) |
| `schema [--output FILE]` | The JSON Schema of `routing.yaml`, for an editor |

Diagnostics go to stderr as `routing.yaml:LINE: CODE: message`. An `E_*`
code exits 1 (the genrule fails and the build log shows the file); a
`W_*` code (`W_HANDLER_NEVER_SENT`, `W_ROUTE_CYCLE`) is printed and the run
succeeds. A usage error exits 2. `--output -` is stdout, the default for
`docs` and `schema`.

## How a tenant runs it

`tools/messenger.bzl`'s `messenger_generated(name, crate, spec, tool, out)`
genrule calls `@crates//:basable-messenger-gen__basable-messenger-gen
generate` for `crates/interfaces` and `crates/messenger`; the scaffolder
renders that BUILD glue. The crate carries a library target beside the
binary so a tenant's `crates/messenger` can name it as a dev-dependency and
the lock keeps it — cargo drops a dependency on a bin-only crate (porting
note 76).

## Tests

The option parser (any order, unknown flags refused) and the usage errors
are unit tests here; the generator's behaviour is pinned in the codegen
crate's corpus (`spec/routing/fixtures`, the orderly goldens) and the
generated crates compiled under `tests/messenger/`.
