//! Application state and the key handling that mutates it.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{ListState, TableState};
use ratatui_textarea::TextArea;
use tokio::sync::mpsc;

use crate::event::AppEvent;
use crate::message::{EmailBody, EmailSummary, Mailbox};
use crate::net::{NetCmd, NetMsg};

/// Which screen is on top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Compose,
    Search,
}

/// Which pane the arrow keys drive while in [`Mode::Normal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Mailboxes,
    Messages,
    Body,
}

/// Which compose field receives typed characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeField {
    To,
    Cc,
    Bcc,
    Subject,
    Body,
}

/// A single-line, cursor-aware text field (`To:`, `Subject:`, search box).
#[derive(Debug, Default, Clone)]
pub struct LineInput {
    pub text: String,
    /// Byte offset of the cursor inside `text` (always a char boundary).
    pub cursor: usize,
}

impl LineInput {
    /// Apply a key event. Returns `true` when the key was consumed.
    pub fn handle(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = self.prev_char_len();
                    self.cursor -= prev;
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(self.prev_char_len()),
            KeyCode::Right => {
                if self.cursor < self.text.len() {
                    self.cursor += self.next_char_len();
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.text.len(),
            _ => return false,
        }
        true
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Cursor position in *characters*, for terminal cursor placement.
    pub fn cursor_col(&self) -> u16 {
        self.text[..self.cursor].chars().count().min(u16::MAX as usize) as u16
    }

    fn prev_char_len(&self) -> usize {
        self.text[..self.cursor]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8)
    }

    fn next_char_len(&self) -> usize {
        self.text[self.cursor..].chars().next().map_or(0, char::len_utf8)
    }
}

/// State for the compose screen.
pub struct ComposeState {
    pub to: LineInput,
    pub cc: LineInput,
    pub bcc: LineInput,
    pub subject: LineInput,
    pub body: TextArea<'static>,
    pub field: ComposeField,
}

impl ComposeState {
    fn new() -> Self {
        let mut body = TextArea::default();
        // tui-textarea / ratatui-textarea underlines the cursor line by
        // default; drop it, the reader pane look is nicer without it.
        body.set_cursor_line_style(Style::default());
        body.set_cursor_style(Style::new().add_modifier(Modifier::REVERSED));

        Self {
            to: LineInput::default(),
            cc: LineInput::default(),
            bcc: LineInput::default(),
            subject: LineInput::default(),
            body,
            field: ComposeField::To,
        }
    }

    fn is_empty(&self) -> bool {
        self.to.is_blank()
            && self.cc.is_blank()
            && self.bcc.is_blank()
            && self.subject.is_blank()
            && self.body.is_empty()
    }

    fn reset(&mut self) {
        self.to.clear();
        self.cc.clear();
        self.bcc.clear();
        self.subject.clear();
        self.body.clear();
        self.field = ComposeField::To;
    }

    fn next_field(&mut self) {
        self.field = match self.field {
            ComposeField::To => ComposeField::Cc,
            ComposeField::Cc => ComposeField::Bcc,
            ComposeField::Bcc => ComposeField::Subject,
            ComposeField::Subject => ComposeField::Body,
            ComposeField::Body => ComposeField::To,
        };
    }

    fn prev_field(&mut self) {
        self.field = match self.field {
            ComposeField::To => ComposeField::Body,
            ComposeField::Cc => ComposeField::To,
            ComposeField::Bcc => ComposeField::Cc,
            ComposeField::Subject => ComposeField::Bcc,
            ComposeField::Body => ComposeField::Subject,
        };
    }

    /// The textarea contents as one string.
    fn text(&self) -> String {
        self.body.lines().join("\n")
    }
}

pub struct AppState {
    pub should_quit: bool,
    pub mode: Mode,
    pub pane: Pane,
    pub status: String,

    /// True while an SMTP submit is in flight. The draft is kept until the
    /// server answers, so a second send — or a new draft that would overwrite
    /// it — has to wait.
    sending: bool,

