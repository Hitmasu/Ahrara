//! Ahrara's Web UI. The CLI serves it from its local server; the browser app
//! answers the same routes from a Service Worker.
use ahrara_core::{
    mail::{self, Attachment, HtmlEmail, ParsedEmail},
    model::{Address, CustomDomain, Email, Filters},
};
use anyhow::{Result, anyhow, bail};
use askama::Template;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use serde::Deserialize;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

#[cfg(target_arch = "wasm32")]
mod browser;
pub mod i18n;

use i18n::Strings;

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
// The browser app shows every page inside its own same-origin shell.
const BROWSER_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'self'";
const HTML_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox";
const HTML_IMAGES_CSP: &str = "default-src 'none'; img-src data: http: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox";

/// Who serves the UI. Only the CLI has a local server to configure.
pub enum Mode {
    Cli(LocalSettings),
    Browser,
}

impl Mode {
    pub fn browser(&self) -> bool {
        matches!(self, Self::Browser)
    }
}

/// The CLI's local server, as shown on the Settings page.
pub struct LocalSettings {
    pub url: String,
    pub lan_enabled: bool,
    pub lan_no_auth_acknowledged: bool,
    pub password: bool,
    pub api_enabled: bool,
    pub mcp_enabled: bool,
    pub mcp_auth_required: bool,
}

pub struct Status {
    pub connected: bool,
    pub emails: i64,
    pub fingerprint: String,
    pub route: String,
}

/// The data behind the UI: the CLI's encrypted profile or the browser's IndexedDB.
#[allow(async_fn_in_trait)] // Callers name the concrete backend, so the CLI's futures stay Send.
pub trait Backend {
    fn version(&self) -> &'static str;
    fn mode(&self) -> Mode;
    /// The token every form must echo.
    fn csrf(&self) -> &str;
    async fn status(&self) -> Result<Status>;
    async fn latest_enabled_address(&self) -> Result<Option<Address>>;
    async fn inbox_page(
        &self,
        filters: Filters,
        before: Option<i64>,
        search: String,
    ) -> Result<Vec<Email>>;
    async fn addresses(&self, after_id: i64, limit: u32) -> Result<Vec<Address>>;
    async fn email(&self, id: i64) -> Result<(Email, Vec<u8>)>;
    async fn create_address(&self, label: Option<String>) -> Result<()>;
    async fn change_address(&self, id: i64, enabled: Option<bool>) -> Result<()>;
    async fn delete_email(&self, id: i64) -> Result<()>;
    async fn custom_domain(&self) -> Result<Option<CustomDomain>>;
    async fn set_custom_domain(&self, domain: String, forwarding_address: String) -> Result<()>;
    async fn remove_custom_domain(&self) -> Result<()>;
    /// The language chosen in Settings; none follows the browser's.
    async fn language(&self) -> Option<String>;
    async fn set_language(&self, language: Option<String>) -> Result<()>;

    // MIME work runs inline by default; the CLI moves it off its async workers.
    async fn parsed_email(&self, id: i64, max_body: usize) -> Result<ParsedEmail> {
        let (metadata, raw) = self.email(id).await?;
        mail::parse(metadata, &raw, max_body)
    }

    async fn html_email(&self, id: i64, allow_remote: bool) -> Result<HtmlEmail> {
        mail::html_body(&self.email(id).await?.1, allow_remote)
    }

    async fn attachment(&self, id: i64, attachment_id: &str) -> Result<(Attachment, Vec<u8>)> {
        mail::attachment(&self.email(id).await?.1, attachment_id)
    }

    /// Errors that mean the item is gone (404) rather than a bad request (400).
    fn not_found(&self, error: &anyhow::Error) -> bool {
        missing(error)
    }
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .to_string()
        .split_once(':')
        .is_some_and(|(code, _)| code.ends_with("_not_found"))
}

