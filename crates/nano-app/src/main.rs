//! Nano TUI entry point: terminal lifecycle, host startup, and the input loop.

mod app;
mod cli;
mod link;
mod restart;
mod ui;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use cli::Cli;
use link::HostLink;

fn main() {
    if let Err(error) = run() {
        eprintln!("nano: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let cli = Cli::parse(std::env::args().skip(1))?;
    if cli.help {
        print!("{}", cli::HELP);
        return Ok(());
    }
    let options = cli.host_options();
    cli.ensure_profile(&options)?;
    let (root_session_id, mode) = cli.session();
    let mut app = App::new(cli.clone(), root_session_id.clone(), false);
    let link = HostLink::start(&root_session_id, mode, &options)?;
    app.attach_link(link);

    let mut terminal =
        TerminalGuard::new(!cli.no_alt_screen).map_err(|error| format!("terminal: {error}"))?;
    let mut dirty = true;
    loop {
        let mut link_events = Vec::new();
        if let Some(link) = &app.link {
            while let Ok(event) = link.events.try_recv() {
                link_events.push(event);
            }
        }
        for event in link_events {
            dirty |= app.handle_link(event);
        }
        dirty |= app.tick();
        // Drain every already-queued terminal event first, then draw once.
        // A wheel burst therefore causes one layout pass instead of one per
        // event; the event loop's command-driven scroll stays coalesced.
        while event::poll(Duration::ZERO).map_err(|error| format!("poll: {error}"))? {
            let terminal_event = event::read().map_err(|error| format!("read: {error}"))?;
            dirty |= handle_terminal_event(&mut app, terminal_event);
        }
        if dirty {
            terminal
                .terminal
                .draw(|frame| ui::draw(frame, &mut app))
                .map_err(|error| format!("draw: {error}"))?;
            dirty = false;
        }
        if app.should_quit {
            break;
        }
        if event::poll(Duration::from_millis(16)).map_err(|error| format!("poll: {error}"))? {
            let terminal_event = event::read().map_err(|error| format!("read: {error}"))?;
            dirty |= handle_terminal_event(&mut app, terminal_event);
        }
    }
    drop(terminal);

    if let Some(link) = app.link.take() {
        let outcome = link.shutdown();
        if !outcome.clean {
            eprintln!(
                "nano: shutdown was not clean (ack={}, exited={}, forced={})",
                outcome.ack, outcome.process_exited, outcome.forced
            );
        }
    }
    Ok(())
}

/// Apply one terminal event. Returns whether the screen needs a redraw.
fn handle_terminal_event(app: &mut App, event: Event) -> bool {
    match event {
        Event::Key(key) => app.handle_key(key),
        Event::Paste(text) => app.handle_paste(text),
        Event::Mouse(mouse) => app.handle_mouse(mouse),
        Event::Resize(_, _) => true,
        _ => false,
    }
}

/// Raw mode, bracketed paste, and (optionally) the alternate screen.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    alt_screen: bool,
    enhanced_keyboard: bool,
}

impl TerminalGuard {
    fn new(alt_screen: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        // Any failure after raw mode must restore it; the guard is not
        // constructed yet, so its Drop cannot run.
        let mut enhanced_keyboard = false;
        let result = (|| -> io::Result<Self> {
            let mut stdout = io::stdout();
            execute!(stdout, EnableBracketedPaste)?;
            execute!(stdout, EnableMouseCapture)?;
            if alt_screen {
                execute!(stdout, EnterAlternateScreen)?;
            }
            // Ask for disambiguated keys where supported. Unsupported terminals
            // ignore the sequence; Ctrl+J remains the reliable newline key. Do
            // not probe with supports_keyboard_enhancement(), which blocks up
            // to two seconds when the terminal does not answer.
            if execute!(
                stdout,
                PushKeyboardEnhancementFlags(
                    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                )
            )
            .is_ok()
            {
                enhanced_keyboard = true;
            }
            let backend = CrosstermBackend::new(io::stdout());
            let terminal = Terminal::new(backend)?;
            Ok(Self {
                terminal,
                alt_screen,
                enhanced_keyboard,
            })
        })();
        match result {
            Ok(guard) => Ok(guard),
            Err(error) => {
                let mut stdout = io::stdout();
                if enhanced_keyboard {
                    let _ = execute!(stdout, PopKeyboardEnhancementFlags);
                }
                let _ = disable_raw_mode();
                let _ = execute!(stdout, DisableMouseCapture);
                let _ = execute!(stdout, DisableBracketedPaste);
                if alt_screen {
                    let _ = execute!(stdout, LeaveAlternateScreen);
                }
                Err(error)
            }
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.enhanced_keyboard {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = disable_raw_mode();
        let _ = execute!(stdout, DisableMouseCapture);
        let _ = execute!(stdout, DisableBracketedPaste);
        if self.alt_screen {
            let _ = execute!(stdout, LeaveAlternateScreen);
        }
    }
}