    /// Channel to the IMAP task. The UI can only ever *send* commands.
    net_tx: mpsc::Sender<NetCmd>,

    pub mailboxes: Vec<Mailbox>,
    pub mailbox_state: ListState,

    /// IMAP name of the folder currently listed.
    pub current_mailbox: String,
    pub emails: Vec<EmailSummary>,
    pub email_state: TableState,

    pub message: Option<EmailBody>,
    pub body_scroll: u16,

    /// Reader rows visible inside its border, and the largest meaningful
    /// `body_scroll`. The reader rewrites both on every frame, because only it
    /// knows how the text wrapped.
    pub body_viewport: u16,
    pub body_max_scroll: u16,

    pub search: LineInput,
    pub compose: ComposeState,
}

impl AppState {
    pub fn new(net_tx: mpsc::Sender<NetCmd>) -> Self {
        Self {
            should_quit: false,
            mode: Mode::Normal,
            pane: Pane::Mailboxes,
            status: "starting…".to_string(),
            sending: false,
            net_tx,
            mailboxes: Vec::new(),
            mailbox_state: ListState::default(),
            current_mailbox: "INBOX".to_string(),
            emails: Vec::new(),
            email_state: TableState::default(),
            message: None,
            body_scroll: 0,
            body_viewport: 0,
            body_max_scroll: 0,
            search: LineInput::default(),
            compose: ComposeState::new(),
        }
    }

    // --- event entry points ------------------------------------------------