/// Answers one UI request. Adapters own access control, the body limit and
/// their own routes (the CLI's login, server settings and events).
pub async fn respond<B: Backend>(backend: &B, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let language = backend.language().await;
    let t = strings(language.as_deref(), &request);
    let mode = backend.mode();
    let browser = mode.browser();
    let ui = Ui { mode, t, language };
    let mut response = route(backend, ui, request)
        .await
        .unwrap_or_else(|error| failure(backend.not_found(&error), t));
    harden(response.headers_mut(), browser);
    response
}

/// The language for a request: the saved choice, else the browser's.
pub fn strings<B>(language: Option<&str>, request: &Request<B>) -> &'static Strings {
    let accept = request.headers().get(header::ACCEPT_LANGUAGE);
    i18n::negotiate(language, accept.and_then(|value| value.to_str().ok()))
}

/// What every page of one request shares.
struct Ui {
    mode: Mode,
    t: &'static Strings,
    language: Option<String>,
}

fn failure(missing: bool, t: &Strings) -> Response<Vec<u8>> {
    reply(
        if missing {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        },
        "text/plain; charset=utf-8",
        if missing { t.not_found } else { t.bad_request },
    )
}

fn verify(csrf: &str, expected: &str) -> Result<()> {
    if !bool::from(csrf.as_bytes().ct_eq(expected.as_bytes())) {
        bail!("invalid form token")
    }

    Ok(())
}

