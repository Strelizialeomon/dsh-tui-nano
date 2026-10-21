//! Accepted session renaming.
//!
//! The official title service normalizes the text, rejects an empty result, and
//! appends a durable `session/title` event. Only the active root may be renamed;
//! the bridge enforces that, and this type carries the accepted result.

use serde_json::Value;

/// One accepted rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameOutcome {
    /// Normalized title exactly as accepted by the official service.
    pub title: String,
    /// Durable `session/title` event sequence.
    pub seq: u64,
}

impl RenameOutcome {
    /// Parse one `nano.sessions.rename` result.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "rename result must be an object".to_string())?;
        let title = match object.get("title") {
            Some(Value::String(value)) if !value.is_empty() => value.clone(),
            _ => return Err("rename.title must be a non-empty string".to_string()),
        };
        let seq = object
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or_else(|| "rename.seq must be a non-negative integer".to_string())?;
        Ok(Self { title, seq })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_durable_result() {
        let outcome = RenameOutcome::parse(&json!({"title": "新的标题", "seq": 7})).unwrap();
        assert_eq!(outcome.title, "新的标题");
        assert_eq!(outcome.seq, 7);
    }

    #[test]
    fn rejects_an_empty_title_instead_of_showing_it() {
        assert!(RenameOutcome::parse(&json!({"title": "", "seq": 7})).is_err());
        assert!(RenameOutcome::parse(&json!({"title": "x"})).is_err());
    }
}
