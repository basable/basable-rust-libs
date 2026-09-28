//! The glue between a nanoservice project and `connectrpc`. The generated
//! Connect stubs (see `tools/proto.bzl`) are the API boundary; this crate
//! is what the `api` crate needs beside them:
//!
//! - [`ConnectRouter`]: registers the generated services and mounts them
//!   into the app's axum router ([`ConnectRouter::into_axum`]).
//! - [`into_connect_error`] / [`IntoConnect`]: `AppError` to
//!   `ConnectError`, the sixteen codes one to one ([`connect_code`],
//!   [`app_code`]).
//! - [`with_request_ids`]: the request-id layer, `x-request-id` in and out.
//! - [`request_ctx`]: the `Ctx` a handler derives from its
//!   `RequestContext` (request id, deadline, the validated
//!   [`basable_auth::Identity`]).
//!
//! `connectrpc` itself is re-exported for the generated code's types.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use basable_core::{AppError, Code, Ctx};
pub use connectrpc;
use connectrpc::{ConnectError, ErrorCode, RequestContext, ServiceRegister};
use http::{HeaderName, HeaderValue};

/// The request-id header, read from the client and echoed on the response.
pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// The request's id, in the request extensions once [`with_request_ids`]
/// ran: the client's `x-request-id` if it sent one, else a fresh UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

/// The Connect services of an app, registered one by one and mounted at
/// once.
#[derive(Default)]
pub struct ConnectRouter {
    inner: Option<connectrpc::Router>,
}

impl ConnectRouter {
    /// An empty router.
    pub fn new() -> ConnectRouter {
        ConnectRouter {
            inner: Some(connectrpc::Router::new()),
        }
    }

    /// Registers a generated service implementation (`Arc<impl
    /// XService>`). A second service with the same procedure path panics
    /// at registration, as `connectrpc::Router` does.
    pub fn add_service<S: ?Sized, Marker>(&mut self, service: Arc<S>)
    where
        Arc<S>: ServiceRegister<Marker>,
    {
        let router = self.inner.take().unwrap_or_default().add_service(service);
        self.inner = Some(router);
    }

    /// Merges another router's services in.
    pub fn merge(&mut self, other: ConnectRouter) {
        let router = self
            .inner
            .take()
            .unwrap_or_default()
            .merge(other.into_router());
        self.inner = Some(router);
    }

    /// The `connectrpc` router.
    pub fn into_router(self) -> connectrpc::Router {
        self.inner.unwrap_or_default()
    }

    /// An axum router answering every registered procedure path (as its
    /// fallback service, so it merges beside plain routes).
    pub fn into_axum(self) -> axum::Router {
        axum::Router::new().fallback_service(self.into_router().into_axum_service())
    }
}

/// The Connect code of an `AppError` code: the same sixteen names.
pub fn connect_code(code: Code) -> ErrorCode {
    match code {
        Code::Canceled => ErrorCode::Canceled,
        Code::Unknown => ErrorCode::Unknown,
        Code::InvalidArgument => ErrorCode::InvalidArgument,
        Code::DeadlineExceeded => ErrorCode::DeadlineExceeded,
        Code::NotFound => ErrorCode::NotFound,
        Code::AlreadyExists => ErrorCode::AlreadyExists,
        Code::PermissionDenied => ErrorCode::PermissionDenied,
        Code::ResourceExhausted => ErrorCode::ResourceExhausted,
        Code::FailedPrecondition => ErrorCode::FailedPrecondition,
        Code::Aborted => ErrorCode::Aborted,
        Code::OutOfRange => ErrorCode::OutOfRange,
        Code::Unimplemented => ErrorCode::Unimplemented,
        Code::Internal => ErrorCode::Internal,
        Code::Unavailable => ErrorCode::Unavailable,
        Code::DataLoss => ErrorCode::DataLoss,
        Code::Unauthenticated => ErrorCode::Unauthenticated,
    }
}

