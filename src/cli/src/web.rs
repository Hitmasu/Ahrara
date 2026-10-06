use crate::{
    core::{Core, Status},
    mail::{self, ParsedEmail},
    storage::{Address, Email, Filters},
    web_state::Settings,
};
use anyhow::Result;
use askama::Template;
use axum::{
    Extension, Form, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::Deserialize;
use std::{
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; frame-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
const HTML_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox";
const HTML_IMAGES_CSP: &str = "default-src 'none'; img-src data: http: https:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'; sandbox";

struct WebError(anyhow::Error);

impl From<anyhow::Error> for WebError {
    fn from(e: anyhow::Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let missing = crate::api::not_found(&self.0);

        (
            if missing {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            },
            if missing {
                "This item is no longer available. Go back and refresh."
            } else {
                "The request could not be completed. Check the form and try again."
            },
        )
            .into_response()
    }
}

type WebResult<T> = std::result::Result<T, WebError>;

fn cookie_name(core: &Core) -> &'static str {
    if core.https_enabled() {
        "__Host-ahrara"
    } else {
        "ahrara"
    }
}

fn session_cookie(core: &Core, value: &str, max_age: u32) -> String {
    format!(
        "{}={value}; Path=/; {}HttpOnly; SameSite=Strict; Max-Age={max_age}",
        cookie_name(core),
        if core.https_enabled() { "Secure; " } else { "" }
    )
}

fn session<'a>(core: &Core, headers: &'a HeaderMap) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|s| s.trim().strip_prefix(&format!("{}=", cookie_name(core))))
}

pub(crate) fn loopback(ip: IpAddr) -> bool {
    ip.to_canonical().is_loopback()
}

fn private_ip(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn host_allowed(host: &str, core: &Core) -> bool {
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let hostname = authority.host().trim_matches(['[', ']']);
    let Ok(bind) = core.config.api_bind.parse::<SocketAddr>() else {
        return false;
    };
    if authority
        .port_u16()
        .unwrap_or(if core.https_enabled() { 443 } else { 80 })
        != bind.port()
    {
        return false;
    }

    hostname == "localhost"
        || hostname
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|ip| loopback(ip) || core.web.settings().lan_enabled && private_ip(ip))
}

pub(crate) fn local_request_allowed(core: &Core, request: &Request) -> bool {
    let settings = core.web.settings();
    let ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|a| a.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let host_header = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok());
    let authority = request.uri().authority().map(|a| a.as_str());
    if authority.zip(host_header).is_some_and(|(a, h)| a != h) {
        return false;
    }

    let host = authority.or(host_header).unwrap_or("");
    if !private_ip(ip) || !settings.lan_enabled && !loopback(ip) || !host_allowed(host, core) {
        return false;
    }

    let origin = request
        .headers()
        .get(header::ORIGIN)
        .map(|v| v.to_str().ok());
    if request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v == "cross-site")
        || origin.is_some_and(|origin| {
            origin != Some(format!("{}://{host}", core.http_scheme()).as_str())
        })
    {
        return false;
    }

    true
}

async fn guard(State(core): State<Arc<Core>>, request: Request, next: Next) -> Response {
    if !core.web.settings().web_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !local_request_allowed(&core, &request) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let path = request.uri().path();
    if path != "/login"
        && !path.starts_with("/assets/")
        && !core.web.authenticated(session(&core, request.headers()))
    {
        return Redirect::to("/login").into_response();
    }

    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry(header::CONTENT_SECURITY_POLICY)
        .or_insert(header::HeaderValue::from_static(CSP));
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(header::HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers
        .entry(header::REFERRER_POLICY)
        .or_insert(header::HeaderValue::from_static("same-origin"));
    headers.insert(
        "permissions-policy",
        header::HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );

    response
}

#[derive(Default, Deserialize)]
pub struct InboxQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    address: String,
    before: Option<i64>,
}

struct Row {
    id: i64,
    from: String,
    subject: String,
    received: String,
    recipient: String,
    size: String,
}

impl From<Email> for Row {
    fn from(e: Email) -> Self {
        Self {
            id: e.id,
            from: if e.from_header.is_empty() {
                e.envelope_sender
            } else {
                e.from_header
            },
            subject: if e.subject.is_empty() {
                "(no subject)".into()
            } else {
                e.subject
            },
            received: date(&e.received_at),
            recipient: e.recipient,
            size: bytes(e.size_bytes.max(0) as usize),
        }
    }
}

