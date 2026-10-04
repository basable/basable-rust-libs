# basable-connect — the Connect glue

What a tenant's `api` crate needs beside the generated `connectrpc` stubs.
The monorepo's `golang/controller/lib/server` (gRPC plus grpc-gateway, the
middleware chain, the error mapper, the public-id decode) has no port: a
tenant's boundary is Connect over HTTP, the generated service traits are
the API, and this crate is the thin layer between them and the app
(`docs/porting-notes.md` 69 and 73). The Directive (`docs/DIRECTIVE.md` in
every tenant repository, `golang/controller/lib/scaffold/directive.md` in
the monorepo) is the contract this crate serves: the boundary converts
wire messages to plain messages and sends; it holds no state and makes no
decision a handler would.

## The surface

| Item | What it does |
|---|---|
| `ConnectRouter` | `new()`, `add_service(Arc<impl XService>)` (a second service on the same procedure path panics at registration, as `connectrpc::Router` does), `merge(other)`, `into_router()`, `into_axum()` — the Connect router becomes the app router's fallback service, so every procedure path is served under the auth layer |
| `connect_code(Code) -> ErrorCode`, `app_code(ErrorCode) -> Code` | The sixteen codes one to one; a code this crate does not know maps to `Unknown`, as the protocol says of an unlisted one |
| `into_connect_error(AppError) -> ConnectError` | The code, the message, the `AppError` kept as the source |
| `IntoConnect<T>` | `?` on an `AppError` result inside a handler (`.into_connect()?`) |
| `with_request_ids(router)` | The request-id layer: `RequestId` in the request extensions (the client's `x-request-id`, else a fresh UUID), echoed on the response under `REQUEST_ID_HEADER` |
| `request_ctx(&RequestContext) -> Ctx` | The `Ctx` of a handler: the request id, the Connect deadline as the context's timeout, the validated `basable_auth::Identity` when the auth layer ran |
| `pub use connectrpc` | The generated code's types |

## What a handler looks like

The generated trait returns `ServiceResult<impl Encodable + Send>`; a
handler answering the owned message is the refinement
`#[allow(refining_impl_trait)]` names on the impl (note 73):

```rust
#[allow(refining_impl_trait)]
impl<R: ApiRoutes + Send + Sync + 'static> OrderService for OrderServiceImpl<R> {
    async fn ensure_order(&self, ctx: RequestContext, request: ServiceRequest<'_, EnsureOrderRequest>)
        -> ServiceResult<EnsureOrderResponse> {
        let ctx = request_ctx(&ctx);
        let req = request.to_owned_message();
        let order = self.sender.send_ensure_order_request(&ctx, convert(req)).await.into_connect()?;
        Ok(Response::new(EnsureOrderResponse { .. }))
    }
}
```

A server stream is `Response::stream_ok(futures::stream::iter(..))`.

## Rules for a consumer

- One `ConnectRouter` per app, built by the `api` crate's `mount_all` and
  handed to `Serve::connect`; services register through their module's
  `mount`.
- Errors cross the boundary through `into_connect_error` only; a handler
  never builds a `ConnectError` with a code of its own.
- The `Ctx` a handler sends with is `request_ctx`'s: the client's deadline
  bounds the whole synchronous chain, and the identity is the layer's,
  never re-read from headers.

## Tests

`tests/connect` (the Phase 9 verification): an `EchoService` generated
through `tools/proto.bzl`, mounted on the app behind the auth layer, driven
by the generated client in both codecs over HTTP/1.1 and h2c; a server
stream arrives in order; plain JSON over HTTP/1.1 sees the Connect error
shape and the request id; the auth layer rejects and bypasses as the Go
middleware did; the test bypass is a fixed identity.
