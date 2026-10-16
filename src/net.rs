//! The IMAP worker, and the SMTP submitter that shares its task.
//!
//! Everything here runs on the tokio runtime, off the render loop. The UI only
//! ever talks to this task through the `NetCmd` / `NetMsg` channels, so a slow
//! round-trip can never freeze rendering.

use std::time::Duration;

use async_imap::Session;
use async_imap::error::Error as ImapError;
use async_imap::types::{Fetch, Flag, Name, NameAttribute};
use async_native_tls::TlsStream;
use futures_util::TryStreamExt;
use imap_proto::{BodyContentCommon, BodyStructure, ContentEncoding, SectionPath};
use lettre::message::Mailbox as SmtpAddress;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::credentials::Secret;
use crate::message::{EmailBody, EmailSummary, Mailbox};
use crate::mime;

pub const HOST: &str = "imap.gmail.com";
pub const PORT: u16 = 993;

/// Gmail's submission endpoint. Port 465 speaks TLS from the first byte, which
/// sidesteps the STARTTLS downgrade question entirely.
const SMTP_HOST: &str = "smtp.gmail.com";

/// How many messages per folder are listed. Gmail's UIDs are sparse and grow
/// without bound, so the newest slice is taken by *sequence number*; asking for
/// `1:*` by UID would sweep the whole mailbox.
const HEADER_LIMIT: u32 = 50;

/// Ceiling on TCP + TLS + greeting.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Reconnect backoff. Gmail permits only ten simultaneous IMAP connections and
/// locks the account out for about a day when that is exceeded, so retries must
/// never turn into a tight connection loop.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

type ImapSession = Session<TlsStream<TcpStream>>;

/// Commands the UI sends *to* the network task.
#[derive(Debug)]
pub enum NetCmd {
    /// Re-read the folder list.
    FetchMailboxes,
    /// Re-read headers for one folder.
    FetchHeaders { mailbox: String },
    /// Download and parse a single message.
    FetchBody { mailbox: String, uid: u32 },
    /// Set or clear `\Seen` on one message — the deliberate act that merely
    /// opening a message is not allowed to perform.
    SetSeen {
        mailbox: String,
        uid: u32,
        seen: bool,
    },
    /// Run an IMAP SEARCH and replace the header list with the hits.
    Search { mailbox: String, query: String },
    /// Submit a message over SMTP.
    Send {
        to: String,
        cc: String,
        bcc: String,
        subject: String,
        body: String,
    },
}

/// Unsolicited updates the network task pushes *to* the UI.
#[derive(Debug)]
pub enum NetMsg {
    Mailboxes(Vec<Mailbox>),
    Headers {
        mailbox: String,
        headers: Vec<EmailSummary>,
    },
    Body(EmailBody),
    /// A flag change the server accepted, echoed back so the list can be
    /// updated without refetching it.
    Seen {
        mailbox: String,
        uid: u32,
        seen: bool,
    },
    /// Gmail accepted the message.
    Sent { to: String },
    /// Gmail refused it. The draft is still intact in the UI.
    SendFailed(String),
    Status(String),
    Error(String),
}

/// Spawn the IMAP worker.
pub fn spawn(tx: mpsc::Sender<NetMsg>, rx: mpsc::Receiver<NetCmd>, user: String, secret: Secret) {
    tokio::spawn(async move {
        Worker {
            tx,
            user,
            secret,
            session: None,
            selected: String::new(),
        }
        .run(rx)
        .await;
    });
}

/// How a failed command should be treated.
enum Failure {
    /// The session is dead; reconnect with backoff.
    Disconnected(String),
    /// The command failed, but the session is still usable.
    Command(String),
}

type Dispatch = std::result::Result<(), Failure>;

/// The outcome of trying to establish a session.
enum Connect {
    Ready,
    /// Worth retrying after a backoff.
    Retry(String),
    /// Retrying will not help — bad credentials, or the UI is gone.
    Fatal(String),
}

/// Why the serving loop stopped.
enum Serve {
    /// The UI closed its end of the channel.
    Quit,
    /// The connection died mid-command.
    Disconnected(String),
}

struct Worker {
    tx: mpsc::Sender<NetMsg>,
    user: String,
    secret: Secret,
    session: Option<ImapSession>,
    /// IMAP name of the folder currently selected on `session`.
    selected: String,
}

