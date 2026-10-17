//! The journal: one row per commit of one process, holding the lines of
//! every object the call booked and the snapshot diff of each.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::types::{BatchKey, ObjectId, ProcessId, Timestamp, UnitKey, amount_string};

/// One line: batch key, object, unit, amount. On the wire an array,
/// `[4711, "019a…", 978, "11210"]`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line {
    /// The physical batch (one epoch of a batch id).
    pub batch_key: BatchKey,
    /// The object the line belongs to; what makes an object's booked
    /// positions the sum of its own lines.
    pub object_id: ObjectId,
    /// The unit.
    pub unit: UnitKey,
    /// Signed, in the column of the batch's type.
    pub amount: i128,
}

#[derive(Serialize, Deserialize)]
struct LineWire(i64, Uuid, u32, #[serde(with = "amount_string")] i128);

impl Serialize for Line {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        LineWire(self.batch_key.0, self.object_id.0, self.unit.0, self.amount).serialize(s)
    }
}

impl<'de> Deserialize<'de> for Line {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let LineWire(b, o, u, a) = LineWire::deserialize(d)?;
        Ok(Line {
            batch_key: BatchKey(b),
            object_id: ObjectId(o),
            unit: UnitKey(u),
            amount: a,
        })
    }
}

/// What changed in one object: its full JSON the first time the process
/// sees it, a merge patch against the folded object afterwards. Always with
/// the object's id and type, so the fold knows which reader to apply.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct SnapshotDiff {
    /// The object.
    pub object_id: ObjectId,
    /// The object type's name.
    #[serde(rename = "type")]
    pub type_name: String,
    /// The whole object, on first appearance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full: Option<Value>,
    /// The merge patch, afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<Value>,
}

/// One journal of one process.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Journal {
    /// The process.
    pub process_id: ProcessId,
    /// The process version this journal made: gapless from 1.
    pub sequence: i64,
    /// The latest state-entered instant among the objects booked.
    pub event_time: Timestamp,
    /// The commit time.
    pub booked_at: Timestamp,
    /// Every object's delta, each line naming its object.
    pub lines: Vec<Line>,
    /// One per object the journal booked; empty for a restatement.
    pub snapshot_diffs: Vec<SnapshotDiff>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_an_array_on_the_wire() {
        let id = Uuid::nil();
        let line = Line {
            batch_key: BatchKey(4711),
            object_id: ObjectId(id),
            unit: UnitKey(978),
            amount: -11210,
        };
        let json = serde_json::to_string(&line).unwrap();
        assert_eq!(json, format!("[4711,\"{id}\",978,\"-11210\"]"));
        let back: Line = serde_json::from_str(&json).unwrap();
        assert_eq!(back, line);
    }

    #[test]
    fn a_diff_carries_full_or_patch() {
        let d = SnapshotDiff {
            object_id: ObjectId(Uuid::nil()),
            type_name: "invoice".into(),
            full: None,
            patch: Some(serde_json::json!({"paid": "5"})),
        };
        let json = serde_json::to_value(&d).unwrap();
        assert!(json.get("full").is_none());
        assert_eq!(json["type"], "invoice");
        let back: SnapshotDiff = serde_json::from_value(json).unwrap();
        assert_eq!(back, d);
    }
}
