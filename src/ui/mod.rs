//! All rendering. Nothing in here performs I/O; the only state it mutates is
//! the widget state ratatui needs (`ListState`, `TableState`, scroll offsets).

mod compose;
mod mailbox;
mod search;
mod status;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Block;
use ratatui::Frame;

use crate::app::{AppState, Mode};

pub fn draw(frame: &mut Frame, app: &mut AppState) {
    // Everything above a one-line status bar.
    let [body, status_bar] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());

    match app.mode {
        Mode::Normal => mailbox::draw(frame, app, body),
        Mode::Compose => compose::draw(frame, app, body),
        Mode::Search => {
            // The modal is drawn *over* the mailbox view.
            mailbox::draw(frame, app, body);
            search::draw(frame, app, body);
        }
    }

    status::draw(frame, app, status_bar);
}

/// A rectangle centred inside `area`, `percent_x`% wide and `height` rows tall.
fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    let [_top, middle, _bottom] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);

    let [_left, middle, _right] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Percentage(percent_x),
        Constraint::Fill(1),
    ])
    .areas(middle);

    middle
}

/// A bordered panel; the border turns yellow when it is the focused pane.
fn panel(title: String, focused: bool) -> Block<'static> {
    let block = Block::bordered().title(title);
    if focused {
        block.border_style(Style::new().fg(Color::Yellow))
    } else {
        block
    }
}

/// The colour for a header key in the reader and composer. The five envelope
/// keys are bold and tinted; `Date` is deliberately muted so it recedes; the
/// value next to a key always keeps the terminal's default colour.
fn header_label_style(label: &str) -> Style {
    match label {
        "From" => Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
        "To" => Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        "Cc" => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        "Bcc" => Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        "Subject" => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        "Date" => Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
        _ => Style::new(),
    }
}