    pub fn on_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Tick => {}
            AppEvent::Term(Event::Key(key)) => {
                // Windows terminals report both press and release.
                if key.kind == KeyEventKind::Press {
                    self.on_key(key);
                }
            }
            AppEvent::Term(_) => {}
        }
    }

    /// Apply an update pushed by the IMAP task.
    pub fn on_net(&mut self, msg: NetMsg) {
        match msg {
            NetMsg::Mailboxes(mailboxes) => {
                let first_load = self.mailbox_state.selected().is_none();
                self.mailboxes = mailboxes;
                if self.mailboxes.is_empty() {
                    return;
                }
                let index = self
                    .mailbox_state
                    .selected()
                    .unwrap_or(0)
                    .min(self.mailboxes.len() - 1);
                self.mailbox_state.select(Some(index));
                if first_load {
                    self.select_mailbox(index);
                }
            }
            NetMsg::Headers { mailbox, headers } => {
                // Ignore replies for a folder the user has already left.
                if !mailbox.eq_ignore_ascii_case(&self.current_mailbox) {
                    return;
                }
                self.emails = headers;
                self.email_state
                    .select(if self.emails.is_empty() { None } else { Some(0) });
                self.load_selected_message();
            }
            NetMsg::Body(body) => {
                self.message = Some(body);
                self.body_scroll = 0;
            }
            NetMsg::Seen { mailbox, uid, seen } => {
                // Ignore replies for a folder the user has already left.
                if !mailbox.eq_ignore_ascii_case(&self.current_mailbox) {
                    return;
                }
                if let Some(email) = self.emails.iter_mut().find(|email| email.uid == uid) {
                    email.seen = seen;
                }
                self.adjust_unread(seen);
                self.status = if seen {
                    "marked read".to_string()
                } else {
                    "marked unread".to_string()
                };
            }
            NetMsg::Sent { to } => {
                self.sending = false;
                self.compose.reset();
                self.status = format!("message sent to {to}");
            }
            NetMsg::SendFailed(reason) => {
                self.sending = false;
                // Hand the untouched draft back rather than losing what was typed.
                self.mode = Mode::Compose;
                self.status = format!("send failed: {reason}");
            }
            NetMsg::Status(status) => self.status = status,
            NetMsg::Error(err) => self.status = format!("error: {err}"),
        }
    }

    // --- key handling -----------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Normal => self.on_key_normal(key),
            Mode::Compose => self.on_key_compose(key),
            Mode::Search => self.on_key_search(key),
        }
    }

    fn on_key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Tab => self.toggle_pane(),
            KeyCode::Enter => match self.pane {
                Pane::Mailboxes => self.select_mailbox(self.mailbox_index()),
                Pane::Messages => self.load_selected_message(),
                // The reader is already open; enter has nothing left to do.
                Pane::Body => {}
            },
            KeyCode::Char('c') => self.begin_compose(),
            KeyCode::Char('/') => self.begin_search(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('m') => self.toggle_seen(),
            // The scroll keys act on the reader from any pane, because there is
            // only ever one thing to scroll.
            KeyCode::PageDown => self.scroll_body(self.page_rows() as isize),
            KeyCode::PageUp => self.scroll_body(-(self.page_rows() as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.body_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => self.body_scroll = self.body_max_scroll,
            _ => {}
        }
    }

    fn on_key_compose(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.status = if self.compose.is_empty() {
                    "compose closed".to_string()
                } else {
                    "draft discarded".to_string()
                };
                self.compose.reset();
                self.mode = Mode::Normal;
            }
            KeyCode::Tab => self.compose.next_field(),
            KeyCode::BackTab => self.compose.prev_field(),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.send_compose();
            }
            _ => match self.compose.field {
                // The textarea owns its own key map (arrows, undo, selection…).
                ComposeField::Body => {
                    let _ = self.compose.body.input(key);
                }
                ComposeField::To => {
                    self.compose.to.handle(key);
                }
                ComposeField::Cc => {
                    self.compose.cc.handle(key);
                }
                ComposeField::Bcc => {
                    self.compose.bcc.handle(key);
                }
                ComposeField::Subject => {
                    self.compose.subject.handle(key);
                }
            },
        }
    }

    fn on_key_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.search.clear();
                self.status = "search cancelled".to_string();
            }
            KeyCode::Enter => self.run_search(),
            _ => {
                self.search.handle(key);
            }
        }
    }

    // --- actions ----------------------------------------------------------

    fn toggle_pane(&mut self) {
        self.pane = match self.pane {
            Pane::Mailboxes => Pane::Messages,
            Pane::Messages => Pane::Body,
            Pane::Body => Pane::Mailboxes,
        };
    }

    /// The arrow keys: move the highlighted row, or scroll the reader when it
    /// holds focus.
    fn move_selection(&mut self, delta: isize) {
        match self.pane {
            Pane::Mailboxes => {
                let Some(next) = step(self.mailbox_index(), self.mailboxes.len(), delta) else {
                    return;
                };
                self.mailbox_state.select(Some(next));
                self.select_mailbox(next);
            }
            Pane::Messages => {
                let Some(next) = step(self.email_index(), self.emails.len(), delta) else {
                    return;
                };
                self.email_state.select(Some(next));
                self.load_selected_message();
            }
            Pane::Body => self.scroll_body(delta),
        }
    }

    /// Scroll the reader, clamped to the content the last frame measured.
    fn scroll_body(&mut self, delta: isize) {
        let next = self.body_scroll as isize + delta;
        self.body_scroll = next.clamp(0, self.body_max_scroll as isize) as u16;
    }

    /// Rows advanced by one PageDown: a full viewport less one line of overlap,
    /// so the reader keeps a visual anchor and never skips a line.
    fn page_rows(&self) -> u16 {
        self.body_viewport.saturating_sub(1).max(1)
    }

    fn refresh(&mut self) {
        let cmd = match self.pane {
            Pane::Mailboxes => NetCmd::FetchMailboxes,
            Pane::Messages => NetCmd::FetchHeaders {
                mailbox: self.current_mailbox.clone(),
            },
            // There is nothing in the reader to reload but the message itself.
            Pane::Body => {
                self.load_selected_message();
                return;
            }
        };
        self.status = "refreshing…".to_string();
        self.send(cmd);
    }

    /// Switch to a folder and ask the network task for its headers.
    fn select_mailbox(&mut self, index: usize) {
        let Some(mailbox) = self.mailboxes.get(index) else {
            return;
        };
        let mailbox = mailbox.name.clone();

        self.current_mailbox = mailbox.clone();
        self.emails.clear();
        self.email_state.select(None);
        self.message = None;
        self.body_scroll = 0;
        self.status = format!("loading {mailbox}…");
        self.send(NetCmd::FetchHeaders { mailbox });
    }

    /// Ask the network task for the highlighted message.
    fn load_selected_message(&mut self) {
        let Some(summary) = self.emails.get(self.email_index()).cloned() else {
            self.message = None;
            return;
        };

        self.body_scroll = 0;
        // Show the envelope immediately; the body fills in when it arrives.
        self.message = Some(EmailBody::loading(&summary));
        self.send(NetCmd::FetchBody {
            mailbox: self.current_mailbox.clone(),
            uid: summary.uid,
        });
    }

    /// Flip `\Seen` on the highlighted message.
    ///
    /// Reading a message uses `BODY.PEEK[]`, so this is the only thing that
    /// marks one read — a deliberate act rather than a side effect of looking.
    fn toggle_seen(&mut self) {
        let Some(summary) = self.emails.get(self.email_index()) else {
            self.status = "no message to mark".to_string();
            return;
        };
        let (uid, seen) = (summary.uid, !summary.seen);

        self.status = if seen {
            "marking read…".to_string()
        } else {
            "marking unread…".to_string()
        };
        let mailbox = self.current_mailbox.clone();
        self.send(NetCmd::SetSeen { mailbox, uid, seen });
    }

    /// Keep the folder's unread badge honest after a flag change.
    fn adjust_unread(&mut self, seen: bool) {
        let current = self.current_mailbox.clone();
        let Some(mailbox) = self
            .mailboxes
            .iter_mut()
            .find(|mailbox| mailbox.name.eq_ignore_ascii_case(&current))
        else {
            return;
        };

        if seen {
            mailbox.unread = mailbox.unread.saturating_sub(1);
        } else {
            mailbox.unread = mailbox.unread.saturating_add(1);
        }
    }

    fn begin_compose(&mut self) {
        if self.sending {
            self.status = "a message is still being sent".to_string();
            return;
        }
        self.compose.reset();
        self.mode = Mode::Compose;
        self.status = "composing…".to_string();
    }

    fn send_compose(&mut self) {
        if self.sending {
            self.status = "a message is already being sent".to_string();
            return;
        }

        let to = self.compose.to.text.trim().to_string();
        if to.is_empty() {
            self.compose.field = ComposeField::To;
            self.status = "compose: \"To\" must not be empty".to_string();
            return;
        }

        let cc = self.compose.cc.text.trim().to_string();
        let bcc = self.compose.bcc.text.trim().to_string();
        let subject = self.compose.subject.text.trim().to_string();
        let body = self.compose.text();
        // The draft is deliberately kept until the server accepts it, so a
        // failed send hands the text straight back instead of discarding it.
        // `sending` also stops a second ctrl+s from sending twice.
        self.sending = true;
        self.mode = Mode::Normal;
        self.status = format!("sending to {to}…");
        self.send(NetCmd::Send { to, cc, bcc, subject, body });
    }

    fn begin_search(&mut self) {
        self.search.clear();
        self.mode = Mode::Search;
        self.status = "search".to_string();
    }

    fn run_search(&mut self) {
        let query = self.search.text.trim().to_string();
        self.mode = Mode::Normal;
        if query.is_empty() {
            self.status = "search cancelled".to_string();
            return;
        }
        self.status = format!("searching \"{query}\"…");
        self.send(NetCmd::Search {
            mailbox: self.current_mailbox.clone(),
            query,
        });
    }

    /// `try_send` keeps the UI non-blocking; a dropped command is surfaced in
    /// the status bar rather than stalling the render loop.
    fn send(&mut self, cmd: NetCmd) {
        if let Err(err) = self.net_tx.try_send(cmd) {
            self.status = format!("network: {err}");
        }
    }

    pub fn mailbox_index(&self) -> usize {
        self.mailbox_state.selected().unwrap_or(0)
    }

    pub fn email_index(&self) -> usize {
        self.email_state.selected().unwrap_or(0)
    }
}

