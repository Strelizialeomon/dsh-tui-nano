//! Command palette and selection panes.
//!
//! The panel only describes rows and actions; the application owns key events
//! and network calls. Rows carry the official description or the official
//! preset's sandbox/approval values so a destructive choice shows its complete
//! consequence before the user confirms it.

use crate::catalog::CatalogModel;
use crate::model::{CommandEntry, ControlsModel};

/// Which pane the overlay currently shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelMode {
    /// Official commands plus Nano view entries.
    Commands,
    /// Provider-grouped model catalog.
    Models,
    /// Efforts advertised by one exact model route.
    Efforts {
        /// Provider identity.
        provider: String,
        /// Model id.
        model: String,
    },
    /// Official permission presets.
    Permissions,
}

impl PanelMode {
    /// Short title for the overlay block.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Commands => "命令",
            Self::Models => "模型",
            Self::Efforts { .. } => "推理强度",
            Self::Permissions => "权限预设",
        }
    }

    /// Whether typing edits the filter text.
    pub fn query_enabled(&self) -> bool {
        !matches!(self, Self::Efforts { .. })
    }
}

/// What the application should do after Enter on one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelAction {
    /// Show the model catalog pane.
    OpenModels,
    /// Show the effort pane of one exact model route.
    OpenEfforts {
        /// Provider identity.
        provider: String,
        /// Model id.
        model: String,
    },
    /// Show the permission preset pane.
    OpenPermissions,
    /// Execute one official slash line immediately.
    ExecuteCommand(String),
    /// Put a slash line with its argument hint into the input box.
    PrefillCommand(String),
    /// Request one exact model/effort route.
    SelectModel {
        /// Provider identity.
        provider: String,
        /// Model id.
        model: String,
        /// Effort id; `None` keeps the adapter default.
        effort: Option<String>,
    },
    /// Switch one official permission preset through the command path.
    SetPermission(String),
    /// Leave the pane without a write.
    Close,
}

/// One rendered row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelRow {
    /// Provider heading; rendered when it differs from the previous row.
    pub group: Option<String>,
    /// Primary text.
    pub label: String,
    /// Secondary explanation, including official consequences.
    pub detail: Option<String>,
    /// Whether this row is the current official value.
    pub current: bool,
    /// Why the row cannot be chosen; `None` means selectable.
    pub disabled: Option<String>,
    /// Action on Enter.
    pub action: PanelAction,
}

impl PanelRow {
    /// Whether Enter can run this row.
    pub fn selectable(&self) -> bool {
        self.disabled.is_none()
    }
}

/// Overlay state: open flag, pane, filter, and selected row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Panel {
    open: bool,
    mode: PanelMode,
    query: String,
    selected: usize,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            open: false,
            mode: PanelMode::Commands,
            query: String::new(),
            selected: 0,
        }
    }
}

impl Panel {
    /// Closed panel.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open one pane with an empty filter.
    pub fn open(&mut self, mode: PanelMode) {
        self.open = true;
        self.mode = mode;
        self.query.clear();
        self.selected = 0;
    }

