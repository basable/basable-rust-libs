//! The small value types the ledger speaks: identities, keys, the batch
//! type, a batch's row as a store returns it, and the amount's wire form.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::accounting::LedgerBatch;
use crate::error::Rejection;

/// A UTC instant to the microsecond, as Postgres stores it.
pub type Timestamp = chrono::DateTime<chrono::Utc>;

/// The business process every object of a flow shares: the consistency
/// boundary. A root object's process id is its own id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessId(pub Uuid);

/// One object within a process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub Uuid);

/// A ledger: the boundary a balance may not cross. Every batch belongs to
/// one, and the book balances per ledger and unit.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LedgerKey(pub u32);

/// A unit of account, by the key the user's [`Unit`](crate::Unit) returns:
/// what a line carries and what the engine groups and balances by. What the
/// key means (a currency and its decimals, a custom unit) is the user's.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnitKey(pub u32);

/// The key of a batch: the one integer a line carries to name the batch it
/// belongs to. One sequence allocates it over every batch table of the user's
/// when a batch's row is created, just before the journal that first books
/// on it. One key is one batch, in one ledger, of one type, for good.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchKey(pub i64);

/// Keys below this stand in for a batch with no row while a call is planned.
/// A sequence never reaches them; the engine refuses a row that holds one.
const PLACEHOLDER_END: i64 = i64::MIN + (1 << 32);

impl BatchKey {
    /// The placeholder of the `index`th batch of a call that has no row.
    pub(crate) fn placeholder(index: usize) -> Self {
        BatchKey(i64::MIN + index as i64)
    }

    /// Whether this key is a placeholder, not a row's.
    pub(crate) fn is_placeholder(self) -> bool {
        self.0 < PLACEHOLDER_END
    }
}

/// A batch's row as a store returns it: its key, and the ledger and type the
/// row holds. The other columns of the row are the user's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StoredBatch {
    /// The key lines carry.
    pub key: BatchKey,
    /// The ledger the row says it balances in.
    pub ledger: LedgerKey,
    /// The column the row says it falls in.
    pub batch_type: BatchType,
}

impl StoredBatch {
    /// Whether the value names this row under the ledger and type the row
    /// holds. A row is for good, so a value that disagrees rejects.
    pub fn check<B: LedgerBatch>(&self, batch: &B) -> Result<(), Rejection> {
        if self.ledger == batch.ledger() && self.batch_type == batch.batch_type() {
            return Ok(());
        }
        Err(Rejection::new(format!(
            "batch {:?} (key {}) is in ledger {} as {} but the value names it in ledger {} as {}",
            batch.primary_key(),
            self.key.0,
            self.ledger.0,
            self.batch_type,
            batch.ledger().0,
            batch.batch_type()
        )))
    }
}

/// Which column of the book a batch's amounts fall in. `Debit` is the left
/// column (assets, expenses), `Credit` the right (liabilities, equity,
/// revenue). `Memo` stands outside the book: a batch of something that is
/// not money, batched and summed like any other but left out of the
/// left-equals-right check and never exported.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BatchType {
    /// The left column.
    Debit,
    /// The right column.
    Credit,
    /// Outside the book.
    Memo,
}

impl BatchType {
    /// The small integer a line carries: 1 debit, 2 credit, 3 memo.
    pub fn as_i16(self) -> i16 {
        match self {
            BatchType::Debit => 1,
            BatchType::Credit => 2,
            BatchType::Memo => 3,
        }
    }

    /// The inverse of [`as_i16`](Self::as_i16); `None` for anything else.
    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            1 => Some(BatchType::Debit),
            2 => Some(BatchType::Credit),
            3 => Some(BatchType::Memo),
            _ => None,
        }
    }
}

impl fmt::Display for BatchType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BatchType::Debit => "debit",
            BatchType::Credit => "credit",
            BatchType::Memo => "memo",
        })
    }
}

/// Amounts on the wire are decimal strings: protobuf has no 128-bit
/// integer, JSON numbers above 2^53 lose precision in JavaScript, and
/// Postgres keeps them exact as `NUMERIC(39,0)` through text.
pub mod amount_string {
    use super::*;

    /// Serializes an `i128` as its decimal string.
    pub fn serialize<S: Serializer>(amount: &i128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&amount.to_string())
    }

    /// Deserializes an `i128` from its decimal string.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
        let s = String::deserialize(d)?;
        s.parse::<i128>().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_type_round_trips_through_its_integer() {
        for t in [BatchType::Debit, BatchType::Credit, BatchType::Memo] {
            assert_eq!(BatchType::from_i16(t.as_i16()), Some(t));
        }
        assert_eq!(BatchType::from_i16(0), None);
    }

    #[derive(Clone, Debug)]
    struct Named(LedgerKey, BatchType);

    impl LedgerBatch for Named {
        type PrimaryKey = u8;

        fn primary_key(&self) -> u8 {
            1
        }

        fn ledger(&self) -> LedgerKey {
            self.0
        }

        fn batch_type(&self) -> BatchType {
            self.1
        }
    }

    #[test]
    fn a_value_that_disagrees_with_its_row_rejects() {
        let row = StoredBatch {
            key: BatchKey(7),
            ledger: LedgerKey(1),
            batch_type: BatchType::Credit,
        };
        assert!(row.check(&Named(LedgerKey(1), BatchType::Credit)).is_ok());
        for other in [
            Named(LedgerKey(2), BatchType::Credit),
            Named(LedgerKey(1), BatchType::Debit),
        ] {
            let reason = row.check(&other).expect_err("disagrees");
            assert!(reason.message().contains("key 7"), "{reason}");
        }
    }

    #[test]
    fn a_placeholder_is_never_a_sequence_value() {
        assert!(BatchKey::placeholder(0).is_placeholder());
        assert!(BatchKey::placeholder(1_000_000).is_placeholder());
        assert!(!BatchKey(1).is_placeholder());
        assert!(!BatchKey(i64::MAX).is_placeholder());
        assert!(!BatchKey(-1).is_placeholder());
    }

    #[test]
    fn amounts_cross_the_wire_as_strings() {
        #[derive(Serialize, Deserialize)]
        struct W(#[serde(with = "amount_string")] i128);
        let json = serde_json::to_string(&W(i128::MIN)).unwrap();
        assert_eq!(json, format!("\"{}\"", i128::MIN));
        let back: W = serde_json::from_str(&json).unwrap();
        assert_eq!(back.0, i128::MIN);
    }
}
