//! Minimal TUI for query input.
//!
//! Renders a single-line input prompt similar to `gum input`.

#[cfg(not(windows))]
use anyhow::Context as _;
use anyhow::Result;
#[cfg(windows)]
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};
#[cfg(not(windows))]
use std::io::Read;
use std::io::{self, Write};

#[derive(Debug, Eq, PartialEq)]
pub enum TuiResult {
    /// User submitted a query.
    Query(String),
    /// User cancelled (Escape).
    Cancelled,
}

/// Run the TUI and return the user's query.
pub fn run_tui() -> Result<TuiResult> {
    // Like fzf, we use:
    // - stderr for TUI output (goes to terminal even in command substitution)
    // - the controlling terminal (/dev/tty) for keyboard input, so a piped
    //   or closed stdin can never silently cancel the prompt
    // - stdout only for final command (captured by shell)

    // Grab the terminal before touching raw mode: failing here must not
    // leave the terminal in a broken state, and gives the clearest error.
    #[cfg(not(windows))]
    let mut tty = std::fs::File::open("/dev/tty").context(
        "interactive mode needs a terminal (cannot open /dev/tty); \
         pass the query as an argument or use --pipe",
    )?;

    // Setup terminal - write TUI to stderr (like fzf does)
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    if let Err(e) = execute!(stderr, EnterAlternateScreen) {
        disable_raw_mode()?;
        return Err(anyhow::anyhow!("Failed to enter alternate screen: {}", e));
    }

    let backend = CrosstermBackend::new(io::stderr());
    let mut terminal = Terminal::new(backend)?;

    // Run the input loop
    #[cfg(not(windows))]
    let result = run_input_loop(&mut terminal, &mut tty);
    #[cfg(windows)]
    let result = run_input_loop(&mut terminal);

    // Restore terminal state
    disable_raw_mode()?;
    let _ = execute!(io::stderr(), LeaveAlternateScreen);

    result
}

#[derive(Debug, Eq, PartialEq)]
enum InputCommand {
    Character(char),
    Submit,
    Cancel,
    Backspace,
    ClearLine,
    DeleteWord,
    Ignore,
}

fn apply_input_command(input_text: &mut String, command: InputCommand) -> Option<TuiResult> {
    match command {
        InputCommand::Character(character) => input_text.push(character),
        InputCommand::Submit if input_text.is_empty() => return Some(TuiResult::Cancelled),
        InputCommand::Submit => return Some(TuiResult::Query(std::mem::take(input_text))),
        InputCommand::Cancel => return Some(TuiResult::Cancelled),
        InputCommand::Backspace => {
            input_text.pop();
        }
        InputCommand::ClearLine => input_text.clear(),
        InputCommand::DeleteWord => {
            let trimmed = input_text.trim_end();
            if let Some(position) = trimmed.rfind(' ') {
                input_text.truncate(position + 1);
            } else {
                input_text.clear();
            }
        }
        InputCommand::Ignore => {}
    }

    None
}

/// The Unix input loop reads the controlling terminal directly to avoid
/// crossterm event-system issues on macOS.
#[cfg(not(windows))]
fn run_input_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    tty: &mut impl Read,
) -> Result<TuiResult> {
    let mut input_text = String::new();
    let mut buf = [0u8; 1];

    loop {
        terminal.draw(|frame| draw_ui(frame, &input_text))?;

        if tty.read(&mut buf)? == 0 {
            // EOF on the controlling terminal means the terminal went away,
            // not that the user cancelled. Fail loudly.
            return Err(anyhow::anyhow!("terminal closed while awaiting input"));
        }

        let command = match buf[0] {
            b'\r' | b'\n' => InputCommand::Submit,
            0x1b | 0x03 => InputCommand::Cancel,
            0x7f | 0x08 => InputCommand::Backspace,
            0x15 => InputCommand::ClearLine,
            0x17 => InputCommand::DeleteWord,
            byte if (0x20..0x7f).contains(&byte) => InputCommand::Character(byte as char),
            first_byte if first_byte >= 0x80 => {
                let width = if first_byte & 0xE0 == 0xC0 {
                    2
                } else if first_byte & 0xF0 == 0xE0 {
                    3
                } else if first_byte & 0xF8 == 0xF0 {
                    4
                } else {
                    1
                };

                let mut utf8_buf = vec![first_byte];
                for _ in 1..width {
                    let mut byte = [0u8; 1];
                    if tty.read(&mut byte)? == 1 {
                        utf8_buf.push(byte[0]);
                    }
                }
                match std::str::from_utf8(&utf8_buf) {
                    Ok(character) => {
                        for character in character.chars() {
                            if let Some(result) = apply_input_command(
                                &mut input_text,
                                InputCommand::Character(character),
                            ) {
                                return Ok(result);
                            }
                        }
                        InputCommand::Ignore
                    }
                    Err(_) => InputCommand::Ignore,
                }
            }
            _ => InputCommand::Ignore,
        };

        if let Some(result) = apply_input_command(&mut input_text, command) {
            return Ok(result);
        }
    }
}