    /// Close the panel.
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
    }

    /// Whether the overlay is visible.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current pane.
    pub fn mode(&self) -> &PanelMode {
        &self.mode
    }

    /// Switch pane, clearing the filter.
    pub fn set_mode(&mut self, mode: PanelMode) {
        self.mode = mode;
        self.query.clear();
        self.selected = 0;
    }

    /// Escape semantics: the effort pane returns to its model list; other
    /// panes close.
    pub fn back(&mut self) {
        match self.mode.clone() {
            PanelMode::Efforts { .. } => self.set_mode(PanelMode::Models),
            _ => self.close(),
        }
    }

    /// Current filter text.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Selected row index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Append one filter character.
    pub fn push_char(&mut self, character: char) {
        if self.mode.query_enabled() {
            self.query.push(character);
            self.selected = 0;
        }
    }

    /// Remove the last filter character.
    pub fn backspace(&mut self) {
        if self.mode.query_enabled() {
            self.query.pop();
            self.selected = 0;
        }
    }

    /// Move the selection up, clamped.
    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the selection down within the row count.
    pub fn move_down(&mut self, rows: usize) {
        if rows > 0 && self.selected + 1 < rows {
            self.selected += 1;
        }
    }

    /// Keep the selection inside the current row count.
    pub fn clamp(&mut self, rows: usize) {
        if rows == 0 {
            self.selected = 0;
        } else if self.selected >= rows {
            self.selected = rows - 1;
        }
    }

    /// The rows for the current pane and filter.
    pub fn rows(&self, controls: &ControlsModel) -> Vec<PanelRow> {
        match &self.mode {
            PanelMode::Commands => self.command_rows(controls),
            PanelMode::Models => self.model_rows(controls),
            PanelMode::Efforts { provider, model } => self.effort_rows(controls, provider, model),
            PanelMode::Permissions => self.permission_rows(controls),
        }
    }

    /// Action for Enter on the selected row, if it is selectable.
    pub fn selected_action(&self, controls: &ControlsModel) -> Option<PanelAction> {
        let rows = self.rows(controls);
        rows.get(self.selected)
            .filter(|row| row.selectable())
            .map(|row| row.action.clone())
    }

    fn command_rows(&self, controls: &ControlsModel) -> Vec<PanelRow> {
        let query = self.query.to_lowercase();
        controls
            .commands()
            .iter()
            .filter(|entry| matches_command(entry, &query))
            .map(|entry| PanelRow {
                group: None,
                label: entry.line(),
                detail: Some(command_detail(entry)),
                current: false,
                disabled: if entry.available {
                    None
                } else {
                    Some(
                        entry
                            .reason
                            .clone()
                            .unwrap_or_else(|| "当前不可用".to_string()),
                    )
                },
                action: command_action(entry),
            })
            .collect()
    }

    fn model_rows(&self, controls: &ControlsModel) -> Vec<PanelRow> {
        let Some(catalog) = controls.catalog() else {
            return Vec::new();
        };
        let query = self.query.to_lowercase();
        let current = controls.model.next.as_ref();
        let mut rows = Vec::new();
        for group in &catalog.groups {
            for model in &group.models {
                if !matches_model(model, &query) {
                    continue;
                }
                let is_current = current.is_some_and(|selection| {
                    selection.provider == group.id && selection.model == model.id
                });
                rows.push(PanelRow {
                    group: Some(group.name.clone()),
                    label: model.name.clone(),
                    detail: model_detail(model),
                    current: is_current,
                    disabled: None,
                    action: if model
                        .reasoning
                        .as_ref()
                        .is_some_and(|reasoning| !reasoning.efforts.is_empty())
                    {
                        PanelAction::OpenEfforts {
                            provider: group.id.clone(),
                            model: model.id.clone(),
                        }
                    } else {
                        PanelAction::SelectModel {
                            provider: group.id.clone(),
                            model: model.id.clone(),
                            effort: None,
                        }
                    },
                });
            }
        }
        // A provider whose lookup failed is a fact, not an absent provider:
        // show its official reason and never substitute another model.
        for failure in &catalog.failures {
            if !query.is_empty()
                && !failure.name.to_lowercase().contains(&query)
                && !failure.id.to_lowercase().contains(&query)
            {
                continue;
            }
            rows.push(PanelRow {
                group: Some(failure.name.clone()),
                label: format!("{}（目录读取失败）", failure.name),
                detail: Some(failure.message.clone()),
                current: false,
                disabled: Some(format!(
                    "provider {} 不可用：{}",
                    failure.id, failure.message
                )),
                action: PanelAction::Close,
            });
        }
        rows
    }

    fn effort_rows(&self, controls: &ControlsModel, provider: &str, model: &str) -> Vec<PanelRow> {
        let Some(catalog_model) = controls
            .catalog()
            .and_then(|catalog| catalog.find(provider, model))
        else {
            return Vec::new();
        };
        let current_effort = controls
            .model
            .next
            .as_ref()
            .filter(|selection| selection.provider == provider && selection.model == model)
            .and_then(|selection| selection.reasoning_effort.as_deref());
        let mut rows = vec![PanelRow {
            group: None,
            label: "默认".to_string(),
            detail: catalog_model
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.default_effort_name())
                .map(|name| format!("使用模型默认强度（{name}）")),
            current: current_effort.is_none(),
            disabled: None,
            action: PanelAction::SelectModel {
                provider: provider.to_string(),
                model: model.to_string(),
                effort: None,
            },
        }];
        if let Some(reasoning) = &catalog_model.reasoning {
            for effort in &reasoning.efforts {
                rows.push(PanelRow {
                    group: None,
                    label: effort.name.clone(),
                    detail: effort.description.clone(),
                    current: current_effort == Some(effort.id.as_str()),
                    disabled: None,
                    action: PanelAction::SelectModel {
                        provider: provider.to_string(),
                        model: model.to_string(),
                        effort: Some(effort.id.clone()),
                    },
                });
            }
        }
        rows
    }

    fn permission_rows(&self, controls: &ControlsModel) -> Vec<PanelRow> {
        let permission = controls.permission();
        if !permission.loaded {
            return Vec::new();
        }
        let query = self.query.to_lowercase();
        let matches = |name: &str, value: &str| {
            query.is_empty()
                || name.to_lowercase().contains(&query)
                || value.to_lowercase().contains(&query)
        };
        let mut rows = Vec::new();
        if let Some(current) = permission.current.as_deref() {
            let current_name = permission.option_name(current);
            if !permission.catalog.contains(current) && matches(&current_name, current) {
                rows.push(PanelRow {
                    group: None,
                    label: if current == "custom" {
                        "custom（当前组合）".to_string()
                    } else {
                        format!("{current}（当前组合不在预设目录）")
                    },
                    detail: Some(
                        "当前 sandbox/approval 组合不匹配任何命名预设；只能切回命名预设"
                            .to_string(),
                    ),
                    current: true,
                    disabled: Some("custom 不能作为命名预设选中".to_string()),
                    action: PanelAction::Close,
                });
            }
        }
        for option in &permission.catalog.options {
            if !matches(&option.name, &option.value) {
                continue;
            }
            let mut consequences = Vec::new();
            if let Some(description) = &option.description {
                consequences.push(description.clone());
            }
            if option.danger_full_access() {
                consequences.push(
                    "sandbox=danger-full-access：工具可访问工作区之外，风险由你承担".to_string(),
                );
            }
            if option.approval_never() {
                consequences
                    .push("approval=never：所有审批请求自动拒绝，不再出现审批卡".to_string());
            }
            rows.push(PanelRow {
                group: None,
                label: if option.name == option.value {
                    option.value.clone()
                } else {
                    format!("{}（{}）", option.name, option.value)
                },
                detail: (!consequences.is_empty()).then(|| consequences.join(" · ")),
                current: permission.is_current(&option.value),
                disabled: None,
                action: PanelAction::SetPermission(option.value.clone()),
            });
        }
        rows
    }
}

