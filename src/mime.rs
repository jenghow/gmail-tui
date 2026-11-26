//! RFC 2047 and MIME helpers, built on `mailparse`.
//!
//! IMAP's `ENVELOPE` gives us raw header values, which means subjects and
//! sender names arrive as `=?UTF-8?B?...?=` encoded words rather than text.
//! That decoding is why this is Phase 2 work rather than Phase 3.

use html2text::from_read_rich;
use html2text::render::RichAnnotation;
use imap_proto::Address;
use mailparse::{MailHeaderMap, ParsedMail};

use crate::message::{EmailBody, SpanKind, SpanStyle, StyledBody, StyledSpan};

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
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            body: String::from_utf8_lossy(raw).into_owned(),
            html: None,
        };
    };

    let header = |name: &str| parsed.headers.get_first_value(name).unwrap_or_default();
    let (body, html) = text_of(&parsed);

    EmailBody {
        date: header("Date"),
        from: header("From"),
        to: header("To"),
        cc: header("Cc"),
        bcc: header("Bcc"),
        subject: header("Subject"),
        body,
        html,
    }
}

/// Build the reading-pane message from a fetched `BODY[HEADER]` block and an
/// already-decoded text body.
///
/// The section form of the fetch never sees the whole message, so the header
/// block has to be asked for separately.
pub fn body_from_parts(header: &[u8], decoded: DecodedText) -> EmailBody {
    let (body, html) = decoded.into_body();
    let Ok((headers, _)) = mailparse::parse_headers(header) else {
        return EmailBody {
            date: String::new(),
            from: String::new(),
            to: String::new(),
            cc: String::new(),
            bcc: String::new(),
            subject: String::new(),
            body,
            html,
        };
    };

    let value = |name: &str| headers.get_first_value(name).unwrap_or_default();
    EmailBody {
        date: value("Date"),
        from: value("From"),
        to: value("To"),
        cc: value("Cc"),
        bcc: value("Bcc"),
        subject: value("Subject"),
        body,
        html,
    }
}

/// One MIME section decoded to text, remembering whether it is HTML.
///
/// HTML parts keep their source here; the reader lays them out with
/// [`render_html`], because the available width is only known there.
pub struct DecodedText {
    pub text: String,
    pub html: bool,
}

impl DecodedText {
    /// Split into the plain body and the optional HTML source `EmailBody`
    /// carries. An HTML part is not also flattened into plain text.
    fn into_body(self) -> (String, Option<String>) {
        if self.html {
            (String::new(), Some(self.text))
        } else {
            (self.text, None)
        }
    }
}