impl Worker {
    /// Connect, serve commands, and reconnect until the UI goes away.
    async fn run(&mut self, mut rx: mpsc::Receiver<NetCmd>) {
        let mut backoff = BACKOFF_START;

        loop {
            match self.connect().await {
                Connect::Ready => {}
                Connect::Fatal(message) => {
                    self.report(&message).await;
                    return;
                }
                Connect::Retry(message) => {
                    if !self.report(&message).await || !self.wait(backoff).await {
                        return;
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                    continue;
                }
            }

            match self.serve(&mut rx, &mut backoff).await {
                Serve::Quit => return,
                Serve::Disconnected(message) => {
                    // Drop the dead session *before* dialling again: a leaked
                    // connection still counts against Gmail's limit of ten.
                    self.session = None;
                    if !self.report(&message).await || !self.wait(backoff).await {
                        return;
                    }
                    backoff = (backoff * 2).min(BACKOFF_MAX);
                }
            }
        }
    }

    async fn connect(&mut self) -> Connect {
        self.status(format!("connecting to {HOST}:{PORT} …")).await;

        let tcp =
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((HOST, PORT))).await {
                Err(_) => return Connect::Retry(format!("timed out connecting to {HOST}")),
                Ok(Err(err)) => {
                    return Connect::Retry(format!("could not reach {HOST}:{PORT}: {err}"));
                }
                Ok(Ok(tcp)) => tcp,
            };
        let _ = tcp.set_nodelay(true);

        let tls = match async_native_tls::connect(HOST, tcp).await {
            Ok(tls) => tls,
            Err(err) => return Connect::Retry(format!("the TLS handshake failed: {err}")),
        };

        // `async-imap` has no `connect()` helper, so build the client and
        // consume the server greeting before logging in.
        let mut client = async_imap::Client::new(tls);
        if let Err(err) = client.read_response().await {
            return Connect::Retry(format!("no greeting from {HOST}: {err}"));
        }

        match client.login(&self.user, self.secret.expose()).await {
            Ok(session) => {
                self.session = Some(session);
                self.selected.clear();
                Connect::Ready
            }
            Err((err, _client)) => login_failure(&self.user, err),
        }
    }

    /// Refresh the folder list, then restore whichever folder was selected.
    async fn bootstrap(&mut self) -> Dispatch {
        let mailboxes = self.list_mailboxes().await?;

        let target = mailboxes
            .iter()
            .find(|mailbox| mailbox.name.eq_ignore_ascii_case(&self.selected))
            .or_else(|| mailboxes.first())
            .map(|mailbox| mailbox.name.clone());

        self.send(NetMsg::Mailboxes(mailboxes)).await;

        match target {
            Some(target) => self.load_headers(&target).await,
            None => Err(Failure::Command(
                "the account has no selectable folders".to_string(),
            )),
        }
    }

    /// Serve commands until the UI goes away or the connection dies.
    ///
    /// `backoff` is reset only once a command has actually succeeded: a server
    /// that accepts the connection and then immediately drops it must not be
    /// handed a fresh connection every second, because Gmail locks an account
    /// out for roughly a day when its connection limit is abused.
    async fn serve(&mut self, rx: &mut mpsc::Receiver<NetCmd>, backoff: &mut Duration) -> Serve {
        if let Err(failure) = self.bootstrap().await {
            match failure {
                Failure::Disconnected(message) => return Serve::Disconnected(message),
                Failure::Command(message) => {
                    if !self.report(&message).await {
                        return Serve::Quit;
                    }
                }
            }
        } else {
            *backoff = BACKOFF_START;
        }

        while let Some(cmd) = rx.recv().await {
            match self.dispatch(cmd).await {
                Ok(()) => *backoff = BACKOFF_START,
                Err(Failure::Disconnected(message)) => return Serve::Disconnected(message),
                Err(Failure::Command(message)) => {
                    if !self.report(&message).await {
                        return Serve::Quit;
                    }
                }
            }
        }

        Serve::Quit
    }

    async fn dispatch(&mut self, cmd: NetCmd) -> Dispatch {
        match cmd {
            NetCmd::FetchMailboxes => {
                let mailboxes = self.list_mailboxes().await?;
                self.send(NetMsg::Mailboxes(mailboxes)).await;
                Ok(())
            }
            NetCmd::FetchHeaders { mailbox } => self.load_headers(&mailbox).await,
            NetCmd::FetchBody { mailbox, uid } => self.load_body(&mailbox, uid).await,
            NetCmd::SetSeen { mailbox, uid, seen } => self.set_seen(&mailbox, uid, seen).await,
            NetCmd::Search { mailbox, query } => self.search(&mailbox, &query).await,
            NetCmd::Send { to, cc, bcc, subject, body } => {
                // SMTP is a separate connection, so a failure here says nothing
                // about the IMAP session. It is reported to the composer rather
                // than fed to the reconnect logic.
                match self.send_mail(&to, &cc, &bcc, &subject, &body).await {
                    Ok(()) => {
                        self.send(NetMsg::Sent { to }).await;
                    }
                    Err(reason) => {
                        self.send(NetMsg::SendFailed(reason)).await;
                    }
                }
                Ok(())
            }
        }
    }

