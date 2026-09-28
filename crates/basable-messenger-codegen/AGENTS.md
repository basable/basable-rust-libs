# basable-messenger-codegen — the messenger generator

The port of the monorepo's `golang/tools/codegen/messenger-gen-v2` and
`interface-gen-v2` (one generator here, two output crates), with the
runtime half in the sibling crate `basable-messenger` and the command line
in `basable-messenger-gen`. `routing.yaml` in; the tenant's `interfaces`
and `messenger` crates and a topology page out. The Go generators'
semantics are the specification: a typed `response` is a strict 1:1
request with exactly one handler, `response: error` is a sequential
fail-fast fan-out, no `response` is a void fan-out to 0..N handlers in
declaration order (`docs/porting-notes.md` 55–62 lists what Rust changed).

## The pipeline

| Step | Module | What it produces |
|---|---|---|
| YAML → tree | `yaml.rs` | `Node { line, value }` from yaml-rust2's event stream, keys unique, aliases refused. Every later diagnostic points at one of these lines |
| Schema | `spec.rs` + `schema.json` | draft 2020-12, `additionalProperties: false` everywhere, `version: 1`, the name pattern; each error's instance path is walked back to a line (`E_SCHEMA`) |
| Decode + rules | `spec.rs` | `Spec` (messenger settings, nanoservices with line-numbered `handles`/`sends`); `E_NOT_A_TYPE` (syn parses every message path, response, `error_type`, `ctx_type`, `uses`), then the coded rules |
| Message table + cycles | `graph.rs` | `messages()`: one row per message with handlers and senders in declaration order and the one response kind every declaration agrees on (`E_RESPONSE_MISMATCH`); `analyze()`: `W_HANDLER_NEVER_SENT`, and the feedback vertex set of the route graph (`W_ROUTE_CYCLE`) |
| Emit | `emit.rs` | `quote` token streams parsed back through `syn` (a generator bug is caught here, not in the tenant's build) and printed by `prettyplease` behind the `// GENERATED` header |
| Docs | `docs.rs` | the nanoservice and message tables plus a mermaid graph |

Codes (`diagnostic.rs`): `E_YAML`, `E_SCHEMA`, `E_NO_COMPONENTS`,
`E_DUP_COMPONENT`, `E_DUP_MESSAGE_IN_LIST`, `E_1TO1_NO_HANDLER`,
`E_1TO1_MULTI_HANDLER`, `E_RESPONSE_MISMATCH`, `E_NOT_A_TYPE`;
`W_HANDLER_NEVER_SENT`, `W_ROUTE_CYCLE`. They are the contract with the
fixture corpus in `spec/routing/fixtures` (an invalid fixture's first line
is `# expect: CODE`) and with the monorepo's Go validator, which vendors
the corpus at `spec/routing/VERSION`.

## What the generated crates look like

`interfaces` (depends on `messages`, `basable-messenger` and the
`ctx_type`/`error_type` crates only), per nanoservice `x`:

- `source::X`, the marker that keys its routes;
- `XRoutes`: `Route<M, Resp, source::X, Ctx>` for each declared send, plus
  `Sync`, with a blanket impl — the bound a handler is generic over;
- `XSender<'a, R>(&'a R)`, `Copy`, `new(&R)`, and `send_<snake M>` per
  declared send, each `<R as Route<..>>::route(self.0, ctx, msg).await`;
- `XHandler<R: XRoutes>: Send + Sync` with `handle_<snake M>(&self, ctx,
  msg, s: XSender<'_, R>) -> impl Future<Output = Resp> + Send` per handled
  message (absent for a sends-only nanoservice).

`messenger` (depends on every nanoservice crate): `pub struct
AppMessenger { x: x::X, … }` with private fields, `new(..)` in declaration
order, one accessor per component (for the composition root only —
nanoservice crates cannot name this crate), a `Send + Sync` assertion per
component, and one `impl Route<M, Resp, source::S, Ctx> for AppMessenger`
per declared `(source, message)` pair: an `async fn route` calling the
handler(s), or, on a route the cycle analysis chose, `fn route<'a>(&'a
self, ctx: &'a Ctx, msg) -> impl Future + Send + 'a { boxed(async move {
..}) }`.

Names: `snake` and `pascal` in `names.rs` are byte-for-byte the
scaffolder's template functions, because the scaffold writes
`handle_<snake message>` into a nanoservice's `handlers.rs` and the trait
must ask for the same method: `GetProductRequest` →
`handle_get_product_request`. The component type is `<name>::<Pascal>`.

## The cycle rule

Static dispatch puts every route's future inside its caller's future
type, so a cycle in `routing.yaml` is an infinitely sized type (E0733 —
`tests/messenger/cyclic` pins it). `graph::feedback_set` walks the route
graph depth-first in declaration order (nodes are `(source, message)`
routes, an edge runs from a route to every route one of its handlers may
send) and boxes the route a back-edge points at unless that cycle already
passes through a boxed route. Boxing is per route and only there; the
`W_ROUTE_CYCLE` line names the route and the cycle it closes. The `Route`
trait's single lifetime (`&'a self, &'a Ctx … + 'a`) is what makes the
boxed hidden type nameable.

## Testing

- `basable_messenger_codegen_test`: the tree, the names, every rule, the
  cycle cases, and the emitted-text assertions of `error_fanout_test.go`
  (declaration order, one `?` per handler, `Ok(())`).
- `corpus`: every fixture under `spec/routing/fixtures`; the orderly
  goldens (`spec/routing/golden/`), re-blessed with the CLI when an emitter
  change is intended (the module doc of `tests/corpus.rs` has the command).
- `//tests/messenger/orderly` and `//tests/messenger/cyclic`: the generated
  crates compiled against stub components through `tools/messenger.bzl`,
  the routes driven under tokio (chain, fan-out order, error crossing,
  re-entrancy `a → b → a` with 64 concurrent requests), and the
  `compile_fail` doctests (unboxed cycle: E0733; a `MutexGuard` across a
  send: E0277).
