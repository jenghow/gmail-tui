//! RFC 2047 and MIME helpers, built on `mailparse`.
//!
//! IMAP's `ENVELOPE` gives us raw header values, which means subjects and
//! sender names arrive as `=?UTF-8?B?...?=` encoded words rather than text.
//! That decoding is why this is Phase 2 work rather than Phase 3.

use imap_proto::Address;
use mailparse::{MailHeaderMap, ParsedMail};

use crate::message::EmailBody;

/// Decode one RFC 2047 encoded-word header value.
///
/// `mailparse` only decodes through a whole header line, and `ENVELOPE` hands
/// us the bare value, so wrap it in a throwaway name first.
pub fn decode_word(raw: &[u8]) -> String {
    let mut line = Vec::with_capacity(raw.len() + 9);
    line.extend_from_slice(b"Subject: ");
    line.extend_from_slice(raw);

    match mailparse::parse_header(&line) {
        Ok((header, _)) => header.get_value().trim().to_string(),
        Err(_) => String::from_utf8_lossy(raw).trim().to_string(),
    }
}

/// Render an IMAP envelope address as `Name <mailbox@host>`.
pub fn format_address(address: &Address<'_>) -> String {
    let name = address
        .name
        .as_deref()
        .map(decode_word)
        .filter(|name| !name.is_empty());

    let mailbox = address
        .mailbox
        .as_deref()
        .map(|mailbox| String::from_utf8_lossy(mailbox).into_owned())
        .unwrap_or_default();
    let host = address
        .host
        .as_deref()
        .map(|host| String::from_utf8_lossy(host).into_owned())
        .filter(|host| !host.is_empty());

    let email = match host {
        Some(host) if !mailbox.is_empty() => format!("{mailbox}@{host}"),
        _ => mailbox,
    };

    match (name, email.is_empty()) {
        (Some(name), false) => format!("{name} <{email}>"),
        (Some(name), true) => name,
        (None, false) => email,
        (None, true) => String::new(),
    }
}

/// Build the reading-pane message from a raw RFC 5322 message.
pub fn parse_body(raw: &[u8]) -> EmailBody {
    let Ok(parsed) = mailparse::parse_mail(raw) else {
        return EmailBody {
            date: String::new(),
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            body: String::from_utf8_lossy(raw).into_owned(),
        };
    };

    let header = |name: &str| parsed.headers.get_first_value(name).unwrap_or_default();

    EmailBody {
        date: header("Date"),
        from: header("From"),
        to: header("To"),
        subject: header("Subject"),
        body: text_of(&parsed),
    }
}

/// Build the reading-pane message from a fetched `BODY[HEADER]` block and an
/// already-decoded text body.
///
/// The section form of the fetch never sees the whole message, so the header
/// block has to be asked for separately.
pub fn body_from_parts(header: &[u8], text: String) -> EmailBody {
    let Ok((headers, _)) = mailparse::parse_headers(header) else {
        return EmailBody {
            date: String::new(),
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            body: text,
        };
    };

    let value = |name: &str| headers.get_first_value(name).unwrap_or_default();
    EmailBody {
        date: value("Date"),
        from: value("From"),
        to: value("To"),
        subject: value("Subject"),
        body: text,
    }
}

/// Decode one MIME section that was fetched on its own.
///
/// A part pulled out with `BODY[1.2]` arrives without its own headers, so the
/// type, charset and transfer encoding are the ones `BODYSTRUCTURE` reported,
/// synthesised into a one-part message for `mailparse` to decode.
pub fn decode_section(raw: &[u8], content_type: &str, encoding: &str) -> String {
    let mut message = Vec::with_capacity(raw.len() + 128);
    message.extend_from_slice(b"Content-Type: ");
    message.extend_from_slice(content_type.as_bytes());
    message.extend_from_slice(b"\r\nContent-Transfer-Encoding: ");
    message.extend_from_slice(encoding.as_bytes());
    message.extend_from_slice(b"\r\n\r\n");
    message.extend_from_slice(raw);

    let Ok(part) = mailparse::parse_mail(&message) else {
        return String::from_utf8_lossy(raw).into_owned();
    };
    let text = part
        .get_body()
        .unwrap_or_else(|_| String::from_utf8_lossy(raw).into_owned());

    if mimetype(content_type).eq_ignore_ascii_case("text/html") {
        strip_html(&text)
    } else {
        text
    }
}

