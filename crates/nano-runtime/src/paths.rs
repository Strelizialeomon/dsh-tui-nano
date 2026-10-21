//! Path resolution, version preflight, profile installation, and the
//! per-launch runtime patch.
//!
//! The launcher never builds a shell command string: every argument is passed
//! as an argument vector, and the bridge/profile paths are resolved to real
//! filesystem locations before the host is spawned.

use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::host::SessionMode;
use crate::protocol_error::HostError;
use crate::{PROFILE_NAME, SUPPORTED_OFFICIAL_VERSION};

static RANDOM_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Resolved absolute paths used for one launch.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    /// Node executable.
    pub node: PathBuf,
    /// dsh launcher executable.
    pub dsh: PathBuf,
    /// Linked bridge package directory.
    pub bridge_package: PathBuf,
    /// Nano data root.
    pub data_root: PathBuf,
    /// Official dsh home.
    pub dsh_home: PathBuf,
    /// Canonical working directory.
    pub cwd: PathBuf,
}

/// Result of the preflight checks.
#[derive(Debug, Clone)]
pub struct Preflight {
    /// Resolved paths.
    pub paths: RuntimePaths,
    /// Node version reported by `--version`.
    pub node_version: String,
    /// Official dsh version reported by `--version`.
    pub official_version: String,
}

/// Summary printed by `bridge/profile/install.mjs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileInstallSummary {
    /// Profile directory.
    pub profile_dir: PathBuf,
    /// Data root.
    pub data_root: PathBuf,
    /// Whether pnpm install ran and the composed profile was verified.
    pub installed: bool,
}

impl Preflight {
    /// Resolve paths and verify that this machine can boot the verified host.
    pub fn resolve(
        cwd: Option<&Path>,
        data_root: Option<&Path>,
        dsh_home: Option<&Path>,
        node_bin: Option<&Path>,
        dsh_bin: Option<&Path>,
        bridge_package: Option<&Path>,
    ) -> Result<Self, HostError> {
        let node = resolve_executable(node_bin, "NANO_NODE", "node")?;
        let dsh = resolve_executable(dsh_bin, "NANO_DSH", "dsh")?;
        let bridge_package = resolve_bridge(bridge_package)?;
        let data_root = resolve_data_root(data_root)?;
        let dsh_home = resolve_dsh_home(dsh_home)?;
        let cwd = match cwd {
            Some(path) => path.to_path_buf(),
            None => std::env::current_dir()?,
        };
        let cwd = fs::canonicalize(&cwd).map_err(|error| {
            HostError::Spawn(format!("working directory {} is not accessible: {error}", cwd.display()))
        })?;
        if !cwd.is_dir() {
            return Err(HostError::Spawn(format!(
                "working directory {} is not a directory",
                cwd.display()
            )));
        }
        ensure_private_dir(&data_root)?;
        ensure_private_dir(&data_root.join("runtime"))?;
        let data_root = fs::canonicalize(&data_root)?;
        if !bridge_package.join("package.json").is_file() {
            return Err(HostError::Spawn(format!(
                "bridge package is missing package.json: {}",
                bridge_package.display()
            )));
        }
        let protocol = bridge_package
            .parent()
            .map(|parent| parent.join("protocol").join("nano-protocol.json"));
        match protocol {
            Some(path) if path.is_file() => {}
            _ => {
                return Err(HostError::Spawn(format!(
                    "protocol contract is missing beside the bridge package: {}",
                    bridge_package.display()
                )))
            }
        }

        let node_version = run_version(&node, &["--version"])?;
        if !node_version.starts_with('v') {
            return Err(HostError::Spawn(format!(
                "node --version returned an unexpected value: {node_version}"
            )));
        }
        let official_version = match run_version(&dsh, &["--version"]) {
            Ok(version) => version,
            Err(_) => {
                let dsh_argument = dsh.to_string_lossy().to_string();
                run_version(&node, &[dsh_argument.as_str(), "--version"])?
            }
        };
        if official_version != SUPPORTED_OFFICIAL_VERSION {
            return Err(HostError::UnsupportedVersion(official_version));
        }

        Ok(Self {
            paths: RuntimePaths {
                node,
                dsh,
                bridge_package,
                data_root,
                dsh_home,
                cwd,
            },
            node_version,
            official_version,
        })
    }