/// `current + delta`, clamped into `0..len`. `None` when the list is empty.
fn step(current: usize, len: usize, delta: isize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some((current as isize + delta).clamp(0, len as isize - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppState {
        // `mpsc::channel` does not need a runtime to be constructed, so this
        // works from a plain `#[test]`. Dropping the receiver simply closes the
        // channel, which `send` already handles.
        let (tx, _rx) = mpsc::channel(1);
        AppState::new(tx)
    }

    #[test]
    fn tab_cycles_through_every_pane() {
        let mut app = app();
        assert_eq!(app.pane, Pane::Mailboxes);
        app.toggle_pane();
        assert_eq!(app.pane, Pane::Messages);
        app.toggle_pane();
        assert_eq!(app.pane, Pane::Body);
        app.toggle_pane();
        assert_eq!(app.pane, Pane::Mailboxes);
    }

    #[test]
    fn tab_cycles_every_compose_field_then_returns() {
        let mut compose = ComposeState::new();
        assert_eq!(compose.field, ComposeField::To);

        for expected in [
            ComposeField::Cc,
            ComposeField::Bcc,
            ComposeField::Subject,
            ComposeField::Body,
            ComposeField::To,
        ] {
            compose.next_field();
            assert_eq!(compose.field, expected);
        }

        // And the reverse order, from `To`, lands on `Body`.
        compose.prev_field();
        assert_eq!(compose.field, ComposeField::Body);
    }

    #[test]
    fn a_draft_with_only_cc_text_is_not_empty() {
        let mut compose = ComposeState::new();
        assert!(compose.is_empty());

        compose.cc.text.push_str("carol@example.com");
        assert!(!compose.is_empty());

        compose.reset();
        assert!(compose.is_empty());
    }

    #[test]
    fn the_arrows_only_scroll_the_reader_when_it_has_focus() {
        let mut app = app();
        app.body_max_scroll = 10;

        // With a list focused the arrows move a selection, never the reader.
        app.mailboxes = vec![Mailbox::new("INBOX", "Inbox", 0)];
        app.move_selection(1);
        assert_eq!(app.body_scroll, 0);

        app.pane = Pane::Body;
        app.move_selection(1);
        assert_eq!(app.body_scroll, 1);
        app.move_selection(-1);
        assert_eq!(app.body_scroll, 0);
    }

    #[test]
    fn scrolling_is_clamped_to_the_measured_content() {
        let mut app = app();
        app.body_max_scroll = 10;

        app.scroll_body(4);
        assert_eq!(app.body_scroll, 4);
        app.scroll_body(100);
        assert_eq!(app.body_scroll, 10);
        app.scroll_body(-100);
        assert_eq!(app.body_scroll, 0);
    }

    #[test]
    fn a_page_keeps_one_line_of_overlap() {
        let mut app = app();
        app.body_viewport = 20;
        assert_eq!(app.page_rows(), 19);

        // A degenerate viewport must still move by something.
        app.body_viewport = 0;
        assert_eq!(app.page_rows(), 1);
    }

    #[test]
    fn stepping_stops_at_both_ends() {
        assert_eq!(step(0, 3, -1), Some(0));
        assert_eq!(step(2, 3, 1), Some(2));
        assert_eq!(step(1, 3, 1), Some(2));
        assert_eq!(step(0, 0, 1), None);
    }
}
