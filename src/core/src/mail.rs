use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use mailparse::{DispositionType, MailHeaderMap, ParsedMail};
use serde::Serialize;
#[cfg(not(target_arch = "wasm32"))]
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};
use utoipa::ToSchema;

#[derive(Clone, Serialize, ToSchema)]
pub struct Attachment {
    pub attachment_id: String,
    pub filename: String,
    pub content_type: String,
    pub size: usize,
    pub content_disposition: String,
}

#[derive(Serialize, ToSchema)]
pub struct ParsedEmail {
    #[serde(flatten)]
    pub metadata: crate::model::Email,
    pub cc: String,
    pub date: String,
    pub forwarded_to: Option<String>,
    pub text_body: String,
    pub text_body_truncated: bool,
    pub html_available: bool,
    pub attachments: Vec<Attachment>,
    pub parse_error: bool,
}

/// An ordered, decoded MIME header. Repeated header names remain separate entries.
#[derive(Serialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

/// One decoded mailbox. Address groups are flattened for host mail APIs.
#[derive(Serialize)]
pub struct MessageAddress {
    pub name: String,
    pub address: String,
}

/// A decoded MIME attachment using the same identifier as the mailbox API.
#[derive(Serialize)]
pub struct MessageAttachment {
    /// Content-Type retains its parameters here; decoded bytes keep their charset.
    #[serde(flatten)]
    pub metadata: Attachment,
    pub content_id: Option<String>,
    pub content_base64: String,
}

/// Internal projection used to construct host-language email objects.
///
/// Bodies contain the complete decoded text, not the CLI preview. HTML is the
/// original content and is not sanitized for display. This model flattens body
/// alternatives and address groups; use the original MIME bytes for fidelity.
#[derive(Serialize)]
pub struct MailMessage {
    pub metadata: crate::model::Email,
    pub headers: Vec<MessageHeader>,
    pub subject: String,
    pub from: Vec<MessageAddress>,
    pub sender: Vec<MessageAddress>,
    pub reply_to: Vec<MessageAddress>,
    pub to: Vec<MessageAddress>,
    pub cc: Vec<MessageAddress>,
    pub bcc: Vec<MessageAddress>,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub attachments: Vec<MessageAttachment>,
}

/// The header fields stored with every received message.
pub struct Summary {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub message_id: Option<String>,
}

pub fn summary(raw: &[u8]) -> Summary {
    let headers = mailparse::parse_headers(raw)
        .map(|x| x.0)
        .unwrap_or_default();
    let header = |name| {
        headers
            .get_first_value(name)
            .map(|s| s.chars().take(4096).collect::<String>())
    };
    Summary {
        from: header("From").unwrap_or_default(),
        to: header("To").unwrap_or_default(),
        subject: header("Subject").unwrap_or_default(),
        message_id: header("Message-ID"),
    }
}

/// Decodes a bounded message for native host mail APIs without another parser.
/// Parsing failures are explicit and never include untrusted message content.
pub fn mail_message(metadata: crate::model::Email, raw: &[u8]) -> Result<MailMessage> {
    if raw.len() > crate::protocol::MAX_EMAIL {
        bail!("invalid_message: message exceeds 10 MiB")
    }

    let parsed = mailparse::parse_mail(raw)
        .map_err(|_| anyhow::anyhow!("invalid_message: invalid MIME message"))?;
    let mut message = MailMessage {
        metadata,
        headers: parsed
            .headers
            .iter()
            .map(|header| MessageHeader {
                name: header.get_key(),
                value: header.get_value(),
            })
            .collect(),
        subject: parsed
            .headers
            .get_first_value("Subject")
            .unwrap_or_default(),
        from: message_addresses(&parsed, "From")?,
        sender: message_addresses(&parsed, "Sender")?,
        reply_to: message_addresses(&parsed, "Reply-To")?,
        to: message_addresses(&parsed, "To")?,
        cc: message_addresses(&parsed, "Cc")?,
        bcc: message_addresses(&parsed, "Bcc")?,
        text_body: None,
        html_body: None,
        attachments: Vec::new(),
    };
    message_bodies(&parsed, &mut message.text_body, &mut message.html_body)?;

    for (index, part) in attachment_parts(&parsed).iter().enumerate() {
        let mut metadata = attachment_metadata(index, part)
            .map_err(|_| anyhow::anyhow!("invalid_message: invalid attachment"))?;
        if let Some(content_type) = part.headers.get_first_value("Content-Type") {
            metadata.content_type = content_type;
        }

        let content = part
            .get_body_raw()
            .map_err(|_| anyhow::anyhow!("invalid_message: invalid attachment encoding"))?;
        let content_id = part.headers.get_first_value("Content-ID").map(|id| {
            let id = id.trim();
            id.strip_prefix('<')
                .and_then(|id| id.strip_suffix('>'))
                .unwrap_or(id)
                .to_owned()
        });

        message.attachments.push(MessageAttachment {
            metadata,
            content_id,
            content_base64: STANDARD.encode(content),
        });
    }

    Ok(message)
}