#[cfg(windows)]
fn run_input_loop<W: Write>(terminal: &mut Terminal<CrosstermBackend<W>>) -> Result<TuiResult> {
    let mut input_text = String::new();

    loop {
        terminal.draw(|frame| draw_ui(frame, &input_text))?;

        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }

        let command = if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c' | 'C') => InputCommand::Cancel,
                KeyCode::Char('u' | 'U') => InputCommand::ClearLine,
                KeyCode::Char('w' | 'W') => InputCommand::DeleteWord,
                _ => InputCommand::Ignore,
            }
        } else {
            match key.code {
                KeyCode::Char(character) => InputCommand::Character(character),
                KeyCode::Enter => InputCommand::Submit,
                KeyCode::Esc => InputCommand::Cancel,
                KeyCode::Backspace => InputCommand::Backspace,
                _ => InputCommand::Ignore,
            }
        };

        if let Some(result) = apply_input_command(&mut input_text, command) {
            return Ok(result);
        }
    }
}

/// Draw the TUI.
fn draw_ui(frame: &mut Frame, input_text: &str) {
    let size = frame.area();

    // Create a centered popup
    let popup_width = size.width.saturating_sub(4).min(80);
    let popup_height = 3;
    let popup_area = centered_rect(popup_width, popup_height, size);

    // Clear the popup area
    frame.render_widget(Clear, popup_area);

    // Create the input block
    let block = Block::default()
        .title(" incant ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    // Inner area for the input
    let inner_area = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Calculate visible portion of input
    let input_width = inner_area.width as usize;
    let cursor_pos = input_text.chars().count();

    // Scroll the input if cursor is beyond visible area
    let scroll = if cursor_pos >= input_width {
        cursor_pos - input_width + 1
    } else {
        0
    };

    let visible_value: String = input_text.chars().skip(scroll).take(input_width).collect();

    // Create the input paragraph
    let input_paragraph = Paragraph::new(Line::from(vec![Span::styled(
        visible_value,
        Style::default().fg(Color::White),
    )]));

    frame.render_widget(input_paragraph, inner_area);

    // Position the cursor at end of text
    let cursor_x = inner_area.x + (cursor_pos - scroll) as u16;
    let cursor_y = inner_area.y;
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Create a centered rectangle.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length((area.height.saturating_sub(height)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length((area.width.saturating_sub(width)) / 2),
            Constraint::Length(width),
            Constraint::Min(0),
        ])
        .split(vertical[1]);

    horizontal[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect() {
        let area = Rect::new(0, 0, 100, 50);
        let centered = centered_rect(40, 10, area);
        assert_eq!(centered.width, 40);
        assert_eq!(centered.height, 10);
        assert_eq!(centered.x, 30); // (100 - 40) / 2
        assert_eq!(centered.y, 20); // (50 - 10) / 2
    }

    #[test]
    fn input_commands_support_unicode_and_editing() {
        let mut input = String::from("show rust 🦀  ");

        assert_eq!(
            apply_input_command(&mut input, InputCommand::DeleteWord),
            None
        );
        assert_eq!(input, "show rust ");
        assert_eq!(
            apply_input_command(&mut input, InputCommand::Backspace),
            None
        );
        assert_eq!(
            apply_input_command(&mut input, InputCommand::Character('界')),
            None
        );
        assert_eq!(input, "show rust界");
        assert_eq!(
            apply_input_command(&mut input, InputCommand::ClearLine),
            None
        );
        assert!(input.is_empty());
    }

    #[test]
    fn input_commands_submit_or_cancel() {
        let mut input = String::from("list files");
        assert_eq!(
            apply_input_command(&mut input, InputCommand::Submit),
            Some(TuiResult::Query(String::from("list files")))
        );
        assert!(input.is_empty());

        assert_eq!(
            apply_input_command(&mut input, InputCommand::Submit),
            Some(TuiResult::Cancelled)
        );
        assert_eq!(
            apply_input_command(&mut input, InputCommand::Cancel),
            Some(TuiResult::Cancelled)
        );
    }
}
