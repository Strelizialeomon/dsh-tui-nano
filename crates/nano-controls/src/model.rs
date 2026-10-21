//! Display state for S04.
//!
//! The state is a read-only projection of official facts. A failed write never
//! flips a displayed value optimistically: the UI keeps the previous fact until
//! the official projection is re-read, and a failed catalog refresh keeps the
//! last catalog marked as possibly stale.

use serde_json::{json, Value};

use crate::catalog::{ModelCatalog, ModelSelection};
use crate::{MAX_COMMANDS, MAX_PERMISSION_OPTIONS};

/// Durable model-selection projection: the route consumed by the last request
/// and the route the next request will use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelState {
    /// Selection consumed by the latest recorded model request.
    pub last_used: Option<ModelSelection>,
    /// Selection the next request should use; `None` means the deployment
    /// default, not a fabricated concrete route.
    pub next: Option<ModelSelection>,
}

/// One permission preset option from the official process catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOption {
    /// Stable option value: a configured preset key or derived `custom`.
    pub value: String,
    /// Display label.
    pub name: String,
    /// Optional user-facing description.
    pub description: Option<String>,
    /// Sandbox mode the preset writes, when the official service resolves it.
    pub sandbox: Option<String>,
    /// Approval policy the preset writes, when resolved.
    pub approval: Option<String>,
}

impl PermissionOption {
    /// Whether switching to this option changes the approval policy to `never`.
    pub fn approval_never(&self) -> bool {
        self.approval.as_deref() == Some("never")
    }

    /// Whether this option grants unrestricted sandbox access.
    pub fn danger_full_access(&self) -> bool {
        self.sandbox.as_deref() == Some("danger-full-access")
    }
}

/// Official process-level permission catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionCatalog {
    /// Every currently selectable preset, in contribution order.
    pub options: Vec<PermissionOption>,
    /// Presets eligible as the default for future sessions.
    pub default_options: Vec<PermissionOption>,
    /// Effective default preset.
    pub default_preset: String,
}

impl PermissionCatalog {
    /// Parse one catalog object.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let options = parse_options(value.get("options"))?;
        let default_options = parse_options(value.get("defaultOptions"))?;
        let default_preset = value
            .get("defaultPreset")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            options,
            default_options,
            default_preset,
        })
    }

    /// Whether the option value is a selectable preset.
    pub fn contains(&self, value: &str) -> bool {
        self.options.iter().any(|option| option.value == value)
    }

    /// Find one option.
    pub fn find(&self, value: &str) -> Option<&PermissionOption> {
        self.options.iter().find(|option| option.value == value)
    }
}

/// Current permission selection plus its process catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PermissionState {
    /// Effective current value: a preset key, `auto`, or `custom`.
    pub current: Option<String>,
    /// Process catalog the official service advertises.
    pub catalog: PermissionCatalog,
    /// Whether a successful read has described the current value.
    pub loaded: bool,
}

impl PermissionState {
    /// Parse one permission object from `nano.controls.state`.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let current = value
            .get("current")
            .and_then(Value::as_str)
            .map(str::to_string);
        let catalog = PermissionCatalog::parse(
            value
                .get("catalog")
                .ok_or_else(|| "permission.catalog is missing".to_string())?,
        )?;
        Ok(Self {
            current,
            catalog,
            loaded: true,
        })
    }

    /// Display value: an official fact, `custom`, or an explicit unknown.
    pub fn label(&self) -> String {
        if !self.loaded {
            return "未知".to_string();
        }
        match self.current.as_deref() {
            Some("custom") => "custom（非命名预设）".to_string(),
            Some(value) if !value.is_empty() => value.to_string(),
            _ => "未知".to_string(),
        }
    }

    /// Display name for one option value, falling back to the raw value.
    pub fn option_name(&self, value: &str) -> String {
        match value {
            "custom" => "custom".to_string(),
            other => self
                .catalog
                .find(other)
                .map(|option| option.name.clone())
                .unwrap_or_else(|| other.to_string()),
        }
    }

    /// Whether the current value matches this option.
    pub fn is_current(&self, value: &str) -> bool {
        self.current.as_deref() == Some(value)
    }
}

