//! Wire parsing for official schedule records.
//!
//! The bridge forwards official records unchanged. Catalog entries are the
//! official record plus `sessionId`, `status`, and `lastDelivery`; the record
//! used for compare-and-set is the same object with those wrapper fields
//! removed, never a reconstructed subset.

use serde_json::{Map, Value};

/// One durable inbox delivery acknowledgment. This confirms inbox entry, not
/// model execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReceipt {
    /// Occurrence target.
    pub scheduled_at: String,
    /// Time the session persistence acknowledged delivery.
    pub delivered_at: String,
    /// Delivered message identity.
    pub message_id: String,
}

/// One saved delivery, optionally carrying the prompt sent for that occurrence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord {
    /// Occurrence target.
    pub scheduled_at: String,
    /// Acknowledgment time.
    pub delivered_at: String,
    /// Delivered message identity.
    pub message_id: String,
    /// Prompt snapshot, absent for legacy receipts.
    pub prompt: Option<String>,
}

/// One catalog entry projected for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleEntry {
    /// Task identity.
    pub id: String,
    /// Rule kind: after, at, every, daily, weekly, cron.
    pub kind: String,
    /// Stored title.
    pub title: String,
    /// Stored reminder instruction.
    pub prompt: String,
    /// Committed next (or final) UTC target.
    pub scheduled_at: String,
    /// `active` or `inactive`.
    pub status: String,
    /// Complete official record, used as `expected` by the compare-and-set update.
    pub record: Value,
    /// Latest acknowledged inbox delivery.
    pub last_delivery: Option<DeliveryReceipt>,
    /// One-shot delay in seconds.
    pub after_seconds: Option<i64>,
    /// Fixed interval in seconds.
    pub every_seconds: Option<i64>,
    /// Normalized local time for daily/weekly rules.
    pub time: Option<String>,
    /// Explicit IANA zone for daily/weekly/cron rules.
    pub time_zone: Option<String>,
    /// ISO weekdays for weekly rules.
    pub weekdays: Vec<u8>,
    /// Canonical cron expression.
    pub expression: Option<String>,
}

impl ScheduleEntry {
    /// Whether the official record says the task no longer schedules.
    pub fn ended(&self) -> bool {
        self.status == "inactive"
    }

    /// Human-readable rule summary.
    pub fn rule_label(&self) -> String {
        match self.kind.as_str() {
            "after" => format!("{} 秒后一次", self.after_seconds.unwrap_or(0)),
            "at" => format!("绝对时间 {}", self.scheduled_at),
            "every" => format!("每 {} 秒", self.every_seconds.unwrap_or(0)),
            "daily" => format!("每日 {}", self.time.clone().unwrap_or_default()),
            "weekly" => format!(
                "每周 {} · 周{}",
                self.time.clone().unwrap_or_default(),
                self.weekdays
                    .iter()
                    .map(|day| day.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            "cron" => format!("cron {}", self.expression.clone().unwrap_or_default()),
            other => other.to_string(),
        }
    }

    /// Zone shown for wall-clock rules.
    pub fn zone_label(&self) -> Option<&str> {
        self.time_zone.as_deref()
    }
}

/// Parse the `entries` array of a catalog result.
pub fn parse_catalog(value: &Value) -> Result<Vec<ScheduleEntry>, String> {
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| "catalog response has no entries array".to_string())?;
    entries.iter().map(parse_entry).collect()
}

/// Parse the `records` array of a list result.
pub fn parse_list(value: &Value) -> Result<Vec<ScheduleEntry>, String> {
    let records = value
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "list response has no records array".to_string())?;
    records.iter().map(parse_entry).collect()
}

