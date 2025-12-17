//! Minimal TUI for query input.
//!
//! Renders a single-line input prompt similar to `gum input`.

use anyhow::Result;
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
use std::io::{self, Read, Write};

/// Result of the TUI interaction.
pub enum TuiResult {
    /// User submitted a query.
    Query(String),
    /// User cancelled (Escape).
    Cancelled,
}

/// Run the TUI and return the user's query.
pub fn run_tui(initial_query: Option<String>) -> Result<TuiResult> {
    // Like fzf, we use:
    // - stderr for TUI output (goes to terminal even in command substitution)
    // - stdin for keyboard input (redirected from /dev/tty by shell widget)
    // - stdout only for final command (captured by shell)

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
    let result = run_input_loop(&mut terminal, initial_query);

    // Restore terminal state
    disable_raw_mode()?;
    let _ = execute!(io::stderr(), LeaveAlternateScreen);

    result
}

/// The main input loop using direct stdin reading (avoids crossterm event system issues on macOS).
fn run_input_loop<W: Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    initial_query: Option<String>,
) -> Result<TuiResult> {
    let mut input_text = initial_query.unwrap_or_default();
    let mut stdin = io::stdin();
    let mut buf = [0u8; 1];

    loop {
        // Draw the UI
        terminal.draw(|frame| draw_ui(frame, &input_text))?;

        // Read single byte from stdin (in raw mode, we get one byte at a time)
        if stdin.read(&mut buf)? == 0 {
            return Ok(TuiResult::Cancelled);
        }

        match buf[0] {
            // Enter
            b'\r' | b'\n' => {
                if input_text.is_empty() {
                    return Ok(TuiResult::Cancelled);
                }
                return Ok(TuiResult::Query(input_text));
            }
            // Escape
            0x1b => {
                // Check if it's just Escape or an escape sequence
                // For simplicity, treat any escape as cancel
                return Ok(TuiResult::Cancelled);
            }
            // Ctrl+C
            0x03 => {
                return Ok(TuiResult::Cancelled);
            }
            // Backspace (ASCII DEL or BS)
            0x7f | 0x08 => {
                input_text.pop();
            }
            // Ctrl+U (clear line)
            0x15 => {
                input_text.clear();
            }
            // Ctrl+W (delete word)
            0x17 => {
                // Delete last word
                let trimmed = input_text.trim_end();
                if let Some(pos) = trimmed.rfind(' ') {
                    input_text.truncate(pos + 1);
                } else {
                    input_text.clear();
                }
            }
            // Regular printable ASCII
            c if c >= 0x20 && c < 0x7f => {
                input_text.push(c as char);
            }
            // UTF-8 multi-byte sequences
            c if c >= 0x80 => {
                // Determine how many bytes in this UTF-8 character
                let width = if c & 0xE0 == 0xC0 { 2 }
                    else if c & 0xF0 == 0xE0 { 3 }
                    else if c & 0xF8 == 0xF0 { 4 }
                    else { 1 };

                let mut utf8_buf = vec![c];
                for _ in 1..width {
                    let mut b = [0u8; 1];
                    if stdin.read(&mut b)? == 1 {
                        utf8_buf.push(b[0]);
                    }
                }
                if let Ok(s) = std::str::from_utf8(&utf8_buf) {
                    input_text.push_str(s);
                }
            }
            _ => {}
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
        .title(" llmcmd ")
        .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
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
    let input_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(visible_value, Style::default().fg(Color::White)),
    ]));

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
}
