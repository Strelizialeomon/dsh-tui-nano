//! Real-host integration tests for S01.
//!
//! These tests spawn the installed `dsh` launcher with the Nano profile and
//! talk over fd 3/4 exactly as the TUI does. They are gated by `NANO_E2E=1`
//! because they run pnpm and real host processes. The slow termination-budget
//! test additionally needs `NANO_E2E_SLOW=1`.

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
};

fn e2e_enabled() -> bool {
    std::env::var("NANO_E2E").ok().as_deref() == Some("1")
}

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
            Ok(HostEvent::StreamEnded(params)) if params.subscription_id == subscription_id => return,
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
    entries.flatten().any(|entry| entry.path().join(session_id).is_dir())
}

fn listening_sockets(pid: u32) -> Vec<String> {
    let output = Command::new("lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-iTCP", "-sTCP:LISTEN"])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1)
            .map(str::to_string)
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn clean_report(outcome: &ShutdownOutcome) {
    assert!(outcome.ack, "host did not acknowledge shutdown");
    assert!(outcome.process_exited, "host process did not exit");
    assert!(!outcome.forced, "shutdown needed TERM/KILL");
    assert!(outcome.clean, "shutdown report is not clean: {outcome:?}");
    let report = outcome.report.as_ref().expect("shutdown report");
    assert!(
        report.steps.iter().all(|step| step.status == StepStatus::Stopped),
        "non-stopped shutdown step: {report:?}"
    );
    assert_eq!(outcome.session_saved, Some(true));
    assert_eq!(outcome.scope_cleared, Some(true));
}

#[test]
fn handshake_subscribe_shutdown_roundtrip() {
    if !e2e_enabled() {
        eprintln!("skip handshake_subscribe_shutdown_roundtrip: set NANO_E2E=1");
        return;
    }
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let host = fixture.start(&session, SessionMode::Create);
    let hello = handshake(&host);

    assert_eq!(hello.root_session_id, session);
    assert_eq!(hello.session.mode, SessionFactMode::Created);
    assert!(hello.session.seq >= 3, "session header events were expected");
    assert_eq!(hello.capabilities["agent"].status, CapabilityStatus::Supported);
    assert_eq!(
        hello.capabilities["tool-ask-user"].mode.as_deref(),
        Some("blocking")
    );
    assert_eq!(
        hello.capabilities["networkListener"].status,
        CapabilityStatus::Unsupported
    );
    assert_eq!(hello.capabilities["schedule"].status, CapabilityStatus::Unsupported);

    let owners = fixture.session_owners(&session);
    assert_eq!(owners, vec![host.pid()]);
    assert!(pid_alive(host.pid()));
    assert!(
        listening_sockets(host.pid()).is_empty(),
        "the Nano host must not open a TCP listener"
    );

    let cancelled = host.cancel_request("req-does-not-exist").expect("cancel request");
    assert!(!cancelled.cancelled);
    match host.request("nano.terminal.attach", json!({ "sessionId": session })) {
        Err(HostError::Rpc(error)) => assert_eq!(error.code, 1008),
        other => panic!("reserved method did not report unsupported: {other:?}"),
    }
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

    host.unsubscribe(&subscription.subscription_id).expect("unsubscribe");
    wait_for_stream_ended(&host, &subscription.subscription_id);

    let outcome = host.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session).is_empty(), "ledger record was not released");
    assert!(
        fixture.data.join("sessions").read_dir().unwrap().count() > 0,
        "session log was not created"
    );
    assert!(
        fixture
            .data
            .join("runtime-data")
            .join(&session)
            .join("storage")
            .is_dir(),
        "per-root storage directory is missing"
    );
    let runtime = fixture.paths();
    assert!(!runtime.data_root.join("runtime").read_dir().unwrap().any(|entry| {
        entry
            .map(|entry| entry.file_name().to_string_lossy().starts_with("generation-"))
            .unwrap_or(false)
    }));
}