/// The `AppError` code of a Connect code.
pub fn app_code(code: ErrorCode) -> Code {
    match code {
        ErrorCode::Canceled => Code::Canceled,
        ErrorCode::Unknown => Code::Unknown,
        ErrorCode::InvalidArgument => Code::InvalidArgument,
        ErrorCode::DeadlineExceeded => Code::DeadlineExceeded,
        ErrorCode::NotFound => Code::NotFound,
        ErrorCode::AlreadyExists => Code::AlreadyExists,
        ErrorCode::PermissionDenied => Code::PermissionDenied,
        ErrorCode::ResourceExhausted => Code::ResourceExhausted,
        ErrorCode::FailedPrecondition => Code::FailedPrecondition,
        ErrorCode::Aborted => Code::Aborted,
        ErrorCode::OutOfRange => Code::OutOfRange,
        ErrorCode::Unimplemented => Code::Unimplemented,
        ErrorCode::Internal => Code::Internal,
        ErrorCode::Unavailable => Code::Unavailable,
        ErrorCode::DataLoss => Code::DataLoss,
        ErrorCode::Unauthenticated => Code::Unauthenticated,
        // The enum is non-exhaustive upstream; a code this crate does not
        // know is Unknown, as the Connect protocol says of an unlisted one.
        _ => Code::Unknown,
    }
}

/// An `AppError` as the Connect error the client receives: the same code,
/// the same message, the source chain kept for the server's logs.
pub fn into_connect_error(err: AppError) -> ConnectError {
    let code = connect_code(err.code());
    let message = err.message().to_string();
    ConnectError::new(code, message).with_source(err)
}

/// `?` on an `AppError` result inside a Connect handler.
pub trait IntoConnect<T> {
    /// Maps the error through [`into_connect_error`].
    fn into_connect(self) -> Result<T, ConnectError>;
}

impl<T> IntoConnect<T> for Result<T, AppError> {
    fn into_connect(self) -> Result<T, ConnectError> {
        self.map_err(into_connect_error)
    }
}

/// Wraps every route of `router` with the request-id layer: the request
/// carries a [`RequestId`] in its extensions (the client's `x-request-id`,
/// else a fresh UUID) and the response echoes it.
pub fn with_request_ids(router: axum::Router) -> axum::Router {
    router.layer(axum::middleware::from_fn(request_id))
}

async fn request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(id.clone()));
    let mut resp = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        resp.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    resp
}

/// The `Ctx` of a Connect handler: the request id, the request deadline
/// as the context's, and the validated identity when the auth layer ran.
pub fn request_ctx(ctx: &RequestContext) -> Ctx {
    let id = ctx
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .or_else(|| {
            ctx.header(&REQUEST_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    let mut out = Ctx::new(id);
    if let Some(remaining) = ctx.time_remaining() {
        out = out.with_timeout(remaining);
    }
    if let Some(identity) = basable_auth::identity_of(ctx.extensions()) {
        out = out.with_value(identity.clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use basable_auth::{AuthCtx, Identity};

    use super::*;

    #[test]
    fn every_code_round_trips() {
        for name in [
            "canceled",
            "unknown",
            "invalid_argument",
            "deadline_exceeded",
            "not_found",
            "already_exists",
            "permission_denied",
            "resource_exhausted",
            "failed_precondition",
            "aborted",
            "out_of_range",
            "unimplemented",
            "internal",
            "unavailable",
            "data_loss",
            "unauthenticated",
        ] {
            let code = Code::from_name(name).unwrap_or_else(|| panic!("{name}"));
            let connect = connect_code(code);
            assert_eq!(connect.as_str(), name);
            assert_eq!(app_code(connect), code);
        }
    }

    #[test]
    fn an_app_error_keeps_its_code_message_and_source() {
        let err = into_connect_error(AppError::not_found("no such order"));
        assert_eq!(err.code, ErrorCode::NotFound);
        assert_eq!(err.message.as_deref(), Some("no such order"));
        assert!(std::error::Error::source(&err).is_some());
        let r: Result<(), ConnectError> = Err(AppError::internal("x")).into_connect();
        assert_eq!(r.unwrap_err().code, ErrorCode::Internal);
    }

    #[test]
    fn the_handler_context_carries_id_deadline_and_identity() {
        let mut ext = http::Extensions::new();
        ext.insert(RequestId("req-7".into()));
        ext.insert(Identity {
            id: "user-1".into(),
            email: String::new(),
        });
        let rc = RequestContext::new(http::HeaderMap::new())
            .with_extensions(ext)
            .with_deadline(Some(
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            ));
        let ctx = request_ctx(&rc);
        assert_eq!(ctx.request_id(), "req-7");
        assert_eq!(ctx.user_id(), Some("user-1"));
        assert!(ctx.deadline().is_some());

        let mut headers = http::HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, HeaderValue::from_static("hdr-1"));
        let ctx = request_ctx(&RequestContext::new(headers));
        assert_eq!(ctx.request_id(), "hdr-1");
        assert_eq!(ctx.user_id(), None);
        assert!(ctx.deadline().is_none());
    }
}
