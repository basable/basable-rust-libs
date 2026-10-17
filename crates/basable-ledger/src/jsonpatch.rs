//! JSON merge patch (RFC 7386): how a journal records what changed in an
//! object. The first time a process sees an object its full JSON is stored;
//! afterwards the patch of the new snapshot against the folded one. Folding
//! a process's diffs in sequence order rebuilds any object at any sequence.
//!
//! A `null` in a patch deletes the member, so an optional field that went
//! back to `None` is absent after the fold, which serde reads as `None`.

use serde_json::{Map, Value};

/// The merge patch that turns `from` into `to`.
pub fn diff(from: &Value, to: &Value) -> Value {
    match (from, to) {
        (Value::Object(a), Value::Object(b)) => {
            let mut patch = Map::new();
            for (k, bv) in b {
                match a.get(k) {
                    Some(av) if av == bv => {}
                    Some(av) => {
                        patch.insert(k.clone(), diff(av, bv));
                    }
                    None => {
                        patch.insert(k.clone(), strip_nulls(bv));
                    }
                }
            }
            for k in a.keys() {
                if !b.contains_key(k) {
                    patch.insert(k.clone(), Value::Null);
                }
            }
            Value::Object(patch)
        }
        (_, b) => strip_nulls(b),
    }
}

/// Applies a merge patch to `target` in place.
pub fn apply(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let t = target.as_object_mut().expect("object");
            for (k, pv) in p {
                if pv.is_null() {
                    t.remove(k);
                } else {
                    let slot = t.entry(k.clone()).or_insert(Value::Null);
                    apply(slot, pv);
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// A value as it is written whole into a patch: a `null` member inside it
/// would read as a deletion, so nulls are dropped from objects. A null
/// element of an array stays (arrays are replaced whole).
fn strip_nulls(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip(from: Value, to: Value) {
        let patch = diff(&from, &to);
        let mut folded = from.clone();
        apply(&mut folded, &patch);
        assert_eq!(
            folded,
            strip_nulls(&to),
            "patch {patch} from {from} to {to}"
        );
    }

    #[test]
    fn changed_added_and_removed_members() {
        round_trip(
            json!({"a": 1, "b": {"c": 2, "d": 3}, "e": "x"}),
            json!({"a": 1, "b": {"c": 20}, "f": true}),
        );
    }

    #[test]
    fn arrays_are_replaced_whole() {
        let patch = diff(&json!({"l": [1, 2, 3]}), &json!({"l": [1, 2]}));
        assert_eq!(patch, json!({"l": [1, 2]}));
    }

    #[test]
    fn a_full_first_snapshot_is_the_value_itself() {
        let v = json!({"id": "x", "net": "100"});
        assert_eq!(diff(&Value::Null, &v), v);
    }

    #[test]
    fn an_unchanged_object_gives_an_empty_patch() {
        let v = json!({"a": {"b": 1}});
        assert_eq!(diff(&v, &v), json!({}));
    }

    #[test]
    fn a_member_set_to_none_is_absent_after_the_fold() {
        round_trip(json!({"a": 1, "o": 5}), json!({"a": 1, "o": null}));
    }
}
