//! Last-good skill catalog from `nano.skills.list`.
//!
//! The official registry decides discovery, merging, and completeness. This
//! projection only keeps the last usable result, marks an incomplete refresh
//! without erasing entries, and answers the command-first adjudication for one
//! slash name.

use serde_json::Value;

/// Error while reading one wire value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillError(pub String);

impl std::fmt::Display for SkillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SkillError {}

/// One user-invocable skill summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    /// Kebab-case name referenced as `/name`.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional extra routing guidance.
    pub when_to_use: Option<String>,
    /// Whether the model-facing catalog and tool include this skill.
    pub model_invocable: bool,
    /// Whether the human-facing catalog includes this skill.
    pub user_invocable: bool,
    /// Absolute instruction file path, when the provider supplied one.
    pub path: Option<String>,
    /// Official discovery source label.
    pub source: String,
    /// Official provider name.
    pub provider: String,
    /// A host command of the same name exists, so the command wins.
    pub shadowed_by_command: bool,
}

impl SkillEntry {
    /// Marker for a skill the model cannot invoke; only `/name` reaches it.
    pub fn user_only(&self) -> bool {
        !self.model_invocable
    }

    /// Read one entry from the wire object.
    pub fn parse(value: &Value) -> Result<Self, SkillError> {
        let name = required_string(value, "name")?;
        let description = required_string(value, "description")?;
        Ok(Self {
            name,
            description,
            when_to_use: optional_string(value, "whenToUse")?,
            model_invocable: required_bool(value, "modelInvocable")?,
            user_invocable: required_bool(value, "userInvocable")?,
            path: optional_string(value, "path")?,
            source: required_string(value, "source")?,
            provider: required_string(value, "provider")?,
            shadowed_by_command: required_bool(value, "shadowedByCommand")?,
        })
    }
}

/// What one slash name means at the client boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameResolution {
    /// A host command owns the name; it must never become a skill prompt.
    Command,
    /// A user-invocable skill matches.
    Skill,
    /// Unknown or deleted; the literal line still passes through as prose.
    Unknown,
}

/// Last-good catalog plus the official command names for adjudication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    entries: Vec<SkillEntry>,
    commands: Vec<String>,
    complete: bool,
    loaded: bool,
    stale_reason: Option<String>,
}

impl SkillCatalog {
    /// Empty catalog that has never loaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one successful `nano.skills.list` result.
    ///
    /// A successful list is authoritative: skills removed from the official
    /// catalog disappear here, while an incomplete observation keeps the
    /// entries it did report and stays marked as possibly incomplete.
    pub fn apply_list(&mut self, value: &Value, expected_root: &str) -> Result<(), SkillError> {
        let address = value
            .get("address")
            .ok_or_else(|| SkillError("skills list needs an address".into()))?;
        let root = required_string(address, "rootSessionId")?;
        if root != expected_root {
            return Err(SkillError(format!(
                "skills list belongs to root session {root}, not {expected_root}"
            )));
        }
        let complete = required_bool(value, "complete")?;
        let commands = string_array(value, "commands")?;
        let skills = value
            .get("skills")
            .and_then(Value::as_array)
            .ok_or_else(|| SkillError("skills must be an array".into()))?;
        let mut entries = Vec::with_capacity(skills.len());
        for skill in skills {
            entries.push(SkillEntry::parse(skill)?);
        }
        self.entries = entries;
        self.commands = commands;
        self.complete = complete;
        self.loaded = true;
        self.stale_reason = None;
        Ok(())
    }

    /// Keep the last usable catalog when a refresh fails.
    ///
    /// The failure is reported as incompleteness, never as "no skills".
    pub fn apply_failure(&mut self, message: impl Into<String>) {
        self.complete = false;
        self.stale_reason = Some(message.into());
    }

    /// Entries in official order.
    pub fn entries(&self) -> &[SkillEntry] {
        &self.entries
    }

    /// Official command names used for command-first adjudication.
    pub fn commands(&self) -> &[String] {
        &self.commands
    }

    /// Whether the last successful list was a complete observation.
    pub fn complete(&self) -> bool {
        self.complete
    }

    /// Whether any list result has been applied.
    pub fn loaded(&self) -> bool {
        self.loaded
    }

    /// Why the catalog is incomplete, when a refresh failed.
    pub fn stale_reason(&self) -> Option<&str> {
        self.stale_reason.as_deref()
    }

    /// Whether the client must warn that the catalog may be incomplete.
    pub fn may_be_incomplete(&self) -> bool {
        self.loaded && (!self.complete || self.stale_reason.is_some())
    }

    /// Resolve one slash name. Commands always win over a same-name skill.
    pub fn resolve(&self, name: &str) -> NameResolution {
        if self.commands.iter().any(|command| command == name) {
            return NameResolution::Command;
        }
        if self
            .entries
            .iter()
            .any(|entry| entry.name == name && entry.user_invocable)
        {
            return NameResolution::Skill;
        }
        NameResolution::Unknown
    }

