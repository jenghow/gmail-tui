//! State 2: the compose screen.

use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{AppState, ComposeField, LineInput};

/// Width of the `To:` / `Subject:` label column.
const LABEL_WIDTH: usize = 9;

pub fn draw(frame: &mut Frame, app: &AppState, area: Rect) {
    let block = Block::bordered().title(" Compose Email ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [to_row, subject_row, rule, body_row] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    draw_field(
        frame,
        to_row,
        "To:",
        &app.compose.to,
        app.compose.field == ComposeField::To,
    );
    draw_field(
        frame,
        subject_row,
        "Subject:",
        &app.compose.subject,
        app.compose.field == ComposeField::Subject,
    );

    // Separator between the envelope and the message body.
    frame.render_widget(Block::new().borders(Borders::BOTTOM), rule);

    // `&TextArea` implements `Widget` and paints its own cursor cell, so the
    // terminal cursor is left alone while the body has focus.
    frame.render_widget(&app.compose.body, body_row);
}

fn draw_field(frame: &mut Frame, area: Rect, label: &str, input: &LineInput, focused: bool) {
    let label_style = Style::new().add_modifier(Modifier::DIM);
    let value_style = if focused {
        Style::new()
    } else {
        Style::new().add_modifier(Modifier::DIM)
    };

    let line = Line::from(vec![
        Span::styled(format!("{label:<width$}", width = LABEL_WIDTH), label_style),
        Span::styled(input.text.as_str(), value_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    if focused {
        let x = area.x + LABEL_WIDTH as u16 + input.cursor_col();
        frame.set_cursor_position(Position {
            x: x.min(area.right().saturating_sub(1)),
            y: area.y,
        });
    }
}
