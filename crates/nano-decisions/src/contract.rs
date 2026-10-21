//! The official question and answer vocabulary.
//!
//! The shapes mirror `@deepseek-ai/dsh-user-questions`: one answer item per
//! question carrying `{ id, selected, custom? }`. A skipped question is
//! `{ id, selected: [] }`; custom text supplements a multi-select answer and
//! replaces a single-select choice. S03 never invents a second result shape.

use serde_json::{json, Value};

/// One selectable option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    /// User-facing label, echoed verbatim in the answer.
    pub label: String,
    /// Optional supporting detail.
    pub description: Option<String>,
}

/// The `plan-review` presentation intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReviewIntent {
    /// The option label that approves the plan.
    pub approve: String,
    /// Tool call whose arguments carry the reviewed plan, when recorded.
    pub call_id: Option<String>,
}

/// One question from the official request, or from a recorded tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    /// Stable caller-provided identity, echoed in the answer.
    pub id: String,
    /// The question text.
    pub question: String,
    /// Optional short heading.
    pub header: Option<String>,
    /// Optional supporting detail (the plan for a `plan-review`).
    pub detail: Option<String>,
    /// Options in official order.
    pub options: Vec<QuestionOption>,
    /// Whether more than one label may be selected.
    pub multi_select: bool,
    /// Presentation intent, when the asker declared one.
    pub intent: Option<PlanReviewIntent>,
}

impl Question {
    /// Whether this question declares the plan-review intent.
    pub fn plan_review(&self) -> Option<&PlanReviewIntent> {
        self.intent
            .as_ref()
            .filter(|intent| intent.approve.len() > 0)
    }

    /// Whether the option labels contain the given label.
    pub fn has_label(&self, label: &str) -> bool {
        self.options.iter().any(|option| option.label == label)
    }
}

/// One answer item in the official batch shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnswerItem {
    /// The answered question identity.
    pub id: String,
    /// Selected option labels; empty for a skipped question.
    pub selected: Vec<String>,
    /// Free-text "Other" answer.
    pub custom: Option<String>,
}

impl AnswerItem {
    /// The official skipped result: an empty selection and no custom text.
    pub fn skipped(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            selected: Vec::new(),
            custom: None,
        }
    }

    /// Whether the item carries any answer.
    pub fn answered(&self) -> bool {
        !self.selected.is_empty()
            || self
                .custom
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
    }

    /// Wire shape, omitting an unused custom field.
    pub fn to_wire(&self) -> Value {
        let mut value = json!({ "id": self.id, "selected": self.selected });
        if let Some(custom) = self
            .custom
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            value["custom"] = Value::String(custom.to_string());
        }
        value
    }
}

/// Parse one official question array. Unknown members are ignored; a question
/// without an id or text is dropped rather than guessed.
pub fn parse_questions(value: Option<&Value>) -> Vec<Question> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?.to_string();
            let question = item.get("question").and_then(Value::as_str)?.to_string();
            let options = item
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(|option| {
                            Some(QuestionOption {
                                label: option.get("label").and_then(Value::as_str)?.to_string(),
                                description: option
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let multi_select = item
                .get("multiSelect")
                .or_else(|| item.get("multi_select"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let intent = item.get("intent").and_then(|intent| {
                let kind = intent.get("kind").and_then(Value::as_str)?;
                if kind != "plan-review" {
                    return None;
                }
                Some(PlanReviewIntent {
                    approve: intent.get("approve").and_then(Value::as_str)?.to_string(),
                    call_id: intent
                        .get("callId")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
            });
            Some(Question {
                id,
                question,
                header: item
                    .get("header")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                detail: item
                    .get("detail")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                options,
                multi_select,
                intent,
            })
        })
        .collect()
}

/// Parse one official answer batch.
pub fn parse_answers(value: Option<&Value>) -> Option<Vec<AnswerItem>> {
    let items = value?.as_array()?;
    let mut answers = Vec::new();
    for item in items {
        let id = item.get("id").and_then(Value::as_str)?.to_string();
        let selected = item
            .get("selected")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let custom = item
            .get("custom")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string);
        answers.push(AnswerItem {
            id,
            selected,
            custom,
        });
    }
    Some(answers)
}

/// Why an answer batch cannot be submitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerError {
    /// A question has no answer item.
    Missing {
        /// Question id.
        id: String,
    },
    /// The same question id appears twice.
    Duplicate {
        /// Question id.
        id: String,
    },
    /// A selected label is not one of the question's options.
    UnknownLabel {
        /// Question id.
        id: String,
        /// Offending label.
        label: String,
    },
    /// A single-select answer carries more than one label.
    TooMany {
        /// Question id.
        id: String,
    },
    /// The batch names an id the request never asked.
    Unexpected {
        /// Question id.
        id: String,
    },
}

impl std::fmt::Display for AnswerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing { id } => write!(formatter, "缺少问题 {id} 的答案"),
            Self::Duplicate { id } => write!(formatter, "问题 {id} 的答案重复"),
            Self::UnknownLabel { id, label } => {
                write!(formatter, "问题 {id} 的选项 {label:?} 不存在")
            }
            Self::TooMany { id } => write!(formatter, "问题 {id} 是单选，不能提交多个选项"),
            Self::Unexpected { id } => write!(formatter, "答案包含未提问的 id {id}"),
        }
    }
}