/// Row action for one command entry. View entries open their pane; official
/// entries with an advertised argument hint prefill the input instead of
/// executing an argument-less line.
fn command_action(entry: &CommandEntry) -> PanelAction {
    match entry.name.as_str() {
        "model" => PanelAction::OpenModels,
        "permission" => PanelAction::OpenPermissions,
        _ => match &entry.input_hint {
            Some(_) => PanelAction::PrefillCommand(format!("{} ", entry.line())),
            None => PanelAction::ExecuteCommand(entry.line()),
        },
    }
}

/// Description line, including the argument hint and unavailability reason.
fn command_detail(entry: &CommandEntry) -> String {
    match &entry.input_hint {
        Some(hint) => format!("{} · 参数 {}", entry.description, hint),
        None => entry.description.clone(),
    }
}

/// Model detail: description plus advertised effort names.
fn model_detail(model: &CatalogModel) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(description) = &model.description {
        parts.push(description.clone());
    }
    if let Some(reasoning) = &model.reasoning {
        if !reasoning.efforts.is_empty() {
            let names: Vec<_> = reasoning
                .efforts
                .iter()
                .map(|effort| effort.name.as_str())
                .collect();
            let mut line = format!("强度：{}", names.join(" / "));
            if let Some(default) = reasoning.default_effort_name() {
                line.push_str(&format!("；默认 {default}"));
            }
            parts.push(line);
        }
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn matches_command(entry: &CommandEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    entry.name.to_lowercase().contains(query) || entry.description.to_lowercase().contains(query)
}

fn matches_model(model: &CatalogModel, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    model.name.to_lowercase().contains(query) || model.id.to_lowercase().contains(query)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ControlsModel;
    use serde_json::json;

    fn controls() -> ControlsModel {
        let mut controls = ControlsModel::new();
        controls.set_capabilities(true, true, true);
        controls
            .apply_state(&json!({
                "address": {"rootSessionId": "root"},
                "model": {
                    "lastUsed": {"provider": "p", "model": "chat"},
                    "next": {"provider": "p", "model": "chat"}
                },
                "permission": {
                    "current": "workspace-write",
                    "catalog": {
                        "options": [
                            {"value": "workspace-write", "name": "workspace-write", "approval": "ask", "sandbox": "workspace-write"},
                            {"value": "danger-full-access", "name": "danger-full-access", "approval": "never", "sandbox": "danger-full-access"}
                        ],
                        "defaultOptions": [],
                        "defaultPreset": "workspace-write"
                    }
                },
                "commands": [
                    {"name": "compact", "description": "Compact"},
                    {"name": "permission", "description": "Switch", "inputHint": "<preset>"}
                ]
            }))
            .unwrap();
        controls
            .apply_catalog(&json!({
                "default": {"provider": "p", "model": "chat"},
                "routableProviders": ["p"],
                "groups": [{
                    "id": "p",
                    "name": "Provider",
                    "models": [
                        {"id": "chat", "name": "Chat", "reasoning": {
                            "efforts": [{"id": "low", "name": "Low"}, {"id": "max", "name": "Max"}],
                            "defaultEffort": "low"
                        }},
                        {"id": "plain", "name": "Plain"}
                    ]
                }],
                "failures": []
            }))
            .unwrap();
        controls
    }

    #[test]
    fn command_rows_filter_and_mark_view_entries() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Commands);
        let rows = panel.rows(&controls);
        let names: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(names, vec!["/compact", "/model", "/permission"]);
        assert_eq!(rows[1].action, PanelAction::OpenModels);
        assert_eq!(rows[2].action, PanelAction::OpenPermissions);

        panel.push_char('c');
        panel.push_char('o');
        let filtered = panel.rows(&controls);
        assert_eq!(filtered.len(), 1);
        assert_eq!(
            filtered[0].action,
            PanelAction::ExecuteCommand("/compact".into())
        );
    }

    #[test]
    fn commands_with_a_hint_prefill_instead_of_executing() {
        let mut state = json!({
            "address": {"rootSessionId": "root"},
            "model": {"lastUsed": null, "next": null},
            "permission": {"current": null, "catalog": {"options": [], "defaultOptions": [], "defaultPreset": ""}},
            "commands": [{"name": "goal", "description": "Set a goal", "inputHint": "<goal>"}]
        });
        let mut controls = ControlsModel::new();
        controls.apply_state(&state).unwrap();
        state["commands"] = json!([]);
        let mut panel = Panel::new();
        panel.open(PanelMode::Commands);
        let rows = panel.rows(&controls);
        let goal = rows.iter().find(|row| row.label == "/goal").unwrap();
        assert_eq!(goal.action, PanelAction::PrefillCommand("/goal ".into()));
    }

    #[test]
    fn model_rows_group_by_provider_and_open_efforts_only_when_advertised() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Models);
        let rows = panel.rows(&controls);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].group.as_deref(), Some("Provider"));
        assert!(rows[0].current);
        assert_eq!(
            rows[0].action,
            PanelAction::OpenEfforts {
                provider: "p".into(),
                model: "chat".into()
            }
        );
        assert_eq!(
            rows[1].action,
            PanelAction::SelectModel {
                provider: "p".into(),
                model: "plain".into(),
                effort: None
            }
        );

        panel.push_char('p');
        panel.push_char('l');
        let filtered = panel.rows(&controls);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].label, "Plain");
    }

    #[test]
    fn provider_catalog_failures_are_visible_and_not_selectable() {
        let mut controls = controls();
        controls
            .apply_catalog(&json!({
                "default": {"provider": "p", "model": "chat"},
                "routableProviders": ["p"],
                "groups": [],
                "failures": [{"id": "third-party", "name": "Third Party", "message": "no credentials"}]
            }))
            .unwrap();
        let mut panel = Panel::new();
        panel.open(PanelMode::Models);
        let rows = panel.rows(&controls);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].label.contains("目录读取失败"));
        assert!(!rows[0].selectable());
        assert!(rows[0]
            .disabled
            .as_deref()
            .unwrap()
            .contains("no credentials"));
    }

    #[test]
    fn effort_rows_offer_default_plus_only_advertised_ids() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Efforts {
            provider: "p".into(),
            model: "chat".into(),
        });
        let rows = panel.rows(&controls);
        let labels: Vec<_> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, vec!["默认", "Low", "Max"]);
        assert!(rows[0].current);
        assert_eq!(
            rows[2].action,
            PanelAction::SelectModel {
                provider: "p".into(),
                model: "chat".into(),
                effort: Some("max".into())
            }
        );
        // The effort pane has no text filter.
        panel.push_char('x');
        assert_eq!(panel.query(), "");
    }

    #[test]
    fn unavailable_commands_show_a_reason_and_do_not_execute() {
        let mut controls = ControlsModel::new();
        controls.set_capabilities(false, false, false);
        let mut panel = Panel::new();
        panel.open(PanelMode::Commands);
        let rows = panel.rows(&controls);
        let model = rows.iter().find(|row| row.label == "/model").unwrap();
        assert!(!model.selectable());
        assert!(model
            .disabled
            .as_deref()
            .unwrap()
            .contains("官方模型选择服务未组合"));
        panel.clamp(rows.len());
        let index = rows.iter().position(|row| row.label == "/model").unwrap();
        for _ in 0..index {
            panel.move_down(rows.len());
        }
        assert_eq!(panel.selected_action(&controls), None);
    }

    #[test]
    fn permission_rows_show_complete_consequences_and_custom_is_not_selectable() {
        let mut controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Permissions);
        let rows = panel.rows(&controls);
        assert_eq!(rows.len(), 2);
        let danger = rows
            .iter()
            .find(|row| row.label.contains("danger-full-access"))
            .unwrap();
        assert!(danger.detail.as_deref().unwrap().contains("approval=never"));
        assert!(danger
            .detail
            .as_deref()
            .unwrap()
            .contains("danger-full-access"));
        assert!(rows[0].current);

        controls.apply_event(
            "controls.permission",
            &json!({"permission": {
                "current": "custom",
                "catalog": {"options": [], "defaultOptions": [], "defaultPreset": "workspace-write"}
            }}),
        );
        let rows = panel.rows(&controls);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].current);
        assert!(!rows[0].selectable());
    }

    #[test]
    fn preset_labels_do_not_repeat_the_value() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Permissions);
        let rows = panel.rows(&controls);
        assert!(rows.iter().any(|row| row.label == "workspace-write"));
        assert!(rows.iter().any(|row| row.label == "danger-full-access"));
        assert!(!rows
            .iter()
            .any(|row| row.label.contains("（workspace-write）")));
    }

    #[test]
    fn permission_rows_respect_the_filter_text() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Permissions);
        panel.push_char('d');
        panel.push_char('a');
        panel.push_char('n');
        let rows = panel.rows(&controls);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].label.contains("danger-full-access"));
        assert_eq!(
            panel.selected_action(&controls),
            Some(PanelAction::SetPermission("danger-full-access".into()))
        );
    }

    #[test]
    fn navigation_is_clamped_and_escape_returns_from_efforts() {
        let controls = controls();
        let mut panel = Panel::new();
        panel.open(PanelMode::Models);
        for _ in 0..10 {
            panel.move_down(2);
        }
        assert_eq!(panel.selected(), 1);
        panel.move_up();
        panel.move_up();
        assert_eq!(panel.selected(), 0);
        panel.set_mode(PanelMode::Efforts {
            provider: "p".into(),
            model: "chat".into(),
        });
        panel.back();
        assert_eq!(panel.mode(), &PanelMode::Models);
        panel.back();
        assert!(!panel.is_open());
        let _ = controls;
    }
}