    /// `LIST` every folder, skipping the non-selectable containers, and ask
    /// each one for its message and unread counts.
    async fn list_mailboxes(&mut self) -> std::result::Result<Vec<Mailbox>, Failure> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| Failure::Disconnected(connection_lost()))?;

        let names: Vec<Name> = session
            .list(None, Some("*"))
            .await
            .map_err(|err| classify(err, "LIST"))?
            .try_collect()
            .await
            .map_err(|err| classify(err, "LIST"))?;

        let mut mailboxes = Vec::with_capacity(names.len());
        for name in &names {
            let attributes = name.attributes();
            if attributes
                .iter()
                .any(|attribute| matches!(attribute, NameAttribute::NoSelect))
            {
                // `[Gmail]` itself is only a container; its children are the
                // folders that matter.
                continue;
            }

            let raw = name.name();
            // `STATUS … UNSEEN` is a count; the `UNSEEN` code on a SELECT
            // response is a sequence number, so it must not be used here.
            let unread = session
                .status(raw, "(MESSAGES UNSEEN)")
                .await
                .ok()
                .and_then(|status| status.unseen)
                .unwrap_or(0);

            mailboxes.push(Mailbox::new(raw, display_name(raw, attributes), unread));
        }

        mailboxes.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then_with(|| a.display.cmp(&b.display))
        });
        Ok(mailboxes)
    }

    async fn load_headers(&mut self, mailbox: &str) -> Dispatch {
        self.status(format!("loading {mailbox} …")).await;

        // SELECT first: the fetch is windowed by sequence number, so `exists`
        // has to be known before the newest slice can be asked for.
        let exists = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;
            session
                .select(mailbox)
                .await
                .map_err(|err| classify(err, &format!("SELECT {mailbox}")))?
                .exists
        };
        self.selected = mailbox.to_string();

        if exists == 0 {
            self.send(NetMsg::Headers {
                mailbox: mailbox.to_string(),
                headers: Vec::new(),
            })
            .await;
            self.status(format!("{mailbox}: empty")).await;
            return Ok(());
        }

        let first = exists.saturating_sub(HEADER_LIMIT) + 1;
        let fetches: Vec<Fetch> = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;
            session
                .fetch(format!("{first}:*"), "(UID FLAGS ENVELOPE INTERNALDATE)")
                .await
                .map_err(|err| classify(err, &format!("FETCH {mailbox}")))?
                .try_collect()
                .await
                .map_err(|err| classify(err, &format!("FETCH {mailbox}")))?
        };

        // Newest first, which is how the list is read.
        let mut headers: Vec<EmailSummary> = fetches.iter().map(summarise).collect();
        headers.reverse();

        self.send(NetMsg::Headers {
            mailbox: mailbox.to_string(),
            headers,
        })
        .await;
        self.status(format!("{mailbox}: {exists} message(s)")).await;
        Ok(())
    }

    async fn load_body(&mut self, mailbox: &str, uid: u32) -> Dispatch {
        let already_selected = self.selected.eq_ignore_ascii_case(mailbox);

        // Ask for the MIME skeleton first: it is a few hundred bytes even when
        // the message carries megabytes of attachments, and it names the one
        // part worth downloading.
        let section = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;

            if !already_selected {
                session
                    .select(mailbox)
                    .await
                    .map_err(|err| classify(err, &format!("SELECT {mailbox}")))?;
            }

            let fetches: Vec<Fetch> = session
                .uid_fetch(uid.to_string(), "(BODYSTRUCTURE)")
                .await
                .map_err(|err| classify(err, "FETCH BODYSTRUCTURE"))?
                .try_collect()
                .await
                .map_err(|err| classify(err, "FETCH BODYSTRUCTURE"))?;

            fetches
                .first()
                .and_then(Fetch::bodystructure)
                .and_then(pick_text_section)
        };
        self.selected = mailbox.to_string();

        let body = match section {
            Some(section) => match self.fetch_section(uid, &section).await {
                Ok(Some(body)) => body,
                // The response did not carry what was asked for; take the long way.
                Ok(None) => self.fetch_whole(uid).await?,
                Err(Failure::Disconnected(message)) => {
                    return Err(Failure::Disconnected(message));
                }
                Err(Failure::Command(message)) => {
                    // A server that will not serve a single section is not
                    // fatal — the whole message still works.
                    self.status(format!("{message}; fetching the whole message"))
                        .await;
                    self.fetch_whole(uid).await?
                }
            },
            None => self.fetch_whole(uid).await?,
        };

        self.send(NetMsg::Body(body)).await;
        Ok(())
    }

    /// Fetch the headers plus the single text part `section` names.
    ///
    /// `Ok(None)` means the response did not contain what was asked for, which
    /// is the cue to fall back to a whole-message fetch.
    async fn fetch_section(
        &mut self,
        uid: u32,
        section: &TextSection,
    ) -> std::result::Result<Option<EmailBody>, Failure> {
        let query = format!("(BODY.PEEK[HEADER] BODY.PEEK[{}])", section.label());

        let fetches: Vec<Fetch> = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;
            session
                .uid_fetch(uid.to_string(), query)
                .await
                .map_err(|err| classify(err, "FETCH"))?
                .try_collect()
                .await
                .map_err(|err| classify(err, "FETCH"))?
        };

        let Some(fetch) = fetches.first() else {
            return Ok(None);
        };

        let path = SectionPath::Part(section.path.clone(), None);
        let (Some(header), Some(raw)) = (fetch.header(), fetch.section(&path)) else {
            return Ok(None);
        };

        Ok(Some(mime::body_from_parts(
            header,
            mime::decode_section(raw, &section.content_type, &section.encoding),
        )))
    }

    /// Download the entire message, attachments and all.
    async fn fetch_whole(&mut self, uid: u32) -> std::result::Result<EmailBody, Failure> {
        let raw = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;

            // `BODY.PEEK[]` rather than `BODY[]`: merely opening a message must
            // not mark it read. Setting `\Seen` is a deliberate, separate act.
            let fetches: Vec<Fetch> = session
                .uid_fetch(uid.to_string(), "(BODY.PEEK[])")
                .await
                .map_err(|err| classify(err, "FETCH"))?
                .try_collect()
                .await
                .map_err(|err| classify(err, "FETCH"))?;

            let Some(body) = fetches.first().and_then(Fetch::body) else {
                return Err(Failure::Command(format!(
                    "the server returned no body for uid {uid}"
                )));
            };
            body.to_vec()
        };

        Ok(mime::parse_body(&raw))
    }

    /// Set or clear `\Seen` on one message.
    ///
    /// Gmail echoes the new flags as untagged `FETCH` responses, so the returned
    /// stream is drained before the session is touched again.
    async fn set_seen(&mut self, mailbox: &str, uid: u32, seen: bool) -> Dispatch {
        let already_selected = self.selected.eq_ignore_ascii_case(mailbox);

        {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;

            if !already_selected {
                session
                    .select(mailbox)
                    .await
                    .map_err(|err| classify(err, &format!("SELECT {mailbox}")))?;
            }

            let query = if seen {
                "+FLAGS (\\Seen)"
            } else {
                "-FLAGS (\\Seen)"
            };
            let updates = session
                .uid_store(uid.to_string(), query)
                .await
                .map_err(|err| classify(err, "STORE"))?;
            updates
                .try_collect::<Vec<Fetch>>()
                .await
                .map_err(|err| classify(err, "STORE"))?;
        }

        self.selected = mailbox.to_string();
        self.send(NetMsg::Seen {
            mailbox: mailbox.to_string(),
            uid,
            seen,
        })
        .await;
        Ok(())
    }

    /// Hand a composed message to Gmail's submission server.
    ///
    /// `From` is always the authenticated address, because Gmail rejects
    /// anything else. Gmail files an accepted message into Sent by itself, so
    /// there is no `APPEND` step.
    async fn send_mail(
        &self,
        to: &str,
        cc: &str,
        bcc: &str,
        subject: &str,
        body: &str,
    ) -> std::result::Result<(), String> {
        let mut builder = Message::builder()
            .from(parse_address(&self.user, "the configured Gmail address")?)
            .subject(subject)
            .date_now();

        // Each field may hold a comma- or semicolon-separated list; `Cc` and
        // `Bcc` are frequently left empty, which simply adds no recipient.
        for address in parse_addresses(to, "the \"To\" address")? {
            builder = builder.to(address);
        }
        for address in parse_addresses(cc, "a \"Cc\" address")? {
            builder = builder.cc(address);
        }
        for address in parse_addresses(bcc, "a \"Bcc\" address")? {
            builder = builder.bcc(address);
        }

        let message = builder
            .body(body.to_string())
            .map_err(|err| format!("could not build the message: {err}"))?;

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(SMTP_HOST)
            .map_err(|err| format!("could not set up {SMTP_HOST}: {err}"))?
            .credentials(Credentials::new(
                self.user.clone(),
                self.secret.expose().to_string(),
            ))
            .build::<Tokio1Executor>();

        mailer
            .send(message)
            .await
            .map(|_accepted| ())
            .map_err(|err| format!("{SMTP_HOST}: {err}"))
    }

    async fn search(&mut self, mailbox: &str, query: &str) -> Dispatch {
        let uids = {
            let already_selected = self.selected.eq_ignore_ascii_case(mailbox);
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;

            if !already_selected {
                session
                    .select(mailbox)
                    .await
                    .map_err(|err| classify(err, &format!("SELECT {mailbox}")))?;
            }

            session
                .uid_search(query)
                .await
                .map_err(|err| classify(err, "SEARCH"))?
        };
        self.selected = mailbox.to_string();

        if uids.is_empty() {
            self.send(NetMsg::Headers {
                mailbox: mailbox.to_string(),
                headers: Vec::new(),
            })
            .await;
            self.status(format!("{mailbox} \"{query}\": no matches")).await;
            return Ok(());
        }

        // Newest first, and no more than a folder listing would show.
        let mut uids: Vec<u32> = uids.into_iter().collect();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(HEADER_LIMIT as usize);
        let uid_set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");

        let fetches: Vec<Fetch> = {
            let session = self
                .session
                .as_mut()
                .ok_or_else(|| Failure::Disconnected(connection_lost()))?;
            session
                .uid_fetch(uid_set, "(UID FLAGS ENVELOPE INTERNALDATE)")
                .await
                .map_err(|err| classify(err, "FETCH"))?
                .try_collect()
                .await
                .map_err(|err| classify(err, "FETCH"))?
        };

        let mut headers: Vec<EmailSummary> = fetches.iter().map(summarise).collect();
        headers.reverse();

        let hits = headers.len();
        self.send(NetMsg::Headers {
            mailbox: mailbox.to_string(),
            headers,
        })
        .await;
        self.status(format!("{mailbox} \"{query}\": {hits} hit(s)"))
            .await;
        Ok(())
    }

    async fn send(&self, msg: NetMsg) -> bool {
        self.tx.send(msg).await.is_ok()
    }

    async fn status(&self, text: impl Into<String>) -> bool {
        self.send(NetMsg::Status(text.into())).await
    }

    async fn report(&self, message: &str) -> bool {
        self.send(NetMsg::Error(message.to_string())).await
    }

    /// Sleep, then report whether the UI is still listening.
    async fn wait(&self, backoff: Duration) -> bool {
        tokio::time::sleep(backoff).await;
        !self.tx.is_closed()
    }
}