fn parse_entry(value: &Value) -> Result<ScheduleEntry, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "schedule entry is not an object".to_string())?;
    let string = |name: &str| -> Result<String, String> {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("schedule entry is missing {name}"))
    };
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("active")
        .to_string();
    let last_delivery = object.get("lastDelivery").and_then(parse_receipt);
    let mut record = object.clone();
    for wrapper in ["sessionId", "status", "lastDelivery", "deliveryHistory"] {
        record.remove(wrapper);
    }
    let weekdays = object
        .get("weekdays")
        .and_then(Value::as_array)
        .map(|days| {
            days.iter()
                .filter_map(Value::as_u64)
                .filter(|day| (1..=7).contains(day))
                .map(|day| day as u8)
                .collect()
        })
        .unwrap_or_default();
    Ok(ScheduleEntry {
        id: string("id")?,
        kind: string("kind")?,
        title: string("title")?,
        prompt: string("prompt")?,
        scheduled_at: string("scheduledAt")?,
        status,
        record: Value::Object(record),
        last_delivery,
        after_seconds: object.get("afterSeconds").and_then(Value::as_i64),
        every_seconds: object.get("everySeconds").and_then(Value::as_i64),
        time: object
            .get("time")
            .and_then(Value::as_str)
            .map(str::to_string),
        time_zone: object
            .get("timeZone")
            .and_then(Value::as_str)
            .map(str::to_string),
        weekdays,
        expression: object
            .get("expression")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn parse_receipt(value: &Value) -> Option<DeliveryReceipt> {
    let object = value.as_object()?;
    Some(DeliveryReceipt {
        scheduled_at: object.get("scheduledAt")?.as_str()?.to_string(),
        delivered_at: object.get("deliveredAt")?.as_str()?.to_string(),
        message_id: object.get("messageId")?.as_str()?.to_string(),
    })
}

/// One bounded history page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    /// Task identity.
    pub id: String,
    /// Newest-first records.
    pub records: Vec<DeliveryRecord>,
    /// Missing receipts were not saved (legacy or unsaved deliveries).
    pub earlier_records_unavailable: bool,
    /// Earlier records were confirmed removed by an append.
    pub earlier_records_pruned: bool,
    /// Retention window in days.
    pub retention_days: u64,
    /// Retention record cap.
    pub retention_records: u64,
    /// Oldest returned message id when more records remain.
    pub next_before: Option<String>,
}

/// Result of one history read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryOutcome {
    /// A normal page, possibly empty.
    Page(HistoryPage),
    /// The task or the cursor is unknown.
    Missing {
        /// Stable official code.
        code: String,
    },
}

