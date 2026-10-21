//! Context-occupancy reference values.
//!
//! Occupancy is `contextPressure.projectedTokens / contextPressure.contextWindow`
//! from the same official projection. Both parts are last-wins records of
//! different moments, so the value is a reference, not an atomic observation.
//! Missing either part means "unknown" — never zero, never the cumulative
//! billing usage. `tokenUsage`, `pressureTokens`, and `contextBreakdown` are
//! kept as separate facts and are never merged into the occupancy number.

use serde_json::Value;

/// Cumulative provider usage for the complete durable log (billing, not context).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Prompt tokens not served from cache.
    pub uncached_input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Prompt tokens read from cache.
    pub cache_read_tokens: u64,
    /// Prompt tokens written to cache.
    pub cache_write_tokens: u64,
}

impl TokenUsage {
    /// Total tokens counted for billing across all four disjoint buckets.
    pub fn total(&self) -> u64 {
        self.uncached_input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_read_tokens)
            .saturating_add(self.cache_write_tokens)
    }
}

/// Heuristic composition of the next request (approximate, not a total).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextBreakdown {
    /// Estimated system-prompt tokens.
    pub system_tokens: u64,
    /// Estimated tool-schema tokens.
    pub tools_tokens: u64,
    /// Estimated other visible surface tokens.
    pub message_tokens: u64,
}

/// One official context-occupancy projection sample with provenance times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMeter {
    /// Session the values belong to.
    pub session_id: String,
    /// Official projection cut these values were read at.
    pub as_of_seq: u64,
    /// Always true: the composition is heuristic and the pair is last-wins.
    pub approximate: bool,
    /// What the next request's prompt would cost.
    pub projected_tokens: Option<u64>,
    /// Newest recorded route capacity.
    pub context_window: Option<u64>,
    /// Last provider-reported prompt size.
    pub pressure_tokens: Option<u64>,
    /// Durable event time of the last usage sample, when tracked.
    pub pressure_at: Option<u64>,
    /// Durable event time of the last `request/context` record, when tracked.
    pub context_window_at: Option<u64>,
    /// Cumulative billing usage for the whole log, when the official
    /// projection provides it. Missing stays unknown; it is never zero-filled.
    pub token_usage: Option<TokenUsage>,
    /// Heuristic composition, when the projection provides it.
    pub breakdown: Option<ContextBreakdown>,
}

/// Known occupancy ratio or the explicit reason it is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    /// Both parts are present and the window is positive.
    Known {
        /// Projected next-request prompt tokens.
        projected: u64,
        /// Current route capacity.
        window: u64,
    },
    /// One or both parts are absent.
    Unknown,
}

impl Occupancy {
    /// Whole-percent occupancy, or `None` when unknown.
    pub fn percent(self) -> Option<u64> {
        match self {
            Self::Known { projected, window } if window > 0 => {
                Some(projected.saturating_mul(100) / window)
            }
            _ => None,
        }
    }

    /// Short display form, such as `12.3k/128k (10%)`.
    pub fn label(self) -> String {
        match self {
            Self::Known { projected, window } if window > 0 => format!(
                "{}/{} ({}%)",
                format_tokens(projected),
                format_tokens(window),
                projected.saturating_mul(100) / window
            ),
            _ => "未知".to_string(),
        }
    }
}

impl ContextMeter {
    /// Parse one `sessionsContext` value.
    /// @param value - wire object.
    /// @returns the parsed meter.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "context payload must be an object".to_string())?;
        let session_id = required_string(object, "sessionId")?;
        let as_of_seq = required_u64(object, "asOfSeq")?;
        let approximate = object
            .get("approximate")
            .and_then(Value::as_bool)
            .ok_or_else(|| "context.approximate must be a boolean".to_string())?;
        let token_usage = match object.get("tokenUsage") {
            None | Some(Value::Null) => None,
            Some(value) => Some(parse_token_usage(value)?),
        };
        let breakdown = match object.get("contextBreakdown") {
            None | Some(Value::Null) => None,
            Some(value) => Some(parse_breakdown(value)?),
        };
        Ok(Self {
            session_id,
            as_of_seq,
            approximate,
            projected_tokens: optional_u64(object, "projectedTokens"),
            context_window: optional_u64(object, "contextWindow").filter(|window| *window > 0),
            pressure_tokens: optional_u64(object, "pressureTokens"),
            pressure_at: optional_u64(object, "pressureAt"),
            context_window_at: optional_u64(object, "contextWindowAt"),
            token_usage,
            breakdown,
        })
    }

    /// Occupancy from the same projection pair.
    pub fn occupancy(&self) -> Occupancy {
        match (self.projected_tokens, self.context_window) {
            (Some(projected), Some(window)) if window > 0 => Occupancy::Known { projected, window },
            _ => Occupancy::Unknown,
        }
    }

    /// Provenance line for the header, relative to `now_ms`.
    pub fn provenance_label(&self, now_ms: u64) -> String {
        let mut parts = Vec::new();
        if let Some(at) = self.pressure_at {
            parts.push(format!(
                "用量记录 {}（{}）",
                format_clock(at),
                format_age(now_ms, at)
            ));
        }
        if let Some(at) = self.context_window_at {
            parts.push(format!(
                "窗口记录 {}（{}）",
                format_clock(at),
                format_age(now_ms, at)
            ));
        }
        if parts.is_empty() {
            "尚无官方记录".to_string()
        } else {
            parts.join(" · ")
        }
    }

    /// Whether this meter belongs to the given session.
    pub fn matches(&self, session_id: &str) -> bool {
        self.session_id == session_id
    }
}

