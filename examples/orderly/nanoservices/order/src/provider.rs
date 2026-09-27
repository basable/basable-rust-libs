//! The provider seam: the narrow trait `effects.rs` sends through, the real
//! HTTP client, and the noop for environments without credentials. A
//! provider answers with a `BoxError` that carries its own verdict on its
//! own call: `definitive(..)` for a parsed 4xx — the receiver's structured
//! answer, and the proof of non-execution an irreversible adapter needs —
//! and a plain error for everything whose outcome is unknowable (a
//! transport error, a 5xx, a timeout), which the classifier treats as
//! ambiguous.

use std::future::Future;

use basable_core::{BoxError, Ctx};
use basable_externaleffect::definitive;

use crate::effects::*;

/// Exactly the calls this nanoservice makes — never a wrapper around the
/// whole provider API. The futures are `Send` because a worker dispatches
/// them.
pub trait Provider: Send + Sync + 'static {
    fn capture_payment(&self, ctx: Ctx, args: CapturePaymentArgs) -> impl Future<Output = Result<CapturePaymentResult, BoxError>> + Send;
    /// Reads `capture_payment` back by the provider id its answer carried.
    fn resolve_capture_payment(&self, ctx: Ctx, provider_id: String) -> impl Future<Output = Result<CapturePaymentResult, BoxError>> + Send;
}

/// The HTTP JSON client for payments.
pub struct HttpProvider {
    pub base_url: String,
    pub client: reqwest::Client,
    pub token: String,
}

impl Provider for HttpProvider {
    async fn capture_payment(&self, _ctx: Ctx, _args: CapturePaymentArgs) -> Result<CapturePaymentResult, BoxError> {
        // TODO: the request. A parsed 4xx is `Err(definitive(..))`; a
        // transport error, a 5xx or a timeout is a plain `Err(..)`.
        Err("order.capture_payment: provider not implemented".into())
    }

    async fn resolve_capture_payment(&self, _ctx: Ctx, _provider_id: String) -> Result<CapturePaymentResult, BoxError> {
        Err("order.resolve_capture_payment: provider not implemented".into())
    }
}

/// No credentials configured (local dev): every call is refused, so nothing
/// is sent and nothing is ambiguous.
pub struct NoopProvider;

impl Provider for NoopProvider {
    async fn capture_payment(&self, _ctx: Ctx, _args: CapturePaymentArgs) -> Result<CapturePaymentResult, BoxError> {
        Err(definitive("order.capture_payment: provider not configured"))
    }

    async fn resolve_capture_payment(&self, _ctx: Ctx, _provider_id: String) -> Result<CapturePaymentResult, BoxError> {
        Err(definitive("order.resolve_capture_payment: provider not configured"))
    }
}
