//! Shell and background-job display models for the Nano TUI.
//!
//! This crate owns no execution and no second task table. `!` commands run
//! through the bridge's official shell call; job projections, output bytes and
//! stop results come from the official job registry. The model here is a
//! bounded, discardable display cache: it never consumes model output and never
//! decides that a process exited.

pub mod bash;
pub mod model;
pub mod view;

pub use bash::{BashRecord, BashState};
pub use model::{JobsModel, OutputBuffer, MAX_JOBS, MAX_OUTPUT_BYTES};
pub use view::{
    CollectedOutput, JobChunk, JobOutput, JobOutputRead, JobStatus, JobToolCall, JobView,
    ShellResult,
};

/// First-use notice shown before any `!` command runs.
pub const SHELL_NOTICE: &str =
    "人工命令以你的用户权限运行，不是 Agent 授权；输出会作为合成上下文注入会话。";
