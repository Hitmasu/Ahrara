use anyhow::{Context, Result, bail};
use mailparse::{DispositionType, MailHeaderMap, ParsedMail};
use serde::Serialize;
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
    pub metadata: crate::storage::Email,
    pub cc: String,
    pub date: String,
    pub text_body: String,
    pub text_body_truncated: bool,
    pub html_available: bool,
    pub attachments: Vec<Attachment>,
    pub parse_error: bool,
}

fn is_attachment(part: &ParsedMail<'_>) -> bool {
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

pub fn safe_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"|?*".contains(*c))
        .collect();
    let cleaned = truncate_utf8(cleaned.trim_matches([' ', '.']), 180);
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
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

pub fn parse(metadata: crate::storage::Email, raw: &[u8], max_body: usize) -> Result<ParsedEmail> {
    if max_body == 0 || max_body > 256 * 1024 {
        bail!("max_body_bytes must be 1..262144")
    }
    let mut result = ParsedEmail {
        metadata,
        cc: String::new(),
        date: String::new(),
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
        .context("invalid attachment id")?
        - 1;
    let parsed = mailparse::parse_mail(raw)?;
    let parts = attachment_parts(&parsed);
    let part = parts
        .get(index)
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    Ok((attachment_metadata(index, part)?, part.get_body_raw()?))
}

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
        .canonicalize()?
        .join(target.file_name().context("invalid destination")?);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .context("cannot create attachment; destination must not already exist")?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&target);
        return Err(e.into());
    }
    Ok(target)
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
