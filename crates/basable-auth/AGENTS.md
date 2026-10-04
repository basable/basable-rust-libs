# basable-auth — Kratos session validation

The port of the monorepo's `golang/lib/auth` (`AuthMiddleware`: the
cookie-to-`/sessions/whoami` check, `WithSessionHook`, the public paths,
`WithTestBypass`), reduced to one shape: a layer on the app's axum router.
Go had three (gRPC unary and stream interceptors and an HTTP middleware)
reading the cookie from metadata or the header; a tenant's API is Connect
over HTTP, so there is one path (`docs/porting-notes.md` 70–71). The
Directive (`docs/DIRECTIVE.md` in every tenant repository,
`golang/controller/lib/scaffold/directive.md` in the monorepo) is the
contract this crate serves: identity is established at the boundary and
travels on `Ctx`; a handler never re-authenticates.

## The surface

| Item | What it does |
|---|---|
| `Validator::kratos(url)` | Asks `<url>/sessions/whoami` with the request's `Cookie`, bounded by `KRATOS_TIMEOUT` (3 s). `/healthz` and `/readyz` are public from the start |
| `.public_paths([..])`, `.public_prefixes([..])` | Exact paths and prefixes that bypass validation (`is_public`); the scaffold adds `/api/webhooks/` |
| `.session_hook(\|identity\| ..)` | Fires after every successful validation, on the request path: the seam for first-touch user provisioning. It cannot fail the request and must be quick |
| `Arc<Validator>::apply(router)` | The axum layer (`from_fn_with_state`): a non-public request without a cookie Kratos accepts is answered `401 {"code":"unauthenticated","message":"invalid session"}`; Kratos unreachable, timing out or answering an unexpected status is `503 unavailable`, never a silent allow |
| `authenticate(cookie)` | The check itself, for a caller that is not an axum layer |
| `Identity { id, email }` | Put in the request extensions on success (`identity_of(&extensions)`), from where `basable-connect`'s `request_ctx` places it on `Ctx` |
| `AuthCtx` | `identity()` / `user_id()` on `Ctx`: how a handler reads the caller |
| `AuthError` | `Unauthenticated` → 401, `Unavailable(why)` → 503; `status()`, `code()`, `message()`, and an `IntoResponse` in Connect's JSON error shape |
| `KratosSession`, `KratosIdentity`, `KratosTraits` | The `whoami` wire shape (`traits.email`) |
| `NEGATIVE_WINDOW` (5 s) | A rejected cookie stays rejected without re-asking Kratos for this long; the cache is bounded (4096 entries) and cleared when full |

## The test bypass

`Validator::bypassed_for_tests(identity)` authenticates every non-public
request as that fixed identity and fires the hook. It exists only behind
the `test-bypass` cargo feature, which `basable-testkit` enables: a binary
that does not carry the feature cannot name it, where Go's
`WithTestBypass()` was a method production simply never called (note 71).
Under crate_universe the feature set resolves workspace-wide, so a tenant
whose testkit is a dev-dependency compiles the constructor in; what stays
mechanical is that it is a distinct, grep-able name the scaffold's
`main.rs` never writes.

## Rules for a consumer

- Apply the validator once, on the whole router, through `Serve::auth`;
  do not mount routes beside it.
- A webhook route lives under a public prefix and verifies the provider's
  signature itself (the scaffold's `webhooks/` module).
- The hook is for provisioning on first sight, not for authorisation: no
  database write on every request.

## Tests

Unit tests in `src/lib.rs`: public paths and prefixes bypass; a missing
cookie is unauthenticated without asking Kratos; an unreachable Kratos is
unavailable, never allowed; the bypass authenticates everything and fires
the hook; the negative cache expires and bounds itself. `tests/connect`
(the Phase 9 verification) drives the layer on a running app against a
Kratos simulator: valid cookie, missing, expired and malformed, the public
path, Kratos down.