    /// Find one user-invocable entry by exact name.
    pub fn find(&self, name: &str) -> Option<&SkillEntry> {
        self.entries
            .iter()
            .find(|entry| entry.name == name && entry.user_invocable)
    }
}

fn required_string(value: &Value, field: &str) -> Result<String, SkillError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| SkillError(format!("{field} must be a string")))
}

fn optional_string(value: &Value, field: &str) -> Result<Option<String>, SkillError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(SkillError(format!("{field} must be a string when present"))),
    }
}

fn required_bool(value: &Value, field: &str) -> Result<bool, SkillError> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| SkillError(format!("{field} must be a boolean")))
}

fn string_array(value: &Value, field: &str) -> Result<Vec<String>, SkillError> {
    let array = value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| SkillError(format!("{field} must be an array")))?;
    let mut result = Vec::with_capacity(array.len());
    for item in array {
        let text = item
            .as_str()
            .ok_or_else(|| SkillError(format!("{field} entries must be strings")))?;
        result.push(text.to_string());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list_value(skills: Value, commands: Value, complete: bool) -> Value {
        json!({
            "address": {"rootSessionId": "root"},
            "complete": complete,
            "commands": commands,
            "skills": skills,
        })
    }

    fn entry(name: &str, shadowed: bool) -> Value {
        json!({
            "name": name,
            "description": "does a thing",
            "modelInvocable": true,
            "userInvocable": true,
            "source": "project-agents",
            "provider": "filesystem",
            "shadowedByCommand": shadowed,
        })
    }

    #[test]
    fn a_successful_list_replaces_the_catalog() {
        let mut catalog = SkillCatalog::new();
        catalog
            .apply_list(
                &list_value(json!([entry("old", false)]), json!([]), true),
                "root",
            )
            .unwrap();
        catalog
            .apply_list(
                &list_value(json!([entry("new", false)]), json!([]), true),
                "root",
            )
            .unwrap();
        assert_eq!(catalog.entries().len(), 1);
        assert_eq!(catalog.entries()[0].name, "new");
        assert!(catalog.complete());
        assert!(!catalog.may_be_incomplete());
        assert_eq!(catalog.resolve("old"), NameResolution::Unknown);
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_catalog_and_marks_it_incomplete() {
        let mut catalog = SkillCatalog::new();
        catalog
            .apply_list(
                &list_value(json!([entry("hello", false)]), json!([]), true),
                "root",
            )
            .unwrap();
        catalog.apply_failure("discovery failed");
        assert_eq!(catalog.entries().len(), 1);
        assert!(catalog.may_be_incomplete());
        assert_eq!(catalog.stale_reason(), Some("discovery failed"));
        assert_eq!(catalog.resolve("hello"), NameResolution::Skill);
    }

    #[test]
    fn an_incomplete_official_observation_is_marked_but_kept() {
        let mut catalog = SkillCatalog::new();
        catalog
            .apply_list(
                &list_value(json!([entry("hello", false)]), json!([]), false),
                "root",
            )
            .unwrap();
        assert!(catalog.may_be_incomplete());
        assert_eq!(catalog.entries().len(), 1);
    }

    #[test]
    fn commands_win_over_same_name_skills_and_stand_alone() {
        let mut catalog = SkillCatalog::new();
        catalog
            .apply_list(
                &list_value(
                    json!([entry("compact", true), entry("hello", false)]),
                    json!(["compact", "model"]),
                    true,
                ),
                "root",
            )
            .unwrap();
        assert_eq!(catalog.resolve("compact"), NameResolution::Command);
        assert_eq!(catalog.resolve("model"), NameResolution::Command);
        assert_eq!(catalog.resolve("hello"), NameResolution::Skill);
        assert_eq!(catalog.resolve("gone"), NameResolution::Unknown);
    }

    #[test]
    fn user_invocable_false_entries_do_not_resolve() {
        let mut catalog = SkillCatalog::new();
        let mut hidden = entry("hidden", false);
        hidden["userInvocable"] = json!(false);
        catalog
            .apply_list(&list_value(json!([hidden]), json!([]), true), "root")
            .unwrap();
        assert_eq!(catalog.resolve("hidden"), NameResolution::Unknown);
        assert!(catalog.find("hidden").is_none());
    }

    #[test]
    fn a_foreign_root_or_missing_field_is_rejected() {
        let mut catalog = SkillCatalog::new();
        assert!(catalog
            .apply_list(&list_value(json!([]), json!([]), true), "other")
            .is_err());
        let mut broken = list_value(json!([{"name": "x"}]), json!([]), true);
        broken["skills"][0]["description"] = json!("d");
        assert!(catalog.apply_list(&broken, "root").is_err());
    }
}
