//! One bounded read-only skill preview returned by `nano.skills.preview`.

use serde_json::Value;

use crate::catalog::SkillError;

/// Instruction-file preview for one skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPreview {
    /// Skill name.
    pub name: String,
    /// Official instruction-file path.
    pub path: String,
    /// UTF-8 text included in the preview.
    pub text: String,
    /// The file was larger than the preview limit.
    pub truncated: bool,
    /// Bytes actually included.
    pub bytes: u64,
    /// Configured preview limit in bytes.
    pub limit: u64,
}

impl SkillPreview {
    /// Read one preview from the wire result.
    pub fn parse(value: &Value) -> Result<Self, SkillError> {
        let name = required_string(value, "name")?;
        let path = required_string(value, "path")?;
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| SkillError("preview text must be a string".into()))?
            .to_string();
        let truncated = required_bool(value, "truncated")?;
        let bytes = required_u64(value, "bytes")?;
        let limit = required_u64(value, "limit")?;
        Ok(Self {
            name,
            path,
            text,
            truncated,
            bytes,
            limit,
        })
    }

    /// Short truncation note, when the file exceeded the limit.
    pub fn truncation_label(&self) -> Option<String> {
        self.truncated.then(|| {
            format!(
                "预览已截断：仅显示前 {} 字节（上限 {} 字节）",
                self.bytes, self.limit
            )
        })
    }

    /// Whether the preview is an actually empty file.
    pub fn empty(&self) -> bool {
        self.text.is_empty() && !self.truncated
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, SkillError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| SkillError(format!("preview.{field} must be a string")))
}

fn required_bool(value: &Value, field: &str) -> Result<bool, SkillError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| SkillError(format!("preview.{field} must be a boolean")))
}

fn required_u64(value: &Value, field: &str) -> Result<u64, SkillError> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| SkillError(format!("preview.{field} must be a non-negative integer")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_preview_parses_and_reports_truncation() {
        let preview = SkillPreview::parse(&json!({
            "address": {"rootSessionId": "root"},
            "name": "hello",
            "path": "/tmp/hello/SKILL.md",
            "text": "hello",
            "truncated": true,
            "bytes": 65536,
            "limit": 65536,
        }))
        .unwrap();
        assert_eq!(preview.name, "hello");
        assert!(preview.truncated);
        assert!(preview.truncation_label().unwrap().contains("65536"));
        assert!(!preview.empty());
    }

    #[test]
    fn an_empty_file_is_distinguishable_from_a_failed_read() {
        let preview = SkillPreview::parse(&json!({
            "address": {"rootSessionId": "root"},
            "name": "empty",
            "path": "/tmp/empty/SKILL.md",
            "text": "",
            "truncated": false,
            "bytes": 0,
            "limit": 65536,
        }))
        .unwrap();
        assert!(preview.empty());
        assert!(preview.truncation_label().is_none());
    }

    #[test]
    fn missing_fields_are_rejected() {
        assert!(SkillPreview::parse(&json!({"name": "hello"})).is_err());
    }
}