fn connection_lost() -> String {
    "the connection to Gmail was lost".to_string()
}

/// The one part of a message a reading pane should download.
struct TextSection {
    /// IMAP part path: `[1, 2]` names section `1.2`.
    path: Vec<u32>,
    /// A synthesised `Content-Type`, carrying whatever charset `BODYSTRUCTURE`
    /// reported. A section fetched on its own arrives without its own headers.
    content_type: String,
    /// `Content-Transfer-Encoding`, as the server reported it.
    encoding: String,
}

impl TextSection {
    fn label(&self) -> String {
        self.path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Pick the part to read: `text/plain` in preference to `text/html`.
fn pick_text_section(structure: &BodyStructure<'_>) -> Option<TextSection> {
    let mut plain = Vec::new();
    let mut html = Vec::new();
    collect_text(structure, &mut Vec::new(), &mut plain, &mut html);

    plain.into_iter().next().or_else(|| html.into_iter().next())
}

/// Walk the MIME tree, recording where each readable text part lives.
///
/// Only lone text parts are considered. An attached `message/rfc822` is not
/// descended into, because fetching inside it would drag down the very payload
/// this exists to avoid.
fn collect_text(
    structure: &BodyStructure<'_>,
    path: &mut Vec<u32>,
    plain: &mut Vec<TextSection>,
    html: &mut Vec<TextSection>,
) {
    match structure {
        BodyStructure::Text { common, other, .. } => {
            let section = TextSection {
                // RFC 3501: a non-multipart message has exactly one part, so its
                // body text is section 1 even though nothing was numbered.
                path: if path.is_empty() {
                    vec![1]
                } else {
                    path.clone()
                },
                content_type: content_type_of(common),
                encoding: encoding_name(&other.transfer_encoding),
            };

            match common.ty.subtype.to_ascii_lowercase().as_str() {
                "plain" => plain.push(section),
                "html" => html.push(section),
                // Some other text flavour is no use to a plain-text reader.
                _ => {}
            }
        }
        BodyStructure::Multipart { bodies, .. } => {
            for (index, body) in bodies.iter().enumerate() {
                path.push(index as u32 + 1);
                collect_text(body, path, plain, html);
                path.pop();
            }
        }
        // `Basic` is non-text (an image, a PDF…).
        BodyStructure::Basic { .. } | BodyStructure::Message { .. } => {}
    }
}

/// `text/html; charset="utf-8"`, built from what `BODYSTRUCTURE` reported.
fn content_type_of(common: &BodyContentCommon<'_>) -> String {
    let ty = common.ty.ty.to_ascii_lowercase();
    let subtype = common.ty.subtype.to_ascii_lowercase();
    let mut value = format!("{ty}/{subtype}");

    for (key, val) in common.ty.params.iter().flatten() {
        value.push_str("; ");
        value.push_str(&key.to_ascii_lowercase());
        value.push_str("=\"");
        value.push_str(val);
        value.push('"');
    }

    value
}

/// The `Content-Transfer-Encoding` spelling for a `BODYSTRUCTURE` encoding.
fn encoding_name(encoding: &ContentEncoding<'_>) -> String {
    match encoding {
        ContentEncoding::SevenBit => "7bit",
        ContentEncoding::EightBit => "8bit",
        ContentEncoding::Binary => "binary",
        ContentEncoding::Base64 => "base64",
        ContentEncoding::QuotedPrintable => "quoted-printable",
        ContentEncoding::Other(value) => value,
    }
    .to_string()
}

/// Parse a bare address, naming the field in the error so a typo is actionable.
fn parse_address(value: &str, what: &str) -> std::result::Result<SmtpAddress, String> {
    value
        .parse()
        .map_err(|err| format!("{what} \"{value}\" is not a valid address: {err}"))
}

/// Parse a comma- or semicolon-separated address list. A blank value yields no
/// addresses, which is how an unused `Cc`/`Bcc` reaches the builder.
fn parse_addresses(value: &str, what: &str) -> std::result::Result<Vec<SmtpAddress>, String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| parse_address(part, what))
        .collect()
}