/// Parse one history result object.
pub fn parse_history(value: &Value) -> Result<HistoryOutcome, String> {
    if let Some(code) = value.get("code").and_then(Value::as_str) {
        return Ok(HistoryOutcome::Missing {
            code: code.to_string(),
        });
    }
    let records = value
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| "history response has no records array".to_string())?;
    let records = records
        .iter()
        .map(|record| {
            let object = record
                .as_object()
                .ok_or_else(|| "delivery record is not an object".to_string())?;
            Ok(DeliveryRecord {
                scheduled_at: object
                    .get("scheduledAt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "delivery record is missing scheduledAt".to_string())?
                    .to_string(),
                delivered_at: object
                    .get("deliveredAt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "delivery record is missing deliveredAt".to_string())?
                    .to_string(),
                message_id: object
                    .get("messageId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "delivery record is missing messageId".to_string())?
                    .to_string(),
                prompt: object
                    .get("prompt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let retention = value.get("retention");
    Ok(HistoryOutcome::Page(HistoryPage {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        records,
        earlier_records_unavailable: value
            .get("earlierRecordsUnavailable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        earlier_records_pruned: value
            .get("earlierRecordsPruned")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        retention_days: retention
            .and_then(|value| value.get("days"))
            .and_then(Value::as_u64)
            .unwrap_or(30),
        retention_records: retention
            .and_then(|value| value.get("records"))
            .and_then(Value::as_u64)
            .unwrap_or(200),
        next_before: value
            .get("nextBefore")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

/// Result of one compare-and-set update.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateOutcome {
    /// The record was committed (or was already equivalent).
    Updated,
    /// A non-mutating miss: `schedule_not_found`, `schedule_ended`, `schedule_conflict`.
    Miss {
        /// Stable official code.
        code: String,
    },
    /// A validation failure returned as a result.
    Invalid {
        /// Stable official code.
        code: String,
        /// Readable reason.
        message: String,
    },
}

/// Parse one update result object.
pub fn parse_update(value: &Value) -> Result<UpdateOutcome, String> {
    if value.get("updated").and_then(Value::as_bool) == Some(true) {
        return Ok(UpdateOutcome::Updated);
    }
    if value.get("updated").and_then(Value::as_bool) == Some(false) {
        // A no-op update returns `{id, updated:false, record}` with no code;
        // a miss returns `{id, updated:false, code}`. Treating the no-op as a
        // miss would close the form and falsely report a missing task.
        if let Some(code) = value.get("code").and_then(Value::as_str) {
            return Ok(UpdateOutcome::Miss {
                code: code.to_string(),
            });
        }
        if value.get("record").is_some() {
            return Ok(UpdateOutcome::Updated);
        }
        return Err("update miss carries neither a code nor a record".to_string());
    }
    if let Some(code) = value.get("code").and_then(Value::as_str) {
        return Ok(UpdateOutcome::Invalid {
            code: code.to_string(),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("官方拒绝修改")
                .to_string(),
        });
    }
    Err("update response has no updated flag or error code".to_string())
}

/// Parse a delete result into `(deleted, code)`.
pub fn parse_delete(value: &Value) -> Result<(bool, Option<String>), String> {
    let deleted = value
        .get("deleted")
        .and_then(Value::as_bool)
        .ok_or_else(|| "delete response has no deleted flag".to_string())?;
    Ok((
        deleted,
        value
            .get("code")
            .and_then(Value::as_str)
            .map(str::to_string),
    ))
}

/// Read an official error code from a JSON-RPC error body.
pub fn rpc_error_code(error: &Value) -> Option<&str> {
    error
        .get("data")
        .and_then(|data| data.get("code"))
        .and_then(Value::as_str)
}

/// Read a JSON-RPC error message.
pub fn rpc_error_message(error: &Value) -> Option<&str> {
    error.get("message").and_then(Value::as_str)
}

/// Remove wrapper fields from a catalog entry, if the caller has one as raw JSON.
pub fn record_of(entry: &Value) -> Option<Value> {
    let mut object: Map<String, Value> = entry.as_object()?.clone();
    for wrapper in ["sessionId", "status", "lastDelivery", "deliveryHistory"] {
        object.remove(wrapper);
    }
    Some(Value::Object(object))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_equivalent_update_is_a_no_op_not_a_miss() {
        let outcome = parse_update(&json!({
            "id": "s-1",
            "updated": false,
            "record": {"id": "s-1", "kind": "every", "title": "T", "prompt": "P",
                "scheduledAt": "2099-01-01T00:00:00.000Z", "everySeconds": 60}
        }))
        .unwrap();
        assert_eq!(outcome, UpdateOutcome::Updated);
    }

    #[test]
    fn a_miss_still_carries_its_code() {
        for code in ["schedule_conflict", "schedule_not_found", "schedule_ended"] {
            assert_eq!(
                parse_update(&json!({"id": "s-1", "updated": false, "code": code})).unwrap(),
                UpdateOutcome::Miss {
                    code: code.to_string()
                }
            );
        }
    }

    #[test]
    fn a_validation_result_stays_an_invalid_result() {
        assert_eq!(
            parse_update(&json!({"id": "s-1", "code": "invalid_rule", "message": "bad cron"}))
                .unwrap(),
            UpdateOutcome::Invalid {
                code: "invalid_rule".into(),
                message: "bad cron".into()
            }
        );
    }

    #[test]
    fn catalog_strips_wrappers_and_keeps_the_complete_record() {
        let entries = parse_catalog(&json!({
            "entries": [{
                "id": "s-1", "kind": "at", "title": "T", "prompt": "P",
                "scheduledAt": "2099-01-01T00:00:00.000Z",
                "sessionId": "root", "status": "inactive",
                "lastDelivery": {"scheduledAt": "2099-01-01T00:00:00.000Z",
                    "deliveredAt": "2099-01-01T00:00:01.000Z", "messageId": "m-1"}
            }]
        }))
        .unwrap();
        assert_eq!(entries[0].status, "inactive");
        assert_eq!(entries[0].record["id"], json!("s-1"));
        assert!(entries[0].record.get("sessionId").is_none());
        assert!(entries[0].record.get("lastDelivery").is_none());
        assert_eq!(entries[0].last_delivery.as_ref().unwrap().message_id, "m-1");
    }

    #[test]
    fn history_parse_keeps_the_two_flags_apart() {
        let outcome = parse_history(&json!({
            "id": "s-1",
            "records": [],
            "earlierRecordsUnavailable": true,
            "earlierRecordsPruned": false,
            "retention": {"days": 30, "records": 200}
        }))
        .unwrap();
        match outcome {
            HistoryOutcome::Page(page) => {
                assert!(page.earlier_records_unavailable);
                assert!(!page.earlier_records_pruned);
            }
            other => panic!("unexpected history outcome {other:?}"),
        }
    }
}
