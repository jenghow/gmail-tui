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

/// A fully fetched message.
#[derive(Debug, Clone)]
pub struct EmailBody {
    pub date: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub body: String,
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
        }
    }
}
