# basable-messenger — the messenger runtime

The runtime half of the port of the monorepo's `golang/controller/messenger`
and its generated interfaces: two items, because everything else about a
messenger is generated at build from `routing.yaml` by
`basable-messenger-gen` (the generator's design and rules are in
`../basable-messenger-codegen/AGENTS.md`; `docs/porting-notes.md` 55–62
list what Rust changed). The Directive (`docs/DIRECTIVE.md` in every
tenant repository, `golang/controller/lib/scaffold/directive.md` in the
monorepo) is the contract this crate serves: a message is an external
effect under invariant 7, sent outside any transaction and never under the
envelope lock.

## The two items

| Item | What it is |
|---|---|
| `Route<M, Resp, Source, Ctx>` | The dispatch trait: `fn route<'a>(&'a self, ctx: &'a Ctx, msg: M) -> impl Future<Output = Resp> + Send + 'a`. The generated router implements it once per declared `(source, message)` pair; the generated sender of each nanoservice calls it through the router's `*Routes` bound. `Source` is the generated marker of the sending nanoservice, so one message sent by two nanoservices is two impls, and an undeclared send has no impl to call. `Resp` is the declaration's shape: `Result<T, E>` for a 1:1 request, `Result<(), E>` for a fail-fast error fan-out, `()` for a void fan-out |
| `boxed(future) -> BoxFuture<'a, T>` | `Pin<Box<dyn Future<Output = T> + Send + 'a>>`: the one place a future is boxed. With static dispatch every route's future is part of its caller's future type, so a cycle in the route graph is an infinitely sized type (E0733); the generator boxes a feedback set of routes (`W_ROUTE_CYCLE`) and every other call stays zero-cost |

The single lifetime `'a` on `route` ties the router and the context borrows
together so a boxed route's hidden type is nameable from the one opaque
return type; two independent borrows leave the box's lifetime an
intersection the opaque type cannot express (E0700).

## Rules the design fixes (B2, B2b)

- Handlers take `&self`; the concrete router is passed by shared reference
  into every handler, reconciler and service; no `Arc<dyn>`, no channels.
- Every route's future is `Send`, so the router and the context are `Sync`
  and a `std::sync::MutexGuard` held across a send is a compile error.
- Boxing is per route and only on cycles; a nanoservice never names
  `boxed` itself.

## Tests

The crate's own unit tests pin that a boxed route is a `Send` future with
the declared output. The generated crates are exercised in
`tests/messenger/orderly` (the reference topology) and
`tests/messenger/cyclic` (the boxed back-edge, re-entrancy, and the two
`compile_fail` doctests: an unboxed cycle is E0733, a `MutexGuard` across
a send is E0277).
