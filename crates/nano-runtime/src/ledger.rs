//! Read-only view of the host-owned session mount ledger.
//!
//! The Rust side never claims or writes the ledger; the host does. This module
//! exists for diagnostics and for integration evidence (who owns which root
//! session, and whether a dead owner was swept).

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// One ledger owner record.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountOwner {
    /// Host process pid.
    pub pid: u32,
    /// Diagnostic start time.
    pub started_at: serde_json::Value,
    /// Claimed root session identities.
    pub session_ids: Vec<String>,
    /// Fencing token.
    pub token: String,
}

/// Parsed ledger.
#[derive(Debug, Clone, Deserialize)]
pub struct MountLedger {
    /// Contract ledger version.
    pub version: u32,
    /// Live-or-last-known owners.
    pub owners: Vec<MountOwner>,
}

/// Read the ledger. A missing file is `Ok(None)`; malformed content is an error.
pub fn read_ledger(path: &Path) -> Result<Option<MountLedger>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let ledger: MountLedger =
        serde_json::from_str(&text).map_err(|error| format!("invalid ledger {}: {error}", path.display()))?;
    if ledger.version != 1 {
        return Err(format!("unsupported ledger version {}", ledger.version));
    }
    Ok(Some(ledger))
}

impl MountLedger {
    /// Find a live-or-recorded owner of one root session.
    pub fn owner_for_session(&self, session_id: &str) -> Option<&MountOwner> {
        self.owners
            .iter()
            .find(|owner| owner.session_ids.iter().any(|id| id == session_id))
    }
}

/// Whether a pid exists. `EPERM` counts as alive, matching the host ledger rule.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if result == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_owner_and_finds_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session-mounts.json");
        let mut file = fs::File::create(&path).unwrap();
        write!(
            file,
            "{{\"version\":1,\"owners\":[{{\"pid\":4242,\"startedAt\":\"2026-10-21T00:00:00.000Z\",\"sessionIds\":[\"session-a\"],\"token\":\"4242-abc\"}}]}}"
        )
        .unwrap();
        let ledger = read_ledger(&path).unwrap().unwrap();
        let owner = ledger.owner_for_session("session-a").unwrap();
        assert_eq!(owner.pid, 4242);
        assert!(ledger.owner_for_session("session-b").is_none());
    }

    #[test]
    fn missing_is_none_and_corrupt_is_error() {
        let directory = tempfile::tempdir().unwrap();
        assert!(read_ledger(&directory.path().join("missing.json")).unwrap().is_none());
        let path = directory.path().join("broken.json");
        fs::write(&path, "{not json").unwrap();
        assert!(read_ledger(&path).is_err());
    }
}
