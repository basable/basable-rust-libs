//! The recorder a desired state names its batches through. It notes which
//! batches the lines want and under what ledger and type; the engine
//! resolves them afterwards in one store query and creates the missing ones
//! in the commit. A recorder, not a store handle, so `desired_state` stays
//! synchronous and pure.

use crate::accounting::BatchAccount;
use crate::types::{BatchId, BatchType, LedgerKey};

/// A batch a desired state named: an index into the recorder.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BatchRef(pub(crate) usize);

/// Whether a missing batch may be created.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BatchMode {
    /// Reuse the open batch or have the commit create it.
    Open,
    /// Reuse only: no open batch for the id rejects the object.
    Existing,
}

/// One batch the desired state asked for.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchRequest {
    /// The user's batch id.
    pub id: BatchId,
    /// The ledger the account returned.
    pub ledger: LedgerKey,
    /// The column the account returned.
    pub batch_type: BatchType,
    /// Create if absent, or reuse only.
    pub mode: BatchMode,
}

/// A batch id asked for twice with different ledgers or types in one
/// evaluation: a bug in the desired state, rejected before anything books.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BatchConflict {
    /// The id.
    pub id: BatchId,
    /// What it was first asked with.
    pub first: (LedgerKey, BatchType),
    /// What it was asked with afterwards.
    pub second: (LedgerKey, BatchType),
}

/// The recorder.
#[derive(Default, Debug)]
pub struct Batches {
    requests: Vec<BatchRequest>,
    conflicts: Vec<BatchConflict>,
}

impl Batches {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// The open batch for the user's batch id, created if absent with this
    /// account's ledger and type.
    pub fn open(&mut self, account: impl BatchAccount, id: impl Into<BatchId>) -> BatchRef {
        self.record(account, id.into(), BatchMode::Open)
    }

    /// Reuse only: no open batch for the id rejects the object. For a payout
    /// batch a job must have opened first.
    pub fn existing(&mut self, account: impl BatchAccount, id: impl Into<BatchId>) -> BatchRef {
        self.record(account, id.into(), BatchMode::Existing)
    }

    fn record(&mut self, account: impl BatchAccount, id: BatchId, mode: BatchMode) -> BatchRef {
        let ledger = account.ledger();
        let batch_type = account.batch_type();
        if let Some(i) = self.requests.iter().position(|r| r.id == id) {
            let existing = &mut self.requests[i];
            if existing.ledger != ledger || existing.batch_type != batch_type {
                self.conflicts.push(BatchConflict {
                    id,
                    first: (existing.ledger, existing.batch_type),
                    second: (ledger, batch_type),
                });
            } else if mode == BatchMode::Existing {
                existing.mode = BatchMode::Existing;
            }
            return BatchRef(i);
        }
        self.requests.push(BatchRequest {
            id,
            ledger,
            batch_type,
            mode,
        });
        BatchRef(self.requests.len() - 1)
    }

    /// Every batch asked for, in order of first request.
    pub fn requests(&self) -> &[BatchRequest] {
        &self.requests
    }

    /// The request behind a ref.
    pub fn get(&self, r: BatchRef) -> &BatchRequest {
        &self.requests[r.0]
    }

    /// The ids asked for with two different accounts.
    pub fn conflicts(&self) -> &[BatchConflict] {
        &self.conflicts
    }

    /// Appends another recorder's requests, returning the mapping from its
    /// refs to refs in this one. How the engine joins the recorders of
    /// several objects into one resolution.
    pub(crate) fn absorb(&mut self, other: &Batches) -> Vec<BatchRef> {
        let mut map = Vec::with_capacity(other.requests.len());
        for r in &other.requests {
            map.push(self.record(Flat(r.ledger, r.batch_type), r.id.clone(), r.mode));
        }
        self.conflicts.extend(other.conflicts.iter().cloned());
        map
    }
}

struct Flat(LedgerKey, BatchType);

impl BatchAccount for Flat {
    fn ledger(&self) -> LedgerKey {
        self.0
    }

    fn batch_type(&self) -> BatchType {
        self.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Acct(u32, BatchType);
    impl BatchAccount for Acct {
        fn ledger(&self) -> LedgerKey {
            LedgerKey(self.0)
        }
        fn batch_type(&self) -> BatchType {
            self.1
        }
    }

    #[test]
    fn the_same_id_is_one_batch() {
        let mut b = Batches::new();
        let a = b.open(Acct(1, BatchType::Debit), "x");
        let c = b.open(Acct(1, BatchType::Debit), "x");
        assert_eq!(a, c);
        assert_eq!(b.requests().len(), 1);
        assert!(b.conflicts().is_empty());
    }

    #[test]
    fn two_accounts_on_one_id_is_a_conflict() {
        let mut b = Batches::new();
        b.open(Acct(1, BatchType::Debit), "x");
        b.open(Acct(1, BatchType::Credit), "x");
        assert_eq!(b.conflicts().len(), 1);
    }

    #[test]
    fn existing_is_the_stricter_mode() {
        let mut b = Batches::new();
        b.open(Acct(1, BatchType::Debit), "x");
        b.existing(Acct(1, BatchType::Debit), "x");
        assert_eq!(b.requests()[0].mode, BatchMode::Existing);
    }
}
