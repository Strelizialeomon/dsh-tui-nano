//! Official model-catalog projection.
//!
//! Identity is the exact `provider + model` pair, never a display name. A
//! model's reasoning metadata is authoritative for the only selectable effort
//! ids; an adapter without reasoning metadata has no effort row and no free
//! numeric input.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{MAX_MODELS_PER_GROUP, MAX_PROVIDER_GROUPS};

/// One complete provider/model/effort route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    /// Provider identity.
    pub provider: String,
    /// Provider-owned model id.
    pub model: String,
    /// Adapter-owned effort id, when explicitly selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ModelSelection {
    /// Build one route.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            reasoning_effort: None,
        }
    }

    /// Exact `provider/model` identity for display and comparisons.
    pub fn identity(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    /// Parse one wire selection, returning `None` for absent or malformed data.
    pub fn parse(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        if value.is_null() {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

/// One adapter-owned reasoning effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoningEffort {
    /// Effort id accepted by the adapter.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Selectable reasoning metadata for one exact route.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoning {
    /// Advertised efforts, in adapter order.
    #[serde(default)]
    pub efforts: Vec<ModelReasoningEffort>,
    /// The adapter default used when the session names no effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

impl ModelReasoning {
    /// Display name for one effort id, if advertised.
    pub fn effort_name(&self, id: &str) -> Option<&str> {
        self.efforts
            .iter()
            .find(|effort| effort.id == id)
            .map(|effort| effort.name.as_str())
    }

    /// The default effort's display name.
    pub fn default_effort_name(&self) -> Option<&str> {
        self.default_effort
            .as_deref()
            .and_then(|id| self.effort_name(id))
    }
}

/// One model inside its provider group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    /// Provider-owned model id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Reasoning metadata, absent when the adapter has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

/// One provider and its successfully loaded models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderGroup {
    /// Provider identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Available models.
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

/// One provider whose catalog lookup failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFailure {
    /// Provider identity.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Official failure message.
    pub message: String,
}

/// Host-generation catalog used by the model selector.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    /// Deployment default for a session that has no selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<ModelSelection>,
    /// Providers with at least one currently routable model.
    #[serde(default)]
    pub routable_providers: Vec<String>,
    /// Provider groups with at least one model.
    #[serde(default)]
    pub groups: Vec<ProviderGroup>,
    /// Per-provider failures, kept as facts rather than hidden.
    #[serde(default)]
    pub failures: Vec<CatalogFailure>,
}

impl ModelCatalog {
    /// Parse the official catalog result, bounding the retained groups.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let mut catalog: Self = serde_json::from_value(value.clone())
            .map_err(|error| format!("模型目录无效：{error}"))?;
        catalog.groups.truncate(MAX_PROVIDER_GROUPS);
        for group in &mut catalog.groups {
            group.models.truncate(MAX_MODELS_PER_GROUP);
        }
        Ok(catalog)
    }

    /// Whether no provider currently offers any model.
    pub fn is_empty(&self) -> bool {
        self.groups.iter().all(|group| group.models.is_empty())
    }

    /// Find one exact model route.
    pub fn find(&self, provider: &str, model: &str) -> Option<&CatalogModel> {
        self.groups
            .iter()
            .find(|group| group.id == provider)?
            .models
            .iter()
            .find(|entry| entry.id == model)
    }

    /// Reasoning metadata for one exact route.
    pub fn reasoning_for(&self, selection: &ModelSelection) -> Option<&ModelReasoning> {
        self.find(&selection.provider, &selection.model)?
            .reasoning
            .as_ref()
    }

    /// Whether the exact route is advertised by a successful provider group.
    pub fn contains(&self, selection: &ModelSelection) -> bool {
        self.find(&selection.provider, &selection.model).is_some()
    }

    /// Why an exact route is not selectable right now, when the catalog knows.
    ///
    /// A provider lookup failure is reported with its official message; a
    /// provider present without the model is reported as a removed model.
    pub fn unavailability_reason(&self, selection: &ModelSelection) -> Option<String> {
        if self.contains(selection) {
            return None;
        }
        if let Some(failure) = self
            .failures
            .iter()
            .find(|entry| entry.id == selection.provider)
        {
            return Some(format!(
                "provider {} 目录读取失败：{}",
                failure.name, failure.message
            ));
        }
        if self
            .routable_providers
            .iter()
            .any(|provider| provider == &selection.provider)
        {
            return Some(format!(
                "模型 {} 不在当前目录（可能已被移除或凭据已退出）",
                selection.identity()
            ));
        }
        Some(format!(
            "provider {} 当前不可路由（凭据缺失或目录未加载）",
            selection.provider
        ))
    }

    /// Display label for one exact route, using the catalog name when present.
    pub fn label_for(&self, selection: &ModelSelection) -> String {
        match self.find(&selection.provider, &selection.model) {
            Some(model) => format!("{} · {}", model.name, selection.provider),
            None => selection.identity(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog_value() -> Value {
        json!({
            "default": {"provider": "deepseek-official", "model": "deepseek-chat"},
            "routableProviders": ["deepseek-official", "third-party"],
            "groups": [
                {
                    "id": "deepseek-official",
                    "name": "DeepSeek",
                    "models": [
                        {
                            "id": "deepseek-chat",
                            "name": "DeepSeek Chat",
                            "reasoning": {
                                "efforts": [
                                    {"id": "low", "name": "Low"},
                                    {"id": "max", "name": "Max", "description": "Slowest"}
                                ],
                                "defaultEffort": "low"
                            }
                        },
                        {"id": "deepseek-chat-fast", "name": "Fast"}
                    ]
                }
            ],
            "failures": [
                {"id": "third-party", "name": "Third Party", "message": "no credentials"}
            ]
        })
    }

    #[test]
    fn parses_groups_efforts_and_failures() {
        let catalog = ModelCatalog::parse(&catalog_value()).unwrap();
        assert_eq!(catalog.groups.len(), 1);
        assert_eq!(catalog.groups[0].models.len(), 2);
        assert!(catalog.contains(&ModelSelection::new("deepseek-official", "deepseek-chat")));
        assert!(!catalog.contains(&ModelSelection::new("deepseek-official", "removed")));
        assert_eq!(catalog.failures[0].message, "no credentials");
        let reasoning = catalog
            .reasoning_for(&ModelSelection::new("deepseek-official", "deepseek-chat"))
            .unwrap();
        assert_eq!(reasoning.efforts.len(), 2);
        assert_eq!(reasoning.default_effort_name(), Some("Low"));
        assert_eq!(reasoning.effort_name("max"), Some("Max"));
        assert!(catalog
            .reasoning_for(&ModelSelection::new(
                "deepseek-official",
                "deepseek-chat-fast"
            ))
            .is_none());
    }

    #[test]
    fn reports_removal_and_provider_failure_reasons() {
        let catalog = ModelCatalog::parse(&catalog_value()).unwrap();
        let removed = ModelSelection::new("deepseek-official", "removed");
        assert!(catalog
            .unavailability_reason(&removed)
            .unwrap()
            .contains("不在当前目录"));
        let failed = ModelSelection::new("third-party", "any");
        assert!(catalog
            .unavailability_reason(&failed)
            .unwrap()
            .contains("no credentials"));
        let unknown = ModelSelection::new("never-seen", "any");
        assert!(catalog
            .unavailability_reason(&unknown)
            .unwrap()
            .contains("不可路由"));
    }

    #[test]
    fn empty_and_malformed_catalogs_are_distinguishable() {
        let empty = ModelCatalog::parse(&json!({
            "default": {"provider": "p", "model": "m"},
            "routableProviders": [],
            "groups": [],
            "failures": []
        }))
        .unwrap();
        assert!(empty.is_empty());
        assert!(ModelCatalog::parse(&json!({"groups": "wrong"})).is_err());
    }

    #[test]
    fn selection_identity_ignores_display_names() {
        let selection = ModelSelection::new("provider", "model");
        assert_eq!(selection.identity(), "provider/model");
        let parsed = ModelSelection::parse(Some(
            &json!({"provider": "p", "model": "m", "reasoningEffort": "high"}),
        ))
        .unwrap();
        assert_eq!(parsed.reasoning_effort.as_deref(), Some("high"));
        assert!(ModelSelection::parse(Some(&json!(null))).is_none());
        assert!(ModelSelection::parse(None).is_none());
    }
}