fn date(s: &str) -> String {
    mail::local_date(s, "%b %d, %H:%M %:z")
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
    settings: Settings,
    mcp_auth_required: bool,
    url: String,
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
    custom_domain: Option<crate::storage::CustomDomain>,
}

impl Page {
    fn timestamp(&self, value: &str) -> String {
        date(value)
    }

    async fn new(core: &Core, section: &'static str, title: &'static str) -> Result<Self> {
        let Status {
            connected,
            emails_count,
            identity_fingerprint,
            route_id,
            ..
        } = core.status().await?;

        Ok(Self {
            section,
            title,
            version: env!("CARGO_PKG_VERSION"),
            csrf: core.web.csrf.clone(),
            connected,
            total: emails_count,
            default_address: core
                .storage
                .latest_enabled_address(identity_fingerprint.clone())
                .await?
                .map(|a| a.address)
                .unwrap_or_default(),
            fingerprint: identity_fingerprint,
            route: route_id,
            settings: core.web.settings(),
            mcp_auth_required: core.mcp_auth_required(),
            url: core.web_url(),
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
        })
    }

    fn response(self) -> WebResult<Html<String>> {
        Ok(Html(self.render().map_err(anyhow::Error::from)?))
    }
}

async fn inbox(
    State(core): State<Arc<Core>>,
    Query(q): Query<InboxQuery>,
) -> WebResult<Html<String>> {
    let mut page = Page::new(&core, "inbox", "Inbox").await?;
    let filters = Filters {
        address: if q.address.is_empty() {
            None
        } else {
            Some(q.address.clone())
        },
        limit: Some(21),
        ..Default::default()
    };
    let mut emails = core
        .storage
        .inbox_page(
            filters,
            core.identity.fingerprint(),
            q.before,
            Some(q.q.clone()),
        )
        .await?;
    if emails.len() > 20 {
        emails.pop();
        page.next = emails.last().map(|e| e.id).unwrap_or(0);
    }

    page.rows = emails.into_iter().map(Into::into).collect();
    page.query = q.q;
    page.address = q.address;
    page.previous = q.before.is_some();
    // The filter accepts any exact local address, including aliases beyond this first selector page.
    page.addresses = core
        .storage
        .addresses(core.identity.fingerprint(), 0, 100, false)
        .await?;

    page.response()
}

async fn detail(State(core): State<Arc<Core>>, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let mut page = Page::new(&core, "email", "Email").await?;
    let mut email = core.get_email(id, 256 * 1024).await?;
    email.date = mail::local_date(&email.date, "%Y-%m-%d %H:%M:%S %:z");
    page.size = bytes(email.metadata.size_bytes.max(0) as usize);
    page.received = date(&email.metadata.received_at);
    if email.html_available {
        page.remote_resources = core.html_email(id, false).await?.remote_resources;
    }

    page.email = Some(email);
    page.response()
}

async fn raw(State(core): State<Arc<Core>>, Path(id): Path<i64>) -> WebResult<Html<String>> {
    let mut page = Page::new(&core, "email", "Raw EML").await?;
    let (_, raw) = core.storage.email(id).await?;
    page.raw = Some(String::from_utf8_lossy(&raw).into_owned());
    page.email = Some(core.get_email(id, 1).await?);
    page.response()
}

