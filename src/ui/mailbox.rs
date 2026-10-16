//! State 1: the three-pane mailbox view.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Cell, HighlightSpacing, List, ListItem, Paragraph, Row, Table, Wrap,
};
use ratatui::Frame;

use crate::app::{AppState, Pane};
use crate::message::EmailBody;

pub fn draw(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(80)]).areas(area);
    let [headers, reader] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(right);

    draw_mailboxes(frame, app, left);
    draw_headers(frame, app, headers);
    draw_reader(frame, app, reader);
}

fn draw_mailboxes(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let items: Vec<ListItem> = app
        .mailboxes
        .iter()
        .map(|mailbox| ListItem::new(mailbox.label()))
        .collect();

    let list = List::new(items)
        .block(super::panel(
            " Mailboxes ".to_string(),
            app.pane == Pane::Mailboxes,
        ))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.mailbox_state);
}

fn draw_headers(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let name = if app.current_mailbox.is_empty() {
        "Mailbox"
    } else {
        app.current_mailbox.as_str()
    };
    let title = format!(" {name} ");

    // Unread messages are bold, like every other mail client.
    let rows: Vec<Row> = app
        .emails
        .iter()
        .map(|email| {
            let style = if email.seen {
                Style::new()
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            Row::new([
                Cell::from(email.date.clone()),
                Cell::from(email.from.clone()),
                Cell::from(email.subject.clone()),
            ])
            .style(style)
        })
        .collect();

    let header = Row::new(["Date", "Sender", "Subject"])
        .style(Style::new().add_modifier(Modifier::BOLD | Modifier::UNDERLINED));

    let widths = [
        Constraint::Length(10),
        Constraint::Length(24),
        Constraint::Min(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(super::panel(title, app.pane == Pane::Messages))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(table, area, &mut app.email_state);
}

fn draw_reader(frame: &mut Frame, app: &mut AppState, area: Rect) {
    let (title, text) = match &app.message {
        Some(message) => (
            format!(" Message: {} ", message.subject),
            message_lines(message),
        ),
        None => (
            " Message ".to_string(),
            Text::raw("No message selected.\n\nUse ↑/↓ to pick one and enter to (re)load it."),
        ),
    };

    let block = super::panel(title, app.pane == Pane::Body);
    let inner = block.inner(area);
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false });

    // `line_count` wraps at whatever width it is handed, then adds the block's
    // border rows back on top of the result. Measuring at the inner width and
    // subtracting those rows therefore leaves the number of *wrapped content*
    // lines — which is what `scroll` offsets into.
    let borders = area.height.saturating_sub(inner.height) as usize;
    let content = paragraph.line_count(inner.width).saturating_sub(borders);

    app.body_viewport = inner.height;
    app.body_max_scroll = content
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    // Clamp here as well as in the key handler: a resize, or a shorter message,
    // must never leave the reader scrolled past its own content.
    app.body_scroll = app.body_scroll.min(app.body_max_scroll);

    frame.render_widget(paragraph.scroll((app.body_scroll, 0)), area);
}

/// The reading pane's content: the header keys styled with [`super::header_label_style`],
/// a blank line, then the message body verbatim. The result is owned so the
/// reader can mutate `AppState` after building it.
fn message_lines(message: &EmailBody) -> Text<'static> {
    let mut lines: Vec<Line<'static>> = message
        .header_lines()
        .into_iter()
        .map(|(label, value)| {
            Line::from(vec![
                Span::styled(
                    format!("{label}: "),
                    super::header_label_style(label),
                ),
                Span::raw(value.to_string()),
            ])
        })
        .collect();

    lines.push(Line::default());
    if !message.body.is_empty() {
        lines.extend(Text::raw(message.body.clone()).lines);
    }

    Text::from(lines)
}