/// The `type/subtype` of a `Content-Type` value, without its parameters.
fn mimetype(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
}

/// Prefer `text/plain`; fall back to `text/html` with the tags stripped.
fn text_of(part: &ParsedMail<'_>) -> String {
    if let Some(text) = find_part(part, "text/plain") {
        return text;
    }
    if let Some(html) = find_part(part, "text/html") {
        return strip_html(&html);
    }
    part.get_body().unwrap_or_default()
}

fn find_part(part: &ParsedMail<'_>, mime: &str) -> Option<String> {
    if part.ctype.mimetype.eq_ignore_ascii_case(mime) {
        let body = part.get_body().ok()?;
        if !body.trim().is_empty() {
            return Some(body);
        }
    }
    part.subparts.iter().find_map(|sub| find_part(sub, mime))
}

/// Crude but adequate HTML reduction.
///
/// Tags are dropped, a newline is inserted where a block-level element opens,
/// and the indentation and blank lines that HTML formatting leaves behind are
/// tidied up.
fn strip_html(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        // A '<' that does not look like the start of a tag is literal text, as
        // in "5 < 10"; treating it as one would swallow the rest of the body.
        let starts_tag = ch == '<'
            && matches!(chars.peek(), Some(next) if next.is_ascii_alphabetic() || *next == '/');
        if !starts_tag {
            text.push(ch);
            continue;
        }

        let mut tag = String::new();
        for ch in chars.by_ref() {
            if ch == '>' {
                break;
            }
            tag.push(ch);
        }

        if opens_block(&tag) {
            text.push('\n');
        }
    }

    tidy(&text)
}

