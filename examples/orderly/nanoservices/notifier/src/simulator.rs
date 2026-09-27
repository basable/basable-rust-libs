//! The in-memory simulator of email: idempotent by
//! key, counts what landed, injects faults. Drives the effect audit and the
//! integration tests; the receiver's store the audit's `landed_count`
//! observes.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use basable_core::{BoxError, Ctx};
use basable_externaleffect::definitive;

use crate::effects::*;
use crate::provider::Provider;

#[derive(Default)]
pub struct Simulator {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The distinct keys that landed, per operation.
    landed: HashMap<&'static str, HashSet<String>>,
    /// Per-operation fault: the next N calls fail this way.
    faults: HashMap<&'static str, (Fault, u32)>,
}

#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// A transport failure after the write landed (ack loss): an ambiguous
    /// error.
    AckLoss,
    /// A structured refusal before anything landed: `definitive(..)`.
    Refused,
}

impl Simulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many distinct keys landed for an operation.
    pub fn landed_count(&self, op: &'static str) -> usize {
        self.lock().landed.get(op).map_or(0, HashSet::len)
    }

    /// Whether `key` landed for an operation.
    pub fn landed(&self, op: &'static str, key: &str) -> bool {
        self.lock().landed.get(op).is_some_and(|k| k.contains(key))
    }

    pub fn inject(&self, op: &'static str, fault: Fault, times: u32) {
        self.lock().faults.insert(op, (fault, times));
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a test panicked mid-call; the state is still
        // worth reading for the assertion that follows.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn apply(&self, op: &'static str, key: &str) -> Result<(), BoxError> {
        let mut inner = self.lock();
        let fault = match inner.faults.get_mut(op) {
            Some((f, n)) if *n > 0 => {
                *n -= 1;
                Some(*f)
            }
            _ => None,
        };
        if let Some(Fault::Refused) = fault {
            return Err(definitive(format!("{op}: refused")));
        }
        inner.landed.entry(op).or_default().insert(key.to_owned());
        if let Some(Fault::AckLoss) = fault {
            return Err(format!("{op}: connection reset after the write").into());
        }
        Ok(())
    }
}

impl Provider for Simulator {
    async fn send_email(&self, _ctx: Ctx, args: SendEmailArgs) -> Result<SendEmailResult, BoxError> {
        self.apply("send_email", &args.key)?;
        Ok(SendEmailResult {
            provider_id: args.key.clone(),
        })
    }

    async fn resolve_send_email(&self, _ctx: Ctx, provider_id: String) -> Result<SendEmailResult, BoxError> {
        if self.landed("send_email", &provider_id) {
            Ok(SendEmailResult { provider_id })
        } else {
            Err(definitive(format!("send_email {provider_id}: not found")))
        }
    }
}
