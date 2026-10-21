//! Question and approval state for the Nano TUI.
//!
//! The crate owns the cards that temporarily take the input seat, the official
//! answer vocabulary, timed-question countdowns, the read-only replay path, and
//! the exact bridge calls the app must send. It never decides model or
//! permission policy: approvals only expose the official closed outcomes, and
//! question answers always carry one item per question id.

pub mod approval;
pub mod contract;
pub mod model;
pub mod question;

pub use approval::{
    ApprovalAction, ApprovalCard, ApprovalCardState, ApprovalFocus, ApprovalOutcome,
    APPROVAL_LOCAL_TIMEOUT,
};
pub use contract::{
    answers_to_wire, parse_answers, parse_questions, validate_answer_batch, AnswerError,
    AnswerItem, PlanReviewIntent, Question, QuestionOption,
};
pub use model::{
    DecisionAction, DecisionClick, DecisionTop, DecisionsModel, ProjectedQuestion,
    QuestionProjection, SettledQuestion,
};
pub use question::{
    DecisionKey, Draft, PlanFocus, QuestionAction, QuestionCard, QuestionCardState, TimedState,
};
