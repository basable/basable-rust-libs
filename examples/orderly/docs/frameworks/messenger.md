# basable-messenger (crates 0.1.0)

The messenger is generated at build from `routing.yaml` by
`basable-messenger-gen` (see `tools/messenger.bzl`). Two crates come out:

- **`interfaces`** — per nanoservice a handler trait generic over the router
  and a sender wrapper exposing exactly the declared sends:

  ```rust
  pub trait CatalogHandler<R: CatalogRoutes>: Send + Sync {
      fn handle_get_product_request(&self, ctx: &Ctx, msg: GetProductRequest, s: CatalogSender<'_, R>)
          -> impl Future<Output = Result<Product, AppError>> + Send;   // 1:1
      fn handle_order_event(&self, ctx: &Ctx, msg: OrderEvent, s: CatalogSender<'_, R>)
          -> impl Future<Output = ()> + Send;                           // void fan-out
  }
  pub struct CatalogSender<'a, R>(&'a R);
  impl<'a, R: CatalogRoutes> CatalogSender<'a, R> {
      pub async fn send_get_product_request(&self, ctx: &Ctx, msg: GetProductRequest) -> Result<Product, AppError> { … }
  }
  pub trait CatalogRoutes: Route<GetProductRequest, Result<Product, AppError>, source::Catalog, Ctx> + … + Sync {}
  ```
- **`messenger`** — the concrete `AppMessenger` holding EVERY nanoservice's
  component (`<name>::<Pascal>`, sends-only ones included) as a PRIVATE
  field, `new(..)` in `routing.yaml` order, one accessor per component for
  the composition root (`router.api()`; nanoservice crates depend on
  `interfaces` only, so they cannot name the router), and one `Route` impl
  per declared `(source, message)` pair, calling the receivers in
  declaration order.

The naming rule: `handle_<snake message>` on the handler trait,
`send_<snake message>` on the sender, where `snake` is the scaffolder's
(`GetProductRequest` → `get_product_request`; no suffix is stripped).
Response kinds: a typed `response` is `Result<T, AppError>` with exactly one
handler; `response: error` is sequential fail-fast fan-out returning
`Result<(), AppError>`; no response is `()` with 0..N handlers. A fanned-out
message is cloned for all but its last handler, so messages derive `Clone`.

A nanoservice implements `impl<R: OrderRoutes> OrderHandler<R> for
OrderOperator` (the scaffold emits that header); a handler receives its
sender per message, and a reconciler, a ticker or a Connect service holds
its sender (`OrderSender<'static, R>`, a `Copy` wrapper over the router
exposing exactly the declared sends), built once from the router. `main`
builds every component, then `Box::leak(Box::new(AppMessenger::new(..)))`,
and hands the `&'static` router to the Connect server and to each worker,
which build their senders from it. A nanoservice sending an undeclared
message does not compile (no sender method).

Cycles in the route graph are boxed mechanically (a feedback vertex set of
routes, chosen depth-first in declaration order, gets `boxed(..)`);
`W_ROUTE_CYCLE` in the build log names each boxed route and its cycle.
Diagnostics come as `routing.yaml:LINE: CODE: …` (`E_DUP_COMPONENT`,
`E_1TO1_NO_HANDLER`, `E_1TO1_MULTI_HANDLER`, `E_RESPONSE_MISMATCH`,
`E_DUP_MESSAGE_IN_LIST`, `E_NOT_A_TYPE`, `E_SCHEMA`; `W_HANDLER_NEVER_SENT`);
`basable-messenger-gen schema` prints the JSON Schema for the editor. Every
generated handler future is `Send`, so a `std::sync::MutexGuard` held across
a `send_*` does not compile.
