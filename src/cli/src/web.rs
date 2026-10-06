use crate::core::Core;
use ahrara_core::{
    mail::{Attachment, HtmlEmail, ParsedEmail},
    storage::{Address, CustomDomain, Email, Filters},
};
use ahrara_web::{LocalSettings, Mode, Status};
use anyhow::Result;
use askama::Template;
use axum::{
    Extension, Form, Router,
    body::Body,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
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

const BODY_LIMIT: usize = 16 * 1024;

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
    ahrara_web::harden(response.headers_mut(), false);
    response
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
    password: Option<String>,
    password_confirm: Option<String>,
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

impl ahrara_web::Backend for Core {
    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn mode(&self) -> Mode {
        let settings = self.web.settings();
        Mode::Cli(LocalSettings {
            url: self.web_url(),
            lan_enabled: settings.lan_enabled,
            lan_no_auth_acknowledged: settings.lan_no_auth_acknowledged,
            password: settings.password_hash.is_some(),
            api_enabled: settings.api_enabled,
            mcp_enabled: settings.mcp_enabled,
            mcp_auth_required: self.mcp_auth_required(),
        })
    }

    fn csrf(&self) -> &str {
        &self.web.csrf
    }

    // The receiver's inherent methods share names with this trait's.
    async fn status(&self) -> Result<Status> {
        let receiver: &ahrara_core::core::Core = self;
        let status = receiver.status().await?;
        Ok(Status {
            connected: status.connected,
            emails: status.emails_count,
            fingerprint: status.identity_fingerprint,
            route: status.route_id,
        })
    }

    async fn latest_enabled_address(&self) -> Result<Option<Address>> {
        self.storage
            .latest_enabled_address(self.identity.fingerprint())
            .await
    }

    async fn inbox_page(
        &self,
        filters: Filters,
        before: Option<i64>,
        search: String,
    ) -> Result<Vec<Email>> {
        self.storage
            .inbox_page(filters, self.identity.fingerprint(), before, Some(search))
            .await
    }

    async fn addresses(&self, after_id: i64, limit: u32) -> Result<Vec<Address>> {
        self.storage
            .addresses(self.identity.fingerprint(), after_id, limit, false)
            .await
    }

    async fn email(&self, id: i64) -> Result<(Email, Vec<u8>)> {
        self.storage.email(id).await
    }

    async fn create_address(&self, label: Option<String>) -> Result<()> {
        let receiver: &ahrara_core::core::Core = self;
        receiver.create_address(label).await.map(drop)
    }

    async fn change_address(&self, id: i64, enabled: Option<bool>) -> Result<()> {
        let receiver: &ahrara_core::core::Core = self;
        receiver
            .change_address(id.to_string(), enabled)
            .await
            .map(drop)
    }

    async fn delete_email(&self, id: i64) -> Result<()> {
        self.delete_emails(Some(id)).await.map(drop)
    }

    async fn custom_domain(&self) -> Result<Option<CustomDomain>> {
        self.storage
            .custom_domain(self.identity.fingerprint())
            .await
    }

    async fn set_custom_domain(&self, domain: String, forwarding_address: String) -> Result<()> {
        self.storage
            .set_custom_domain(self.identity.fingerprint(), domain, forwarding_address)
            .await
            .map(drop)
    }

    async fn remove_custom_domain(&self) -> Result<()> {
        self.storage
            .remove_custom_domain(self.identity.fingerprint())
            .await
    }

    async fn parsed_email(&self, id: i64, max_body: usize) -> Result<ParsedEmail> {
        self.get_email(id, max_body).await
    }

    async fn html_email(&self, id: i64, allow_remote: bool) -> Result<HtmlEmail> {
        let receiver: &ahrara_core::core::Core = self;
        receiver.html_email(id, allow_remote).await
    }

    async fn attachment(&self, id: i64, attachment_id: &str) -> Result<(Attachment, Vec<u8>)> {
        let receiver: &ahrara_core::core::Core = self;
        receiver.attachment(id, attachment_id.to_owned()).await
    }

    fn not_found(&self, error: &anyhow::Error) -> bool {
        crate::api::not_found(error)
    }
}

async fn shared(State(core): State<Arc<Core>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = axum::body::to_bytes(body, BODY_LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };

    ahrara_web::respond(&*core, Request::from_parts(parts, body.into()))
        .await
        .map(Body::from)
}

pub fn router(core: Arc<Core>) -> Router {
    Router::new()
        .route("/", get(shared))
        .route("/emails/{id}", get(shared))
        .route("/emails/{id}/delete", post(shared))
        .route("/emails/{id}/raw", get(shared))
        .route("/emails/{id}/html", get(shared).post(shared))
        .route("/emails/{id}/eml", get(shared))
        .route("/emails/{id}/attachments/{attachmentId}", get(shared))
        .route("/addresses", get(shared).post(shared))
        .route("/addresses/domain", post(shared))
        .route("/addresses/{id}", post(shared))
        .route("/settings", get(shared).post(settings_action))
        .route("/login", get(login).post(sign_in))
        .route("/logout", post(logout))
        .route("/events", get(events))
        .route("/assets/{name}", get(shared))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
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