fn message_addresses(parsed: &ParsedMail<'_>, name: &str) -> Result<Vec<MessageAddress>> {
    let mut addresses = Vec::new();
    for header in parsed
        .headers
        .iter()
        .filter(|header| header.get_key_ref().eq_ignore_ascii_case(name))
    {
        // Parse the original header so encoded commas stay inside display names.
        let values = mailparse::addrparse_header(header)
            .map_err(|_| anyhow::anyhow!("invalid_message: invalid mailbox header"))?;
        for value in values.into_inner() {
            let mailboxes = match value {
                mailparse::MailAddr::Single(mailbox) => vec![mailbox],
                mailparse::MailAddr::Group(group) => group.addrs,
            };

            addresses.extend(mailboxes.into_iter().map(|mailbox| MessageAddress {
                name: mailbox.display_name.unwrap_or_default(),
                address: mailbox.addr,
            }));
        }
    }

    Ok(addresses)
}

pub(crate) fn message_bodies(
    part: &ParsedMail<'_>,
    text: &mut Option<String>,
    html: &mut Option<String>,
) -> Result<()> {
    // An attached multipart document must not become part of its parent's body.
    if is_attachment(part) {
        return Ok(());
    }

    let body = match part.ctype.mimetype.as_str() {
        "text/plain" => Some(&mut *text),
        "text/html" => Some(&mut *html),
        _ => None,
    };
    if let Some(body) = body {
        let decoded = part
            .get_body()
            .map_err(|_| anyhow::anyhow!("invalid_message: invalid body encoding"))?;

        body.get_or_insert_with(String::new).push_str(&decoded);
    }

    for child in &part.subparts {
        message_bodies(child, text, html)?;
    }

    Ok(())
}

pub(crate) fn is_attachment(part: &ParsedMail<'_>) -> bool {
    let disposition = part.get_content_disposition();
    disposition.disposition == DispositionType::Attachment
        || disposition.params.contains_key("filename")
        || part.ctype.params.contains_key("name")
        || (part.subparts.is_empty()
            && !part.ctype.mimetype.starts_with("text/")
            && !part.ctype.mimetype.starts_with("multipart/"))
}

pub fn truncate_utf8(value: &str, limit: usize) -> String {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }

    value[..end].to_owned()
}