/// Decode one MIME section that was fetched on its own.
///
/// A part pulled out with `BODY[1.2]` arrives without its own headers, so the
/// type, charset and transfer encoding are the ones `BODYSTRUCTURE` reported,
/// synthesised into a one-part message for `mailparse` to decode.
pub fn decode_section(raw: &[u8], content_type: &str, encoding: &str) -> DecodedText {
    let mut message = Vec::with_capacity(raw.len() + 128);
    message.extend_from_slice(b"Content-Type: ");
    message.extend_from_slice(content_type.as_bytes());
    message.extend_from_slice(b"\r\nContent-Transfer-Encoding: ");
    message.extend_from_slice(encoding.as_bytes());
    message.extend_from_slice(b"\r\n\r\n");
    message.extend_from_slice(raw);

    let text = match mailparse::parse_mail(&message) {
        Ok(part) => part
            .get_body()
            .unwrap_or_else(|_| String::from_utf8_lossy(raw).into_owned()),
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    };

    DecodedText {
        text,
        html: mimetype(content_type).eq_ignore_ascii_case("text/html"),
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

/// Prefer `text/plain`; when only HTML is present, hand its source to the
/// reader's renderer instead of flattening it here.
fn text_of(part: &ParsedMail<'_>) -> (String, Option<String>) {
    if let Some(text) = find_part(part, "text/plain") {
        return (text, None);
    }
    if let Some(html) = find_part(part, "text/html") {
        return (String::new(), Some(html));
    }
    (part.get_body().unwrap_or_default(), None)
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

/// Lay out a decoded HTML body as styled lines at `width` columns.
///
/// `html2text` wraps to the width it is given, so the reader re-runs this when
/// the terminal is resized. It is pure and offline. If parsing fails, the
/// crude tag stripper below keeps the message readable.
pub fn render_html(html: &str, width: usize) -> StyledBody {
    let Ok(lines) = from_read_rich(html.as_bytes(), width.max(1)) else {
        return StyledBody::from_plain(&strip_html(html));
    };

    let styled: Vec<Vec<StyledSpan>> = lines
        .iter()
        .map(|line| {
            // `html2text` has no heading annotation and leaves markdown-style
            // `#` prefixes in the line even in rich mode. Recover the visual
            // heading — but never inside preformatted text, where a leading
            // `#` is literal content (a code comment, for instance).
            let preformatted = line.tagged_strings().any(|tagged| {
                tagged
                    .tag
                    .iter()
                    .any(|annotation| matches!(annotation, RichAnnotation::Preformat(_)))
            });

            let mut spans: Vec<StyledSpan> = line
                .tagged_strings()
                .map(|tagged| {
                    let style = span_style(&tagged.tag);
                    let mut text = tagged.s.clone();
                    // `RichDecorator` also *draws* strikeout with combining
                    // overlays. The terminal's crossed-out modifier does that
                    // without polluting the text, so drop the overlays.
                    if style.strike {
                        text.retain(|ch| ch != '\u{0336}');
                    }
                    StyledSpan { text, style }
                })
                .collect();

            if !preformatted {
                style_heading(&mut spans);
            }
            spans
        })
        .collect();

    StyledBody { lines: styled }
}

/// Turn `html2text`'s `#` heading prefixes into bold text.
///
/// Only a run of one to six `#` followed by a space is treated as a heading;
/// ordinary text such as a hashtag is left alone.
fn style_heading(line: &mut [StyledSpan]) {
    let Some(index) = line.iter().position(|span| !span.text.is_empty()) else {
        return;
    };
    let level = line[index].text.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) || line[index].text.as_bytes().get(level) != Some(&b' ') {
        return;
    }

    line[index].text.drain(..=level);
    for span in line {
        span.style.bold = true;
    }
}

/// Turn `html2text`'s annotations into the reader's neutral style flags.
fn span_style(annotations: &[RichAnnotation]) -> SpanStyle {
    let mut style = SpanStyle::default();
    let (mut link, mut code, mut image) = (false, false, false);

    for annotation in annotations {
        match annotation {
            RichAnnotation::Strong => style.bold = true,
            RichAnnotation::Emphasis => style.italic = true,
            RichAnnotation::Strikeout => style.strike = true,
            RichAnnotation::Link(_) => link = true,
            RichAnnotation::Code => code = true,
            RichAnnotation::Image(_) => image = true,
            // CSS colours are ignored on purpose: mail HTML assumes a white
            // page, and honouring them can make text invisible on a dark
            // terminal. `Preformat` needs no style beyond its layout.
            RichAnnotation::Default
            | RichAnnotation::Preformat(_)
            | RichAnnotation::Colour(_)
            | RichAnnotation::BgColour(_) => {}
            // The enum is non-exhaustive; new annotations are harmless.
            _ => {}
        }
    }

    style.kind = if link {
        SpanKind::Link
    } else if code {
        SpanKind::Code
    } else if image {
        SpanKind::Image
    } else {
        SpanKind::Text
    };
    style
}

/// Crude but adequate HTML reduction, used only when the rich renderer fails.
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
                    Cc: carol@example.com\r\n\
                    Bcc: dave@example.com\r\n\
                    Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\
                    Date: Thu, 16 Oct 2026 09:00:00 +0000\r\n\
                    \r\n\
                    Hello there.\r\n";

        let body = parse_body(raw);
        assert_eq!(body.subject, "café");
        assert_eq!(body.from, "Alice <alice@example.com>");
        assert_eq!(body.to, "bob@example.com");
        assert_eq!(body.cc, "carol@example.com");
        assert_eq!(body.bcc, "dave@example.com");
        assert_eq!(body.body.trim(), "Hello there.");
        assert!(body.html.is_none());
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
    fn html_only_messages_keep_their_source_for_the_renderer() {
        let raw = b"From: a@b.com\r\n\
                    Subject: html only\r\n\
                    MIME-Version: 1.0\r\n\
                    Content-Type: text/html\r\n\
                    \r\n\
                    <html><body><h1>Title</h1><p>First</p><p>Second</p></body></html>\r\n";

        let body = parse_body(raw);
        assert!(body.body.is_empty());
        let html = body.html.expect("html source");
        assert!(html.contains("<h1>Title</h1>"));

        let rendered = render_html(&html, 40);
        let text = flatten(&rendered);
        assert!(text.contains("Title"));
        assert!(text.contains("First"));
        assert!(text.contains("Second"));
        assert!(!text.contains('#'));
        assert!(!text.contains('<'));
        assert!(style_of(&rendered, "Title").bold);
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
        let decoded = decode_section(
            b"SGVsbG8sIHdvcmxkIQ==",
            "text/plain; charset=\"UTF-8\"",
            "base64",
        );
        assert_eq!(decoded.text, "Hello, world!");
        assert!(!decoded.html);
    }

    #[test]
    fn decodes_the_charset_of_a_quoted_printable_section() {
        let decoded = decode_section(
            b"caf=C3=A9",
            "text/plain; charset=\"UTF-8\"",
            "quoted-printable",
        );
        assert_eq!(decoded.text, "café");
        assert!(!decoded.html);
    }

    #[test]
    fn leaves_a_plain_section_alone() {
        let decoded = decode_section(b"just text", "text/plain; charset=\"UTF-8\"", "7bit");
        assert_eq!(decoded.text, "just text");
        assert!(!decoded.html);
    }

    #[test]
    fn marks_an_html_section_without_stripping_it() {
        let decoded =
            decode_section(b"<p>one</p><p>two</p>", "text/html; charset=\"UTF-8\"", "7bit");
        assert_eq!(decoded.text, "<p>one</p><p>two</p>");
        assert!(decoded.html);
    }

    #[test]
    fn builds_a_message_from_a_header_block() {
        let header = b"From: Alice <alice@example.com>\r\n\
                       To: bob@example.com\r\n\
                       Cc: carol@example.com\r\n\
                       Subject: =?UTF-8?Q?caf=C3=A9?=\r\n\
                       Date: Thu, 16 Oct 2026 09:00:00 +0000\r\n\
                       \r\n";

        let decoded = DecodedText {
            text: "Hello there.".to_string(),
            html: false,
        };
        let body = body_from_parts(header, decoded);
        assert_eq!(body.subject, "café");
        assert_eq!(body.from, "Alice <alice@example.com>");
        assert_eq!(body.to, "bob@example.com");
        assert_eq!(body.cc, "carol@example.com");
        assert_eq!(body.bcc, "");
        assert_eq!(body.date, "Thu, 16 Oct 2026 09:00:00 +0000");
        assert_eq!(body.body, "Hello there.");
        assert!(body.html.is_none());
    }

    #[test]
    fn keeps_html_source_when_building_from_a_header_block() {
        let decoded = DecodedText {
            text: "<p>Hello <strong>there</strong>.</p>".to_string(),
            html: true,
        };
        let body = body_from_parts(b"Subject: hi\r\n\r\n", decoded);
        assert!(body.body.is_empty());
        assert_eq!(body.html.as_deref(), Some("<p>Hello <strong>there</strong>.</p>"));
    }

    #[test]
    fn header_lines_omit_empty_cc_and_bcc() {
        let plain = EmailBody {
            date: "now".to_string(),
            from: "a@b.com".to_string(),
            to: "c@d.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "hi".to_string(),
            body: "body".to_string(),
            html: None,
        };
        let labels: Vec<&str> = plain.header_lines().iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["From", "To", "Date", "Subject"]);

        let copied = EmailBody { cc: "x@y.com".to_string(), ..plain };
        let labels: Vec<&str> = copied.header_lines().iter().map(|(label, _)| *label).collect();
        assert_eq!(labels, ["From", "To", "Cc", "Date", "Subject"]);
    }

    /// The reader's lines joined back into plain text, for compact assertions.
    fn flatten(body: &StyledBody) -> String {
        body.lines
            .iter()
            .map(|line| line.iter().map(|span| span.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn style_of(body: &StyledBody, needle: &str) -> SpanStyle {
        body.lines
            .iter()
            .flatten()
            .find(|span| span.text.contains(needle))
            .unwrap_or_else(|| panic!("no span containing {needle:?}"))
            .style
    }

    #[test]
    fn renders_emphasis_as_styles_instead_of_markup() {
        let body = render_html(
            "<p>plain <strong>bold</strong> and <em>italic</em> and <s>gone</s>.</p>",
            60,
        );
        let text = flatten(&body);

        assert!(text.contains("plain bold and italic and gone."));
        assert!(!text.contains(['*', '_']), "no markdown markers: {text:?}");
        assert!(style_of(&body, "bold").bold);
        assert!(style_of(&body, "italic").italic);
        assert!(style_of(&body, "gone").strike);
    }

    #[test]
    fn marks_links_and_keeps_their_text() {
        let body = render_html("<p><a href=\"https://example.com\">example</a></p>", 60);
        let link = style_of(&body, "example");

        assert_eq!(link.kind, SpanKind::Link);
        assert!(!flatten(&body).contains("https://example.com"));
    }

    #[test]
    fn decodes_entities_and_drops_script_and_style_bodies() {
        let body = render_html(
            "<style>.hidden { color: red }</style>\
             <p>Visible &amp; clear&nbsp;text</p>\
             <script>alert('hidden')</script>",
            60,
        );
        let text = flatten(&body);

        assert!(text.contains("Visible & clear"));
        assert!(!text.contains("color: red"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("&amp;"));
    }

    #[test]
    fn headings_become_bold_text_without_hash_markers() {
        let body = render_html("<h1>Title</h1><h2>Sub</h2>", 40);
        let text = flatten(&body);

        assert!(!text.contains('#'), "no markdown heading markers: {text:?}");
        assert!(text.contains("Title"));
        assert!(text.contains("Sub"));
        assert!(style_of(&body, "Title").bold);
        assert!(style_of(&body, "Sub").bold);
    }

    #[test]
    fn hash_comments_in_preformatted_text_are_left_alone() {
        let body = render_html("<pre># not a heading</pre>", 40);
        let text = flatten(&body);

        assert_eq!(text, "# not a heading");
        assert!(!style_of(&body, "#").bold);
    }

    #[test]
    fn a_narrower_width_produces_more_lines() {
        let html = format!("<p>{}</p>", vec!["word"; 40].join(" "));
        let narrow = render_html(&html, 20);
        let wide = render_html(&html, 100);

        assert!(
            narrow.lines.len() > wide.lines.len(),
            "{} vs {} lines",
            narrow.lines.len(),
            wide.lines.len()
        );
    }
}