/// Security headers for every UI response; route-specific values win.
pub fn harden(headers: &mut HeaderMap, browser: bool) {
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(HeaderValue::from_static(if browser {
            BROWSER_CSP
        } else {
            CSP
        }));
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(HeaderValue::from_static("same-origin"));
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

async fn route<B: Backend>(
    backend: &B,
    ui: Ui,
    request: Request<Vec<u8>>,
) -> Result<Response<Vec<u8>>> {
    let t = ui.t;
    let (parts, body) = request.into_parts();
    let post = match parts.method {
        Method::GET => false,
        Method::POST => true,
        _ => return Ok(reply(StatusCode::METHOD_NOT_ALLOWED, "text/plain", "")),
    };
    let query = parts.uri.query().unwrap_or("");
    let form = || -> Result<Action> {
        let form: Action = serde_urlencoded::from_bytes(&body)?;
        verify(&form.csrf, backend.csrf())?;
        Ok(form)
    };
    let segments: Vec<&str> = parts.uri.path().split('/').skip(1).collect();

    match (post, segments.as_slice()) {
        (false, [""]) => inbox(backend, ui, serde_urlencoded::from_str(query)?).await,
        (false, ["emails", id]) => {
            let id = id.parse()?;
            let mut page = Page::new(backend, ui, "email", t.email).await?;
            let mut email = backend.parsed_email(id, 256 * 1024).await?;
            email.date = mail::local_date(&email.date, "%Y-%m-%d %H:%M:%S %:z");
            page.size = bytes(email.metadata.size_bytes.max(0) as usize);
            page.received = date(&email.metadata.received_at, t);
            if email.html_available {
                page.remote_resources = backend.html_email(id, false).await?.remote_resources;
            }

            page.email = Some(email);
            page.response()
        }
        (true, ["emails", id, "delete"]) => {
            if !form()?.confirm {
                bail!("confirmation required")
            }

            backend.delete_email(id.parse()?).await?;
            Ok(redirect("/"))
        }
        (false, ["emails", id, "raw"]) => {
            let id = id.parse()?;
            let mut page = Page::new(backend, ui, "email", t.raw_eml).await?;
            let (_, raw) = backend.email(id).await?;
            page.raw = Some(String::from_utf8_lossy(&raw).into_owned());
            page.email = Some(backend.parsed_email(id, 1).await?);
            page.response()
        }
        // POST loads remote images for this one view; consent is never stored.
        (_, ["emails", id, "html"]) => {
            if post {
                form()?;
            }

            let mut response = html(backend.html_email(id.parse()?, post).await?.html);
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(if post { HTML_IMAGES_CSP } else { HTML_CSP }),
            );
            headers.insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            Ok(response)
        }
        (false, ["emails", id, "eml"]) => {
            let (metadata, raw) = backend.email(id.parse()?).await?;
            Ok(eml(metadata.id, raw))
        }
        (false, ["emails", id, "attachments", attachment]) => {
            let (metadata, bytes) = backend.attachment(id.parse()?, attachment).await?;
            Ok(download(&metadata, bytes))
        }
        (false, ["addresses"]) => {
            let query: AddressQuery = serde_urlencoded::from_str(query)?;
            let mut page = Page::new(backend, ui, "addresses", t.addresses).await?;
            page.custom_domain = backend.custom_domain().await?;
            page.addresses = backend.addresses(query.after_id.unwrap_or(0), 21).await?;
            if page.addresses.len() > 20 {
                page.addresses.pop();
                page.next = page.addresses.last().map(|a| a.id).unwrap_or(0);
            }

            page.previous = query.after_id.is_some();
            page.response()
        }
        (true, ["addresses"]) => {
            let label = form()?.label.filter(|l| !l.trim().is_empty());
            if label.as_ref().is_some_and(|l| l.len() > 120) {
                bail!("label too long")
            }

            backend.create_address(label).await?;
            Ok(redirect("/addresses"))
        }
        (true, ["addresses", "domain"]) => {
            let form = form()?;
            match form.action.as_str() {
                "remove" => backend.remove_custom_domain().await?,
                "set" => {
                    backend
                        .set_custom_domain(
                            form.domain.ok_or_else(|| anyhow!("domain required"))?,
                            form.forwarding_address
                                .ok_or_else(|| anyhow!("forwarding destination required"))?,
                        )
                        .await?
                }
                _ => bail!("invalid domain action"),
            }

            Ok(redirect("/addresses"))
        }
        (true, ["addresses", id]) => {
            let form = form()?;
            let enabled = match form.action.as_str() {
                "enable" => Some(true),
                "disable" => Some(false),
                "delete" if form.confirm => None,
                _ => bail!("confirm action"),
            };
            backend.change_address(id.parse()?, enabled).await?;
            Ok(redirect("/addresses"))
        }
        (false, ["settings"]) => Page::new(backend, ui, "settings", t.settings)
            .await?
            .response(),
        (true, ["settings", "language"]) => {
            let language = form()?.language.filter(|l| !l.is_empty());
            if language
                .as_deref()
                .is_some_and(|code| !i18n::LANGUAGES.iter().any(|s| s.code == code))
            {
                bail!("unknown language")
            }

            backend.set_language(language).await?;
            Ok(redirect("/settings"))
        }
        // The browser app's host serves these files itself.
        #[cfg(not(target_arch = "wasm32"))]
        (false, ["assets", name]) => Ok(asset(name)),
        _ => Ok(reply(StatusCode::NOT_FOUND, "text/plain", "")),
    }
}

async fn inbox<B: Backend>(backend: &B, ui: Ui, q: InboxQuery) -> Result<Response<Vec<u8>>> {
    let t = ui.t;
    let mut page = Page::new(backend, ui, "inbox", t.inbox).await?;
    let filters = Filters {
        address: Some(q.address.clone()).filter(|a| !a.is_empty()),
        limit: Some(21),
        ..Default::default()
    };
    let mut emails = backend.inbox_page(filters, q.before, q.q.clone()).await?;
    if emails.len() > 20 {
        emails.pop();
        page.next = emails.last().map(|e| e.id).unwrap_or(0);
    }

    page.rows = emails.into_iter().map(|email| Row::new(email, t)).collect();
    page.query = q.q;
    page.address = q.address;
    page.previous = q.before.is_some();
    // The filter accepts any exact local address, including aliases beyond this first selector page.
    page.addresses = backend.addresses(0, 100).await?;
    page.response()
}

#[derive(Default, Deserialize)]
struct InboxQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    address: String,
    before: Option<i64>,
}

#[derive(Default, Deserialize)]
struct AddressQuery {
    after_id: Option<i64>,
}