/// Where a palette entry comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOrigin {
    /// The official command registry.
    Official,
    /// A Nano view action that is not a model-facing command.
    View,
}

/// One command-palette entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// Command name without the leading slash.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Free-form input hint advertised by the official registry.
    pub input_hint: Option<String>,
    /// Official or Nano view entry.
    pub origin: CommandOrigin,
    /// Whether the entry can run right now.
    pub available: bool,
    /// Why the entry is unavailable, when it is.
    pub reason: Option<String>,
}

impl CommandEntry {
    /// Full slash line with no arguments.
    pub fn line(&self) -> String {
        format!("/{}", self.name)
    }
}

/// S04 display state.
#[derive(Debug, Clone)]
pub struct ControlsModel {
    /// Durable model projection.
    pub model: ModelState,
    catalog: Option<ModelCatalog>,
    catalog_error: Option<String>,
    catalog_attempted: bool,
    permission: PermissionState,
    official_commands: Vec<CommandEntry>,
    commands: Vec<CommandEntry>,
    state_loaded: bool,
    state_error: Option<String>,
    model_selection_available: bool,
    permission_available: bool,
    commands_available: bool,
}

impl Default for ControlsModel {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlsModel {
    /// Empty state before the first official read.
    pub fn new() -> Self {
        Self {
            model: ModelState::default(),
            catalog: None,
            catalog_error: None,
            catalog_attempted: false,
            permission: PermissionState::default(),
            official_commands: Vec::new(),
            commands: Vec::new(),
            state_loaded: false,
            state_error: None,
            model_selection_available: false,
            permission_available: false,
            commands_available: false,
        }
    }

    /// Record which official services the handshake reported.
    pub fn set_capabilities(&mut self, model_selection: bool, permissions: bool, commands: bool) {
        self.model_selection_available = model_selection;
        self.permission_available = permissions;
        self.commands_available = commands;
        self.rebuild_commands();
    }

    /// Whether the official model-selection service is composed.
    pub fn model_selection_available(&self) -> bool {
        self.model_selection_available
    }

    /// Whether the official permission service is composed.
    pub fn permission_available(&self) -> bool {
        self.permission_available
    }

    /// Whether the official command service is composed.
    pub fn commands_available(&self) -> bool {
        self.commands_available
    }

    /// Parse a successful `nano.controls.state` result.
    pub fn apply_state(&mut self, value: &Value) -> Result<(), String> {
        let model = value
            .get("model")
            .ok_or_else(|| "controls state has no model object".to_string())?;
        self.model.last_used = ModelSelection::parse(model.get("lastUsed"));
        self.model.next = ModelSelection::parse(model.get("next"));
        self.permission = PermissionState::parse(
            value
                .get("permission")
                .ok_or_else(|| "controls state has no permission object".to_string())?,
        )?;
        let commands = value
            .get("commands")
            .and_then(Value::as_array)
            .ok_or_else(|| "controls state has no command list".to_string())?;
        self.official_commands = parse_commands(commands)?;
        self.state_loaded = true;
        self.state_error = None;
        self.rebuild_commands();
        Ok(())
    }

    /// Parse the permission object returned by a preset write.
    pub fn apply_permission(&mut self, value: Option<&Value>) -> Result<(), String> {
        let value = value.ok_or_else(|| "command result has no permission object".to_string())?;
        self.permission = PermissionState::parse(value)?;
        Ok(())
    }

    /// Apply the normalized selection receipt from `nano.controls.selectModel`.
    pub fn apply_selection(&mut self, value: &Value) -> Result<(), String> {
        let selected = ModelSelection::parse(value.get("selected"))
            .ok_or_else(|| "selectModel result has no selected route".to_string())?;
        self.model.next = Some(selected);
        Ok(())
    }