/// Whether an opening tag begins a new line.
fn opens_block(tag: &str) -> bool {
    if tag.starts_with('/') {
        // A closing tag adds nothing: the next opening tag already breaks.
        return false;
    }

    let name = tag
        .split(|ch: char| ch.is_whitespace() || ch == '/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    matches!(
        name.as_str(),
        "br" | "hr"
            | "p"
            | "div"
            | "tr"
            | "li"
            | "ul"
            | "ol"
            | "table"
            | "blockquote"
            | "pre"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// Collapse the whitespace that HTML formatting leaves behind.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn address(name: Option<&'static str>, mailbox: &'static str, host: &'static str) -> Address<'static> {
        Address {
            name: name.map(|name| Cow::Borrowed(name.as_bytes())),
            adl: None,
            mailbox: Some(Cow::Borrowed(mailbox.as_bytes())),
            host: Some(Cow::Borrowed(host.as_bytes())),
        }
    }

    #[test]
    fn decodes_base64_encoded_words() {
        // "SMO2aGxl" is base64 for the UTF-8 bytes of "Höhle".
        assert_eq!(decode_word(b"=?UTF-8?B?SMO2aGxl?="), "Höhle");
    }

    #[test]
    fn decodes_quoted_printable_encoded_words() {
        assert_eq!(decode_word(b"=?UTF-8?Q?caf=C3=A9?="), "café");
    }

    #[test]
    fn joins_adjacent_encoded_words() {
        // RFC 2047 says the whitespace between encoded words is not significant.
        assert_eq!(decode_word(b"=?UTF-8?Q?Hello_?= =?UTF-8?Q?World?="), "Hello World");
    }

    #[test]
    fn passes_plain_values_through() {
        assert_eq!(decode_word(b"Release v0.29.0"), "Release v0.29.0");
        assert_eq!(decode_word(b""), "");
    }

    #[test]
    fn formats_addresses() {
        assert_eq!(
            format_address(&address(Some("Alice Smith"), "alice", "example.com")),
            "Alice Smith <alice@example.com>"
        );
        assert_eq!(
            format_address(&address(None, "noreply", "github.com")),
            "noreply@github.com"
        );
        assert_eq!(format_address(&address(Some("GitHub"), "noreply", "github.com")), "GitHub <noreply@github.com>");
    }

    #[test]
    fn decodes_encoded_names_in_addresses() {
        assert_eq!(
            format_address(&address(Some("=?UTF-8?Q?H=C3=B6hle?="), "h", "example.com")),
            "Höhle <h@example.com>"
        );
    }

    #[test]
    fn parses_a_simple_message() {
        let raw = b"From: Alice <alice@example.com>\r\n\
                    To: bob@example.com\r\n\
                    Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\
                    Date: Thu, 16 Oct 2026 09:00:00 +0000\r\n\
                    \r\n\
                    Hello there.\r\n";

        let body = parse_body(raw);
        assert_eq!(body.subject, "café");
        assert_eq!(body.from, "Alice <alice@example.com>");
        assert_eq!(body.to, "bob@example.com");
        assert_eq!(body.body.trim(), "Hello there.");
    }

    #[test]
    fn prefers_text_plain_over_html() {
        let raw = b"From: a@b.com\r\n\
                    Subject: both\r\n\
                    MIME-Version: 1.0\r\n\
                    Content-Type: multipart/alternative; boundary=\"X\"\r\n\
                    \r\n\
                    --X\r\n\
                    Content-Type: text/plain\r\n\
                    \r\n\
                    plain version\r\n\
                    --X\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <p>html version</p>\r\n\
                    --X--\r\n";

        assert_eq!(parse_body(raw).body.trim(), "plain version");
    }

    #[test]
    fn strips_html_when_there_is_no_plain_part() {
        let raw = b"From: a@b.com\r\n\
                    Subject: html only\r\n\
                    MIME-Version: 1.0\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <html><body><h1>Title</h1><p>First</p><p>Second</p></body></html>\r\n";

        assert_eq!(parse_body(raw).body, "Title\nFirst\nSecond");
    }

    #[test]
    fn keeps_literal_angle_brackets() {
        // "5 < 10" is not a tag; swallowing it would lose the rest of the body.
        assert_eq!(strip_html("<p>5 < 10 and 3 > 2</p>"), "5 < 10 and 3 > 2");
    }

    #[test]
    fn breaks_lines_at_br_tags() {
        assert_eq!(strip_html("one<br>two<br/>three"), "one\ntwo\nthree");
    }

    #[test]
    fn decodes_a_base64_section() {
        // "SGVsbG8sIHdvcmxkIQ==" is base64 for "Hello, world!".
        assert_eq!(
            decode_section(
                b"SGVsbG8sIHdvcmxkIQ==",
                "text/plain; charset=\"UTF-8\"",
                "base64"
            ),
            "Hello, world!"
        );
    }

    #[test]
    fn decodes_the_charset_of_a_quoted_printable_section() {
        assert_eq!(
            decode_section(
                b"caf=C3=A9",
                "text/plain; charset=\"UTF-8\"",
                "quoted-printable"
            ),
            "café"
        );
    }

    #[test]
    fn leaves_a_plain_section_alone() {
        assert_eq!(
            decode_section(b"just text", "text/plain; charset=\"UTF-8\"", "7bit"),
            "just text"
        );
    }

    #[test]
    fn reduces_an_html_section_to_text() {
        assert_eq!(
            decode_section(
                b"<p>one</p><p>two</p>",
                "text/html; charset=\"UTF-8\"",
                "7bit"
            ),
            "one\ntwo"
        );
    }

    #[test]
    fn builds_a_message_from_a_header_block() {
        let header = b"From: Alice <alice@example.com>\r\n\
                       To: bob@example.com\r\n\
                       Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\
                       Date: Thu, 16 Oct 2026 09:00:00 +0000\r\n\
                       \r\n";

        let body = body_from_parts(header, "Hello there.".to_string());
        assert_eq!(body.subject, "café");
        assert_eq!(body.from, "Alice <alice@example.com>");
        assert_eq!(body.to, "bob@example.com");
        assert_eq!(body.date, "Thu, 16 Oct 2026 09:00:00 +0000");
        assert_eq!(body.body, "Hello there.");
    }
}
