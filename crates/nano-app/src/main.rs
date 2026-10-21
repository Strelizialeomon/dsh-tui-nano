//! Nano TUI entry point: terminal lifecycle, host startup, and the input loop.

mod app;
mod cli;
mod link;
mod ui;

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event};
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
            match event::read().map_err(|error| format!("read: {error}"))? {
                Event::Key(key) => {
                    dirty |= app.handle_key(key);
                }
                Event::Paste(text) => {
                    dirty |= app.handle_paste(text);
                }
                Event::Resize(_, _) => {
                    dirty = true;
                }
                _ => {}
            }
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

/// Raw mode, bracketed paste, and (optionally) the alternate screen.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    alt_screen: bool,
}

impl TerminalGuard {
    fn new(alt_screen: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnableBracketedPaste)?;
        if alt_screen {
            execute!(stdout, EnterAlternateScreen)?;
        }
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal,
            alt_screen,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste);
        if self.alt_screen {
            let _ = execute!(stdout, LeaveAlternateScreen);
        }
    }
}