async fn html(State(core): State<Arc<Core>>, Path(id): Path<i64>) -> WebResult<Response> {
    Ok((
        [
            (header::CONTENT_SECURITY_POLICY, HTML_CSP),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(core.html_email(id, false).await?.html),
    )
        .into_response())
}

async fn load_images(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
    Form(form): Form<Action>,
) -> WebResult<Response> {
    form.check(&core)?;
    Ok((
        [
            (header::CONTENT_SECURITY_POLICY, HTML_IMAGES_CSP),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(core.html_email(id, true).await?.html),
    )
        .into_response())
}

#[derive(Default, Deserialize)]
struct AddressQuery {
    after_id: Option<i64>,
}

async fn addresses(
    State(core): State<Arc<Core>>,
    Query(q): Query<AddressQuery>,
) -> WebResult<Html<String>> {
    let mut page = Page::new(&core, "addresses", "Addresses").await?;
    page.custom_domain = core
        .storage
        .custom_domain(core.identity.fingerprint())
        .await?;
    page.addresses = core
        .storage
        .addresses(
            core.identity.fingerprint(),
            q.after_id.unwrap_or(0),
            21,
            false,
        )
        .await?;
    if page.addresses.len() > 20 {
        page.addresses.pop();
        page.next = page.addresses.last().map(|a| a.id).unwrap_or(0);
    }

    page.previous = q.after_id.is_some();
    page.response()
}

async fn settings(State(core): State<Arc<Core>>) -> WebResult<Html<String>> {
    Page::new(&core, "settings", "Settings").await?.response()
}

async fn login(State(core): State<Arc<Core>>) -> WebResult<Html<String>> {
    // Login must not query or render inbox/Identity metadata before authentication.
    let page = Login {
        csrf: core.web.csrf.clone(),
        error: false,
    };

    Ok(Html(page.render().map_err(anyhow::Error::from)?))
}

#[derive(Template)]
#[template(path = "login.html")]
struct Login {
    csrf: String,
    error: bool,
}

#[derive(Deserialize)]
struct Action {
    csrf: String,
    #[serde(default)]
    action: String,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    acknowledge: bool,
    label: Option<String>,
    password: Option<String>,
    password_confirm: Option<String>,
    domain: Option<String>,
    forwarding_address: Option<String>,
    #[serde(default)]
    confirm: bool,
}

impl Action {
    fn check(&self, core: &Core) -> Result<()> {
        if !bool::from(self.csrf.as_bytes().ct_eq(core.web.csrf.as_bytes())) {
            anyhow::bail!("invalid form token")
        }

        Ok(())
    }
}

async fn create_address(
    State(core): State<Arc<Core>>,
    Form(form): Form<Action>,
) -> WebResult<Redirect> {
    form.check(&core)?;
    let label = form.label.filter(|l| !l.trim().is_empty());
    if label.as_ref().is_some_and(|l| l.len() > 120) {
        return Err(anyhow::anyhow!("label too long").into());
    }

    core.create_address(label).await?;
    Ok(Redirect::to("/addresses"))
}

async fn address_action(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
    Form(form): Form<Action>,
) -> WebResult<Redirect> {
    form.check(&core)?;
    let enabled = match form.action.as_str() {
        "enable" => Some(true),
        "disable" => Some(false),
        "delete" if form.confirm => None,
        _ => return Err(anyhow::anyhow!("confirm action").into()),
    };
    core.change_address(id.to_string(), enabled).await?;
    Ok(Redirect::to("/addresses"))
}

async fn delete_email(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
    Form(form): Form<Action>,
) -> WebResult<Redirect> {
    form.check(&core)?;
    if !form.confirm {
        return Err(anyhow::anyhow!("confirmation required").into());
    }

    core.delete_emails(Some(id)).await?;
    Ok(Redirect::to("/"))
}

async fn domain_action(
    State(core): State<Arc<Core>>,
    Form(form): Form<Action>,
) -> WebResult<Redirect> {
    form.check(&core)?;
    if form.action == "remove" {
        core.storage
            .remove_custom_domain(core.identity.fingerprint())
            .await?;
    } else if form.action == "set" {
        core.storage
            .set_custom_domain(
                core.identity.fingerprint(),
                form.domain
                    .ok_or_else(|| anyhow::anyhow!("domain required"))?,
                form.forwarding_address
                    .ok_or_else(|| anyhow::anyhow!("forwarding destination required"))?,
            )
            .await?;
    } else {
        return Err(anyhow::anyhow!("invalid domain action").into());
    }

    Ok(Redirect::to("/addresses"))
}

async fn settings_action(
    State(core): State<Arc<Core>>,
    Form(mut form): Form<Action>,
) -> WebResult<Redirect> {
    form.check(&core)?;
    let password = form.password.take().map(Zeroizing::new);
    let confirmation = form.password_confirm.take().map(Zeroizing::new);
    if form.action == "password" && password.as_deref() != confirmation.as_deref() {
        return Err(anyhow::anyhow!("password confirmation mismatch").into());
    }
    if form.action == "remove_password" && !form.confirm {
        return Err(anyhow::anyhow!("confirmation required").into());
    }

    let scheme = core.http_scheme();
    core.update_interfaces(&form.action, form.enabled, form.acknowledge, password)
        .await?;
    if scheme != core.http_scheme() {
        return Ok(Redirect::to(&format!("{}/settings", core.web_url())));
    }

    Ok(Redirect::to(if form.action == "password" {
        "/login"
    } else {
        "/settings"
    }))
}

async fn sign_in(
    State(core): State<Arc<Core>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Form(mut form): Form<Action>,
) -> WebResult<Response> {
    form.check(&core)?;
    if !core.web.allow_login(peer.ip()) {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "30")],
            "Please wait before trying again.",
        )
            .into_response());
    }

    let Some(hash) = core.web.settings().password_hash else {
        return Ok(Redirect::to("/").into_response());
    };
    let password = Zeroizing::new(form.password.take().unwrap_or_default());
    if password.len() > 1024 {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }

    let Ok(permit) = core.web.password_work.try_acquire() else {
        return Ok(StatusCode::TOO_MANY_REQUESTS.into_response());
    };
    let check_hash = hash.clone();
    let valid = tokio::task::spawn_blocking(move || {
        crate::web_state::verify_password(&check_hash, &password)
    })
    .await
    .map_err(anyhow::Error::from)?;
    drop(permit);
    if !valid || core.web.settings().password_hash.as_ref() != Some(&hash) {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Html(
                Login {
                    csrf: core.web.csrf.clone(),
                    error: true,
                }
                .render()
                .map_err(anyhow::Error::from)?,
            ),
        )
            .into_response());
    }

    let Some(session) = core.web.session(&hash) else {
        return Ok(StatusCode::UNAUTHORIZED.into_response());
    };
    core.web.login_succeeded(peer.ip());
    let cookie = session_cookie(&core, &session, 43200);
    Ok(([(header::SET_COOKIE, cookie)], Redirect::to("/")).into_response())
}

