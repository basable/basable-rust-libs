//! The external-effect adapters: one validated `Call` per external call
//! this nanoservice makes, with the strategy the plan declared (the
//! Directive §6). `send` is the ONE dispatch path; a raw provider call
//! anywhere else fails review. Audited one by one in tests/effects_audit.rs.

use std::sync::Arc;
use std::time::Duration;

#[allow(unused_imports)]
use basable_externaleffect::{
    Adapter, Call, Classifier, DeclaredResolution, LateCall, SlotIdentity, Strategy,
    intent_age_fn, key_fn, lookup_fn, provider_id_fn, resolve_by_lookup, resolve_keyed_fn,
    send_fn,
};

use crate::provider::{HttpProvider, NoopProvider, Provider};

/// The validated adapter set, built once at construction. Each call is
/// shared (`Arc`) so the audit can hold the very handle production
/// dispatches through.
pub struct Calls {
    pub capture_payment: Arc<Call<CapturePaymentArgs, CapturePaymentArgs, CapturePaymentResult>>,
}

/// Arguments of `capture_payment` (payments). Everything `key` and `send`
/// derive from must be here, durable before the send.
#[derive(Debug, Clone)]
pub struct CapturePaymentArgs {
    /// The deterministic identity of this effect (never a retry count).
    pub key: String,
    /// The database-clock time the intent was declared, for the replay
    /// window gate.
    pub intent_declared_at: chrono::DateTime<chrono::Utc>,
    // TODO: the payload.
}

impl CapturePaymentArgs {
    pub fn intent_age(&self) -> Duration {
        (chrono::Utc::now() - self.intent_declared_at).to_std().unwrap_or_default()
    }
}

/// What `capture_payment` returns.
#[derive(Debug, Clone)]
pub struct CapturePaymentResult {
    /// The provider's id for what landed (its receipt, its object id).
    pub provider_id: String,
    // TODO: the rest of the provider's answer.
}

impl Calls {
    /// Picks the provider from the environment, once, at construction: the
    /// HTTP client when the credential is set, the noop otherwise.
    pub fn from_env() -> Self {
        match std::env::var("ORDER_PROVIDER_TOKEN") {
            Ok(token) if !token.is_empty() => Calls::new(Arc::new(HttpProvider {
                base_url: std::env::var("ORDER_PROVIDER_URL").unwrap_or_default(),
                client: reqwest::Client::new(),
                token,
            })),
            _ => Calls::new(Arc::new(NoopProvider)),
        }
    }

    /// Builds every adapter over one provider; the tests pass the simulator.
    pub fn new<P: Provider>(provider: Arc<P>) -> Self {
        Self {
            capture_payment: Arc::new(
                Call::new(Adapter {
                    operation: "capture_payment".into(),
                    key: Some(key_fn(|a: &CapturePaymentArgs| a.key.clone())),
                    send: {
                        let p = Arc::clone(&provider);
                        send_fn(move |ctx, args: CapturePaymentArgs| {
                            let p = Arc::clone(&p);
                            async move { p.capture_payment(ctx, args).await }
                        })
                    },
                    // Irreversible: only the provider's definitive answer (a
                    // parsed 4xx) is a verdict; anything else is ambiguous
                    // and re-driven.
                    classify: Some(Classifier::fail_closed_on_definitive()),
                    call_timeout: Duration::from_secs(30),
                    irreversible: true,
                    late_call: LateCall::KeyScoped,
                    strategy: Strategy::KeyedReplay {
                        window: Duration::from_secs(24 * 3600),
                        intent_age: intent_age_fn(|a: &CapturePaymentArgs| a.intent_age()),
                        provider_id: provider_id_fn(|r: &CapturePaymentResult| r.provider_id.clone()),
                        resolve: {
                            let p = Arc::clone(&provider);
                            resolve_keyed_fn(move |ctx, id: String| {
                                let p = Arc::clone(&p);
                                async move { p.resolve_capture_payment(ctx, id).await }
                            })
                        },
                    },
                })
                .expect("order.capture_payment: invalid adapter"),
            ),
        }
    }
}