/// Decide whether a failed login is worth retrying.
fn login_failure(user: &str, err: ImapError) -> Connect {
    let message = err.to_string();
    let upper = message.to_uppercase();

    if matches!(&err, ImapError::ConnectionLost | ImapError::Io(_)) {
        return Connect::Retry(format!("the connection dropped during login: {message}"));
    }

    // Gmail reports a bad app password as `NO [AUTHENTICATIONFAILED]`.
    if upper.contains("AUTHENTICATIONFAILED") || upper.contains("INVALID CREDENTIALS") {
        return Connect::Fatal(format!(
            "Gmail rejected the app password for {user}.\n\
             Generate a new one at https://myaccount.google.com/apppasswords, \
             then run `gmail-tui --forget` and start again."
        ));
    }

    // Exceeding Gmail's ten simultaneous IMAP connections is account-locking,
    // so back off instead of hammering.
    if upper.contains("LIMIT") || upper.contains("TOO MANY") {
        return Connect::Retry(format!("Gmail is refusing further connections: {message}"));
    }

    Connect::Fatal(format!("login failed: {message}"))
}

/// Turn a command error into "retry" or "report and carry on".
fn classify(err: ImapError, what: &str) -> Failure {
    let message = format!("{what}: {err}");
    match &err {
        // A dead connection is worth reconnecting for.
        ImapError::ConnectionLost | ImapError::Io(_) => Failure::Disconnected(message),
        _ => Failure::Command(message),
    }
}

