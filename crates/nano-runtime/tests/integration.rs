//! Real-host integration tests for S01.
//!
//! These tests spawn the installed `dsh` launcher with the Nano profile and
//! talk over fd 3/4 exactly as the TUI does. They are `#[ignore]`d because they
//! run pnpm and real host processes; run them with `--ignored`. The slow
//! termination-budget test additionally needs `NANO_E2E_SLOW=1`.

use std::fs;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use nano_runtime::host::generate_root_session_id;
use nano_runtime::ledger::{pid_alive, read_ledger};
use nano_runtime::lifecycle::LifecycleState;
use nano_runtime::paths::RuntimePaths;
use nano_runtime::protocol::{Address, CapabilityStatus, SessionFactMode, StepStatus};
use nano_runtime::{
    Hello, Host, HostError, HostEvent, HostOptions, SessionMode, ShutdownOutcome,
    SHUTDOWN_STEP_NAMES,
};

fn slow_enabled() -> bool {
    std::env::var("NANO_E2E_SLOW").ok().as_deref() == Some("1")
}

struct Fixture {
    _root: tempfile::TempDir,
    data: PathBuf,
    options: HostOptions,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temp dir");
        let home = root.path().join("dsh-home");
        let data = root.path().join("data");
        let work = root.path().join("work");
        fs::create_dir_all(&work).expect("work dir");
        let options = HostOptions {
            cwd: Some(work.clone()),
            data_root: Some(data.clone()),
            dsh_home: Some(home.clone()),
            ..HostOptions::default()
        };
        Self {
            _root: root,
            data,
            options,
        }
    }

    fn install(&self) {
        let summary = Host::install_profile(&self.options).expect("profile install");
        assert!(summary.installed, "profile install did not run pnpm");
        assert_eq!(
            summary.data_root,
            fs::canonicalize(&self.data).expect("canonical data root")
        );
    }

    fn start(&self, session: &str, mode: SessionMode) -> HostGuard {
        self.start_with(session, mode, &self.options)
    }

    fn start_with(&self, session: &str, mode: SessionMode, options: &HostOptions) -> HostGuard {
        let host = Host::spawn(
            format!("rt-test-{}", nano_runtime::host::generate_runtime_id()),
            1,
            session,
            mode,
            options,
        )
        .expect("host spawn");
        HostGuard(host)
    }

    fn worktree(&self, name: &str) -> PathBuf {
        let path = self._root.path().join(name);
        fs::create_dir_all(&path).expect("worktree dir");
        path
    }

    fn options_for(&self, cwd: &Path) -> HostOptions {
        let mut options = self.options.clone();
        options.cwd = Some(cwd.to_path_buf());
        options
    }

    fn paths(&self) -> RuntimePaths {
        Host::preflight(&self.options).expect("preflight").paths
    }

    fn session_owners(&self, session: &str) -> Vec<u32> {
        read_ledger(&self.data.join("session-mounts.json"))
            .expect("read ledger")
            .map(|ledger| {
                ledger
                    .owners
                    .into_iter()
                    .filter(|owner| owner.session_ids.iter().any(|id| id == session))
                    .map(|owner| owner.pid)
                    .collect()
            })
            .unwrap_or_default()
    }
}

struct HostGuard(Host);

impl Deref for HostGuard {
    type Target = Host;
    fn deref(&self) -> &Host {
        &self.0
    }
}

impl DerefMut for HostGuard {
    fn deref_mut(&mut self) -> &mut Host {
        &mut self.0
    }
}

impl Drop for HostGuard {
    fn drop(&mut self) {
        // Never leave an orphan behind when an assertion panics. A normal
        // shutdown already exited the process, so this is a no-op then.
        if !self.0.wait_for_exit(Duration::from_millis(100)) {
            #[cfg(unix)]
            unsafe {
                libc::kill(self.0.pid() as libc::pid_t, libc::SIGKILL);
            }
            self.0.wait_for_exit(Duration::from_secs(5));
        }
    }
}

fn handshake(host: &Host) -> Hello {
    host.handshake(Some(Duration::from_secs(90)))
        .expect("handshake")
}

fn next_event_matching(host: &Host, subscription_id: &str) -> HostEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::Event(params)) if params.subscription_id == subscription_id => {
                return HostEvent::Event(params);
            }
            Ok(other) => panic!("unexpected event: {other:?}"),
            Err(error) => panic!("no event arrived: {error}"),
        }
    }
}

fn wait_for_stream_ended(host: &Host, subscription_id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::StreamEnded(params)) if params.subscription_id == subscription_id => {
                return
            }
            Ok(HostEvent::Event(_)) => continue,
            Ok(other) => panic!("unexpected event before streamEnded: {other:?}"),
            Err(error) => panic!("streamEnded did not arrive: {error}"),
        }
    }
}