#[derive(Deserialize)]
struct Action {
    csrf: String,
    #[serde(default)]
    action: String,
    label: Option<String>,
    domain: Option<String>,
    forwarding_address: Option<String>,
    language: Option<String>,
    #[serde(default)]
    confirm: bool,
}

struct Row {
    id: i64,
    from: String,
    subject: String,
    received: String,
    recipient: String,
    size: String,
}

impl Row {
    fn new(e: Email, t: &Strings) -> Self {
        Self {
            id: e.id,
            from: if e.from_header.is_empty() {
                e.envelope_sender
            } else {
                e.from_header
            },
            subject: if e.subject.is_empty() {
                t.no_subject.into()
            } else {
                e.subject
            },
            received: date(&e.received_at, t),
            recipient: e.recipient,
            size: bytes(e.size_bytes.max(0) as usize),
        }
    }
}

fn date(s: &str, t: &Strings) -> String {
    mail::local_date(s, t.date_format)
}

pub fn bytes(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.)
    } else {
        format!("{:.1} MiB", n as f64 / 1048576.)
    }
}

#[derive(Template)]
#[template(path = "page.html")]
struct Page {
    section: &'static str,
    title: &'static str,
    version: &'static str,
    csrf: String,
    connected: bool,
    total: i64,
    default_address: String,
    fingerprint: String,
    route: String,
    mode: Mode,
    t: &'static Strings,
    /// The saved language; none follows the browser's.
    language: Option<String>,
    query: String,
    address: String,
    rows: Vec<Row>,
    addresses: Vec<Address>,
    next: i64,
    previous: bool,
    email: Option<ParsedEmail>,
    size: String,
    received: String,
    raw: Option<String>,
    remote_resources: usize,
    custom_domain: Option<CustomDomain>,
    /// The browser's Recovery Key, shown on request only.
    recovery: Option<Zeroizing<String>>,
    import_error: bool,
}

impl Page {
    fn timestamp(&self, value: &str) -> String {
        date(value, self.t)
    }

    fn languages(&self) -> [&'static Strings; 3] {
        i18n::LANGUAGES
    }

    fn selected(&self, code: &str) -> bool {
        self.language.as_deref() == Some(code)
    }

    async fn new<B: Backend>(
        backend: &B,
        Ui { mode, t, language }: Ui,
        section: &'static str,
        title: &'static str,
    ) -> Result<Self> {
        let Status {
            connected,
            emails,
            fingerprint,
            route,
        } = backend.status().await?;

        Ok(Self {
            section,
            title,
            version: backend.version(),
            csrf: backend.csrf().to_owned(),
            connected,
            total: emails,
            default_address: backend
                .latest_enabled_address()
                .await?
                .map(|a| a.address)
                .unwrap_or_default(),
            fingerprint,
            route,
            mode,
            t,
            language,
            query: String::new(),
            address: String::new(),
            rows: vec![],
            addresses: vec![],
            next: 0,
            previous: false,
            email: None,
            size: String::new(),
            received: String::new(),
            raw: None,
            remote_resources: 0,
            custom_domain: None,
            recovery: None,
            import_error: false,
        })
    }

    fn response(self) -> Result<Response<Vec<u8>>> {
        Ok(html(self.render()?))
    }
}

fn reply(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Vec<u8>>,
) -> Response<Vec<u8>> {
    let mut response = Response::new(body.into());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn html(body: String) -> Response<Vec<u8>> {
    reply(StatusCode::OK, "text/html; charset=utf-8", body)
}

fn redirect(location: &'static str) -> Response<Vec<u8>> {
    let mut response = reply(StatusCode::SEE_OTHER, "text/plain", "");
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static(location));
    response
}

/// The original message as a download; every byte is preserved.
pub fn eml(id: i64, raw: Vec<u8>) -> Response<Vec<u8>> {
    let mut response = reply(StatusCode::OK, "message/rfc822", raw);
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"email-{id}.eml\""))
            .expect("a numeric filename is a valid header"),
    );
    response
}

