//! The small value types the ledger speaks: identities, keys, the batch
//! type, a batch as a line names it, and the amount's wire form.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::accounting::BatchAccount;

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

/// The user's key of a batch: the one integer a line carries to name the
/// batch it belongs to. The framework keeps no batch table. The key is what
/// [`BatchAccount::batch_key`] returns: allocated in a batch table of the
/// user's own, or encoded or hashed from whatever names the batch. One key
/// is one batch, in one ledger, of one type, for good.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchKey(pub i64);

impl<A: BatchAccount + ?Sized> From<&A> for BatchKey {
    fn from(account: &A) -> Self {
        account.batch_key()
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

/// A batch as a line names it: the user's key and the two things the
/// framework knows about the batch, the ledger it balances in and its
/// column. A desired state names all three; a stored line carries the key
/// alone, and the user's batches table has the other two by it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Batch {
    /// The user's key.
    pub key: BatchKey,
    /// The ledger it balances in.
    pub ledger: LedgerKey,
    /// Its column.
    pub batch_type: BatchType,
}

impl Batch {
    /// From whatever names the batch.
    pub fn of(account: &(impl BatchAccount + ?Sized)) -> Self {
        Batch {
            key: account.batch_key(),
            ledger: account.ledger(),
            batch_type: account.batch_type(),
        }
    }
}

impl BatchAccount for Batch {
    fn batch_key(&self) -> BatchKey {
        self.key
    }

    fn ledger(&self) -> LedgerKey {
        self.ledger
    }

    fn batch_type(&self) -> BatchType {
        self.batch_type
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

    #[test]
    fn a_batch_key_comes_from_whatever_names_the_batch() {
        let b = Batch {
            key: BatchKey(7),
            ledger: LedgerKey(1),
            batch_type: BatchType::Credit,
        };
        let key: BatchKey = (&b).into();
        assert_eq!(key, BatchKey(7));
        assert_eq!(Batch::of(&b), b);
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
