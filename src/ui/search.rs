//! State 3: the centred search modal.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use ratatui::Frame;

use crate::app::AppState;

pub fn draw(frame: &mut Frame, app: &AppState, area: Rect) {
    let popup = super::centered(area, 60, 3);

    // `Clear` erases whatever the mailbox view already painted underneath.
    frame.render_widget(Clear, popup);

    let block = Block::bordered()
        .border_type(BorderType::Thick)
        .border_style(Style::new().fg(Color::Cyan))
        .title(" Search ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    frame.render_widget(Paragraph::new(app.search.text.as_str()), inner);

    // Park the terminal cursor right after the last typed character.
    let x = inner.x + app.search.cursor_col();
    frame.set_cursor_position(Position {
        x: x.min(inner.right().saturating_sub(1)),
        y: inner.y,
    });
}