fn session_log_exists(data_root: &Path, session_id: &str) -> bool {
    let Ok(entries) = fs::read_dir(data_root.join("sessions")) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| entry.path().join(session_id).is_dir())
}

fn listening_sockets(pid: u32) -> Result<Vec<String>, String> {
    let output = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output()
        .map_err(|error| format!("lsof is required for the no-listener assertion: {error}"))?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .map(str::to_string)
        .collect())
}

fn clean_report(outcome: &ShutdownOutcome) {
    assert!(outcome.ack, "host did not acknowledge shutdown");
    assert!(outcome.process_exited, "host process did not exit");
    assert!(!outcome.forced, "shutdown needed TERM/KILL");
    assert!(outcome.clean, "shutdown report is not clean: {outcome:?}");
    let report = outcome.report.as_ref().expect("shutdown report");
    assert!(
        report
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Stopped),
        "non-stopped shutdown step: {report:?}"
    );
    let names: Vec<&str> = report.steps.iter().map(|step| step.name.as_str()).collect();
    assert_eq!(names, SHUTDOWN_STEP_NAMES.to_vec());
    assert_eq!(outcome.session_saved, Some(true));
    assert_eq!(outcome.scope_cleared, Some(true));
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn handshake_subscribe_shutdown_roundtrip() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    let hello = handshake(&host);

    assert_eq!(hello.root_session_id, session);
    assert_eq!(hello.session.mode, SessionFactMode::Created);
    assert!(
        hello.session.seq >= 3,
        "session header events were expected"
    );
    assert_eq!(hello.versions.official, "0.2.1-rc.2");
    assert_eq!(hello.versions.bridge, "0.1.0");
    assert!(hello.versions.node.starts_with('v'));
    assert_eq!(
        hello.capabilities["agent"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        hello.capabilities["approval"].status,
        CapabilityStatus::Supported
    );
    let provenance = hello.provenance.as_ref().expect("source labels");
    let config = provenance.config.as_ref().expect("profile patch path");
    assert!(
        config.path.ends_with("cordis.patch.yml"),
        "unexpected profile patch: {}",
        config.path
    );
    let credentials = provenance.credentials.as_ref().expect("credential source");
    assert_eq!(credentials.reference, "DEEPSEEK_API_KEY");
    assert_eq!(
        hello.capabilities["userQuestions"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        hello.capabilities["tool-ask-user"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        hello.capabilities["tool-ask-user"].mode.as_deref(),
        Some("blocking")
    );
    assert_eq!(
        hello.capabilities["networkListener"].status,
        CapabilityStatus::Unsupported
    );
    assert_eq!(
        hello.capabilities["schedule"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        hello.capabilities["terminal"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        hello.capabilities["schedule"].status,
        CapabilityStatus::Supported
    );

    let owners = fixture.session_owners(&session);
    assert_eq!(owners, vec![host.pid()]);
    assert!(pid_alive(host.pid()));
    assert!(
        listening_sockets(host.pid())
            .expect("lsof must run for the no-listener assertion")
            .is_empty(),
        "the Nano host must not open a TCP listener"
    );

    let cancelled = host
        .cancel_request("req-does-not-exist")
        .expect("cancel request");
    assert!(!cancelled.cancelled);
    match host.request("nano.mystery", json!({})) {
        Err(HostError::Rpc(error)) => assert_eq!(error.code, -32601),
        other => panic!("unknown method did not report method-not-found: {other:?}"),
    }

    match host.subscribe(
        Address {
            root_session_id: "session-other".into(),
            child_session_id: None,
        },
        None,
        None,
    ) {
        Err(HostError::Rpc(error)) => assert_eq!(error.code, 1008),
        other => panic!("foreign root subscription was not refused: {other:?}"),
    }

    let subscription = host
        .subscribe(
            Address {
                root_session_id: session.clone(),
                child_session_id: None,
            },
            None,
            Some(0),
        )
        .expect("subscribe");
    assert!(subscription.seq >= 3);
    match next_event_matching(&host, &subscription.subscription_id) {
        HostEvent::Event(params) => {
            assert_eq!(params.generation, 1);
            assert_eq!(params.kind, "session.event");
            assert!(params.payload.get("type").is_some());
            assert!(params.payload.get("seq").is_some());
        }
        other => panic!("expected event, got {other:?}"),
    }

    host.unsubscribe(&subscription.subscription_id)
        .expect("unsubscribe");
    wait_for_stream_ended(&host, &subscription.subscription_id);

    let outcome = host.shutdown();
    clean_report(&outcome);
    assert!(
        fixture.session_owners(&session).is_empty(),
        "ledger record was not released"
    );
    assert!(
        fixture.data.join("sessions").read_dir().unwrap().count() > 0,
        "session log was not created"
    );
    let storage = fixture
        .data
        .join("runtime-data")
        .join(&session)
        .join("storage");
    assert!(storage.is_dir(), "per-root storage directory is missing");
    assert!(
        storage.join("session_projcache").is_dir(),
        "the official storage domain did not write under the per-root storage"
    );
    assert!(
        !fixture
            .options
            .dsh_home
            .as_ref()
            .expect("dsh home")
            .join("storages")
            .exists(),
        "the host must not write $DSH_HOME/storages"
    );
    let runtime = fixture.paths();
    assert!(!runtime
        .data_root
        .join("runtime")
        .read_dir()
        .unwrap()
        .any(|entry| {
            entry
                .map(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with("generation-")
                })
                .unwrap_or(false)
        }));
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn second_host_same_session_is_occupied() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let first = fixture.start(&session, SessionMode::Create);
    handshake(&first);

    let second = fixture.start(&session, SessionMode::Create);
    match second.handshake(Some(Duration::from_secs(30))) {
        Err(HostError::Startup {
            code, holder_pid, ..
        }) => {
            assert_eq!(code, "occupied");
            assert_eq!(holder_pid, Some(first.pid()));
        }
        other => panic!("second host was not refused: {other:?}"),
    }
    assert!(second.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(fixture.session_owners(&session), vec![first.pid()]);

    let outcome = first.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session).is_empty());
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn simultaneous_hosts_for_one_session_leave_exactly_one_winner() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let first = fixture.start(&session, SessionMode::Create);
    let second = fixture.start(&session, SessionMode::Create);
    let first_result = first.handshake(Some(Duration::from_secs(30)));
    let second_result = second.handshake(Some(Duration::from_secs(30)));
    let (winner, loser) = match (first_result, second_result) {
        (Ok(_), Err(HostError::Startup { code, .. })) => {
            assert_eq!(code, "occupied");
            (&first, &second)
        }
        (Err(HostError::Startup { code, .. }), Ok(_)) => {
            assert_eq!(code, "occupied");
            (&second, &first)
        }
        (first, second) => {
            panic!("expected one winner and one occupied host, got {first:?} / {second:?}")
        }
    };
    assert!(loser.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(fixture.session_owners(&session), vec![winner.pid()]);
    let outcome = winner.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session).is_empty());
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn reused_pid_causes_false_occupancy_without_double_write() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    fs::create_dir_all(&fixture.data).unwrap();
    // A dead host's pid was reused by this live test process. The ledger has no
    // way to know, so it must report occupied rather than risk a double writer.
    fs::write(
        fixture.data.join("session-mounts.json"),
        serde_json::json!({
            "version": 1,
            "owners": [{
                "pid": std::process::id(),
                "startedAt": "2026-10-21T00:00:00.000Z",
                "sessionIds": [session],
                "token": "reused-pid"
            }]
        })
        .to_string(),
    )
    .unwrap();
    let host = fixture.start(&session, SessionMode::Create);
    match host.handshake(Some(Duration::from_secs(30))) {
        Err(HostError::Startup {
            code, holder_pid, ..
        }) => {
            assert_eq!(code, "occupied");
            assert_eq!(holder_pid, Some(std::process::id()));
        }
        other => panic!("reused pid was not treated as occupied: {other:?}"),
    }
    assert!(host.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(
        fixture.data.join("sessions").read_dir().unwrap().count(),
        0,
        "the refused host must not create a session"
    );
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn stdio_fallback_handshake_and_shutdown() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.force_fallback = true;
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    let hello = handshake(&host);
    assert_eq!(hello.root_session_id, session);
    let outcome = host.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn corrupt_ledger_refuses_startup() {
    let fixture = Fixture::new();
    fixture.install();
    fs::create_dir_all(&fixture.data).unwrap();
    fs::write(fixture.data.join("session-mounts.json"), "{ not json").unwrap();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    match host.handshake(Some(Duration::from_secs(30))) {
        Err(HostError::Startup {
            code, repair_path, ..
        }) => {
            assert_eq!(code, "startup-failed");
            assert!(repair_path.is_some());
        }
        other => panic!("corrupt ledger was not refused: {other:?}"),
    }
    assert!(host.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(
        fixture.data.join("sessions").read_dir().unwrap().count(),
        0,
        "no session may be created when the ledger is corrupt"
    );
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn dead_owner_is_swept_and_session_resumes() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let first = fixture.start(&session, SessionMode::Create);
    handshake(&first);
    let first_pid = first.pid();
    #[cfg(unix)]
    unsafe {
        libc::kill(first_pid as libc::pid_t, libc::SIGKILL);
    }
    assert!(first.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(
        fixture.session_owners(&session),
        vec![first_pid],
        "a killed host leaves its record for the next claimant to sweep"
    );

    let second = fixture.start(&session, SessionMode::Resume);
    let hello = handshake(&second);
    assert_eq!(hello.session.mode, SessionFactMode::Resumed);
    assert_eq!(fixture.session_owners(&session), vec![second.pid()]);
    let outcome = second.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session).is_empty());
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn multiple_windows_and_two_sessions_in_one_checkout_are_isolated() {
    let fixture = Fixture::new();
    fixture.install();
    // A and C share one checkout (two sessions); B is a separate worktree.
    let work_a = fixture.worktree("wt-a");
    let work_b = fixture.worktree("wt-b");
    let options_a = fixture.options_for(&work_a);
    let options_b = fixture.options_for(&work_b);
    let session_a = generate_root_session_id();
    let session_b = generate_root_session_id();
    let session_c = generate_root_session_id();
    let first = fixture.start_with(&session_a, SessionMode::Create, &options_a);
    let second = fixture.start_with(&session_b, SessionMode::Create, &options_b);
    let third = fixture.start_with(&session_c, SessionMode::Create, &options_a);
    handshake(&first);
    handshake(&second);
    handshake(&third);

    assert_eq!(fixture.session_owners(&session_a), vec![first.pid()]);
    assert_eq!(fixture.session_owners(&session_b), vec![second.pid()]);
    assert_eq!(fixture.session_owners(&session_c), vec![third.pid()]);
    let storage = |session: &str| {
        fixture
            .data
            .join("runtime-data")
            .join(session)
            .join("storage")
    };
    assert!(storage(&session_a).is_dir());
    assert!(storage(&session_b).is_dir());
    assert!(storage(&session_c).is_dir());
    assert_ne!(storage(&session_a), storage(&session_b));
    assert_ne!(storage(&session_a), storage(&session_c));
    for session in [&session_a, &session_b, &session_c] {
        assert!(
            session_log_exists(&fixture.data, session),
            "shared sessions root is missing the log for {session}"
        );
    }
    assert_eq!(
        fixture.data.join("sessions").read_dir().unwrap().count(),
        2,
        "A and C share one checkout directory; B has its own worktree directory"
    );

    let outcome = first.shutdown();
    clean_report(&outcome);
    assert_eq!(second.lifecycle_state(), LifecycleState::Ready);
    assert_eq!(third.lifecycle_state(), LifecycleState::Ready);
    assert!(pid_alive(second.pid()) && pid_alive(third.pid()));
    assert!(fixture.session_owners(&session_a).is_empty());

    let outcome = second.shutdown();
    clean_report(&outcome);
    assert_eq!(third.lifecycle_state(), LifecycleState::Ready);
    let outcome = third.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session_b).is_empty());
    assert!(fixture.session_owners(&session_c).is_empty());
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn writer_held_is_reported_without_switching_sessions() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let first = fixture.start(&session, SessionMode::Create);
    handshake(&first);

    // Test-only: remove the Nano fence to exercise the official per-session
    // write lock, which is the second defense.
    fs::write(
        fixture.data.join("session-mounts.json"),
        "{\"version\":1,\"owners\":[]}",
    )
    .unwrap();

    let second = fixture.start(&session, SessionMode::Resume);
    match second.handshake(Some(Duration::from_secs(30))) {
        Err(HostError::Startup { code, message, .. }) => {
            // S07 maps the official writer lock to the same "occupied" startup
            // code as the Nano ledger; there is no holder pid in this error.
            assert_eq!(code, "occupied");
            assert!(
                message.contains("session/writer-held"),
                "resume failure did not name the official writer lock: {message}"
            );
        }
        other => panic!("writer-held session was not refused: {other:?}"),
    }
    assert!(second.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(first.lifecycle_state(), LifecycleState::Ready);
    let outcome = first.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn startup_signal_cancels_cleanly() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    #[cfg(unix)]
    unsafe {
        libc::kill(host.pid() as libc::pid_t, libc::SIGTERM);
    }
    assert!(
        host.wait_for_exit(Duration::from_secs(15)),
        "host did not exit after a startup signal"
    );
    assert!(
        fixture.session_owners(&session).is_empty(),
        "startup cancellation left a mount record"
    );
    let outcome = host.shutdown();
    assert!(outcome.process_exited);
    assert!(!outcome.clean);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn transport_eof_runs_host_cleanup() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    handshake(&host);
    host.close_transport();
    assert!(
        host.wait_for_exit(Duration::from_secs(15)),
        "host did not exit after protocol EOF"
    );
    assert!(
        fixture.session_owners(&session).is_empty(),
        "EOF cleanup did not release the ledger"
    );
    let outcome = host.shutdown();
    assert!(!outcome.ack);
    assert!(outcome.process_exited);
    assert!(!outcome.clean);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn direct_signals_enter_the_shutdown_path() {
    let fixture = Fixture::new();
    fixture.install();
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        let session = generate_root_session_id();
        let host = fixture.start(&session, SessionMode::Create);
        handshake(&host);
        #[cfg(unix)]
        unsafe {
            libc::kill(host.pid() as libc::pid_t, signal);
        }
        assert!(
            host.wait_for_exit(Duration::from_secs(15)),
            "host did not exit after signal {signal}"
        );
        assert!(
            fixture.session_owners(&session).is_empty(),
            "signal {signal} did not release the ledger"
        );
        let outcome = host.shutdown();
        assert!(outcome.process_exited);
        assert!(
            !outcome.clean,
            "a signalled host has no verified shutdown ACK"
        );
    }
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn persistence_failure_is_reported_without_lying() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.test_faults = Some(json!({ "failPersistence": true }));
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    handshake(&host);
    let outcome = host.shutdown();
    assert!(outcome.ack);
    assert!(outcome.process_exited);
    assert!(!outcome.forced);
    assert!(!outcome.clean);
    assert_eq!(outcome.session_saved, Some(false));
    assert_eq!(outcome.exit_code, Some(1));
    let report = outcome.report.as_ref().expect("report");
    assert!(report
        .steps
        .iter()
        .any(|step| step.name == "persistence" && step.status == StepStatus::Failed));
    assert!(report
        .steps
        .iter()
        .any(|step| step.name == "ledger" && step.status == StepStatus::Failed));
    assert_eq!(
        fixture.session_owners(&session),
        vec![host.pid()],
        "a failed cleanup must retain the claim for pid-based recovery"
    );
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn unknown_cleanup_step_is_reported_and_claim_retained() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.test_faults = Some(json!({ "unknownStep": "persistence" }));
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    handshake(&host);
    let outcome = host.shutdown();
    assert!(outcome.ack);
    assert!(outcome.process_exited);
    assert!(!outcome.clean);
    assert_eq!(outcome.session_saved, Some(false));
    assert_eq!(outcome.exit_code, Some(1));
    let report = outcome.report.as_ref().expect("report");
    assert!(report
        .steps
        .iter()
        .any(|step| step.name == "persistence" && step.status == StepStatus::Unknown));
    assert!(report
        .steps
        .iter()
        .any(|step| step.name == "ledger" && step.status == StepStatus::Failed));
    assert_eq!(
        fixture.session_owners(&session),
        vec![host.pid()],
        "an unknown cleanup step must retain the claim"
    );
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn shutdown_budget_is_bounded_when_cleanup_hangs() {
    if !slow_enabled() {
        eprintln!("skip shutdown_budget_is_bounded_when_cleanup_hangs: set NANO_E2E_SLOW=1");
        return;
    }
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.test_faults = Some(json!({ "hangShutdown": true }));
    let witness_session = generate_root_session_id();
    let witness = fixture.start(&witness_session, SessionMode::Create);
    handshake(&witness);
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    handshake(&host);
    let started = Instant::now();
    let outcome = host.shutdown();
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(9_500),
        "shutdown returned before the graceful budget: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(16),
        "shutdown exceeded the 10s + 3s budget: {elapsed:?}"
    );
    assert!(!outcome.ack);
    assert!(outcome.process_exited);
    assert!(outcome.forced);
    assert!(!outcome.clean);
    assert_eq!(
        fixture.session_owners(&session),
        vec![host.pid()],
        "a host killed after an unverified cleanup retains its claim"
    );
    assert_eq!(
        witness.lifecycle_state(),
        LifecycleState::Ready,
        "the host's process-group kill must not touch another dsh host"
    );
    assert!(pid_alive(witness.pid()));
    let witness_outcome = witness.shutdown();
    clean_report(&witness_outcome);
}

fn wait_terminal_notification(
    host: &Host,
    method: &str,
    mut predicate: impl FnMut(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for {method}");
        }
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::TerminalNotification {
                method: received,
                params,
            }) if received == method && predicate(&params) => return params,
            Ok(_) => continue,
            Err(error) => panic!("no terminal notification arrived: {error}"),
        }
    }
}

