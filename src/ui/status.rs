//! The one-line status bar: current mode, key bindings and network status.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{AppState, Mode, Pane};

pub fn draw(frame: &mut Frame, app: &AppState, area: Rect) {
    let (mode, keys) = match app.mode {
        Mode::Normal => ("NORMAL", normal_keys(app.pane)),
        Mode::Compose => (
            "COMPOSE",
            "tab: Next field | ctrl+s: Send | esc: Cancel / Discard",
        ),
        Mode::Search => ("SEARCH", "enter: Execute | esc: Close"),
    };

    let bar = Line::from(vec![
        Span::styled(
            format!(" {mode} "),
            Style::new()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Black)
                .bg(Color::Cyan),
        ),
        Span::raw(" "),
        Span::styled(keys, Style::new().fg(Color::Gray)),
    ]);

    // Right-align the most recent network status message.
    let status_width = (app.status.chars().count() as u16 + 2).min(area.width / 2);
    let [bar_area, status_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(status_width)]).areas(area);

    frame.render_widget(Paragraph::new(bar), bar_area);
    frame.render_widget(
        Paragraph::new(Line::raw(format!("{} ", app.status)))
            .style(Style::new().fg(Color::DarkGray))
            .alignment(Alignment::Right),
        status_area,
    );
}

/// In normal mode the useful bindings depend on which pane has focus, and the
/// focused pane is also the cue that the reader scrolls rather than the lists.
fn normal_keys(pane: Pane) -> &'static str {
    match pane {
        Pane::Mailboxes => {
            "↑/↓: Folder | tab: Pane | enter: Open | c: Compose | /: Search | r: Refresh | q: Quit"
        }
        Pane::Messages => {
            "↑/↓: Message | tab: Pane | enter: Reload | m: Read/Unread | c: Compose | /: Search | r: Refresh | q: Quit"
        }
        Pane::Body => {
            "↑/↓: Line | pgup/pgdn: Page | g/G: Top/Bottom | m: Read/Unread | tab: Pane | c: Compose | q: Quit"
        }
    }
}