/// Bidi controls and zero-width characters can disguise a name, such as an
/// executable displayed with a document extension.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !invisible(*c) && !"<>:\"|?*".contains(*c))
        .collect();
    let cleaned = truncate_utf8(cleaned.trim_matches([' ', '.']), 180);
    // Windows ignores trailing spaces here, so `NUL .txt` still opens the device.
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    let port = |prefix| {
        stem.strip_prefix(prefix).is_some_and(|n: &str| {
            (n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
                || ["\u{B9}", "\u{B2}", "\u{B3}"].contains(&n)
        })
    };
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || port("COM")
        || port("LPT");

    if cleaned.is_empty() {
        "attachment.bin".into()
    } else if reserved {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

fn attachment_parts<'a>(mail: &'a ParsedMail<'a>) -> Vec<&'a ParsedMail<'a>> {
    mail.parts().filter(|p| is_attachment(p)).collect()
}

fn attachment_metadata(index: usize, part: &ParsedMail<'_>) -> Result<Attachment> {
    let disposition = part.get_content_disposition();
    let name = disposition
        .params
        .get("filename")
        .or_else(|| part.ctype.params.get("name"))
        .cloned()
        .unwrap_or_else(|| format!("attachment-{}.bin", index + 1));
    let content_disposition = match disposition.disposition {
        DispositionType::Attachment => "attachment",
        DispositionType::Inline => "inline",
        _ => "other",
    };

    Ok(Attachment {
        attachment_id: (index + 1).to_string(),
        filename: safe_filename(&name),
        content_type: truncate_utf8(&part.ctype.mimetype, 128),
        size: part.get_body_raw()?.len(),
        content_disposition: content_disposition.into(),
    })
}

pub fn parse(metadata: crate::model::Email, raw: &[u8], max_body: usize) -> Result<ParsedEmail> {
    if max_body == 0 || max_body > 256 * 1024 {
        bail!("invalid_argument: max_body_bytes must be 1..262144")
    }

    let mut result = ParsedEmail {
        metadata,
        cc: String::new(),
        date: String::new(),
        forwarded_to: None,
        text_body: String::new(),
        text_body_truncated: false,
        html_available: false,
        attachments: Vec::new(),
        parse_error: false,
    };
    let parsed = match mailparse::parse_mail(raw) {
        Ok(mail) => mail,
        Err(_) => {
            result.parse_error = true;
            return Ok(result);
        }
    };
    result.cc = truncate_utf8(
        &parsed.headers.get_first_value("Cc").unwrap_or_default(),
        4096,
    );
    result.date = truncate_utf8(
        &parsed.headers.get_first_value("Date").unwrap_or_default(),
        4096,
    );
    // Presentation metadata only. Do not claim forwarding to a different inbox
    // just because an untrusted MIME header names it.
    let forwarded = parsed.headers.get_all_values("X-Forwarded-To");
    if !forwarded.is_empty()
        && forwarded.iter().all(|value| {
            mailparse::addrparse(value)
                .ok()
                .and_then(|list| list.extract_single_info())
                .and_then(|a| crate::auth::decode_address(&a.addr).ok())
                .is_some_and(|(_, _, recipient)| recipient == result.metadata.recipient)
        })
    {
        result.forwarded_to = Some(result.metadata.recipient.clone());
    }

    for part in parsed.parts() {
        if is_attachment(part) {
            continue;
        }
        if part.ctype.mimetype == "text/html" {
            result.html_available = true;
        }

        if part.ctype.mimetype == "text/plain" {
            match part.get_body() {
                Ok(body) => {
                    let remaining = max_body.saturating_sub(result.text_body.len());
                    let piece = truncate_utf8(&body, remaining);
                    result.text_body_truncated |= piece.len() < body.len();
                    result.text_body.push_str(&piece);
                }
                Err(_) => result.parse_error = true,
            }
        }
    }

    for (index, part) in attachment_parts(&parsed).iter().enumerate() {
        match attachment_metadata(index, part) {
            Ok(a) => result.attachments.push(a),
            Err(_) => result.parse_error = true,
        }
    }

    Ok(result)
}

pub fn attachment(raw: &[u8], id: &str) -> Result<(Attachment, Vec<u8>)> {
    let index = id
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .context("invalid_argument: invalid attachment id")?
        - 1;
    let parsed = mailparse::parse_mail(raw)
        .map_err(|_| anyhow::anyhow!("invalid_message: invalid MIME message"))?;
    let parts = attachment_parts(&parsed);
    let part = parts
        .get(index)
        .context("attachment_not_found: attachment does not exist")?;
    let invalid = || anyhow::anyhow!("invalid_message: invalid attachment encoding");

    Ok((
        attachment_metadata(index, part).map_err(|_| invalid())?,
        part.get_body_raw().map_err(|_| invalid())?,
    ))
}

pub use crate::email_html::HtmlEmail;

pub fn html_body(raw: &[u8], allow_remote: bool) -> Result<HtmlEmail> {
    let parsed = mailparse::parse_mail(raw)
        .map_err(|_| anyhow::anyhow!("invalid_message: invalid MIME message"))?;
    let part = parsed
        .parts()
        .find(|p| !is_attachment(p) && p.ctype.mimetype == "text/html")
        .context("html_not_found: email has no HTML body")?;
    let html = part
        .get_body()
        .map_err(|_| anyhow::anyhow!("invalid_message: invalid body encoding"))?;
    crate::email_html::render(&html, &parsed, allow_remote)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn save_attachment(metadata: &Attachment, bytes: &[u8], destination: &Path) -> Result<PathBuf> {
    let target = if destination.is_dir() {
        destination.join(safe_filename(&metadata.filename))
    } else {
        destination.to_owned()
    };
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let target = parent
        .canonicalize()
        .context("invalid_argument: attachment destination directory does not exist")?
        .join(
            target
                .file_name()
                .context("invalid_argument: invalid attachment destination")?,
        );
    let mut f = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("invalid_argument: attachment destination already exists")
        }
        Err(error) => return Err(anyhow::Error::from(error).context("cannot create attachment")),
    };
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&target);
        return Err(e.into());
    }

    Ok(target)
}

