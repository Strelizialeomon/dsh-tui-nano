//! Wire helpers for the S06 request methods.

use serde_json::{json, Value};

use crate::composer::AttachmentKind;
use crate::grammar::{CandidateKind, FileReferenceCandidate};

/// Build `nano.attachment.begin` params.
pub fn begin_params(
    address: &Value,
    receipt_id: &str,
    kind: AttachmentKind,
    name: Option<&str>,
    media_type: Option<&str>,
    bytes: u64,
) -> Value {
    let mut params = json!({
        "address": address,
        "receiptId": receipt_id,
        "kind": kind.wire_kind(),
        "bytes": bytes,
    });
    if let Some(name) = name {
        params["name"] = Value::String(name.to_string());
    }
    if let Some(media_type) = media_type {
        params["mediaType"] = Value::String(media_type.to_string());
    }
    params
}

/// Build `nano.attachment.chunk` params.
pub fn chunk_params(receipt_id: &str, sequence: u64, bytes_base64: &str) -> Value {
    json!({
        "receiptId": receipt_id,
        "sequence": sequence,
        "bytes": bytes_base64,
    })
}

/// Build `nano.attachment.finish` params.
pub fn finish_params(receipt_id: &str) -> Value {
    json!({ "receiptId": receipt_id })
}

/// Build `nano.attachment.discard` params.
pub fn discard_params(receipt_id: &str) -> Value {
    json!({ "receiptId": receipt_id })
}

/// Build `nano.fileReferences.list` params.
pub fn list_params(address: &Value, query: &str) -> Value {
    json!({ "address": address, "query": query })
}

/// Parse `nano.attachment.finish` result facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedReceipt {
    /// Receipt identity.
    pub receipt_id: String,
    /// Image or file.
    pub kind: AttachmentKind,
    /// Durable file reference facts for generic files.
    pub file: Option<FinishedFile>,
}

/// Generic-file facts returned by finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedFile {
    /// Opaque durable attachment id.
    pub attachment_id: String,
    /// Sanitized display name.
    pub name: String,
    /// Exact byte length.
    pub bytes: u64,
}

/// Parse one finish result.
pub fn parse_finished(value: &Value) -> Result<FinishedReceipt, String> {
    let receipt_id = value
        .get("receiptId")
        .and_then(Value::as_str)
        .ok_or_else(|| "finish result is missing receiptId".to_string())?;
    let kind = match value.get("kind").and_then(Value::as_str) {
        Some("image") => AttachmentKind::Image,
        Some("file") => AttachmentKind::File,
        other => return Err(format!("finish result has unknown kind {other:?}")),
    };
    let file = value.get("file").and_then(|file| {
        Some(FinishedFile {
            attachment_id: file.get("attachmentId")?.as_str()?.to_string(),
            name: file.get("name")?.as_str()?.to_string(),
            bytes: file.get("bytes")?.as_u64()?,
        })
    });
    Ok(FinishedReceipt {
        receipt_id: receipt_id.to_string(),
        kind,
        file,
    })
}

/// Parse one file-reference result.
pub fn parse_candidates(value: &Value) -> Result<Vec<FileReferenceCandidate>, String> {
    let candidates = value
        .get("candidates")
        .and_then(Value::as_array)
        .ok_or_else(|| "fileReferences result is missing candidates".to_string())?;
    let mut parsed = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let path = candidate
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "candidate is missing path".to_string())?;
        let kind = match candidate.get("kind").and_then(Value::as_str) {
            Some("directory") => CandidateKind::Directory,
            Some("file") => CandidateKind::File,
            other => return Err(format!("candidate has unknown kind {other:?}")),
        };
        parsed.push(FileReferenceCandidate {
            path: path.to_string(),
            kind,
        });
    }
    Ok(parsed)
}

/// A display name with any local directory information and control characters
/// removed.
pub fn display_name(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    name.chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_and_candidates_parse() {
        let finished = parse_finished(&json!({
            "receiptId": "r-1",
            "kind": "file",
            "ready": true,
            "file": {"attachmentId": "att-1", "name": "a.txt", "bytes": 5}
        }))
        .unwrap();
        assert_eq!(finished.kind, AttachmentKind::File);
        assert_eq!(finished.file.unwrap().attachment_id, "att-1");

        let candidates = parse_candidates(&json!({
            "candidates": [
                {"path": "src/main.rs", "kind": "file"},
                {"path": "src", "kind": "directory"}
            ]
        }))
        .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1].kind, CandidateKind::Directory);
    }

    #[test]
    fn a_name_never_carries_a_directory_or_control_characters() {
        assert_eq!(display_name(std::path::Path::new("/a/b/c.txt")), "c.txt");
        assert_eq!(
            display_name(std::path::Path::new("/a/b/bad\u{7}name.txt")),
            "badname.txt"
        );
    }
}