async fn logout(
    State(core): State<Arc<Core>>,
    headers: HeaderMap,
    Form(form): Form<Action>,
) -> WebResult<Response> {
    form.check(&core)?;
    core.web.logout(session(&core, &headers));
    Ok((
        [(header::SET_COOKIE, session_cookie(&core, "", 0))],
        Redirect::to("/login"),
    )
        .into_response())
}

async fn events(
    State(core): State<Arc<Core>>,
    headers: HeaderMap,
    shutdown: Option<Extension<CancellationToken>>,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let shutdown = shutdown.map(|extension| extension.0).unwrap_or_default();
    let session = session(&core, &headers).map(str::to_owned);
    let mut receiver = core.events.subscribe();
    Sse::new(async_stream::stream! {
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = heartbeat.tick() => {
                    if !core.web.settings().web_enabled
                        || !core.web.authenticated(session.as_deref())
                    {
                        yield Ok(Event::default().event("session_expired").data("{}"));
                        break
                    }

                    yield Ok(Event::default().comment("keepalive"));
                }
                next = receiver.recv() => {
                    if !core.web.authenticated(session.as_deref()) {
                        yield Ok(Event::default().event("session_expired").data("{}"));
                        break
                    }

                    let Ok(event) = next else {
                        yield Ok(Event::default().event("resync").data("{}"));
                        break
                    };

                    yield Ok(Event::default().event(event.kind).data(event.data.to_string()));
                    if !core.web.settings().web_enabled {
                        break
                    }
                }
            }
        }
    })
}

async fn asset(Path(name): Path<String>) -> Response {
    let (mime, bytes): (&str, &[u8]) = match name.as_str() {
        "app.css" => ("text/css; charset=utf-8", include_bytes!("../web/app.css")),
        "app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../web/app.js"),
        ),
        "logo.png" => ("image/png", include_bytes!("../web/logo.png")),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };

    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=0, must-revalidate"),
        ],
        Body::from(bytes),
    )
        .into_response()
}

pub fn router(core: Arc<Core>) -> Router {
    Router::new()
        .route("/", get(inbox))
        .route("/emails/{id}", get(detail))
        .route("/emails/{id}/delete", post(delete_email))
        .route("/emails/{id}/raw", get(raw))
        .route("/emails/{id}/html", get(html).post(load_images))
        .route("/emails/{id}/eml", get(crate::api::eml))
        .route(
            "/emails/{id}/attachments/{attachmentId}",
            get(crate::api::download_attachment),
        )
        .route("/addresses", get(addresses).post(create_address))
        .route("/addresses/domain", post(domain_action))
        .route("/addresses/{id}", post(address_action))
        .route("/settings", get(settings).post(settings_action))
        .route("/login", get(login).post(sign_in))
        .route("/logout", post(logout))
        .route("/events", get(events))
        .route("/assets/{name}", get(asset))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(core.clone(), guard))
        .with_state(core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_mapped_peers_follow_ipv4_rules() {
        let ip = |value: &str| value.parse::<IpAddr>().unwrap();

        assert!(loopback(ip("::ffff:127.0.0.1")) && loopback(ip("::1")));
        assert!(!loopback(ip("::ffff:10.0.0.1")));
        assert!(private_ip(ip("::ffff:192.168.1.2")) && private_ip(ip("fd00::1")));
        assert!(!private_ip(ip("::ffff:8.8.8.8")) && !private_ip(ip("2001:db8::1")));
    }
}
