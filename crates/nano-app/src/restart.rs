//! Controlled main-session restart.
//!
//! S07 must stop the old host completely, let it release its mount claim, and
//! only then start a new generation. The old host is the thing that owns the
//! claim and the persistence flush, so the shutdown runs on this worker thread
//! while the UI stays responsive. A failed or unclean shutdown never spawns a
//! replacement; the window reports "no usable host" instead.

use std::sync::mpsc::Sender;
use std::thread;

use nano_runtime::{HostOptions, SessionMode};

use crate::link::{HostLink, LinkEvent};

/// Stop `link` and start a new host generation for the requested session.
///
/// The caller keeps the event receiver; the new link arrives as
/// `LinkEvent::Switched` and its handshake then reports `Ready` or
/// `StartupFailed`. Anything that prevents the new host from starting is
/// reported as `LinkEvent::SwitchFailed`, never as a connected session.
pub fn spawn_restart(
    link: Option<HostLink>,
    session_id: String,
    mode: SessionMode,
    options: HostOptions,
    events: Sender<LinkEvent>,
) {
    thread::spawn(move || {
        if let Some(link) = link {
            let outcome = link.shutdown();
            drop(link);
            if !outcome.clean {
                let _ = events.send(LinkEvent::SwitchFailed {
                    message: format!(
                        "旧宿主没有干净退出（ack={}, exited={}, forced={}）；未启动新宿主",
                        outcome.ack, outcome.process_exited, outcome.forced
                    ),
                    code: Some("cleanup-failed".to_string()),
                    holder_pid: None,
                });
                return;
            }
        }
        match HostLink::start(&session_id, mode, &options) {
            Ok(link) => {
                let _ = events.send(LinkEvent::Switched { link });
            }
            Err(message) => {
                let _ = events.send(LinkEvent::SwitchFailed {
                    message,
                    code: None,
                    holder_pid: None,
                });
            }
        }
    });
}