    /// Apply one live controls event.
    pub fn apply_event(&mut self, kind: &str, payload: &Value) -> bool {
        match kind {
            "controls.model" => {
                let Some(model) = payload.get("model") else {
                    return false;
                };
                self.model.last_used = ModelSelection::parse(model.get("lastUsed"));
                self.model.next = ModelSelection::parse(model.get("next"));
                true
            }
            "controls.permission" => {
                let Some(permission) = payload.get("permission") else {
                    return false;
                };
                match PermissionState::parse(permission) {
                    Ok(parsed) => {
                        self.permission = parsed;
                        true
                    }
                    Err(_) => false,
                }
            }
            _ => false,
        }
    }

    /// Parse a successful catalog read.
    pub fn apply_catalog(&mut self, value: &Value) -> Result<(), String> {
        let catalog = ModelCatalog::parse(value)?;
        self.catalog = Some(catalog);
        self.catalog_error = None;
        self.catalog_attempted = true;
        Ok(())
    }

    /// Keep the last catalog and mark it possibly stale after a failed refresh.
    pub fn note_catalog_failure(&mut self, message: impl Into<String>) {
        self.catalog_error = Some(message.into());
        self.catalog_attempted = true;
    }

    /// Keep the last state and record why a re-read failed.
    pub fn note_state_failure(&mut self, message: impl Into<String>) {
        self.state_error = Some(message.into());
    }

    /// Clear every fact on a Host or connection reset. A failed refresh keeps
    /// values; a reset is the only event that empties them.
    pub fn reset(&mut self) {
        let model_selection_available = self.model_selection_available;
        let permission_available = self.permission_available;
        let commands_available = self.commands_available;
        *self = Self::new();
        self.model_selection_available = model_selection_available;
        self.permission_available = permission_available;
        self.commands_available = commands_available;
    }

    /// Last successful catalog, if any.
    pub fn catalog(&self) -> Option<&ModelCatalog> {
        self.catalog.as_ref()
    }

    /// Why the last catalog refresh failed, if it did.
    pub fn catalog_error(&self) -> Option<&str> {
        self.catalog_error.as_deref()
    }

    /// Whether a catalog read has ever been attempted.
    pub fn catalog_attempted(&self) -> bool {
        self.catalog_attempted
    }

    /// Whether the last refresh failed while an older catalog is still shown.
    pub fn catalog_stale(&self) -> bool {
        self.catalog.is_some() && self.catalog_error.is_some()
    }

    /// Whether the control state has been read successfully.
    pub fn state_loaded(&self) -> bool {
        self.state_loaded
    }

    /// Why the last state read failed, if it did.
    pub fn state_error(&self) -> Option<&str> {
        self.state_error.as_deref()
    }

    /// Current permission state.
    pub fn permission(&self) -> &PermissionState {
        &self.permission
    }

    /// Merged command entries: official commands win over same-name views.
    pub fn commands(&self) -> &[CommandEntry] {
        &self.commands
    }

    /// Official commands plus Nano view entries, after conflict resolution.
    fn rebuild_commands(&mut self) {
        let mut merged = self.official_commands.clone();
        if !self.commands_available {
            for entry in &mut merged {
                entry.available = false;
                entry.reason = Some("官方命令服务未组合".to_string());
            }
        }
        for entry in self.view_commands() {
            if merged.iter().any(|existing| existing.name == entry.name) {
                continue;
            }
            merged.push(entry);
        }
        merged.sort_by(|left, right| left.name.cmp(&right.name));
        merged.truncate(MAX_COMMANDS);
        self.commands = merged;
    }