#[test]
fn second_host_same_session_is_occupied() {
    if !e2e_enabled() {
        eprintln!("skip second_host_same_session_is_occupied: set NANO_E2E=1");
        return;
    }
    let fixture = Fixture::new();
    fixture.install();
    let session = generate_root_session_id();
    let first = fixture.start(&session, SessionMode::Create);
    handshake(&first);

    let second = fixture.start(&session, SessionMode::Create);
    match second.handshake(Some(Duration::from_secs(30))) {
        Err(HostError::Startup { code, holder_pid, .. }) => {
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
fn simultaneous_hosts_for_one_session_leave_exactly_one_winner() {
    if !e2e_enabled() {
        eprintln!("skip simultaneous_hosts_for_one_session_leave_exactly_one_winner: set NANO_E2E=1");
        return;
    }
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
        (first, second) => panic!("expected one winner and one occupied host, got {first:?} / {second:?}"),
    };
    assert!(loser.wait_for_exit(Duration::from_secs(10)));
    assert_eq!(fixture.session_owners(&session), vec![winner.pid()]);
    let outcome = winner.shutdown();
    clean_report(&outcome);
    assert!(fixture.session_owners(&session).is_empty());
}

#[test]
fn reused_pid_causes_false_occupancy_without_double_write() {
    if !e2e_enabled() {
        eprintln!("skip reused_pid_causes_false_occupancy_without_double_write: set NANO_E2E=1");
        return;
    }
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
        Err(HostError::Startup { code, holder_pid, .. }) => {
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
fn corrupt_ledger_refuses_startup() {
    if !e2e_enabled() {
        eprintln!("skip corrupt_ledger_refuses_startup: set NANO_E2E=1");
        return;
    }
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
fn dead_owner_is_swept_and_session_resumes() {
    if !e2e_enabled() {
        eprintln!("skip dead_owner_is_swept_and_session_resumes: set NANO_E2E=1");
        return;
    }
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
fn multiple_windows_and_two_sessions_in_one_checkout_are_isolated() {
    if !e2e_enabled() {
        eprintln!("skip multiple_windows_and_two_sessions_in_one_checkout_are_isolated: set NANO_E2E=1");
        return;
    }
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
    let storage = |session: &str| fixture.data.join("runtime-data").join(session).join("storage");
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
fn writer_held_is_reported_without_switching_sessions() {
    if !e2e_enabled() {
        eprintln!("skip writer_held_is_reported_without_switching_sessions: set NANO_E2E=1");
        return;
    }
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
            assert_eq!(code, "startup-failed");
            let lower = message.to_lowercase();
            assert!(
                lower.contains("writer") || lower.contains("held") || lower.contains("own"),
                "resume failure did not name the writer lock: {message}"
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
fn startup_signal_cancels_cleanly() {
    if !e2e_enabled() {
        eprintln!("skip startup_signal_cancels_cleanly: set NANO_E2E=1");
        return;
    }
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
fn transport_eof_runs_host_cleanup() {
    if !e2e_enabled() {
        eprintln!("skip transport_eof_runs_host_cleanup: set NANO_E2E=1");
        return;
    }
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
fn direct_signals_enter_the_shutdown_path() {
    if !e2e_enabled() {
        eprintln!("skip direct_signals_enter_the_shutdown_path: set NANO_E2E=1");
        return;
    }
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
        assert!(!outcome.clean, "a signalled host has no verified shutdown ACK");
    }
}

#[test]
fn persistence_failure_is_reported_without_lying() {
    if !e2e_enabled() {
        eprintln!("skip persistence_failure_is_reported_without_lying: set NANO_E2E=1");
        return;
    }
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
    assert!(fixture.session_owners(&session).is_empty());
}

#[test]
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
    let mut sibling = Command::new("sleep").arg("30").spawn().expect("sibling sleep");
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
        elapsed < Duration::from_secs(20),
        "shutdown exceeded the 10s + 3s budget: {elapsed:?}"
    );
    assert!(!outcome.ack);
    assert!(outcome.process_exited);
    assert!(outcome.forced);
    assert!(!outcome.clean);
    assert!(
        fixture.session_owners(&session).is_empty(),
        "the SIGTERM tree disposal must still release the ledger through its Cordis effect"
    );
    assert!(
        sibling.try_wait().expect("sibling status").is_none(),
        "the host's process-group kill must not touch an unrelated process"
    );
    let _ = sibling.kill();
    let _ = sibling.wait();
}
