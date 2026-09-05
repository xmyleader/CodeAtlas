use std::{
    error::Error,
    io::{self, Write as _},
    time::Duration,
};

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use thiserror::Error;

use crate::{ApplicationPort, InputMode, TuiApp};

const INPUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const MAX_EVENTS_PER_FRAME: usize = 256;

/// Runs a [`TuiApp`] against an injected application port.
///
/// Terminal rendering is the only side effect performed here. Repository and
/// model work remain behind [`ApplicationPort`]. Raw mode, cursor visibility,
/// and the alternate screen are restored by a drop guard on every return path.
///
/// # Errors
///
/// Returns [`TuiError::Terminal`] for terminal I/O failures or
/// [`TuiError::ApplicationPort`] when the injected transport fails.
pub fn run_tui<P>(app: &mut TuiApp, port: &mut P) -> Result<(), TuiError>
where
    P: ApplicationPort,
{
    let guard = TerminalModeGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_event_loop(app, port, &mut terminal);
    drop(terminal);
    drop(guard);
    if let Some(report) = app.error_report() {
        eprintln!("\n{report}");
    }
    result
}

fn run_event_loop<P>(
    app: &mut TuiApp,
    port: &mut P,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<(), TuiError>
where
    P: ApplicationPort,
{
    loop {
        drain_application_events(app, port)?;
        terminal.draw(|frame| app.render(frame))?;
        if app.is_quit_requested() {
            return Ok(());
        }

        if event::poll(INPUT_POLL_INTERVAL)? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(command) = app.handle_key(key) {
                        port.send_command(command).map_err(port_error)?;
                    }
                    handle_clipboard_request(app);
                }
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Mouse(_) => {}
                Event::Paste(text) => {
                    if app.input_mode() != InputMode::Navigation {
                        for character in text.chars() {
                            let key = crossterm::event::KeyEvent::new(
                                crossterm::event::KeyCode::Char(character),
                                crossterm::event::KeyModifiers::NONE,
                            );
                            if let Some(command) = app.handle_key(key) {
                                port.send_command(command).map_err(port_error)?;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn handle_clipboard_request(app: &mut TuiApp) {
    let Some(report) = app.take_clipboard_request() else {
        return;
    };
    match copy_to_clipboard(&report) {
        Ok(()) => app.set_clipboard_status("Copied to clipboard (OSC 52)."),
        Err(error) => app.set_clipboard_status(format!("Clipboard copy failed: {error}.")),
    }
}

fn copy_to_clipboard(value: &str) -> io::Result<()> {
    let encoded = encode_base64(value.as_bytes());
    let mut output = io::stdout().lock();
    write!(output, "\x1b]52;c;{encoded}\x07")?;
    output.flush()
}

pub(crate) fn encode_base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut encoded = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        encoded.push(char::from(TABLE[usize::from(first >> 2)]));
        encoded.push(char::from(
            TABLE[usize::from(((first & 0b11) << 4) | (second >> 4))],
        ));
        if chunk.len() > 1 {
            encoded.push(char::from(
                TABLE[usize::from(((second & 0b1111) << 2) | (third >> 6))],
            ));
        } else {
            encoded.push('=');
        }
        if chunk.len() > 2 {
            encoded.push(char::from(TABLE[usize::from(third & 0b11_1111)]));
        } else {
            encoded.push('=');
        }
    }
    encoded
}

fn drain_application_events<P>(app: &mut TuiApp, port: &mut P) -> Result<(), TuiError>
where
    P: ApplicationPort,
{
    for _ in 0..MAX_EVENTS_PER_FRAME {
        let Some(event) = port.try_recv_event().map_err(port_error)? else {
            break;
        };
        app.reduce(event);
    }
    Ok(())
}

fn port_error(error: impl Error + Send + Sync + 'static) -> TuiError {
    TuiError::ApplicationPort(Box::new(error))
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("terminal I/O failed: {0}")]
    Terminal(#[from] io::Error),
    #[error("application port failed: {0}")]
    ApplicationPort(#[source] Box<dyn Error + Send + Sync>),
}

#[derive(Debug, Default)]
struct TerminalModeGuard {
    raw_mode: bool,
    alternate_screen: bool,
    cursor_hidden: bool,
}

impl TerminalModeGuard {
    fn enter() -> io::Result<Self> {
        let mut guard = Self::default();
        enable_raw_mode()?;
        guard.raw_mode = true;

        guard.alternate_screen = true;
        execute!(io::stdout(), EnterAlternateScreen)?;

        guard.cursor_hidden = true;
        execute!(io::stdout(), Hide)?;
        Ok(guard)
    }
}

impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if self.cursor_hidden {
            let _ = execute!(io::stdout(), Show);
        }
        if self.alternate_screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
        if self.raw_mode {
            let _ = disable_raw_mode();
        }
    }
}