    /// Nano view entries that are not official model-facing commands.
    fn view_commands(&self) -> Vec<CommandEntry> {
        let mut entries = Vec::new();
        entries.push(CommandEntry {
            name: "model".to_string(),
            description: "选择模型与推理强度（纯视图；读取官方目录）".to_string(),
            input_hint: None,
            origin: CommandOrigin::View,
            available: self.model_selection_available,
            reason: (!self.model_selection_available).then(|| "官方模型选择服务未组合".to_string()),
        });
        // When the official command registry already advertises /permission,
        // that entry wins; the view entry only keeps the read side discoverable
        // when the registry is absent or does not expose it.
        if !self
            .official_commands
            .iter()
            .any(|entry| entry.name == "permission")
        {
            entries.push(CommandEntry {
                name: "permission".to_string(),
                description: "查看或切换权限预设（切换走官方 /permission 命令）".to_string(),
                input_hint: None,
                origin: CommandOrigin::View,
                available: self.permission_available && self.commands_available,
                reason: if !self.permission_available {
                    Some("官方权限服务未组合".to_string())
                } else if !self.commands_available {
                    Some("官方命令服务未组合，无法写入权限".to_string())
                } else {
                    None
                },
            });
        }
        entries
    }

    /// Whether a command name resolves to a merged entry.
    pub fn find_command(&self, name: &str) -> Option<&CommandEntry> {
        self.commands.iter().find(|entry| entry.name == name)
    }

    /// Command prefixes the scene registry should claim.
    pub fn registry_prefixes(&self) -> Vec<String> {
        self.commands
            .iter()
            .map(|entry| format!("/{}", entry.name))
            .collect()
    }

    /// Official command names only, used by tests and diagnostics.
    pub fn official_command_names(&self) -> Vec<&str> {
        self.official_commands
            .iter()
            .map(|entry| entry.name.as_str())
            .collect()
    }

    /// Current model selection as a display label. `None` means the official
    /// deployment default, shown as "默认" rather than a concrete route.
    pub fn model_label(&self) -> String {
        let Some(selection) = &self.model.next else {
            return "默认".to_string();
        };
        match &self.catalog {
            Some(catalog) => match catalog.unavailability_reason(selection) {
                Some(reason) => format!("{}（{}）", selection.identity(), reason),
                None => catalog.label_for(selection),
            },
            None => selection.identity(),
        }
    }

    /// Current effort as a display label. An explicit effort uses its official
    /// display name; no explicit effort is shown as "默认".
    pub fn effort_label(&self) -> String {
        let Some(selection) = &self.model.next else {
            return "默认".to_string();
        };
        let Some(effort) = &selection.reasoning_effort else {
            return "默认".to_string();
        };
        self.catalog
            .as_ref()
            .and_then(|catalog| catalog.reasoning_for(selection))
            .and_then(|reasoning| reasoning.effort_name(effort))
            .map(str::to_string)
            .unwrap_or_else(|| effort.clone())
    }

    /// Permission display label.
    pub fn permission_label(&self) -> String {
        self.permission.label()
    }

    /// Request params for `nano.controls.selectModel`.
    pub fn select_model_params(&self, provider: &str, model: &str, effort: Option<&str>) -> Value {
        let mut params = json!({ "provider": provider, "model": model });
        if let Some(effort) = effort {
            params["reasoningEffort"] = Value::String(effort.to_string());
        }
        params
    }

    /// Request params for `nano.controls.setPermission`.
    pub fn set_permission_params(&self, preset: &str) -> Value {
        json!({ "preset": preset })
    }

    /// Request params for `nano.controls.executeCommand`.
    pub fn execute_command_params(&self, line: &str) -> Value {
        json!({ "line": line })
    }
}

/// Parse one command list from the official registry snapshot.
fn parse_commands(entries: &[Value]) -> Result<Vec<CommandEntry>, String> {
    let mut commands = Vec::new();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "a command entry has no name".to_string())?;
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input_hint = entry
            .get("inputHint")
            .and_then(Value::as_str)
            .map(str::to_string);
        commands.push(CommandEntry {
            name: name.to_string(),
            description,
            input_hint,
            origin: CommandOrigin::Official,
            available: true,
            reason: None,
        });
        if commands.len() >= MAX_COMMANDS {
            break;
        }
    }
    Ok(commands)
}