/// Format a timestamp for display in the machine's timezone; keep invalid headers readable.
pub fn local_date(value: &str, format: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(value))
        .map(|date| {
            date.with_timezone(&chrono::Local)
                .format(format)
                .to_string()
        })
        .unwrap_or_else(|_| value.to_owned())
}

pub fn terminal_safe(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod message_tests {
    use super::*;

    #[test]
    fn filenames_drop_disguising_characters_and_windows_devices() {
        // U+202E would display `invoice\u{202E}fdp.exe` as `invoiceexe.pdf`.
        assert_eq!(safe_filename("invoice\u{202E}fdp.exe"), "invoicefdp.exe");
        assert_eq!(
            safe_filename("\u{200B}report\u{2066}.pdf\u{FEFF}"),
            "report.pdf"
        );
        for device in ["NUL .txt", "CONOUT$", "conin$.log", "COM\u{B9}.txt", "lpt3"] {
            assert!(safe_filename(device).starts_with('_'), "{device}");
        }

        assert_eq!(safe_filename("COMPUTER.txt"), "COMPUTER.txt");
    }

    fn metadata(raw: &[u8]) -> crate::model::Email {
        crate::model::Email {
            id: 7,
            transfer_id: "synthetic-transfer".into(),
            identity_fingerprint: "synthetic-identity".into(),
            recipient: "recipient@example.test".into(),
            envelope_sender: "sender@example.test".into(),
            received_at: "2026-10-02T12:00:00Z".into(),
            from_header: String::new(),
            to_header: String::new(),
            subject: "stored preview".into(),
            message_id: None,
            size_bytes: raw.len() as i64,
        }
    }

    #[test]
    fn projection_preserves_decoded_headers_addresses_bodies_and_attachment_bytes() {
        let nested =
            b"From: nested@example.test\r\nSubject: Attached message\r\n\r\nNested body\r\n";
        let raw = format!(
            concat!(
                "From: =?UTF-8?Q?Ana=2C_Jos=C3=A9?= <sender@example.test>\r\n",
                "Sender: courier@example.test\r\n",
                "To: Team: \"One, Two\" <one@example.test>, two@example.test;\r\n",
                "To: third@example.test\r\n",
                "Cc: copy@example.test\r\nBcc: blind@example.test\r\n",
                "Reply-To: =?UTF-8?Q?Suporte?= <reply@example.test>\r\n",
                "Subject: =?UTF-8?Q?Ol=C3=A1?=\r\n",
                "X-Trace: first\r\nX-Trace: second\r\n continued\r\n",
                "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n",
                "--outer\r\nContent-Type: multipart/alternative; boundary=alt\r\n\r\n",
                "--alt\r\nContent-Type: text/plain; charset=iso-8859-1\r\n",
                "Content-Transfer-Encoding: quoted-printable\r\n\r\nOl=E1, mundo.\r\n",
                "--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
                "<p>Olá <img src=\"cid:pixel\"></p>\r\n--alt--\r\n",
                "--outer\r\nContent-Type: image/png\r\n",
                "Content-Disposition: inline; filename=pixel.png\r\nContent-ID: <pixel>\r\n",
                "Content-Transfer-Encoding: base64\r\n\r\nAQIDBA==\r\n",
                "--outer\r\nContent-Type: application/octet-stream\r\n",
                "Content-Disposition: attachment; filename=payload.bin\r\n",
                "Content-Transfer-Encoding: quoted-printable\r\n\r\n=00=FF=80\r\n",
                "--outer\r\nContent-Type: message/rfc822\r\n",
                "Content-Disposition: attachment; filename=forwarded.eml\r\n",
                "Content-Transfer-Encoding: base64\r\n\r\n{}\r\n--outer--\r\n"
            ),
            STANDARD.encode(nested)
        );
        let raw = raw.as_bytes();
        let message = mail_message(metadata(raw), raw).unwrap();

        assert_eq!(message.metadata.id, 7);
        assert_eq!(message.subject, "Olá");
        assert_eq!(message.from[0].name, "Ana, José");
        assert_eq!(message.from[0].address, "sender@example.test");
        assert_eq!(message.sender[0].address, "courier@example.test");
        assert_eq!(message.to.len(), 3);
        assert_eq!(message.to[0].name, "One, Two");
        assert_eq!(message.to[2].address, "third@example.test");
        assert_eq!(message.cc[0].address, "copy@example.test");
        assert_eq!(message.bcc[0].address, "blind@example.test");
        assert_eq!(message.reply_to[0].address, "reply@example.test");
        let trace: Vec<_> = message
            .headers
            .iter()
            .filter(|header| header.name.eq_ignore_ascii_case("X-Trace"))
            .map(|header| header.value.as_str())
            .collect();

        assert_eq!(trace, ["first", "second continued"]);
        assert_eq!(message.text_body.as_deref(), Some("Olá, mundo."));
        assert_eq!(
            message.html_body.as_deref(),
            Some("<p>Olá <img src=\"cid:pixel\"></p>")
        );
        assert_eq!(message.attachments.len(), 3);
        let expected: [&[u8]; 3] = [&[1, 2, 3, 4], &[0, 255, 128], nested];

        for (index, projected) in message.attachments.iter().enumerate() {
            let id = (index + 1).to_string();
            let (legacy, bytes) = attachment(raw, &id).unwrap();

            assert_eq!(projected.metadata.attachment_id, id);
            assert_eq!(projected.metadata.filename, legacy.filename);
            assert_eq!(bytes, expected[index]);
            assert_eq!(STANDARD.decode(&projected.content_base64).unwrap(), bytes);
        }

        assert_eq!(message.attachments[0].content_id.as_deref(), Some("pixel"));
        assert_eq!(
            message.attachments[0].metadata.content_disposition,
            "inline"
        );
        assert_eq!(
            message.attachments[2].metadata.content_type,
            "message/rfc822"
        );
        let wire = serde_json::to_value(&message).unwrap();

        assert_eq!(wire["metadata"]["id"], 7);
        assert_eq!(wire["attachments"][0]["attachment_id"], "1");
        assert!(wire.get("content_base64").is_none());
    }

    #[test]
    fn projection_keeps_full_bodies_and_excludes_attachment_descendants() {
        let body = "é".repeat(140 * 1024);
        let raw = format!("Content-Type: text/plain; charset=utf-8\r\n\r\n{body}");
        let message = mail_message(metadata(raw.as_bytes()), raw.as_bytes()).unwrap();

        assert_eq!(message.text_body.as_deref(), Some(body.as_str()));
        assert!(message.html_body.is_none());

        let raw = concat!(
            "Content-Type: multipart/mixed; boundary=outer\r\n\r\n",
            "--outer\r\nContent-Type: text/plain\r\n\r\nVisible body\r\n",
            "--outer\r\nContent-Type: multipart/alternative; boundary=attached\r\n",
            "Content-Disposition: attachment; filename=alternative.mime\r\n\r\n",
            "--attached\r\nContent-Type: text/plain\r\n\r\nAttached plain body\r\n",
            "--attached\r\nContent-Type: text/html\r\n\r\n<p>Attached HTML body</p>\r\n",
            "--attached--\r\n--outer--\r\n"
        )
        .as_bytes();
        let message = mail_message(metadata(raw), raw).unwrap();

        assert_eq!(message.text_body.as_deref(), Some("Visible body"));
        assert!(message.html_body.is_none());
        assert_eq!(message.attachments[0].metadata.attachment_id, "1");
    }

    #[test]
    fn projection_preserves_attachment_content_type_parameters_without_transcoding_bytes() {
        let raw = concat!(
            "Content-Type: text/plain; charset=iso-8859-1;\r\n",
            " name*=utf-8''caf%C3%A9.txt\r\n",
            "Content-Disposition: attachment; filename*=utf-8''caf%C3%A9.txt\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n\r\nOl=E1"
        )
        .as_bytes();
        let message = mail_message(metadata(raw), raw).unwrap();

        assert!(message.text_body.is_none());
        let projected = &message.attachments[0];

        assert_eq!(projected.metadata.filename, "café.txt");
        assert_eq!(
            projected.metadata.content_type,
            "text/plain; charset=iso-8859-1; name*=utf-8''caf%C3%A9.txt"
        );
        assert_eq!(
            STANDARD.decode(&projected.content_base64).unwrap(),
            b"Ol\xe1"
        );

        assert_eq!(attachment(raw, "1").unwrap().0.content_type, "text/plain");
    }

    #[test]
    fn missing_parts_and_existing_destinations_report_stable_error_codes() {
        let code = |error: anyhow::Error| error.to_string().split(':').next().unwrap().to_owned();
        let raw = b"Content-Type: text/plain\r\n\r\nNo HTML";
        let mut malformed = String::from("Content-Type: multipart/mixed; boundary=m\r\n\r\n");

        for _ in 0..256 {
            malformed.push_str("--m\r\nContent-Type: text/plain\r\n\r\npart\r\n");
        }

        assert_eq!(
            code(attachment(raw, "1").err().unwrap()),
            "attachment_not_found"
        );
        assert_eq!(
            code(attachment(raw, "first").err().unwrap()),
            "invalid_argument"
        );
        assert_eq!(code(html_body(raw, false).err().unwrap()), "html_not_found");
        assert_eq!(
            code(html_body(malformed.as_bytes(), false).err().unwrap()),
            "invalid_message"
        );
        let directory = tempfile::tempdir().unwrap();
        let metadata = Attachment {
            attachment_id: "1".into(),
            filename: "file.txt".into(),
            content_type: "text/plain".into(),
            size: 5,
            content_disposition: "attachment".into(),
        };
        save_attachment(&metadata, b"saved", directory.path()).unwrap();

        for destination in [
            directory.path().to_owned(),
            directory.path().join("missing").join("file.txt"),
        ] {
            assert_eq!(
                code(
                    save_attachment(&metadata, b"other", &destination)
                        .err()
                        .unwrap()
                ),
                "invalid_argument"
            );
        }
    }

    #[test]
    fn projection_rejects_bad_addresses_and_existing_mime_complexity_limits() {
        let invalid = b"From: private-invalid-address\r\n\r\nbody";
        let error = mail_message(metadata(invalid), invalid).err().unwrap();

        assert!(error.to_string().starts_with("invalid_message:"));
        assert!(!error.to_string().contains("private-invalid-address"));

        let mut raw = String::from("Content-Type: multipart/mixed; boundary=m\r\n\r\n");

        for _ in 0..256 {
            raw.push_str("--m\r\nContent-Type: text/plain\r\n\r\npart\r\n");
        }

        raw.push_str("--m--\r\n");

        assert!(mailparse::parse_mail(raw.as_bytes()).is_err());
        let error = mail_message(metadata(raw.as_bytes()), raw.as_bytes())
            .err()
            .unwrap();

        assert!(error.to_string().starts_with("invalid_message:"));
    }
}
