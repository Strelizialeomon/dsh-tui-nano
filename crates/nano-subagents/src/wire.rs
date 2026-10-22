//! Wire parsing for the S12 methods and event payloads.
//!
//! Unknown modes, activities, and stop reasons are never guessed: an
//! unrecognized mode becomes `unknown`, an unrecognized activity is rejected
//! as malformed, and an unrecognized stop reason stays `unknown`.

use nano_conversation::{Delivery, SessionAddress};
use serde_json::{json, Value};

use crate::lifecycle::{LifecycleEvent, StopReason};
use crate::model::{Activity, AgentMode, AgentRow, Catalog, Diagnostic};

/// Error while reading one wire payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(pub String);

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

/// Parse `nano.subagents.snapshot`.
pub fn parse_snapshot(value: &Value) -> Result<(Vec<AgentRow>, Vec<Diagnostic>, bool), WireError> {
    let agents = value
        .get("agents")
        .and_then(Value::as_array)
        .ok_or_else(|| WireError("snapshot.agents is missing".to_string()))?
        .iter()
        .map(parse_row)
        .collect::<Result<Vec<_>, _>>()?;
    let diagnostics = parse_diagnostics(value.get("diagnostics"))?;
    let parent_live = value
        .get("parentLive")
        .and_then(Value::as_bool)
        .ok_or_else(|| WireError("snapshot.parentLive is missing".to_string()))?;
    Ok((agents, diagnostics, parent_live))
}

/// Parse `nano.subagents.list`.
pub fn parse_list(value: &Value) -> Result<(Vec<AgentRow>, Vec<Diagnostic>), WireError> {
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| WireError("list.entries is missing".to_string()))?
        .iter()
        .map(parse_row)
        .collect::<Result<Vec<_>, _>>()?;
    let diagnostics = parse_diagnostics(value.get("diagnostics"))?;
    Ok((entries, diagnostics))
}

/// Parse `nano.subagents.describe` into one row.
pub fn parse_description(value: &Value) -> Result<AgentRow, WireError> {
    let id = require_string(value, "id")?;
    let parent_id = require_string(value, "parentId")?;
    let mode = parse_mode(&require_string(value, "mode")?);
    let activity = parse_activity(&require_string(value, "activity")?)?;
    Ok(AgentRow {
        id,
        parent_id,
        depth: 1,
        mode,
        label: optional_string(value, "label"),
        activity,
        has_children: false,
        last_turn_completed: value.get("lastTurnCompleted").and_then(Value::as_bool),
        settled_ms: value.get("settledMs").and_then(Value::as_u64),
        active_since: value.get("activeSince").and_then(Value::as_u64),
        active_through: value.get("activeThrough").and_then(Value::as_u64),
        parent_live: value.get("parentLive").and_then(Value::as_bool),
        seq: value.get("seq").and_then(Value::as_u64),
    })
}

/// Parse one `subagents.event` payload.
pub fn parse_event(value: &Value) -> Result<LifecycleEvent, WireError> {
    let event_type = require_string(value, "type")?;
    match event_type.as_str() {
        "start" => Ok(LifecycleEvent::Start {
            child_id: require_string(value, "childId")?,
        }),
        "end" => Ok(LifecycleEvent::End {
            child_id: require_string(value, "childId")?,
            reason: value
                .get("stopReason")
                .and_then(Value::as_str)
                .map(StopReason::parse)
                .unwrap_or(StopReason::Unknown),
        }),
        "changed" => Ok(LifecycleEvent::Changed),
        other => Err(WireError(format!("unknown subagents.event type {other}"))),
    }
}

/// Build `nano.subagents.prompt` params.
pub fn prompt_params(
    address: &SessionAddress,
    request_id: &str,
    delivery: Delivery,
    blocks: &[Value],
) -> Value {
    json!({
        "address": address.wire_value(),
        "requestId": request_id,
        "delivery": delivery.as_str(),
        "content": blocks,
    })
}

/// Build `nano.subagents.interrupt` params.
pub fn interrupt_params(address: &SessionAddress) -> Value {
    json!({ "address": address.wire_value() })
}

/// Apply a parsed child description to the catalog.
pub fn apply_description(catalog: &mut Catalog, row: &AgentRow) {
    catalog.update_row(row);
}

/// Parse one catalog row.
pub fn parse_row(value: &Value) -> Result<AgentRow, WireError> {
    Ok(AgentRow {
        id: require_string(value, "id")?,
        parent_id: require_string(value, "parentId")?,
        depth: value
            .get("depth")
            .and_then(Value::as_u64)
            .ok_or_else(|| WireError("row.depth is missing".to_string()))? as u32,
        mode: parse_mode(&require_string(value, "mode")?),
        label: optional_string(value, "label"),
        activity: parse_activity(&require_string(value, "activity")?)?,
        has_children: value
            .get("hasChildren")
            .and_then(Value::as_bool)
            .ok_or_else(|| WireError("row.hasChildren is missing".to_string()))?,
        last_turn_completed: value.get("lastTurnCompleted").and_then(Value::as_bool),
        settled_ms: value.get("settledMs").and_then(Value::as_u64),
        active_since: value.get("activeSince").and_then(Value::as_u64),
        active_through: value.get("activeThrough").and_then(Value::as_u64),
        parent_live: None,
        seq: None,
    })
}

/// Parse the official mode spelling.
pub fn parse_mode(value: &str) -> AgentMode {
    match value {
        "continuable" => AgentMode::Continuable,
        "one-shot" => AgentMode::OneShot,
        "external" => AgentMode::External,
        _ => AgentMode::Unknown,
    }
}

fn parse_activity(value: &str) -> Result<Activity, WireError> {
    match value {
        "running" => Ok(Activity::Running),
        "inactive" => Ok(Activity::Inactive),
        other => Err(WireError(format!("unknown activity {other}"))),
    }
}

fn parse_diagnostics(value: Option<&Value>) -> Result<Vec<Diagnostic>, WireError> {
    let Some(array) = value.and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    array
        .iter()
        .map(|entry| {
            Ok(Diagnostic {
                id: require_string(entry, "id")?,
                parent_id: optional_string(entry, "parentId"),
                depth: entry
                    .get("depth")
                    .and_then(Value::as_u64)
                    .map(|depth| depth as u32),
                reason: require_string(entry, "reason")?,
            })
        })
        .collect()
}

fn require_string(value: &Value, field: &str) -> Result<String, WireError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| WireError(format!("{field} must be a non-empty string")))
}

fn optional_string(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}
