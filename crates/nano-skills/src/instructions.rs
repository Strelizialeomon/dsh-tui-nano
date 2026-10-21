//! Workspace-instruction sources folded by the bridge.
//!
//! The official `agent-instructions` loader owns discovery order, duplicate
//! collapsing, and the byte budget. This projection keeps the source list it
//! actually rendered, the latest load time, and the official budget notice.

use serde_json::Value;

use crate::catalog::SkillError;

/// The last state transition reported for one instruction file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionAction {
    /// The file entered the baseline.
    Set,
    /// The file content changed.
    Replace,
    /// The file no longer applies.
    Remove,
}

impl InstructionAction {
    /// Short user-facing label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Set => "载入",
            Self::Replace => "更新",
            Self::Remove => "移除",
        }
    }

    /// Parse the wire action.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "set" => Some(Self::Set),
            "replace" => Some(Self::Replace),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }
}

/// One instruction file in official load order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionSource {
    /// Display path, such as `AGENTS.md` or `~/.agents/AGENTS.md`.
    pub path: String,
    /// Last transition applied to this file.
    pub action: InstructionAction,
}

/// Current workspace-instruction projection for one session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstructionState {
    /// Whether the official loader produced at least one message.
    pub available: bool,
    /// Whether the latest message was a complete baseline.
    pub baseline: bool,
    /// Durable event time of the latest instruction message, in epoch ms.
    pub loaded_at: Option<u64>,
    /// Sources in official load order; duplicates are already collapsed.
    pub sources: Vec<InstructionSource>,
    /// Official byte-budget notice, when rendering omitted or truncated files.
    pub budget_notice: Option<String>,
    /// The projection itself was bounded.
    pub truncated: bool,
}

impl InstructionState {
    /// Empty state before the first instruction message.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the projection with one absolute snapshot or live event.
    pub fn apply(&mut self, value: &Value, expected_root: &str) -> Result<(), SkillError> {
        let address = value
            .get("address")
            .ok_or_else(|| SkillError("instruction snapshot needs an address".into()))?;
        let root = address
            .get("rootSessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| SkillError("instruction address needs rootSessionId".into()))?;
        if root != expected_root {
            return Err(SkillError(format!(
                "instruction snapshot belongs to root session {root}, not {expected_root}"
            )));
        }
        let available = value
            .get("available")
            .and_then(Value::as_bool)
            .ok_or_else(|| SkillError("available must be a boolean".into()))?;
        let baseline = value
            .get("baseline")
            .map(|item| {
                item.as_bool()
                    .ok_or_else(|| SkillError("baseline must be a boolean".into()))
            })
            .transpose()?
            .unwrap_or(false);
        let loaded_at = value
            .get("loadedAt")
            .map(|item| {
                item.as_u64()
                    .ok_or_else(|| SkillError("loadedAt must be a non-negative integer".into()))
            })
            .transpose()?;
        let budget_notice = match value.get("budgetNotice") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return Err(SkillError("budgetNotice must be a string".into())),
        };
        let truncated = value
            .get("truncated")
            .map(|item| {
                item.as_bool()
                    .ok_or_else(|| SkillError("truncated must be a boolean".into()))
            })
            .transpose()?
            .unwrap_or(false);
        let sources = value
            .get("sources")
            .and_then(Value::as_array)
            .ok_or_else(|| SkillError("sources must be an array".into()))?;
        let mut folded = Vec::with_capacity(sources.len());
        for source in sources {
            let path = source
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| SkillError("source.path must be a string".into()))?;
            let action = source
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| SkillError("source.action must be a string".into()))?;
            let action = InstructionAction::parse(action)
                .ok_or_else(|| SkillError(format!("unknown instruction action {action}")))?;
            folded.push(InstructionSource {
                path: path.to_string(),
                action,
            });
        }
        self.available = available;
        self.baseline = baseline;
        self.loaded_at = loaded_at;
        self.sources = folded;
        self.budget_notice = budget_notice;
        self.truncated = truncated;
        Ok(())
    }

    /// Human-readable age of the latest load.
    pub fn age_label(&self, now_ms: u64) -> String {
        let Some(loaded) = self.loaded_at else {
            return "未知".to_string();
        };
        let seconds = now_ms.saturating_sub(loaded) / 1000;
        if seconds < 5 {
            "刚刚".to_string()
        } else if seconds < 60 {
            format!("{seconds} 秒前")
        } else if seconds < 3_600 {
            format!("{} 分钟前", seconds / 60)
        } else if seconds < 86_400 {
            format!("{} 小时前", seconds / 3_600)
        } else {
            format!("{} 天前", seconds / 86_400)
        }
    }

    /// Fixed explanation for the official duplicate collapse. The loader hides
    /// the shadowed duplicate, so the UI explains the rule instead of inventing
    /// a second source row.
    pub fn merge_hint() -> &'static str {
        "同目录内容相同的 base 文件已由官方合并，只列一次。"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(sources: Value) -> Value {
        json!({
            "address": {"rootSessionId": "root"},
            "available": true,
            "baseline": true,
            "loadedAt": 1_000_000,
            "sources": sources,
            "budgetNotice": "Workspace instruction budget 65536 bytes: truncated AGENTS.md from 70000 to 65000 bytes",
        })
    }

    #[test]
    fn an_absolute_snapshot_replaces_the_source_list() {
        let mut state = InstructionState::new();
        state
            .apply(
                &snapshot(json!([
                    {"path": "~/.agents/AGENTS.md", "action": "set"},
                    {"path": "AGENTS.md", "action": "replace"},
                ])),
                "root",
            )
            .unwrap();
        assert!(state.available);
        assert!(state.baseline);
        assert_eq!(state.loaded_at, Some(1_000_000));
        assert_eq!(
            state
                .sources
                .iter()
                .map(|source| (source.path.as_str(), source.action))
                .collect::<Vec<_>>(),
            vec![
                ("~/.agents/AGENTS.md", InstructionAction::Set),
                ("AGENTS.md", InstructionAction::Replace),
            ]
        );
        assert!(state
            .budget_notice
            .as_deref()
            .unwrap()
            .starts_with("Workspace instruction budget"));
    }

    #[test]
    fn an_unavailable_snapshot_is_not_an_empty_success() {
        let mut state = InstructionState::new();
        state
            .apply(
                &json!({
                    "address": {"rootSessionId": "root"},
                    "available": false,
                    "sources": [],
                }),
                "root",
            )
            .unwrap();
        assert!(!state.available);
        assert!(state.sources.is_empty());
    }

    #[test]
    fn foreign_roots_and_unknown_actions_are_rejected() {
        let mut state = InstructionState::new();
        assert!(state.apply(&snapshot(json!([])), "other").is_err());
        assert!(state
            .apply(
                &snapshot(json!([{"path": "AGENTS.md", "action": "grow"}])),
                "root"
            )
            .is_err());
    }

    #[test]
    fn age_labels_are_relative_to_the_event_time() {
        let mut state = InstructionState::new();
        state.loaded_at = Some(1_000_000);
        assert_eq!(state.age_label(1_000_000), "刚刚");
        assert_eq!(state.age_label(1_030_000), "30 秒前");
        assert_eq!(state.age_label(1_600_000), "10 分钟前");
        assert_eq!(state.age_label(1_000_000 + 7_200_000), "2 小时前");
        assert_eq!(state.age_label(1_000_000 + 172_800_000), "2 天前");
        assert_eq!(InstructionState::new().age_label(1_000_000), "未知");
        assert_eq!(state.age_label(900_000), "刚刚");
    }
}
