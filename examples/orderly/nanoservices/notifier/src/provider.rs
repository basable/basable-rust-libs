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
    fn send_email(&self, ctx: Ctx, args: SendEmailArgs) -> impl Future<Output = Result<SendEmailResult, BoxError>> + Send;
    /// Reads `send_email` back by the provider id its answer carried.
    fn resolve_send_email(&self, ctx: Ctx, provider_id: String) -> impl Future<Output = Result<SendEmailResult, BoxError>> + Send;
}

/// The HTTP JSON client for email.
pub struct HttpProvider {
    pub base_url: String,
    pub client: reqwest::Client,
    pub token: String,
}

impl Provider for HttpProvider {
    async fn send_email(&self, _ctx: Ctx, _args: SendEmailArgs) -> Result<SendEmailResult, BoxError> {
        // TODO: the request. A parsed 4xx is `Err(definitive(..))`; a
        // transport error, a 5xx or a timeout is a plain `Err(..)`.
        Err("notifier.send_email: provider not implemented".into())
    }

    async fn resolve_send_email(&self, _ctx: Ctx, _provider_id: String) -> Result<SendEmailResult, BoxError> {
        Err("notifier.resolve_send_email: provider not implemented".into())
    }
}

/// No credentials configured (local dev): every call is refused, so nothing
/// is sent and nothing is ambiguous.
pub struct NoopProvider;

impl Provider for NoopProvider {
    async fn send_email(&self, _ctx: Ctx, _args: SendEmailArgs) -> Result<SendEmailResult, BoxError> {
        Err(definitive("notifier.send_email: provider not configured"))
    }

    async fn resolve_send_email(&self, _ctx: Ctx, _provider_id: String) -> Result<SendEmailResult, BoxError> {
        Err(definitive("notifier.resolve_send_email: provider not configured"))
    }
}