impl std::error::Error for AnswerError {}

/// Validate one batch against the request vocabulary before it reaches the
/// bridge. The official service validates question ids; S03 additionally
/// refuses selected labels that the question never offered.
pub fn validate_answer_batch(
    questions: &[Question],
    answers: &[AnswerItem],
) -> Result<(), AnswerError> {
    if answers.len() != questions.len() {
        for question in questions {
            if !answers.iter().any(|answer| answer.id == question.id) {
                return Err(AnswerError::Missing {
                    id: question.id.clone(),
                });
            }
        }
    }
    for (index, answer) in answers.iter().enumerate() {
        if answers[..index].iter().any(|other| other.id == answer.id) {
            return Err(AnswerError::Duplicate {
                id: answer.id.clone(),
            });
        }
        let Some(question) = questions.iter().find(|question| question.id == answer.id) else {
            return Err(AnswerError::Unexpected {
                id: answer.id.clone(),
            });
        };
        if !question.multi_select && answer.selected.len() > 1 {
            return Err(AnswerError::TooMany {
                id: answer.id.clone(),
            });
        }
        for label in &answer.selected {
            if !question.has_label(label) {
                return Err(AnswerError::UnknownLabel {
                    id: answer.id.clone(),
                    label: label.clone(),
                });
            }
        }
    }
    for question in questions {
        if !answers.iter().any(|answer| answer.id == question.id) {
            return Err(AnswerError::Missing {
                id: question.id.clone(),
            });
        }
    }
    Ok(())
}

/// Wire batch.
pub fn answers_to_wire(answers: &[AnswerItem]) -> Value {
    Value::Array(answers.iter().map(AnswerItem::to_wire).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_service_and_tool_spellings() {
        let questions = parse_questions(Some(&json!([
            {
                "id": "q1",
                "question": "Pick one",
                "header": "H",
                "multi_select": true,
                "options": [{"label": "A", "description": "first"}]
            },
            {
                "id": "q2",
                "question": "Plan?",
                "detail": "# Plan",
                "intent": {"kind": "plan-review", "approve": "Approve", "callId": "c-1"},
                "options": [{"label": "Approve"}, {"label": "Change"}]
            }
        ])));
        assert_eq!(questions.len(), 2);
        assert!(questions[0].multi_select);
        assert_eq!(
            questions[0].options[0].description.as_deref(),
            Some("first")
        );
        assert_eq!(questions[1].plan_review().unwrap().approve, "Approve");
        assert_eq!(
            questions[1].plan_review().unwrap().call_id.as_deref(),
            Some("c-1")
        );
    }

    #[test]
    fn a_skipped_item_is_an_empty_selection() {
        let wire = AnswerItem::skipped("q").to_wire();
        assert_eq!(wire, json!({"id": "q", "selected": []}));
    }

    #[test]
    fn multi_select_can_carry_selected_and_custom_together() {
        let item = AnswerItem {
            id: "q".into(),
            selected: vec!["A".into()],
            custom: Some("other".into()),
        };
        assert_eq!(
            item.to_wire(),
            json!({"id": "q", "selected": ["A"], "custom": "other"})
        );
    }

    #[test]
    fn validation_refuses_missing_duplicate_and_unknown_labels() {
        let questions = parse_questions(Some(&json!([
            {"id": "q1", "question": "Q", "options": [{"label": "A"}]},
            {"id": "q2", "question": "Q", "multi_select": true, "options": [{"label": "B"}]}
        ])));
        assert!(validate_answer_batch(
            &questions,
            &[AnswerItem::skipped("q1"), AnswerItem::skipped("q2"),]
        )
        .is_ok());
        assert_eq!(
            validate_answer_batch(&questions, &[AnswerItem::skipped("q1")]),
            Err(AnswerError::Missing { id: "q2".into() })
        );
        assert_eq!(
            validate_answer_batch(
                &questions,
                &[AnswerItem::skipped("q1"), AnswerItem::skipped("q2")]
            ),
            Ok(())
        );
        assert!(matches!(
            validate_answer_batch(
                &questions,
                &[
                    AnswerItem {
                        id: "q1".into(),
                        selected: vec!["Z".into()],
                        custom: None
                    },
                    AnswerItem::skipped("q2")
                ]
            ),
            Err(AnswerError::UnknownLabel { .. })
        ));
    }

    #[test]
    fn single_select_refuses_multiple_labels() {
        let questions = parse_questions(Some(&json!([
            {"id": "q", "question": "Q", "options": [{"label": "A"}, {"label": "B"}]}
        ])));
        assert!(matches!(
            validate_answer_batch(
                &questions,
                &[AnswerItem {
                    id: "q".into(),
                    selected: vec!["A".into(), "B".into()],
                    custom: None
                }]
            ),
            Err(AnswerError::TooMany { .. })
        ));
    }
}