/// An attachment as an opaque download under an ASCII-only name.
pub fn download(attachment: &Attachment, bytes: Vec<u8>) -> Response<Vec<u8>> {
    let name: String = attachment
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut response = reply(StatusCode::OK, "application/octet-stream", bytes);
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
            .expect("an ASCII filename is a valid header"),
    );
    response
}

#[cfg(not(target_arch = "wasm32"))]
fn asset(name: &str) -> Response<Vec<u8>> {
    let (mime, bytes): (&'static str, &'static [u8]) = match name {
        "app.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../assets/app.css"),
        ),
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../assets/app.js"),
        ),
        "logo.png" => ("image/png", include_bytes!("../assets/logo.png")),
        _ => return reply(StatusCode::NOT_FOUND, "text/plain", ""),
    };
    let mut response = reply(StatusCode::OK, mime, bytes);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        pin::pin,
        task::{Context, Poll, Waker},
    };

    const RAW: &[u8] = b"From: sender@example.test\r\nSubject: Hello\r\n\r\nBody\r\n";

    #[derive(Default)]
    struct Memory {
        labels: RefCell<Vec<Option<String>>>,
        language: RefCell<Option<String>>,
    }

    impl Backend for Memory {
        fn version(&self) -> &'static str {
            "0.0.0"
        }

        fn mode(&self) -> Mode {
            Mode::Browser
        }

        fn csrf(&self) -> &str {
            "token"
        }

        async fn status(&self) -> Result<Status> {
            Ok(Status {
                connected: true,
                emails: 1,
                fingerprint: "fingerprint-value".into(),
                route: "route-value".into(),
            })
        }

        async fn latest_enabled_address(&self) -> Result<Option<Address>> {
            Ok(None)
        }

        async fn inbox_page(&self, _: Filters, _: Option<i64>, _: String) -> Result<Vec<Email>> {
            Ok(vec![self.email(1).await?.0])
        }

        async fn addresses(&self, _: i64, _: u32) -> Result<Vec<Address>> {
            Ok(vec![])
        }

        async fn email(&self, id: i64) -> Result<(Email, Vec<u8>)> {
            if id != 1 {
                bail!("email_not_found: email does not exist")
            }

            let email = Email {
                id,
                transfer_id: "transfer".into(),
                identity_fingerprint: "fingerprint-value".into(),
                recipient: "user@ahrara.dev".into(),
                envelope_sender: "sender@example.test".into(),
                received_at: "2026-10-06T12:00:00Z".into(),
                from_header: "sender@example.test".into(),
                to_header: "user@ahrara.dev".into(),
                subject: "Hello".into(),
                message_id: None,
                size_bytes: RAW.len() as i64,
            };
            Ok((email, RAW.to_vec()))
        }

        async fn create_address(&self, label: Option<String>) -> Result<()> {
            self.labels.borrow_mut().push(label);
            Ok(())
        }

        async fn change_address(&self, _: i64, _: Option<bool>) -> Result<()> {
            Ok(())
        }

        async fn delete_email(&self, _: i64) -> Result<()> {
            Ok(())
        }

        async fn custom_domain(&self) -> Result<Option<CustomDomain>> {
            Ok(None)
        }

        async fn set_custom_domain(&self, _: String, _: String) -> Result<()> {
            Ok(())
        }

        async fn remove_custom_domain(&self) -> Result<()> {
            Ok(())
        }

        async fn language(&self) -> Option<String> {
            self.language.borrow().clone()
        }

        async fn set_language(&self, language: Option<String>) -> Result<()> {
            *self.language.borrow_mut() = language;
            Ok(())
        }
    }

    fn send(backend: &Memory, method: &str, path: &str, body: &str) -> Response<Vec<u8>> {
        send_as(backend, method, path, body, "")
    }

    fn send_as(
        backend: &Memory,
        method: &str,
        path: &str,
        body: &str,
        languages: &str,
    ) -> Response<Vec<u8>> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::ACCEPT_LANGUAGE, languages)
            .body(body.as_bytes().to_vec())
            .unwrap();
        // The in-memory backend never waits.
        match pin!(respond(backend, request)).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(response) => response,
            Poll::Pending => unreachable!(),
        }
    }

    fn text(response: &Response<Vec<u8>>) -> &str {
        std::str::from_utf8(response.body()).unwrap()
    }

    #[test]
    fn browser_pages_frame_in_the_shell_and_explain_missing_options() {
        let backend = Memory::default();
        for path in ["/", "/emails/1", "/emails/1/raw", "/addresses", "/settings"] {
            let response = send(&backend, "GET", path, "");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert!(
                response.headers()[header::CONTENT_SECURITY_POLICY]
                    .to_str()
                    .unwrap()
                    .ends_with("frame-ancestors 'self'")
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert!(!text(&response).contains("Sign out") && !text(&response).contains("/mcp"));
            assert!(text(&response).contains(
                "href=\"https://ahrara.dev/docs/#receive-app-support\" target=\"_blank\""
            ));
        }

        // The installed app's server options stay visible, disabled and explained.
        let settings = send(&backend, "GET", "/settings", "");
        assert!(text(&settings).contains("fingerprint-value"));
        assert!(!text(&settings).contains("action=\"/settings\""));
        for tooltip in [i18n::EN.server_unavailable, i18n::EN.mcp_unavailable] {
            assert!(text(&settings).contains(&format!(
                "aria-disabled=\"true\" title=\"{}\"",
                tooltip.replace('\'', "&#39;")
            )));
        }
    }

    #[test]
    fn pages_follow_the_saved_language_then_the_browser() {
        let backend = Memory::default();
        let inbox = send_as(&backend, "GET", "/", "", "pt-BR,pt;q=0.9");
        assert!(text(&inbox).contains("<html lang=\"pt-BR\">"));
        assert!(text(&inbox).contains("Caixa de entrada"));
        let missing = send_as(&backend, "GET", "/emails/2", "", "es");
        assert_eq!(text(&missing), i18n::ES.not_found);

        let chosen = send(
            &backend,
            "POST",
            "/settings/language",
            "csrf=token&language=es",
        );
        assert_eq!(chosen.status(), StatusCode::SEE_OTHER);
        let settings = send_as(&backend, "GET", "/settings", "", "pt-BR");
        assert!(text(&settings).contains("<option value=\"es\" selected>Español</option>"));
        assert!(text(&settings).contains(i18n::ES.language_auto));
        let unknown = send(
            &backend,
            "POST",
            "/settings/language",
            "csrf=token&language=xx",
        );
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        send(
            &backend,
            "POST",
            "/settings/language",
            "csrf=token&language=",
        );
        assert!(text(&send(&backend, "GET", "/", "")).contains("<html lang=\"en\">"));
    }

    #[test]
    fn routes_check_tokens_methods_and_missing_items() {
        let backend = Memory::default();
        let rejected = send(&backend, "POST", "/addresses", "csrf=wrong&label=x");
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
        let created = send(&backend, "POST", "/addresses", "csrf=token&label=Work");
        assert_eq!(created.status(), StatusCode::SEE_OTHER);
        assert_eq!(created.headers()[header::LOCATION], "/addresses");
        assert_eq!(*backend.labels.borrow(), [Some("Work".to_owned())]);

        let eml = send(&backend, "GET", "/emails/1/eml", "");
        assert_eq!(eml.body(), RAW);
        assert_eq!(
            eml.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=\"email-1.eml\""
        );
        let html = send(&backend, "POST", "/emails/1/html", "csrf=wrong");
        assert_eq!(html.status(), StatusCode::BAD_REQUEST);

        assert_eq!(
            send(&backend, "GET", "/emails/2", "").status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(&backend, "GET", "/emails/x", "").status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send(&backend, "GET", "/unknown", "").status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            send(&backend, "PUT", "/", "").status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        let asset = send(&backend, "GET", "/assets/app.js", "");
        assert_eq!(
            asset.headers()[header::CACHE_CONTROL],
            "public, max-age=0, must-revalidate"
        );
    }
}
