//! The small value types the ledger speaks: identities, keys, the batch
//! type, and the amount's wire form.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

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

/// The store's key of one physical batch (one epoch of a batch id): an
/// incremental integer, all a line carries beside object, unit and amount.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchKey(pub i64);

/// The user's name for a logical batch: any string. It is the batch's whole
/// identity, so an invoice's receivable and its cash position are two ids.
/// A pair `("receivable", id)` renders as `receivable/<id>`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BatchId(String);

impl BatchId {
    /// A batch id from any string.
    pub fn new(id: impl Into<String>) -> Self {
        BatchId(id.into())
    }

    /// The id as the user wrote it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BatchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for BatchId {
    fn from(s: &str) -> Self {
        BatchId(s.to_owned())
    }
}

impl From<String> for BatchId {
    fn from(s: String) -> Self {
        BatchId(s)
    }
}

impl<T: fmt::Display> From<(&str, T)> for BatchId {
    fn from((prefix, rest): (&str, T)) -> Self {
        BatchId(format!("{prefix}/{rest}"))
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
    /// The small integer a store keeps: 1 debit, 2 credit, 3 memo.
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

/// Where a batch is in its life. Lines only ever land in an open batch; a
/// consumed one never changes again, and a line aimed at it goes to the
/// next epoch of the same batch id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum BatchStatus {
    /// Takes lines.
    Open,
    /// Closed by a claimant; the claim waits for the horizon before summing.
    Closed,
    /// Summed by its claimant; the money is being moved.
    Claimed,
    /// Done. Never changes.
    Consumed,
}

impl BatchStatus {
    /// The small integer a store keeps: 1 open, 2 closed, 3 claimed, 4 consumed.
    pub fn as_i16(self) -> i16 {
        match self {
            BatchStatus::Open => 1,
            BatchStatus::Closed => 2,
            BatchStatus::Claimed => 3,
            BatchStatus::Consumed => 4,
        }
    }

    /// The inverse of [`as_i16`](Self::as_i16); `None` for anything else.
    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            1 => Some(BatchStatus::Open),
            2 => Some(BatchStatus::Closed),
            3 => Some(BatchStatus::Claimed),
            4 => Some(BatchStatus::Consumed),
            _ => None,
        }
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
    fn batch_id_from_pair_renders_with_a_slash() {
        let id: BatchId = ("receivable", 42).into();
        assert_eq!(id.as_str(), "receivable/42");
    }

    #[test]
    fn batch_type_round_trips_through_its_integer() {
        for t in [BatchType::Debit, BatchType::Credit, BatchType::Memo] {
            assert_eq!(BatchType::from_i16(t.as_i16()), Some(t));
        }
        assert_eq!(BatchType::from_i16(0), None);
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