fn parse_token_usage(value: &Value) -> Result<TokenUsage, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "context.tokenUsage must be an object".to_string())?;
    Ok(TokenUsage {
        uncached_input_tokens: required_u64(object, "uncachedInputTokens")?,
        output_tokens: required_u64(object, "outputTokens")?,
        cache_read_tokens: required_u64(object, "cacheReadTokens")?,
        cache_write_tokens: required_u64(object, "cacheWriteTokens")?,
    })
}

fn parse_breakdown(value: &Value) -> Result<ContextBreakdown, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "context.contextBreakdown must be an object".to_string())?;
    Ok(ContextBreakdown {
        system_tokens: required_u64(object, "systemTokens")?,
        tools_tokens: required_u64(object, "toolsTokens")?,
        message_tokens: required_u64(object, "messageTokens")?,
    })
}

fn required_string(object: &serde_json::Map<String, Value>, key: &str) -> Result<String, String> {
    match object.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(format!("context.{key} must be a non-empty string")),
    }
}

fn required_u64(object: &serde_json::Map<String, Value>, key: &str) -> Result<u64, String> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("context.{key} must be a non-negative integer"))
}

fn optional_u64(object: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    object.get(key).and_then(Value::as_u64)
}

/// Compact token count: exact below 1000, one decimal below 100k, whole k above.
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 100_000 {
        let value = tokens as f64 / 1_000.0;
        let text = format!("{value:.1}k");
        text.strip_suffix(".0k")
            .map(|whole| format!("{whole}k"))
            .unwrap_or(text)
    } else {
        format!("{}k", tokens / 1_000)
    }
}

/// UTC clock for one Unix epoch-millisecond instant.
pub fn format_clock(epoch_ms: u64) -> String {
    let seconds = epoch_ms / 1_000;
    let time_of_day = seconds % 86_400;
    format!(
        "{:02}:{:02}:{:02} UTC",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// Human age of one recorded instant.
pub fn format_age(now_ms: u64, then_ms: u64) -> String {
    if then_ms >= now_ms {
        return "刚刚".to_string();
    }
    let seconds = (now_ms - then_ms) / 1_000;
    if seconds < 60 {
        "刚刚".to_string()
    } else if seconds < 3_600 {
        format!("{} 分钟前", seconds / 60)
    } else if seconds < 86_400 {
        format!("{} 小时前", seconds / 3_600)
    } else {
        format!("{} 天前", seconds / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({
            "sessionId": "s-1",
            "asOfSeq": 9,
            "approximate": true,
            "projectedTokens": 12_345,
            "contextWindow": 128_000,
            "pressureTokens": 12_000,
            "pressureAt": 1_760_000_100_000u64,
            "contextWindowAt": 1_760_000_000_000u64,
            "tokenUsage": {
                "uncachedInputTokens": 100,
                "outputTokens": 20,
                "cacheReadTokens": 3,
                "cacheWriteTokens": 4
            },
            "contextBreakdown": {
                "systemTokens": 10,
                "toolsTokens": 20,
                "messageTokens": 30
            }
        })
    }

    #[test]
    fn occupancy_uses_only_the_projection_pair() {
        let meter = ContextMeter::parse(&payload()).unwrap();
        assert_eq!(
            meter.occupancy(),
            Occupancy::Known {
                projected: 12_345,
                window: 128_000
            }
        );
        assert_eq!(meter.occupancy().percent(), Some(9));
        assert_eq!(meter.occupancy().label(), "12.3k/128k (9%)");
        assert_eq!(meter.token_usage.unwrap().total(), 127);
    }

    #[test]
    fn a_missing_part_is_unknown_not_zero() {
        let mut value = payload();
        value.as_object_mut().unwrap().remove("contextWindow");
        let meter = ContextMeter::parse(&value).unwrap();
        assert_eq!(meter.occupancy(), Occupancy::Unknown);
        assert_eq!(meter.occupancy().label(), "未知");
        assert_eq!(meter.occupancy().percent(), None);

        let mut zero = payload();
        zero["contextWindow"] = json!(0);
        let meter = ContextMeter::parse(&zero).unwrap();
        assert_eq!(meter.context_window, None);
        assert_eq!(meter.occupancy(), Occupancy::Unknown);
    }

    #[test]
    fn billing_usage_and_breakdown_stay_separate() {
        let meter = ContextMeter::parse(&payload()).unwrap();
        assert_eq!(meter.pressure_tokens, Some(12_000));
        assert_eq!(
            meter.breakdown,
            Some(ContextBreakdown {
                system_tokens: 10,
                tools_tokens: 20,
                message_tokens: 30
            })
        );
        // The heuristic composition is not the occupancy numerator.
        assert_ne!(meter.occupancy().percent(), Some(60));
    }

    #[test]
    fn provenance_reports_clock_and_age() {
        let meter = ContextMeter::parse(&payload()).unwrap();
        let now = 1_760_000_160_000u64;
        let label = meter.provenance_label(now);
        assert!(label.contains("UTC"));
        assert!(label.contains("1 分钟前"));
    }

    #[test]
    fn a_missing_billing_projection_stays_unknown() {
        let mut value = payload();
        value.as_object_mut().unwrap().remove("tokenUsage");
        let meter = ContextMeter::parse(&value).unwrap();
        assert_eq!(meter.token_usage, None);
    }

    #[test]
    fn malformed_payloads_are_rejected() {
        assert!(ContextMeter::parse(&json!({})).is_err());
        let mut value = payload();
        value["tokenUsage"]
            .as_object_mut()
            .unwrap()
            .remove("outputTokens");
        assert!(ContextMeter::parse(&value).is_err());
    }
}
