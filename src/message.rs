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
    pub subject: String,
    pub body: String,
}

impl EmailBody {
    /// Header block, blank line, then the message body — as shown in the
    /// reading pane.
    pub fn to_display(&self) -> String {
        format!(
            "From: {}\nTo: {}\nDate: {}\nSubject: {}\n\n{}",
            self.from, self.to, self.date, self.subject, self.body
        )
    }

    /// Envelope-only placeholder shown while the real body is downloading.
    pub fn loading(summary: &EmailSummary) -> Self {
        Self {
            date: summary.date.clone(),
            from: summary.from.clone(),
            to: String::new(),
            subject: summary.subject.clone(),
            body: "(loading…)".to_string(),
        }
    }
}
