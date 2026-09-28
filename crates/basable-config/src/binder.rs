//! Helpers a binder uses to turn message fields into columns: the shapes a
//! proto3 message cannot carry natively (a reference id, a timestamp, a
//! JSON document arrive as strings) parsed once, with the caller's input
//! named as the fault.

use chrono::{DateTime, Utc};
use serde_json::Value;
use uuid::Uuid;

use crate::error::BinderError;

/// A reference field: `#{Type:namespace:name}` in the seed, the target's
/// id once the loader resolved it (runtime callers pass ids directly).
/// Empty is no reference.
pub fn parse_reference(raw: &str) -> Result<Option<Uuid>, BinderError> {
    if raw.is_empty() {
        return Ok(None);
    }
    Uuid::parse_str(raw)
        .map(Some)
        .map_err(|e| BinderError::Invalid(format!("reference {raw:?} is not a UUID: {e}")))
}

/// A timestamp field: RFC 3339 text, empty for none.
pub fn parse_timestamp(raw: &str) -> Result<Option<DateTime<Utc>>, BinderError> {
    if raw.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|t| Some(t.with_timezone(&Utc)))
        .map_err(|e| BinderError::Invalid(format!("timestamp {raw:?} is not RFC 3339: {e}")))
}

/// A JSON field: a document as text, empty for `null`.
pub fn parse_json(raw: &str) -> Result<Value, BinderError> {
    if raw.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(raw).map_err(|e| BinderError::Invalid(format!("json field: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_string_shapes_parse_or_name_the_input() {
        assert_eq!(parse_reference("").unwrap(), None);
        let id = Uuid::new_v4();
        assert_eq!(parse_reference(&id.to_string()).unwrap(), Some(id));
        assert!(matches!(
            parse_reference("#{Unit:ns:eur}"),
            Err(BinderError::Invalid(_))
        ));
        assert_eq!(parse_timestamp("").unwrap(), None);
        assert_eq!(
            parse_timestamp("2026-01-01T00:00:00Z")
                .unwrap()
                .unwrap()
                .to_rfc3339(),
            "2026-01-01T00:00:00+00:00"
        );
        assert!(parse_timestamp("yesterday").is_err());
        assert_eq!(parse_json("").unwrap(), Value::Null);
        assert_eq!(parse_json(r#"{"a":1}"#).unwrap()["a"], 1);
        assert!(parse_json("{").is_err());
    }
}
