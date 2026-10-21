//! S06 attachment and file-reference model.
//!
//! This crate owns the display-independent half of S06: the `@file` grammar,
//! local image checks against deployment limits, ordered attachment cards
//! bound to draft text offsets, and cancellation-safe completion state. It
//! never reads a referenced file for completion, never uploads, and never
//! computes a durable digest: the official attachment service owns admission,
//! normalization, persistence, and the model-facing file handle.

pub mod completion;
pub mod composer;
pub mod grammar;
pub mod image;
pub mod wire;

pub use completion::{AcceptedMention, CompletionRequest, CompletionState, CompletionUpdate};
pub use composer::{
    AttachmentAnchor, AttachmentBlocker, AttachmentCard, AttachmentDraftStore, AttachmentKind,
    AttachmentState, ComposerAttachments,
};
pub use grammar::{
    active_at_token, format_file_mention, AtToken, CandidateKind, FileReferenceCandidate,
};
pub use image::{
    check as check_image, encode_base64, probe as probe_image, ImageLimitError, ImageLimits,
    ImageMediaType, ImageProbe, ImageProbeError,
};
pub use wire::{
    begin_params, chunk_params, discard_params, finish_params, list_params, parse_candidates,
    parse_finished, FinishedFile, FinishedReceipt,
};
