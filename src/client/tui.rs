//! Minimal TUI for query input.
//!
//! Renders a single-line input prompt similar to `gum input`.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
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
use std::io::{self, Stdout};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

/// Result of the TUI interaction.
pub enum TuiResult {
    /// User submitted a query.
    Query(String),
    /// User cancelled (Escape).
    Cancelled,
}

/// Run the TUI and return the user's query.
pub fn run_tui(initial_query: Option<String>) -> Result<TuiResult> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the input loop
    let result = run_input_loop(&mut terminal, initial_query);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    result
}

/// The main input loop.
fn run_input_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    initial_query: Option<String>,
) -> Result<TuiResult> {
    let mut input = Input::default();

    // Set initial value if provided
    if let Some(query) = initial_query {
        input = input.with_value(query);
    }

    loop {
        // Draw the UI
        terminal.draw(|frame| draw_ui(frame, &input))?;

        // Handle events
        if let Event::Key(key) = event::read()? {
            // Only handle key press events (not release)
            if key.kind != KeyEventKind::Press {
                continue;
            }

            match key.code {
                KeyCode::Enter => {
                    let query = input.value().to_string();
                    if query.is_empty() {
                        return Ok(TuiResult::Cancelled);
                    }
                    return Ok(TuiResult::Query(query));
                }
                KeyCode::Esc => {
                    return Ok(TuiResult::Cancelled);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(TuiResult::Cancelled);
                }
                _ => {
                    // Handle other input events
                    input.handle_event(&Event::Key(key));
                }
            }
        }
    }
}

/// Draw the TUI.
fn draw_ui(frame: &mut Frame, input: &Input) {
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
    let value = input.value();
    let cursor_pos = input.visual_cursor();

    // Scroll the input if cursor is beyond visible area
    let scroll = if cursor_pos >= input_width {
        cursor_pos - input_width + 1
    } else {
        0
    };

    let visible_value: String = value.chars().skip(scroll).take(input_width).collect();

    // Create the input paragraph
    let input_paragraph = Paragraph::new(Line::from(vec![
        Span::styled(visible_value, Style::default().fg(Color::White)),
    ]));

    frame.render_widget(input_paragraph, inner_area);

    // Position the cursor
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