    /// Run the profile installer. Idempotent; safe to call before every spawn.
    pub fn install_profile(&self) -> Result<ProfileInstallSummary, HostError> {
        let script = self.paths.bridge_package.join("profile").join("install.mjs");
        if !script.is_file() {
            return Err(HostError::Spawn(format!(
                "profile installer is missing: {}",
                script.display()
            )));
        }
        let output = Command::new(&self.paths.node)
            .arg(&script)
            .arg("--dsh-home")
            .arg(&self.paths.dsh_home)
            .arg("--data-root")
            .arg(&self.paths.data_root)
            .arg("--bridge-package")
            .arg(&self.paths.bridge_package)
            .arg("--dsh-bin")
            .arg(&self.paths.dsh)
            .arg("--json")
            .env("DSH_HOME", &self.paths.dsh_home)
            .output()
            .map_err(|error| HostError::Spawn(format!("could not run the profile installer: {error}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(HostError::Spawn(format!(
                "profile installer exited {}: {}\n{}",
                output.status,
                stderr.trim(),
                stdout.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .ok_or_else(|| HostError::Spawn("profile installer printed no JSON summary".into()))?;
        serde_json::from_str(line)
            .map_err(|error| HostError::Spawn(format!("profile installer summary is invalid: {error}")))
    }
}

/// Write the per-launch patch that sets the storage roots and bridge config.
///
/// The patch is JSON (valid YAML), so no string interpolation or quoting rules
/// can change a path.
pub fn write_runtime_patch(
    paths: &RuntimePaths,
    session_mode: SessionMode,
    runtime_id: &str,
    generation: u64,
    root_session_id: &str,
    nonce: &str,
    official_version: &str,
    test_faults: Option<&Value>,
    provider: Option<&str>,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Result<PathBuf, HostError> {
    let sessions_root = paths.data_root.join("sessions");
    let storage_root = paths
        .data_root
        .join("runtime-data")
        .join(root_session_id)
        .join("storage");
    ensure_private_dir(&sessions_root)?;
    ensure_private_dir(&storage_root)?;

    // S01 mounts the blocking schema; S03 may revise the mode with its own patch.
    let ask_user_mode = "blocking".to_string();
    let mut bridge_config = json!({
        "runtimeId": runtime_id,
        "generation": generation,
        "rootSessionId": root_session_id,
        "cwd": paths.cwd.to_string_lossy(),
        "dataRoot": paths.data_root.to_string_lossy(),
        "sessionMode": session_mode.as_str(),
        "officialVersion": official_version,
        "nonce": nonce,
        "askUserMode": ask_user_mode,
    });
    if let Some(faults) = test_faults {
        bridge_config["allowFaults"] = Value::Bool(true);
        bridge_config["faults"] = faults.clone();
    }
    // The initial route is optional; S04 owns changing it later. Both halves
    // must be present so the host never invents a provider/model pair.
    if let (Some(provider), Some(model)) = (provider, model) {
        bridge_config["provider"] = Value::String(provider.to_string());
        bridge_config["model"] = Value::String(model.to_string());
        if let Some(effort) = reasoning_effort {
            bridge_config["reasoningEffort"] = Value::String(effort.to_string());
        }
    }

    let patch = json!([
        {
            "id": "session-persistence-jsonl",
            "config": { "root": sessions_root.to_string_lossy() }
        },
        {
            "id": "storage-json",
            "config": { "root": storage_root.to_string_lossy() }
        },
        {
            // S11 routes only the `schedule` domain to its dedicated backend.
            // The patch replaces the whole config, so `backend` must stay here
            // even though only `routes` differs from the bundle default.
            "id": "storage-domain",
            "config": {
                "backend": "json",
                "routes": {
                    "schedule": "nano-schedule-json",
                    "nano_probe": "nano-schedule-json"
                }
            }
        },
        {
            "id": "nano-bridge",
            "config": bridge_config
        }
    ]);
    let patch_name = format!(
        "generation-{generation}-{}.json",
        &hex(&random_bytes(6))
    );
    let patch_path = paths.data_root.join("runtime").join(patch_name);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&patch_path)
        .map_err(|error| HostError::Spawn(format!("could not create runtime patch: {error}")))?;
    let bytes = serde_json::to_vec_pretty(&patch)
        .map_err(|error| HostError::Spawn(format!("could not encode runtime patch: {error}")))?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    Ok(patch_path)
}

/// Absolute path to the profile directory for this dsh home.
pub fn profile_dir(paths: &RuntimePaths) -> PathBuf {
    paths.dsh_home.join("profiles").join(PROFILE_NAME)
}

/// Generate a random runtime identity.
pub fn new_runtime_id() -> String {
    format!("rt-{}", hex(&random_bytes(12)))
}

/// Generate a pre-assigned root session identity (`session-<uuid v4>`).
pub fn new_root_session_id() -> String {
    let mut bytes = random_bytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex(&bytes);
    format!(
        "session-{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Generate a fallback-transport nonce.
pub fn new_nonce() -> String {
    hex(&random_bytes(16))
}

fn resolve_executable(
    explicit: Option<&Path>,
    env_name: &str,
    name: &str,
) -> Result<PathBuf, HostError> {
    let candidate = if let Some(path) = explicit {
        path.to_path_buf()
    } else if let Some(value) = std::env::var_os(env_name) {
        PathBuf::from(value)
    } else {
        which(name).ok_or_else(|| HostError::Spawn(format!("{name} was not found on PATH")))?
    };
    let canonical = fs::canonicalize(&candidate).map_err(|error| {
        HostError::Spawn(format!("{name} path {} is not accessible: {error}", candidate.display()))
    })?;
    if !is_executable(&canonical) {
        return Err(HostError::Spawn(format!("{name} path is not executable: {}", canonical.display())));
    }
    Ok(canonical)
}

fn resolve_bridge(explicit: Option<&Path>) -> Result<PathBuf, HostError> {
    let candidate = if let Some(path) = explicit {
        path.to_path_buf()
    } else if let Some(value) = std::env::var_os("NANO_BRIDGE_PACKAGE") {
        PathBuf::from(value)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bridge")
    };
    fs::canonicalize(&candidate).map_err(|error| {
        HostError::Spawn(format!(
            "bridge package {} is not accessible: {error}",
            candidate.display()
        ))
    })
}

fn resolve_data_root(explicit: Option<&Path>) -> Result<PathBuf, HostError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(value) = std::env::var_os("NANO_DATA_ROOT") {
        return Ok(PathBuf::from(value));
    }
    let home = home_dir()?;
    Ok(home.join(".dsh-tui-nano"))
}

fn resolve_dsh_home(explicit: Option<&Path>) -> Result<PathBuf, HostError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(value) = std::env::var_os("DSH_HOME") {
        return Ok(PathBuf::from(value));
    }
    Ok(home_dir()?.join(".dsh"))
}

fn home_dir() -> Result<PathBuf, HostError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| HostError::Spawn("HOME is not set".into()))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn run_version(executable: &Path, args: &[&str]) -> Result<String, HostError> {
    let output = Command::new(executable)
        .args(args)
        .output()
        .map_err(|error| HostError::Spawn(format!("could not run {}: {error}", executable.display())))?;
    if !output.status.success() {
        return Err(HostError::Spawn(format!(
            "{} --version exited {}",
            executable.display(),
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
        .ok_or_else(|| HostError::Spawn(format!("{} printed no version", executable.display())))
}

fn ensure_private_dir(path: &Path) -> Result<(), HostError> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Fill bytes from `/dev/urandom`, with a time/pid fallback for exotic hosts.
pub fn random_bytes(length: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; length];
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        if file.read_exact(&mut bytes).is_ok() {
            return bytes;
        }
    }
    let counter = RANDOM_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let seed = (nanos as u64) ^ (std::process::id() as u64) ^ counter;
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (seed.rotate_left((index as u32 * 7) % 64) >> ((index % 8) * 8)) as u8;
    }
    bytes
}

/// Lowercase hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Helper used by diagnostics and tests.
pub fn os_str(path: &Path) -> &OsStr {
    path.as_os_str()
}
