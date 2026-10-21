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
    /// Read one preview from the wire result. The address must name the root
    /// session that asked for the preview; a stale or foreign response is
    /// rejected instead of displayed in this window.
    pub fn parse(value: &Value, expected_root: &str) -> Result<Self, SkillError> {
        let address = value
            .get("address")
            .ok_or_else(|| SkillError("preview needs an address".into()))?;
        let root = address
            .get("rootSessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| SkillError("preview address needs rootSessionId".into()))?;
        if root != expected_root {
            return Err(SkillError(format!(
                "preview belongs to root session {root}, not {expected_root}"
            )));
        }
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
        let preview = SkillPreview::parse(
            &json!({
                "address": {"rootSessionId": "root"},
                "name": "hello",
                "path": "/tmp/hello/SKILL.md",
                "text": "hello",
                "truncated": true,
                "bytes": 65536,
                "limit": 65536,
            }),
            "root",
        )
        .unwrap();
        assert_eq!(preview.name, "hello");
        assert!(preview.truncated);
        assert!(preview.truncation_label().unwrap().contains("65536"));
        assert!(!preview.empty());
    }

    #[test]
    fn an_empty_file_is_distinguishable_from_a_failed_read() {
        let preview = SkillPreview::parse(
            &json!({
                "address": {"rootSessionId": "root"},
                "name": "empty",
                "path": "/tmp/empty/SKILL.md",
                "text": "",
                "truncated": false,
                "bytes": 0,
                "limit": 65536,
            }),
            "root",
        )
        .unwrap();
        assert!(preview.empty());
        assert!(preview.truncation_label().is_none());
    }

    #[test]
    fn missing_fields_are_rejected() {
        assert!(SkillPreview::parse(&json!({"name": "hello"}), "root").is_err());
    }

    #[test]
    fn a_foreign_address_is_rejected() {
        let error = SkillPreview::parse(
            &json!({
                "address": {"rootSessionId": "other"},
                "name": "hello",
                "path": "/tmp/hello/SKILL.md",
                "text": "hello",
                "truncated": false,
                "bytes": 5,
                "limit": 65536,
            }),
            "root",
        )
        .unwrap_err();
        assert!(error.0.contains("belongs to root session"));
    }
}
