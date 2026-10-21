//! Preflight refusal without spawning a host: an unsupported official version
//! must fail before any process or session is created.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use nano_runtime::paths::Preflight;
use nano_runtime::protocol_error::HostError;

#[test]
fn unsupported_official_version_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let fake = root.path().join("fake-dsh");
    fs::write(&fake, "#!/bin/sh\necho 9.9.9\n").unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let bridge = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bridge");
    let error = Preflight::resolve(
        Some(root.path()),
        Some(&root.path().join("data")),
        Some(&root.path().join("dsh-home")),
        None,
        Some(&fake),
        Some(&bridge),
    )
    .unwrap_err();
    match error {
        HostError::UnsupportedVersion(version) => assert_eq!(version, "9.9.9"),
        other => panic!("expected UnsupportedVersion, got {other}"),
    }
}

#[test]
fn missing_bridge_package_is_refused() {
    // Pass the real dsh explicitly so the missing bridge is the only failure;
    // otherwise a missing dsh would make this test pass for the wrong reason.
    let dsh = String::from_utf8(
        Command::new("which")
            .arg("dsh")
            .output()
            .expect("which dsh")
            .stdout,
    )
    .expect("utf8 path");
    let dsh = dsh.trim();
    assert!(!dsh.is_empty(), "dsh must be on PATH for this test");
    let root = tempfile::tempdir().unwrap();
    let error = Preflight::resolve(
        Some(root.path()),
        Some(&root.path().join("data")),
        Some(&root.path().join("dsh-home")),
        None,
        Some(std::path::Path::new(dsh)),
        Some(&root.path().join("missing-bridge")),
    )
    .unwrap_err();
    assert!(
        matches!(error, HostError::Spawn(ref message) if message.contains("bridge package")),
        "got {error}"
    );
}