/// Parse one option array, bounding the retained entries.
fn parse_options(value: Option<&Value>) -> Result<Vec<PermissionOption>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| "permission options must be an array".to_string())?;
    let mut options = Vec::new();
    for entry in array.iter().take(MAX_PERMISSION_OPTIONS) {
        let value = entry
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| "a permission option has no value".to_string())?;
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(value)
            .to_string();
        options.push(PermissionOption {
            value: value.to_string(),
            name,
            description: entry
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            sandbox: entry
                .get("sandbox")
                .and_then(Value::as_str)
                .map(str::to_string),
            approval: entry
                .get("approval")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_value() -> Value {
        json!({
            "address": {"rootSessionId": "root"},
            "model": {
                "lastUsed": {"provider": "deepseek-official", "model": "deepseek-chat"},
                "next": {"provider": "deepseek-official", "model": "deepseek-chat", "reasoningEffort": "max"}
            },
            "permission": {
                "current": "workspace-write",
                "catalog": {
                    "options": [
                        {"value": "workspace-write", "name": "Workspace write", "sandbox": "workspace-write", "approval": "ask"},
                        {"value": "danger-full-access", "name": "Danger", "sandbox": "danger-full-access", "approval": "never"}
                    ],
                    "defaultOptions": [
                        {"value": "workspace-write", "name": "Workspace write"}
                    ],
                    "defaultPreset": "workspace-write"
                }
            },
            "commands": [
                {"name": "compact", "description": "Compact"},
                {"name": "permission", "description": "Switch preset", "inputHint": "<preset>"}
            ]
        })
    }

    fn catalog_value() -> Value {
        json!({
            "default": {"provider": "deepseek-official", "model": "deepseek-chat"},
            "routableProviders": ["deepseek-official"],
            "groups": [{
                "id": "deepseek-official",
                "name": "DeepSeek",
                "models": [{
                    "id": "deepseek-chat",
                    "name": "DeepSeek Chat",
                    "reasoning": {"efforts": [{"id": "max", "name": "Max"}], "defaultEffort": "max"}
                }]
            }],
            "failures": []
        })
    }

    #[test]
    fn state_parses_projection_permission_and_commands() {
        let mut controls = ControlsModel::new();
        controls.apply_state(&state_value()).unwrap();
        assert_eq!(
            controls
                .model
                .next
                .as_ref()
                .unwrap()
                .reasoning_effort
                .as_deref(),
            Some("max")
        );
        assert_eq!(
            controls.permission().current.as_deref(),
            Some("workspace-write")
        );
        assert!(controls.permission().catalog.contains("danger-full-access"));
        assert_eq!(
            controls.official_command_names(),
            vec!["compact", "permission"]
        );
        // The official /permission wins over the view entry; /model is added.
        let names: Vec<_> = controls
            .commands()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["compact", "model", "permission"]);
        assert_eq!(
            controls
                .commands()
                .iter()
                .find(|entry| entry.name == "permission")
                .unwrap()
                .origin,
            CommandOrigin::Official
        );
        assert_eq!(controls.permission_label(), "workspace-write");
    }

    #[test]
    fn a_view_permission_entry_appears_only_when_the_registry_lacks_it() {
        let mut controls = ControlsModel::new();
        controls.set_capabilities(true, true, true);
        let mut state = state_value();
        state["commands"] = json!([{"name": "compact", "description": "Compact"}]);
        controls.apply_state(&state).unwrap();
        let permission = controls.find_command("permission").unwrap();
        assert_eq!(permission.origin, CommandOrigin::View);
        assert!(permission.available);
    }

    #[test]
    fn capabilities_make_view_entries_explicitly_unavailable() {
        let mut controls = ControlsModel::new();
        controls.set_capabilities(false, false, false);
        let model = controls.find_command("model").unwrap();
        assert!(!model.available);
        assert!(model.reason.as_deref().unwrap().contains("模型选择"));
        let permission = controls.find_command("permission").unwrap();
        assert!(!permission.available);
        assert!(permission.reason.as_deref().unwrap().contains("权限"));
    }

    #[test]
    fn catalog_refresh_failure_keeps_the_last_catalog_and_marks_it_stale() {
        let mut controls = ControlsModel::new();
        controls.apply_catalog(&catalog_value()).unwrap();
        assert!(!controls.catalog_stale());
        controls.note_catalog_failure("network down");
        assert!(controls.catalog_stale());
        assert_eq!(controls.catalog_error(), Some("network down"));
        assert!(controls.catalog().is_some());
        controls.reset();
        assert!(controls.catalog().is_none());
        assert!(!controls.catalog_stale());
    }

    #[test]
    fn a_removed_model_is_not_replaced_by_another_model() {
        let mut controls = ControlsModel::new();
        controls.apply_state(&state_value()).unwrap();
        controls.apply_catalog(&catalog_value()).unwrap();
        assert_eq!(controls.model_label(), "DeepSeek Chat · deepseek-official");
        assert_eq!(controls.effort_label(), "Max");

        let mut removed = state_value();
        removed["model"]["next"] = json!({"provider": "deepseek-official", "model": "removed"});
        controls.apply_state(&removed).unwrap();
        let label = controls.model_label();
        assert!(label.contains("deepseek-official/removed"));
        assert!(label.contains("不在当前目录"));
        assert_eq!(controls.effort_label(), "默认");
    }

    #[test]
    fn an_unselected_model_is_shown_as_default_not_fabricated() {
        let controls = ControlsModel::new();
        assert_eq!(controls.model_label(), "默认");
        assert_eq!(controls.effort_label(), "默认");
        assert_eq!(controls.permission_label(), "未知");
    }

    #[test]
    fn live_events_replace_absolute_state() {
        let mut controls = ControlsModel::new();
        assert!(controls.apply_event(
            "controls.model",
            &json!({"address": {"rootSessionId": "root"}, "model": {
                "lastUsed": {"provider": "p", "model": "m"},
                "next": {"provider": "p", "model": "m2"}
            }})
        ));
        assert_eq!(controls.model.next.as_ref().unwrap().model, "m2");
        assert!(controls.apply_event(
            "controls.permission",
            &json!({"address": {"rootSessionId": "root"}, "permission": {
                "current": "custom",
                "catalog": {"options": [], "defaultOptions": [], "defaultPreset": "workspace-write"}
            }})
        ));
        assert_eq!(controls.permission_label(), "custom（非命名预设）");
        assert!(controls.permission().loaded);
        assert!(!controls.apply_event("conversation.status", &json!({})));
    }

    #[test]
    fn malformed_state_is_an_error_and_does_not_claim_success() {
        let mut controls = ControlsModel::new();
        assert!(controls.apply_state(&json!({"model": {}})).is_err());
        assert!(!controls.state_loaded());
        assert!(controls.permission_label() == "未知");
    }

    #[test]
    fn permission_options_expose_official_consequences() {
        let mut controls = ControlsModel::new();
        controls.apply_state(&state_value()).unwrap();
        let danger = controls
            .permission()
            .catalog
            .find("danger-full-access")
            .unwrap();
        assert!(danger.approval_never());
        assert!(danger.danger_full_access());
        let workspace = controls
            .permission()
            .catalog
            .find("workspace-write")
            .unwrap();
        assert!(!workspace.approval_never());
        assert!(!workspace.danger_full_access());
    }

    #[test]
    fn request_params_match_the_contract() {
        let controls = ControlsModel::new();
        assert_eq!(
            controls.select_model_params("p", "m", Some("high")),
            json!({"provider": "p", "model": "m", "reasoningEffort": "high"})
        );
        assert_eq!(
            controls.select_model_params("p", "m", None),
            json!({"provider": "p", "model": "m"})
        );
        assert_eq!(
            controls.set_permission_params("custom"),
            json!({"preset": "custom"})
        );
        assert_eq!(
            controls.execute_command_params("/compact"),
            json!({"line": "/compact"})
        );
    }
}