/// One row of the message list, from `ENVELOPE`, `FLAGS` and `INTERNALDATE`.
fn summarise(fetch: &Fetch) -> EmailSummary {
    let envelope = fetch.envelope();

    let subject = envelope
        .and_then(|envelope| envelope.subject.as_deref())
        .map(mime::decode_word)
        .filter(|subject| !subject.is_empty())
        .unwrap_or_else(|| "(no subject)".to_string());

    let from = envelope
        .and_then(|envelope| envelope.from.as_ref())
        .and_then(|addresses| addresses.first())
        .map(mime::format_address)
        .filter(|from| !from.is_empty())
        .unwrap_or_else(|| "(unknown sender)".to_string());

    let date = fetch
        .internal_date()
        .map(|date| date.format("%m/%d/%y").to_string())
        .unwrap_or_else(|| "—".to_string());

    EmailSummary {
        uid: fetch.uid.unwrap_or(0),
        date,
        from,
        subject,
        seen: fetch.flags().any(|flag| matches!(flag, Flag::Seen)),
    }
}

/// A friendly label for a folder.
///
/// Derived from the RFC 6154 special-use attributes rather than from the name
/// on the wire, because Gmail localises `[Gmail]/Sent Mail` and friends.
fn display_name(raw: &str, attributes: &[NameAttribute<'_>]) -> String {
    for attribute in attributes {
        let label = match attribute {
            NameAttribute::All => "All Mail",
            NameAttribute::Archive => "Archive",
            NameAttribute::Drafts => "Drafts",
            NameAttribute::Flagged => "Starred",
            NameAttribute::Junk => "Spam",
            NameAttribute::Sent => "Sent",
            NameAttribute::Trash => "Trash",
            // `NoSelect`, `Marked`, `Unmarked`, `NoInferiors` and extensions
            // carry no meaning for the label.
            _ => continue,
        };
        return label.to_string();
    }

    if raw.eq_ignore_ascii_case("INBOX") {
        return "Inbox".to_string();
    }

    // Fall back to the name itself, minus Gmail's synthetic parent.
    raw.strip_prefix("[Gmail]/").unwrap_or(raw).to_string()
}

/// Sort key: the Gmail folders in their familiar order, then everything else.
fn rank(mailbox: &Mailbox) -> u8 {
    match mailbox.display.as_str() {
        "Inbox" => 0,
        "Starred" => 1,
        "Sent" => 2,
        "Drafts" => 3,
        "All Mail" => 4,
        "Spam" => 5,
        "Trash" => 6,
        _ => 7,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use imap_proto::{BodyContentSinglePart, ContentType};

    use super::*;

    fn attributes(names: &[&'static str]) -> Vec<NameAttribute<'static>> {
        names
            .iter()
            .map(|name| match *name {
                "\\All" => NameAttribute::All,
                "\\Drafts" => NameAttribute::Drafts,
                "\\Flagged" => NameAttribute::Flagged,
                "\\Junk" => NameAttribute::Junk,
                "\\Sent" => NameAttribute::Sent,
                "\\Trash" => NameAttribute::Trash,
                "\\Noselect" => NameAttribute::NoSelect,
                "\\Archive" => NameAttribute::Archive,
                other => NameAttribute::Extension(Cow::Borrowed(other)),
            })
            .collect()
    }

    #[test]
    fn names_folders_from_special_use_attributes() {
        // Gmail localises these names on the wire, so the attribute wins.
        assert_eq!(
            display_name("[Gmail]/Gesendet", &attributes(&["\\Sent"])),
            "Sent"
        );
        assert_eq!(display_name("[Gmail]/Wichtig", &attributes(&["\\Flagged"])), "Starred");
        assert_eq!(display_name("[Gmail]/Papierkorb", &attributes(&["\\Trash"])), "Trash");
        assert_eq!(display_name("[Gmail]/Spam", &attributes(&["\\Junk"])), "Spam");
        assert_eq!(
            display_name("[Gmail]/Alle Nachrichten", &attributes(&["\\All"])),
            "All Mail"
        );
        assert_eq!(display_name("[Gmail]/Entwuerfe", &attributes(&["\\Drafts"])), "Drafts");
    }

    #[test]
    fn names_the_inbox_by_its_wire_name() {
        // `NameAttribute` has no `\Inbox` variant, so INBOX is matched by name.
        assert_eq!(display_name("INBOX", &[]), "Inbox");
        assert_eq!(display_name("inbox", &[]), "Inbox");
    }

    #[test]
    fn falls_back_to_the_wire_name_for_user_folders() {
        assert_eq!(display_name("Work/Projects", &[]), "Work/Projects");
        assert_eq!(display_name("[Gmail]/Whatever", &[]), "Whatever");
    }

    #[test]
    fn orders_the_standard_folders_first() {
        let mut mailboxes = [
            Mailbox::new("Work", "Work", 0),
            Mailbox::new("[Gmail]/Trash", "Trash", 0),
            Mailbox::new("INBOX", "Inbox", 3),
            Mailbox::new("[Gmail]/Sent Mail", "Sent", 0),
        ];
        mailboxes.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.display.cmp(&b.display)));

        let order: Vec<&str> = mailboxes.iter().map(|m| m.display.as_str()).collect();
        assert_eq!(order, ["Inbox", "Sent", "Trash", "Work"]);
    }

    #[test]
    fn parses_a_single_address() {
        let addresses = parse_addresses("bob@example.com", "the \"To\" address").expect("one");
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].email.to_string(), "bob@example.com");
    }

    #[test]
    fn parses_a_comma_or_semicolon_separated_list() {
        let addresses =
            parse_addresses("a@example.com, b@example.com; c@example.com", "a \"Cc\" address")
                .expect("three addresses");
        let emails: Vec<String> = addresses.iter().map(|a| a.email.to_string()).collect();
        assert_eq!(emails, ["a@example.com", "b@example.com", "c@example.com"]);
    }

    #[test]
    fn a_blank_list_yields_no_addresses() {
        assert!(parse_addresses("", "a \"Cc\" address").expect("empty").is_empty());
        assert!(parse_addresses(" , ; ", "a \"Cc\" address").expect("blank").is_empty());
    }

    #[test]
    fn an_invalid_address_names_its_field() {
        let err = parse_addresses("not an address", "a \"Bcc\" address").unwrap_err();
        assert!(err.contains("a \"Bcc\" address"), "{err}");
    }

    #[test]
    fn a_dropped_connection_is_retried_and_a_bad_password_is_not() {
        assert!(matches!(
            classify(ImapError::ConnectionLost, "FETCH"),
            Failure::Disconnected(_)
        ));
        assert!(matches!(
            classify(ImapError::No("nope".into()), "FETCH"),
            Failure::Command(_)
        ));

        assert!(matches!(
            login_failure("me@example.com", ImapError::No("[AUTHENTICATIONFAILED] Invalid credentials".into())),
            Connect::Fatal(_)
        ));
        assert!(matches!(
            login_failure("me@example.com", ImapError::No("Too many simultaneous connections".into())),
            Connect::Retry(_)
        ));
        assert!(matches!(
            login_failure("me@example.com", ImapError::ConnectionLost),
            Connect::Retry(_)
        ));
    }

    #[test]
    fn classifies_reports_are_not_retried() {
        // A `BAD` is a bug in our command, not a dead socket.
        assert!(matches!(
            classify(ImapError::Bad("bogus".into()), "LIST"),
            Failure::Command(_)
        ));
    }

    fn text_part(subtype: &'static str) -> BodyStructure<'static> {
        BodyStructure::Text {
            common: BodyContentCommon {
                ty: ContentType {
                    ty: Cow::Borrowed("TEXT"),
                    subtype: Cow::Borrowed(subtype),
                    params: Some(vec![(Cow::Borrowed("CHARSET"), Cow::Borrowed("UTF-8"))]),
                },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::Base64,
                octets: 12,
            },
            lines: 1,
            extension: None,
        }
    }

    fn attachment() -> BodyStructure<'static> {
        BodyStructure::Basic {
            common: BodyContentCommon {
                ty: ContentType {
                    ty: Cow::Borrowed("APPLICATION"),
                    subtype: Cow::Borrowed("PDF"),
                    params: None,
                },
                disposition: None,
                language: None,
                location: None,
            },
            other: BodyContentSinglePart {
                id: None,
                md5: None,
                description: None,
                transfer_encoding: ContentEncoding::Base64,
                octets: 38838,
            },
            extension: None,
        }
    }

    fn multipart(
        subtype: &'static str,
        bodies: Vec<BodyStructure<'static>>,
    ) -> BodyStructure<'static> {
        BodyStructure::Multipart {
            common: BodyContentCommon {
                ty: ContentType {
                    ty: Cow::Borrowed("MULTIPART"),
                    subtype: Cow::Borrowed(subtype),
                    params: None,
                },
                disposition: None,
                language: None,
                location: None,
            },
            bodies,
            extension: None,
        }
    }

    #[test]
    fn a_single_part_message_is_section_one() {
        let section = pick_text_section(&text_part("PLAIN")).expect("a text part");
        assert_eq!(section.label(), "1");
        assert_eq!(section.content_type, "text/plain; charset=\"UTF-8\"");
        assert_eq!(section.encoding, "base64");
    }

    #[test]
    fn a_multipart_prefers_plain_over_html() {
        let structure = multipart("ALTERNATIVE", vec![text_part("PLAIN"), text_part("HTML")]);
        assert_eq!(pick_text_section(&structure).expect("a text part").label(), "1");
    }

    #[test]
    fn an_attachment_before_the_text_only_shifts_the_number() {
        // The PDF is section 1, so the message body is section 2 — and the PDF
        // is never asked for.
        let structure = multipart("MIXED", vec![attachment(), text_part("PLAIN")]);
        assert_eq!(pick_text_section(&structure).expect("a text part").label(), "2");
    }

    #[test]
    fn nested_multiparts_number_their_parts() {
        // multipart/related > multipart/alternative > [plain, html]
        let inner = multipart("ALTERNATIVE", vec![text_part("PLAIN"), text_part("HTML")]);
        let outer = multipart("RELATED", vec![inner, attachment()]);
        assert_eq!(pick_text_section(&outer).expect("a text part").label(), "1.1");
    }

    #[test]
    fn html_is_used_when_there_is_no_plain_part() {
        let structure = multipart("ALTERNATIVE", vec![text_part("HTML")]);
        assert_eq!(
            pick_text_section(&structure).expect("a text part").content_type,
            "text/html; charset=\"UTF-8\""
        );
    }

    #[test]
    fn a_message_with_no_text_part_has_no_section() {
        assert!(pick_text_section(&attachment()).is_none());
        assert!(pick_text_section(&multipart("MIXED", vec![attachment()])).is_none());
    }
}
