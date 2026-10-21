//! Request identities.
//!
//! A `requestId` is the business identity of exactly one submission. It is not
//! the JSON-RPC request id and must never be regenerated for a retry or a
//! reconnect. The generator only needs local uniqueness; it uses the OS entropy
//! source when available and falls back to time + pid + a process counter.

use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a fresh request id.
pub fn new_request_id() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !filled {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let count = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = u128::from(std::process::id());
        let mixed = now ^ (pid << 64) ^ u128::from(count);
        bytes.copy_from_slice(&mixed.to_le_bytes());
    }
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    format!("nano-{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_ids_are_unique_and_prefixed() {
        let first = new_request_id();
        let second = new_request_id();
        assert!(first.starts_with("nano-"));
        assert_eq!(first.len(), 37);
        assert_ne!(first, second);
    }
}