fn wait_output_contains(host: &Host, attachment_id: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut collected = String::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("terminal output did not contain {needle:?}; collected: {collected:?}");
        }
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::TerminalNotification { method, params })
                if method == "nano.terminal.output"
                    && params["attachmentId"].as_str() == Some(attachment_id) =>
            {
                if let Some(data) = params["data"].as_str() {
                    collected.push_str(data);
                }
                if collected.contains(needle) {
                    return collected;
                }
            }
            Ok(_) => continue,
            Err(error) => panic!("no terminal output arrived: {error}"),
        }
    }
}

fn directory_contains(path: &Path, needle: &str) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if directory_contains(&path, needle) {
                return true;
            }
        } else if let Ok(bytes) = fs::read(&path) {
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return true;
            }
        }
    }
    false
}

fn terminal_shell(shells: &serde_json::Value) -> String {
    let shells = shells["shells"].as_array().expect("shell array");
    assert!(
        !shells.is_empty(),
        "the execution environment must expose a shell"
    );
    let preferred = shells
        .iter()
        .find(|shell| matches!(shell["name"].as_str(), Some("zsh" | "bash")))
        .unwrap_or(&shells[0]);
    preferred["path"].as_str().expect("shell path").to_string()
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn terminal_create_input_resize_reattach_close() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    let hello = handshake(&host);
    assert_eq!(
        hello.capabilities["terminal"].status,
        CapabilityStatus::Supported
    );

    let environment = host
        .request("nano.terminal.environment", json!({}))
        .expect("terminal environment");
    assert!(environment["maxInputBytes"].as_u64().unwrap() >= 1024);
    let shells = host
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    let shell = terminal_shell(&shells);

    let terminal_id = format!("term-e2e-{}", generate_root_session_id());
    let create = host
        .request(
            "nano.terminal.create",
            json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
        )
        .expect("terminal create");
    assert_eq!(create["terminal"]["id"], terminal_id);
    assert_eq!(create["terminal"]["state"], "running");
    let create_again = host
        .request(
            "nano.terminal.create",
            json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
        )
        .expect("idempotent create");
    assert_eq!(create_again["terminal"]["id"], terminal_id);

    let hold = host
        .request(
            "nano.terminal.retain",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("retain")["holdId"]
        .as_str()
        .expect("hold id")
        .to_string();
    let attached = host
        .request(
            "nano.terminal.attach",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("attach");
    let attachment_id = attached["attachmentId"]
        .as_str()
        .expect("attachment id")
        .to_string();
    assert_eq!(attached["info"]["state"], "running");
    assert_eq!(attached["writable"], true);
    assert!(attached["snapshot"].is_string());

    let marker = format!("nano-terminal-ok-{}", std::process::id());
    host.notify(
        "nano.terminal.input",
        json!({
            "attachmentId": attachment_id,
            "sequence": 1,
            "bytes": format!("printf '{marker}\\n'\r"),
        }),
    )
    .expect("terminal input");
    let output = wait_output_contains(&host, &attachment_id, &marker);
    assert!(output.contains(&marker));

    host.notify(
        "nano.terminal.resize",
        json!({
            "attachmentId": attachment_id,
            "sequence": 2,
            "cols": 100,
            "rows": 30,
        }),
    )
    .expect("terminal resize");
    let state = wait_terminal_notification(&host, "nano.terminal.state", |params| {
        params["attachmentId"].as_str() == Some(attachment_id.as_str())
            && params["info"]["cols"].as_u64() == Some(100)
            && params["info"]["rows"].as_u64() == Some(30)
    });
    assert_eq!(state["writable"], true);

    host.request(
        "nano.terminal.detach",
        json!({ "attachmentId": attachment_id }),
    )
    .expect("detach");
    wait_terminal_notification(&host, "nano.terminal.ended", |params| {
        params["attachmentId"].as_str() == Some(attachment_id.as_str())
    });

    let reattached = host
        .request(
            "nano.terminal.attach",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("reattach");
    let screen = reattached["snapshot"].as_str().expect("screen");
    assert!(
        screen.contains(&marker),
        "reattached screen lost the prior output: {screen:?}"
    );
    assert_eq!(reattached["info"]["cols"].as_u64(), Some(100));

    // A real full-screen program drives the same snapshot/output path: vim
    // enters and leaves the alternate screen, so the parser must survive both.
    let reattached_id = reattached["attachmentId"].as_str().expect("reattached id");
    host.notify(
        "nano.terminal.input",
        json!({
            "attachmentId": reattached_id,
            "sequence": 1,
            "bytes": "vim -u NONE -N -n\r",
        }),
    )
    .expect("start vim");
    wait_output_contains(&host, reattached_id, "\u{1b}[?1049h");
    host.notify(
        "nano.terminal.input",
        json!({
            "attachmentId": reattached_id,
            "sequence": 2,
            "bytes": "\u{1b}:q!\r",
        }),
    )
    .expect("quit vim");
    wait_output_contains(&host, reattached_id, "\u{1b}[?1049l");

    // The provider may report one surviving pid while its grace-period reap is
    // still settling; a repeated close must succeed without reviving the id.
    let mut closed = None;
    for _ in 0..5 {
        match host.request("nano.terminal.close", json!({ "terminalId": terminal_id })) {
            Ok(value) => {
                closed = Some(value);
                break;
            }
            Err(HostError::Rpc(error)) if error.code == 1056 => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => panic!("unexpected close error: {error:?}"),
        }
    }
    assert!(closed.is_some(), "terminal close did not settle");
    let list = host.request("nano.terminal.list", json!({})).expect("list");
    assert!(list["terminals"].as_array().unwrap().is_empty());
    host.request("nano.terminal.release", json!({ "holdId": hold }))
        .expect("release after close");

    assert!(
        !directory_contains(&fixture.data.join("sessions"), &marker),
        "terminal output must not enter the session transcript"
    );
    let outcome = host.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn terminal_retain_protects_the_process_until_release() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.terminal_unattended_timeout_ms = Some(200);
    options.terminal_activity_poll_interval_ms = Some(50);
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    handshake(&host);

    let shells = host
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    let shell = terminal_shell(&shells);
    let terminal_id = format!("term-retain-{}", generate_root_session_id());
    host.request(
        "nano.terminal.create",
        json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
    )
    .expect("terminal create");
    let hold = host
        .request(
            "nano.terminal.retain",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("retain")["holdId"]
        .as_str()
        .expect("hold id")
        .to_string();

    std::thread::sleep(Duration::from_millis(900));
    let held = host.request("nano.terminal.list", json!({})).expect("list");
    assert_eq!(
        held["terminals"].as_array().unwrap().len(),
        1,
        "a retained terminal must survive the unattended timeout"
    );

    host.request("nano.terminal.release", json!({ "holdId": hold }))
        .expect("release");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let list = host.request("nano.terminal.list", json!({})).expect("list");
        if list["terminals"].as_array().unwrap().is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("released terminal was not reclaimed: {list}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let outcome = host.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn terminal_process_exit_disables_input_then_drains() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    handshake(&host);
    let shells = host
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    let shell = terminal_shell(&shells);
    let terminal_id = format!("term-exit-{}", generate_root_session_id());
    host.request(
        "nano.terminal.create",
        json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
    )
    .expect("terminal create");
    let hold = host
        .request(
            "nano.terminal.retain",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("retain")["holdId"]
        .as_str()
        .expect("hold id")
        .to_string();
    let attached = host
        .request(
            "nano.terminal.attach",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("attach");
    let attachment_id = attached["attachmentId"]
        .as_str()
        .expect("attachment id")
        .to_string();
    host.notify(
        "nano.terminal.input",
        json!({ "attachmentId": attachment_id, "sequence": 1, "bytes": "exit\r" }),
    )
    .expect("exit shell");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_exited_state = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("process exit was not reported; exited_state={saw_exited_state}");
        }
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::TerminalNotification { method, params })
                if params["attachmentId"].as_str() == Some(attachment_id.as_str()) =>
            {
                if method == "nano.terminal.state"
                    && params["info"]["state"].as_str() == Some("exited")
                    && params["writable"].as_bool() == Some(false)
                {
                    saw_exited_state = true;
                    break;
                }
            }
            Ok(_) => continue,
            Err(error) => panic!("no terminal notification arrived: {error}"),
        }
    }
    assert!(saw_exited_state, "the final state must disable input");

    // The official attachment stays open after process exit so the final screen
    // remains readable; close drains it and then ends the attachment.
    host.request("nano.terminal.close", json!({ "terminalId": terminal_id }))
        .expect("close exited terminal");
    let ended = wait_terminal_notification(&host, "nano.terminal.ended", |params| {
        params["attachmentId"].as_str() == Some(attachment_id.as_str())
    });
    assert_eq!(ended["reason"].as_str(), Some("closed"));
    host.request("nano.terminal.release", json!({ "holdId": hold }))
        .expect("release");
    let outcome = host.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn terminal_without_a_retain_hold_is_reclaimed() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let mut options = fixture.options.clone();
    options.terminal_unattended_timeout_ms = Some(200);
    options.terminal_activity_poll_interval_ms = Some(50);
    let host = fixture.start_with(&session, SessionMode::Create, &options);
    handshake(&host);
    let shells = host
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    let shell = terminal_shell(&shells);
    let terminal_id = format!("term-no-retain-{}", generate_root_session_id());
    host.request(
        "nano.terminal.create",
        json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
    )
    .expect("terminal create");
    let attached = host
        .request(
            "nano.terminal.attach",
            json!({ "sessionId": session, "terminalId": terminal_id }),
        )
        .expect("attach without retain");
    let attachment_id = attached["attachmentId"]
        .as_str()
        .expect("attachment id")
        .to_string();

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let list = host.request("nano.terminal.list", json!({})).expect("list");
        if list["terminals"].as_array().unwrap().is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("an unretained terminal outlived the unattended timeout: {list}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            panic!("reclaimed terminal never ended its attachment");
        }
        match host.next_event(Some(remaining)) {
            Ok(HostEvent::TerminalNotification { method, params }) => {
                if method == "nano.terminal.ended"
                    && params["attachmentId"].as_str() == Some(attachment_id.as_str())
                {
                    break;
                }
            }
            Ok(_) => continue,
            Err(error) => panic!("no terminal notification arrived: {error}"),
        }
    }
    let outcome = host.shutdown();
    clean_report(&outcome);
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn terminals_are_scoped_to_their_root_session_host() {
    let fixture = Fixture::new();
    fixture.install();
    let session_a = generate_root_session_id();
    let session_b = generate_root_session_id();
    let host_a = fixture.start(&session_a, SessionMode::Create);
    handshake(&host_a);
    let host_b = fixture.start(&session_b, SessionMode::Create);
    handshake(&host_b);
    let shells = host_a
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    let shell = terminal_shell(&shells);
    let terminal_id = format!("term-scope-{}", generate_root_session_id());
    host_a
        .request(
            "nano.terminal.create",
            json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
        )
        .expect("terminal create on A");
    let list_a = host_a
        .request("nano.terminal.list", json!({}))
        .expect("list A");
    assert_eq!(list_a["terminals"].as_array().unwrap().len(), 1);
    let list_b = host_b
        .request("nano.terminal.list", json!({}))
        .expect("list B");
    assert!(
        list_b["terminals"].as_array().unwrap().is_empty(),
        "a different root session must not see this host's terminal: {list_b}"
    );
    let mut closed = None;
    for _ in 0..5 {
        match host_a.request("nano.terminal.close", json!({ "terminalId": terminal_id })) {
            Ok(value) => {
                closed = Some(value);
                break;
            }
            Err(HostError::Rpc(error)) if error.code == 1056 => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => panic!("unexpected close error: {error:?}"),
        }
    }
    assert!(closed.is_some(), "terminal close did not settle");
    clean_report(&host_a.shutdown());
    clean_report(&host_b.shutdown());
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn nano_profile_has_no_user_terminal_agent_tool() {
    let fixture = Fixture::new();
    fixture.install();
    let dsh_home = fixture
        .options
        .dsh_home
        .clone()
        .expect("fixture sets DSH_HOME");
    let work = fixture.options.cwd.clone().expect("fixture sets cwd");
    let output = Command::new("dsh")
        .args(["--profile", "dsh-tui-nano", "--dump-config"])
        .current_dir(&work)
        .env("DSH_HOME", &dsh_home)
        .output()
        .expect("dsh --dump-config");
    assert!(
        output.status.success(),
        "dump-config failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("terminal-controller") && text.contains("dsh-api-terminal-controller"),
        "the official user-terminal controller must be composed"
    );
    assert!(
        !text.contains("dsh-tool-terminal"),
        "the model-facing Agent terminal tool must not be composed: {text}"
    );
    assert!(
        !text.contains("dsh-monitor"),
        "the Monitor delivery path must not be composed by S09: {text}"
    );
}

#[test]
#[ignore = "spawns a real dsh host; run with --ignored"]
fn host_shutdown_closes_a_live_terminal() {
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    handshake(&host);
    let shells = host
        .request("nano.terminal.shells", json!({}))
        .expect("terminal shells");
    // Use the plain bash profile here: the developer's zsh prompt can spawn
    // transient helper processes that outlive the test's grace window.
    let shell = shells["shells"]
        .as_array()
        .expect("shell array")
        .iter()
        .find(|shell| shell["name"].as_str() == Some("bash"))
        .and_then(|shell| shell["path"].as_str())
        .expect("bash is available in the test environment")
        .to_string();
    let terminal_id = format!("term-shutdown-{}", generate_root_session_id());
    host.request(
        "nano.terminal.create",
        json!({ "terminalId": terminal_id, "cols": 80, "rows": 24, "shellPath": shell }),
    )
    .expect("terminal create");
    host.request("nano.terminal.retain", json!({ "terminalId": terminal_id }))
        .expect("retain");
    host.request(
        "nano.terminal.attach",
        json!({ "sessionId": session, "terminalId": terminal_id }),
    )
    .expect("attach");
    let outcome = host.shutdown();
    clean_report(&outcome);
    assert!(
        outcome
            .report
            .as_ref()
            .expect("report")
            .steps
            .iter()
            .any(|step| step.name == "agents" && step.status == StepStatus::Stopped),
        "the shutdown agents step owns terminal process cleanup: {outcome:?}"
    );
}
