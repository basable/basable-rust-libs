//! The journal: one row per commit of one process, holding the lines of
//! every object the call booked and the snapshot diff of each.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{BatchKey, ObjectId, ProcessId, Timestamp, UnitKey, amount_string};

/// One line: the batch, by key, the object, the unit, the amount. A line
/// carries the batch's key and nothing else about the batch: its ledger
/// and its type are the user's, in their batches table (at least
/// `batch_key`, `ledger` and `batch_type`), found by the key, and the
/// engine takes them from the [`BatchAccount`](crate::BatchAccount) the
/// desired state names at booking time. On the wire a journal's lines are
/// grouped by object, see [`lines_to_json`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line {
    /// The batch, by key.
    pub batch: BatchKey,
    /// The object the line belongs to; what makes an object's booked
    /// positions the sum of its own lines.
    pub object_id: ObjectId,
    /// The unit.
    pub unit: UnitKey,
    /// Signed, in the column of the batch's type.
    pub amount: i128,
}

/// One line as the row stores it: `[batch_key, unit_key, "amount"]`.
#[derive(Deserialize)]
struct LineWire(i64, u32, #[serde(with = "amount_string")] i128);

/// The lines of one journal as the row stores them, grouped by object so
/// the object id is written once:
/// `{"<object_id>": [[batch_key, unit_key, "amount"], ...], ...}`, for
/// example `{"019a…": [[4711, 978, "-11210"]]}`: batch key 4711, 112.10
/// below zero in unit 978, for that object. Objects in id order, each
/// object's lines in the order given (the engine's: by batch, then unit);
/// no lines is `{}`. Amounts are decimal strings: protobuf has no 128-bit
/// integer and JSON numbers above 2^53 lose precision in JavaScript.
pub fn lines_to_json(lines: &[Line]) -> Value {
    let mut by_object: BTreeMap<ObjectId, Vec<Value>> = BTreeMap::new();
    for l in lines {
        by_object
            .entry(l.object_id)
            .or_default()
            .push(Value::Array(vec![
                Value::from(l.batch.0),
                Value::from(l.unit.0),
                Value::String(l.amount.to_string()),
            ]));
    }
    Value::Object(
        by_object
            .into_iter()
            .map(|(object, lines)| (object.0.to_string(), Value::Array(lines)))
            .collect(),
    )
}

/// The lines of one journal read back from the row's form: grouped per
/// object in id order whatever order the row had its keys in, each
/// object's lines in stored order. A top-level array, a line that is not
/// `[batch_key, unit_key, "amount"]`, an amount that is not a decimal
/// string or a key that is not a UUID does not read.
pub fn lines_from_json(value: Value) -> Result<Vec<Line>, serde_json::Error> {
    let by_object: BTreeMap<ObjectId, Vec<LineWire>> = serde_json::from_value(value)?;
    Ok(by_object
        .into_iter()
        .flat_map(|(object_id, lines)| {
            lines
                .into_iter()
                .map(move |LineWire(key, unit, amount)| Line {
                    batch: BatchKey(key),
                    object_id,
                    unit: UnitKey(unit),
                    amount,
                })
        })
        .collect())
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
    /// The commit time.
    pub booked_at: Timestamp,
    /// Every object's delta, each line naming its object.
    pub lines: Vec<Line>,
    /// One per object the journal booked; empty for a restatement.
    pub snapshot_diffs: Vec<SnapshotDiff>,
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn line(batch: i64, object: ObjectId, unit: u32, amount: i128) -> Line {
        Line {
            batch: BatchKey(batch),
            object_id: object,
            unit: UnitKey(unit),
            amount,
        }
    }

    #[test]
    fn lines_are_grouped_by_object_on_the_wire() {
        let id = Uuid::nil();
        let l = line(4711, ObjectId(id), 978, -11210);
        let json = lines_to_json(&[l]);
        assert_eq!(
            json.to_string(),
            format!("{{\"{id}\":[[4711,978,\"-11210\"]]}}")
        );
        assert_eq!(lines_from_json(json).unwrap(), vec![l]);
    }

    #[test]
    fn lines_read_back_in_object_order_whatever_order_the_row_had() {
        let a = ObjectId(Uuid::from_u128(1));
        let b = ObjectId(Uuid::from_u128(2));
        let lines = [line(1, b, 978, 5), line(2, a, 978, 7), line(1, b, 840, 9)];
        let json = lines_to_json(&lines);
        let keys: Vec<String> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec![a.0.to_string(), b.0.to_string()]);
        assert_eq!(
            lines_from_json(json).unwrap(),
            vec![lines[1], lines[0], lines[2]]
        );
    }

    #[test]
    fn no_lines_is_an_empty_object() {
        assert_eq!(lines_to_json(&[]), serde_json::json!({}));
        assert!(lines_from_json(serde_json::json!({})).unwrap().is_empty());
    }

    #[test]
    fn a_malformed_row_does_not_read() {
        let id = Uuid::nil();
        for bad in [
            // The flat array form.
            r#"[[4711,978,"1"]]"#.to_owned(),
            // Two elements.
            format!(r#"{{"{id}":[[4711,978]]}}"#),
            // Four elements.
            format!(r#"{{"{id}":[[4711,1,978,"1"]]}}"#),
            // A numeric amount.
            format!(r#"{{"{id}":[[4711,978,1]]}}"#),
            // A key that is not an object id.
            r#"{"not-an-id":[[4711,978,"1"]]}"#.to_owned(),
        ] {
            let value: Value = serde_json::from_str(&bad).unwrap();
            assert!(lines_from_json(value).is_err(), "{bad} read");
        }
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
