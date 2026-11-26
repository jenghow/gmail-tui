//! Plain data types shared between the UI and the network task.

/// A mailbox (folder) shown in the left pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    /// IMAP name, e.g. `INBOX` or `[Gmail]/Sent Mail`.
    pub name: String,
    /// Short label for the UI, e.g. `Inbox`.
    pub display: String,
    pub unread: u32,
}

impl Mailbox {
    pub fn new(name: impl Into<String>, display: impl Into<String>, unread: u32) -> Self {
        Self {
            name: name.into(),
            display: display.into(),
            unread,
        }
    }

    /// `Inbox (12)` or `Sent`.
    pub fn label(&self) -> String {
        if self.unread > 0 {
            format!("{} ({})", self.display, self.unread)
        } else {
            self.display.clone()
        }
    }
}

/// One row of the message list: IMAP `ENVELOPE` + flags, no body.
#[derive(Debug, Clone)]
pub struct EmailSummary {
    pub uid: u32,
    pub date: String,
    pub from: String,
    pub subject: String,
    pub seen: bool,
}

/// The semantic role of a styled body span. The UI owns the colours; this
/// module only names what the text *is*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanKind {
    #[default]
    Text,
    Link,
    Image,
    Code,
}

/// Emphasis carried by one run of text in an HTML body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub kind: SpanKind,
}

/// One contiguous run of text with a single style.
#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

/// An HTML body laid out into lines of styled spans.
///
/// The lines are already wrapped for the width they were rendered at, so the
/// reader rebuilds this when its width changes.
#[derive(Debug, Clone, Default)]
pub struct StyledBody {
    pub lines: Vec<Vec<StyledSpan>>,
}

impl StyledBody {
    /// A body with no styling, one span per line. Used for the plain-text
    /// fallback when an HTML body cannot be laid out.
    pub fn from_plain(text: &str) -> Self {
        Self {
            lines: text
                .lines()
                .map(|line| {
                    vec![StyledSpan {
                        text: line.to_string(),
                        style: SpanStyle::default(),
                    }]
                })
                .collect(),
        }
    }
}

/// A fully fetched message.
#[derive(Debug, Clone)]
pub struct EmailBody {
    pub date: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// Plain-text body; empty when [`EmailBody::html`] carries the message.
    pub body: String,
    /// Decoded HTML source, kept so the reader can re-lay it out when the
    /// terminal is resized. `None` for plain-text messages.
    pub html: Option<String>,
}

impl EmailBody {
    /// The reading pane's header as `(label, value)` pairs, in display order.
    /// `Cc` and `Bcc` are dropped when the message carries none, so a plain
    /// message shows no empty rows. Rendering owns the styling; this stays
    /// plain data like the rest of the module.
    pub fn header_lines(&self) -> Vec<(&'static str, &str)> {
        let mut lines = vec![("From", self.from.as_str()), ("To", self.to.as_str())];
        if !self.cc.is_empty() {
            lines.push(("Cc", self.cc.as_str()));
        }
        if !self.bcc.is_empty() {
            lines.push(("Bcc", self.bcc.as_str()));
        }
        lines.push(("Date", self.date.as_str()));
        lines.push(("Subject", self.subject.as_str()));
        lines
    }

    /// Envelope-only placeholder shown while the real body is downloading.
    pub fn loading(summary: &EmailSummary) -> Self {
        Self {
            date: summary.date.clone(),
            from: summary.from.clone(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: summary.subject.clone(),
            body: "(loading…)".to_string(),
            html: None,
        }
    }
}
