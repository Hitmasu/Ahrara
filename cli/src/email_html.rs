//! Transform visual resources before sanitization; the browser never receives active remote URLs by default.
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use cssparser::{Delimiter, Parser, ToCss};
use html5ever::tokenizer::{
    BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, states::RawKind,
};
use mailparse::{MailHeaderMap, ParsedMail};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

pub struct HtmlEmail {
    pub html: String,
    pub remote_resources: usize,
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn remote_url(value: &str) -> Option<String> {
    let value = value.trim();
    let url = ammonia::Url::parse(&if value.starts_with("//") {
        format!("https:{value}")
    } else {
        value.into()
    })
    .ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
fn remote_css(input: &mut Parser<'_>, depth: usize) -> bool {
    while let Ok(token) = input.next().cloned() {
        match token {
            cssparser::Token::UnquotedUrl(value) | cssparser::Token::QuotedString(value)
                if remote_url(&value).is_some() =>
            {
                return true;
            }
            cssparser::Token::Function(_)
            | cssparser::Token::CurlyBracketBlock
            | cssparser::Token::ParenthesisBlock
            | cssparser::Token::SquareBracketBlock
                if depth < 32
                    && input
                        .parse_nested_block(|p| {
                            Ok::<_, cssparser::ParseError<()>>(remote_css(p, depth + 1))
                        })
                        .unwrap_or(false) =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}
struct Images {
    output: RefCell<String>,
    remote: Cell<usize>,
    allow_remote: bool,
    inline: HashMap<String, String>,
    embedded_bytes: Cell<usize>,
    raw_tag: RefCell<Option<String>>,
}
impl Images {
    fn source(&self, value: &str) -> Option<String> {
        if let Some(url) = remote_url(value) {
            return self.allow_remote.then_some(url);
        }
        let (scheme, cid) = value.trim().split_once(':')?;
        if !scheme.eq_ignore_ascii_case("cid") {
            return None;
        }
        let data = self.inline.get(cid)?;
        let total = self.embedded_bytes.get().checked_add(data.len())?;
        // Bound repeated CID references independently of the original EML's 10 MiB limit.
        if total > 16 * 1024 * 1024 {
            return None;
        }
        self.embedded_bytes.set(total);
        Some(data.clone())
    }
    fn srcset(&self, value: &str) -> (String, bool) {
        let mut external = false;
        let candidates = value
            .split(',')
            .filter_map(|candidate| {
                let mut parts = candidate.split_ascii_whitespace();
                let url = parts.next()?;
                external |= remote_url(url).is_some();
                let descriptor = parts.next().unwrap_or("");
                if parts.next().is_some() {
                    return None;
                }
                if !descriptor.is_empty() {
                    let valid = descriptor
                        .strip_suffix('w')
                        .and_then(|v| v.parse::<u32>().ok())
                        .is_some_and(|v| v > 0)
                        || descriptor
                            .strip_suffix('x')
                            .and_then(|v| v.parse::<f64>().ok())
                            .is_some_and(|v| v.is_finite() && v > 0.0);
                    if !valid {
                        return None;
                    }
                }
                // Inline CID images use src; avoid putting data URLs into srcset's comma grammar.
                let url = remote_url(url)?;
                self.allow_remote.then(|| format!("{url} {descriptor}"))
            })
            .collect::<Vec<_>>()
            .join(", ");
        (candidates, external)
    }
    fn style(&self, value: &str) -> (String, bool) {
        let mut input = Parser::new(value);
        let mut output = String::new();
        let mut external = remote_css(&mut Parser::new(value), 0);
        while !input.is_exhausted() {
            let declaration: std::result::Result<_, cssparser::ParseError<()>> = input
                .parse_until_before(Delimiter::Semicolon, |p| {
                    let name = p.expect_ident()?.to_ascii_lowercase();
                    p.expect_colon()?;
                    if name == "background-image" {
                        let url = p.expect_url()?;
                        let _ = p.try_parse(cssparser::parse_important);
                        p.expect_exhausted()?;
                        external |= remote_url(&url).is_some();
                        return Ok(self
                            .source(&url)
                            .map(|url| {
                                format!(
                                    "background-image:url({});",
                                    cssparser::Token::QuotedString(url.into()).to_css_string()
                                )
                            })
                            .unwrap_or_default());
                    }
                    // Formatting only: no fonts, positioning, URLs, functions or custom properties.
                    if !matches!(
                        name.as_str(),
                        "color"
                            | "background-color"
                            | "font-size"
                            | "font-weight"
                            | "font-style"
                            | "text-align"
                            | "text-decoration"
                            | "width"
                            | "height"
                            | "max-width"
                            | "padding"
                            | "margin"
                            | "border"
                            | "display"
                            | "visibility"
                    ) {
                        return Err(cssparser::ParseError::from_basic_kind(
                            cssparser::BasicParseErrorKind::UnexpectedToken,
                        ));
                    }
                    let mut values = Vec::new();
                    while !p.is_exhausted() {
                        if p.try_parse(cssparser::parse_important).is_ok() {
                            p.expect_exhausted()?;
                            break;
                        }
                        let token = p.next()?;
                        if !matches!(
                            token,
                            cssparser::Token::Ident(_)
                                | cssparser::Token::Hash(_)
                                | cssparser::Token::IDHash(_)
                                | cssparser::Token::Number { .. }
                                | cssparser::Token::Dimension { .. }
                                | cssparser::Token::Percentage { .. }
                        ) {
                            return Err(cssparser::ParseError::from_basic_kind(
                                cssparser::BasicParseErrorKind::UnexpectedToken,
                            ));
                        }
                        values.push(token.to_css_string());
                    }
                    Ok(format!("{name}:{};", values.join(" ")))
                });
            if let Ok(style) = declaration {
                output.push_str(&style);
            }
            let _ = input.try_parse(|p| p.expect_semicolon());
        }
        (output, external)
    }
}
impl TokenSink for Images {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        if let Some(raw) = self.raw_tag.borrow().as_deref() {
            if let Token::CharacterTokens(text) = &token {
                if raw == "style" && remote_css(&mut Parser::new(text), 0) {
                    self.remote.set(self.remote.get() + 1);
                }
                return TokenSinkResult::Continue;
            }
            if matches!(&token, Token::TagToken(tag) if tag.kind == TagKind::EndTag && &*tag.name == raw)
            {
                // Release the RefCell borrow before clearing it below.
            } else {
                return TokenSinkResult::Continue;
            }
        }
        self.raw_tag.borrow_mut().take();
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                if matches!(
                    name,
                    "script" | "style" | "iframe" | "object" | "textarea" | "title"
                ) {
                    if tag.kind == TagKind::StartTag {
                        *self.raw_tag.borrow_mut() = Some(name.into());
                        return TokenSinkResult::RawData(RawKind::Rawtext);
                    }
                    return TokenSinkResult::Continue;
                }
                let mut output = self.output.borrow_mut();
                if tag.kind == TagKind::EndTag {
                    output.push_str(&format!("</{name}>"));
                    return TokenSinkResult::Continue;
                }
                let get = |attr: &str| {
                    tag.attrs
                        .iter()
                        .find(|a| &*a.name.local == attr)
                        .map(|a| a.value.as_ref())
                        .unwrap_or("")
                };
                let (srcset, external_set) = self.srcset(get("srcset"));
                let (style, external_style) = self.style(get("style"));
                let remote_src = remote_url(get("src"));
                let external = remote_src.is_some()
                    || external_set
                    || external_style
                    || (name == "link" && remote_url(get("href")).is_some())
                    || remote_url(get("background")).is_some();
                if external {
                    self.remote.set(self.remote.get() + 1);
                }
                if matches!(
                    name,
                    "link"
                        | "base"
                        | "meta"
                        | "form"
                        | "input"
                        | "button"
                        | "audio"
                        | "video"
                        | "embed"
                ) {
                    return TokenSinkResult::Continue;
                }
                let src = self.source(get("src"));
                if name == "img"
                    && src.is_none()
                    && (remote_src.is_some() || external_set)
                    && !self.allow_remote
                {
                    let tiny = ["width", "height"]
                        .iter()
                        .any(|n| get(n).parse::<u32>().is_ok_and(|n| n <= 1))
                        || style.contains("display:none")
                        || style.contains("visibility:hidden")
                        || style.contains("width:1px")
                        || style.contains("height:1px");
                    if !tiny {
                        output.push_str(&format!("<span class=\"remote-image\" role=\"img\" data-ahrara-remote-src=\"{}\"><span aria-hidden=\"true\">▧</span> {}<small>External image blocked</small></span>", escape(remote_src.as_deref().unwrap_or("")), escape(get("alt"))));
                    }
                    return TokenSinkResult::Continue;
                }
                output.push_str(&format!("<{name}"));
                for attr in &tag.attrs {
                    let key = attr.name.local.as_ref();
                    if matches!(
                        key,
                        "src" | "srcset" | "href" | "style" | "background" | "poster" | "class"
                    ) || key.starts_with("data-")
                        || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    {
                        continue;
                    }
                    output.push_str(&format!(" {key}=\"{}\"", escape(&attr.value)));
                }
                if name == "img"
                    && let Some(src) = src
                {
                    output.push_str(&format!(" src=\"{}\"", escape(&src)));
                }
                if matches!(name, "img" | "source") && !srcset.is_empty() {
                    output.push_str(&format!(" srcset=\"{}\"", escape(&srcset)));
                }
                if !style.is_empty() {
                    output.push_str(&format!(" style=\"{}\"", escape(&style)));
                }
                output.push('>');
            }
            Token::CharacterTokens(text) => self.output.borrow_mut().push_str(&escape(&text)),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

pub fn render(html: &str, parsed: &ParsedMail<'_>, allow_remote: bool) -> Result<HtmlEmail> {
    let mut inline = HashMap::new();
    for part in parsed.parts() {
        if matches!(
            part.ctype.mimetype.as_str(),
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/avif"
        ) && let Some(id) = part.headers.get_first_value("Content-ID")
        {
            let bytes = part.get_body_raw()?;
            inline
                .entry(id.trim().trim_matches(['<', '>']).to_owned())
                .or_insert_with(|| {
                    format!(
                        "data:{};base64,{}",
                        part.ctype.mimetype,
                        STANDARD.encode(bytes)
                    )
                });
        }
    }
    let tokenizer = Tokenizer::new(
        Images {
            output: RefCell::new(String::new()),
            remote: Cell::new(0),
            allow_remote,
            inline,
            embedded_bytes: Cell::new(0),
            raw_tag: RefCell::new(None),
        },
        Default::default(),
    );
    let input = BufferQueue::default();
    input.push_back(html.into());
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    let images = tokenizer.sink;
    let remote_resources = images.remote.get();
    // Ammonia remains the final HTML sanitizer after all resource rewriting.
    let clean = ammonia::Builder::default()
        .add_tags(&["picture", "source"])
        .add_tag_attributes("source", &["srcset", "media", "type"])
        .add_tag_attributes("img", &["srcset"])
        .add_tag_attributes(
            "span",
            &["class", "role", "aria-hidden", "data-ahrara-remote-src"],
        )
        .add_generic_attributes(&["style"])
        .url_schemes(["http", "https", "data"].into())
        .url_relative(ammonia::UrlRelative::Deny)
        .clean(&images.output.into_inner())
        .to_string();
    Ok(HtmlEmail {
        html: format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><style>body{{font:14px/1.6 system-ui;margin:16px;color:#172039;overflow-wrap:anywhere}}img{{max-width:100%;height:auto}}.remote-image{{display:inline-flex;flex-direction:column;gap:4px;padding:8px 12px;border:1px dashed #bdc6d4;color:#53637d;background:#f8f9fb;max-width:100%;box-sizing:border-box}}.remote-image small{{font-size:12px}}</style></head><body>{clean}</body></html>"
        ),
        remote_resources,
    })
}
