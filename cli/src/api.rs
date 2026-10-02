use crate::{
    auth,
    core::{Core, Status, WaitResult},
    mail::{Attachment, ParsedEmail},
    storage::{Address, Email, Filters},
};
use anyhow::Result;
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{convert::Infallible, sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use utoipa::{IntoParams, Modify, OpenApi, ToSchema};
use zeroize::Zeroizing;

pub(crate) struct ApiError(anyhow::Error);
impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self(value)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let missing = self
            .0
            .downcast_ref::<rusqlite::Error>()
            .is_some_and(|e| matches!(e, rusqlite::Error::QueryReturnedNoRows));
        let (status, code) = if missing {
            (StatusCode::NOT_FOUND, "not_found")
        } else if self.0.to_string().starts_with("listener_") {
            (StatusCode::CONFLICT, "listener_unavailable")
        } else {
            (StatusCode::BAD_REQUEST, "request_failed")
        };
        (status,Json(json!({"error":{"code":code,"message":if missing {"Resource not found".into()} else {self.0.to_string()}}}))).into_response()
    }
}
type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Clone)]
pub(crate) struct Authorization([u8; 32]);
async fn token(core: &Core) -> Result<Zeroizing<String>> {
    let value = Zeroizing::new(tokio::fs::read_to_string(&core.config.api_token_path).await?);
    auth::validate_api_token(&value)?;
    Ok(value)
}
pub(crate) async fn still_authorized(core: &Core, authorization: &Authorization) -> bool {
    match token(core).await {
        Ok(value) => bool::from(
            Sha256::digest(value.as_bytes())
                .as_slice()
                .ct_eq(&authorization.0),
        ),
        Err(_) => false,
    }
}
pub(crate) fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED,[(header::WWW_AUTHENTICATE,"Bearer")],Json(json!({"error":{"code":"unauthorized","message":"Valid local API bearer token required"}}))).into_response()
}
async fn authorize(State(core): State<Arc<Core>>, mut request: Request, next: Next) -> Response {
    if !core.web.settings().api_enabled
        && !core
            .web
            .api_forced
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|peer| !crate::web::loopback(peer.0.ip()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(authorization) = authenticate(&core, request.headers()).await else {
        return unauthorized();
    };
    request.extensions_mut().insert(authorization);
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response
}
pub(crate) async fn authenticate(core: &Core, headers: &HeaderMap) -> Option<Authorization> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let expected = token(core).await.ok()?;
    let supplied = supplied?;
    if !bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
        return None;
    }
    Some(Authorization(Sha256::digest(expected.as_bytes()).into()))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAddress {
    pub label: Option<String>,
}
#[derive(Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct Pagination {
    pub after_id: Option<i64>,
    pub limit: Option<u32>,
}
#[derive(Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct EmailQuery {
    pub address: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub after_id: Option<i64>,
    pub limit: Option<u32>,
}
impl From<EmailQuery> for Filters {
    fn from(q: EmailQuery) -> Self {
        Self {
            address: q.address,
            from_contains: q.from,
            subject_contains: q.subject,
            after_id: q.after_id,
            limit: q.limit,
            ..Default::default()
        }
    }
}
#[derive(Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct WaitQuery {
    pub timeout: Option<u64>,
    pub address: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub after_id: Option<i64>,
}
#[derive(Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct BodyQuery {
    pub max_body_bytes: Option<usize>,
}
#[derive(Serialize, ToSchema)]
pub struct Deleted {
    pub deleted: usize,
}

#[utoipa::path(get,path="/v1/status",responses((status=200,body=Status)))]
async fn status(State(core): State<Arc<Core>>) -> ApiResult<Json<Status>> {
    Ok(Json(core.status().await?))
}
#[utoipa::path(post,path="/v1/addresses",request_body=CreateAddress,responses((status=201,body=Address)))]
async fn create_address(
    State(core): State<Arc<Core>>,
    Json(input): Json<CreateAddress>,
) -> ApiResult<(StatusCode, Json<Address>)> {
    Ok((
        StatusCode::CREATED,
        Json(core.create_address(input.label).await?),
    ))
}
#[utoipa::path(get,path="/v1/addresses",params(Pagination),responses((status=200,body=Vec<Address>)))]
async fn list_addresses(
    State(core): State<Arc<Core>>,
    Query(q): Query<Pagination>,
) -> ApiResult<Json<Vec<Address>>> {
    Ok(Json(
        core.storage
            .addresses(
                core.identity.fingerprint(),
                q.after_id.unwrap_or(0),
                q.limit.unwrap_or(20),
                false,
            )
            .await?,
    ))
}
#[utoipa::path(get,path="/v1/addresses/{id}",params(("id"=i64,Path)),responses((status=200,body=Address),(status=404,description="Not found")))]
async fn get_address(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Address>> {
    Ok(Json(
        core.storage
            .get_address(core.identity.fingerprint(), id.to_string())
            .await?,
    ))
}
#[utoipa::path(delete,path="/v1/addresses/{id}",params(("id"=i64,Path)),responses((status=200,body=Address)))]
async fn delete_address(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Address>> {
    Ok(Json(core.change_address(id.to_string(), None).await?))
}
#[utoipa::path(post,path="/v1/addresses/{id}/enable",params(("id"=i64,Path)),responses((status=200,body=Address)))]
async fn enable_address(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Address>> {
    Ok(Json(core.change_address(id.to_string(), Some(true)).await?))
}
#[utoipa::path(post,path="/v1/addresses/{id}/disable",params(("id"=i64,Path)),responses((status=200,body=Address)))]
async fn disable_address(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Address>> {
    Ok(Json(
        core.change_address(id.to_string(), Some(false)).await?,
    ))
}
#[utoipa::path(get,path="/v1/emails",params(EmailQuery),responses((status=200,body=Vec<Email>)))]
async fn list_emails(
    State(core): State<Arc<Core>>,
    Query(q): Query<EmailQuery>,
) -> ApiResult<Json<Vec<Email>>> {
    Ok(Json(core.storage.list(q.into(), None).await?))
}
#[utoipa::path(get,path="/v1/emails/{id}",params(("id"=i64,Path),BodyQuery),responses((status=200,body=ParsedEmail)))]
async fn get_email(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
    Query(q): Query<BodyQuery>,
) -> ApiResult<Json<ParsedEmail>> {
    Ok(Json(
        core.get_email(id, q.max_body_bytes.unwrap_or(16 * 1024))
            .await?,
    ))
}
#[utoipa::path(delete,path="/v1/emails/{id}",params(("id"=i64,Path)),responses((status=200,body=Deleted)))]
async fn delete_email(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Deleted>> {
    Ok(Json(Deleted {
        deleted: core.delete_emails(Some(id)).await?,
    }))
}
/// Destructive: permanently delete every email in this local profile. Aliases remain.
#[utoipa::path(delete,path="/v1/emails",responses((status=200,body=Deleted)))]
async fn clear_inbox(State(core): State<Arc<Core>>) -> ApiResult<Json<Deleted>> {
    Ok(Json(Deleted {
        deleted: core.delete_emails(None).await?,
    }))
}
#[utoipa::path(get,path="/v1/emails/wait",params(WaitQuery),responses((status=200,body=WaitResult),(status=401,description="Token revoked")))]
async fn wait(
    State(core): State<Arc<Core>>,
    Extension(auth): Extension<Authorization>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<Response> {
    let filters = Filters {
        address: q.address,
        from_contains: q.from,
        subject_contains: q.subject,
        after_id: q.after_id,
        ..Default::default()
    };
    let result = core.wait_email(filters, q.timeout.unwrap_or(60)).await?;
    if !still_authorized(&core, &auth).await {
        return Ok(unauthorized());
    }
    Ok(Json(result).into_response())
}
#[utoipa::path(get,path="/v1/emails/{id}/eml",params(("id"=i64,Path),("Range"=Option<String>,Header,description="Single bytes range")),responses((status=200,body=String,content_type="message/rfc822"),(status=206,description="Partial EML"),(status=416,description="Invalid range")))]
pub(crate) async fn eml(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let (meta, raw) = core.storage.email(id).await?;
    let total = raw.len();
    let mut start = 0;
    let mut end = total;
    let mut status = StatusCode::OK;
    if let Some(range) = headers.get(header::RANGE) {
        let parsed = range
            .to_str()
            .ok()
            .and_then(|s| s.strip_prefix("bytes="))
            .and_then(|s| s.split_once('-'))
            .and_then(|(a, b)| {
                if a.is_empty() {
                    let suffix = b.parse::<usize>().ok()?;
                    if suffix == 0 {
                        return None;
                    }
                    Some((total.saturating_sub(suffix), total))
                } else {
                    let first = a.parse::<usize>().ok()?;
                    let last = if b.is_empty() {
                        total
                    } else {
                        b.parse::<usize>().ok()?.checked_add(1)?.min(total)
                    };
                    Some((first, last))
                }
            });
        let Some((a, b)) = parsed.filter(|(a, b)| a < b && *a < total) else {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response());
        };
        start = a;
        end = b;
        status = StatusCode::PARTIAL_CONTENT;
    }
    let bytes = axum::body::Bytes::from(raw).slice(start..end);
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("message/rfc822"),
    );
    response.headers_mut().insert(
        header::ACCEPT_RANGES,
        header::HeaderValue::from_static("bytes"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"email-{}.eml\"", meta.id)
            .parse()
            .unwrap(),
    );
    if status == StatusCode::PARTIAL_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{total}", end - 1)
                .parse()
                .unwrap(),
        );
    }
    Ok(response)
}
#[utoipa::path(get,path="/v1/emails/{id}/attachments",params(("id"=i64,Path)),responses((status=200,body=Vec<Attachment>)))]
async fn list_attachments(
    State(core): State<Arc<Core>>,
    Path(id): Path<i64>,
) -> ApiResult<Json<Vec<Attachment>>> {
    Ok(Json(core.get_email(id, 1).await?.attachments))
}
#[utoipa::path(get,path="/v1/emails/{id}/attachments/{attachmentId}",params(("id"=i64,Path),("attachmentId"=String,Path)),responses((status=200,body=String,content_type="application/octet-stream")))]
pub(crate) async fn download_attachment(
    State(core): State<Arc<Core>>,
    Path((id, attachment_id)): Path<(i64, String)>,
) -> ApiResult<Response> {
    let (meta, bytes) = core.attachment(id, attachment_id).await?;
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    let ascii: String = meta
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
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{ascii}\"").parse().unwrap(),
    );
    Ok(response)
}
/// Ephemeral metadata events from this API process only; resynchronize via REST after disconnect.
#[utoipa::path(get,path="/v1/events",responses((status=200,body=String,content_type="text/event-stream")))]
async fn events(
    State(core): State<Arc<Core>>,
    Extension(auth): Extension<Authorization>,
    shutdown: Option<Extension<CancellationToken>>,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let shutdown = shutdown.map(|extension| extension.0).unwrap_or_default();
    let mut events = core.events.subscribe();
    let stream = async_stream::stream! {
        let mut heartbeat=tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _=shutdown.cancelled()=>break,
                _=heartbeat.tick()=> {if !still_authorized(&core,&auth).await{break} yield Ok(Event::default().comment("keepalive"));}
                next=events.recv()=> {
                    let Ok(event)=next else{break};
                    if !still_authorized(&core,&auth).await{break}
                    yield Ok(Event::default().event(event.kind).data(event.data.to_string()));
                }
            }
        }
    };
    Sse::new(stream)
}

#[derive(OpenApi)]
#[openapi(info(title="Ahrara local API",version="0.1.0",description="Loopback API with optional HTTPS. Server stores neither aliases nor email. All inbox data is local SQLite."),paths(status,create_address,list_addresses,get_address,delete_address,enable_address,disable_address,list_emails,get_email,delete_email,clear_inbox,wait,eml,list_attachments,download_attachment,events),components(schemas(Status,crate::core::Owner,Address,Email,ParsedEmail,Attachment,WaitResult,CreateAddress,Deleted)),modifiers(&Security),security(("localToken"=[])))]
pub struct ApiDoc;
struct Security;
impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(c) = openapi.components.as_mut() {
            c.add_security_scheme(
                "localToken",
                utoipa::openapi::security::SecurityScheme::Http(
                    utoipa::openapi::security::Http::new(
                        utoipa::openapi::security::HttpAuthScheme::Bearer,
                    ),
                ),
            );
        }
    }
}
pub fn openapi_yaml() -> Result<String> {
    Ok(ApiDoc::openapi().to_yaml()?)
}

pub fn router(core: Arc<Core>) -> Router {
    let secured = Router::new()
        .route("/v1/status", get(status))
        .route("/v1/addresses", get(list_addresses).post(create_address))
        .route(
            "/v1/addresses/{id}",
            get(get_address).delete(delete_address),
        )
        .route("/v1/addresses/{id}/enable", post(enable_address))
        .route("/v1/addresses/{id}/disable", post(disable_address))
        .route("/v1/emails", get(list_emails).delete(clear_inbox))
        .route("/v1/emails/wait", get(wait))
        .route("/v1/emails/{id}", get(get_email).delete(delete_email))
        .route("/v1/emails/{id}/eml", get(eml))
        .route("/v1/emails/{id}/attachments", get(list_attachments))
        .route(
            "/v1/emails/{id}/attachments/{attachmentId}",
            get(download_attachment),
        )
        .route("/v1/events", get(events))
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .route_layer(middleware::from_fn_with_state(core.clone(), authorize));
    secured
        .route("/healthz", get(|| async { Json(json!({"status":"ok"})) }))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(core)
}

pub struct HttpServer {
    core: Arc<Core>,
    _ownership: crate::core::Ownership,
    force_api: bool,
    active: Option<(
        std::net::SocketAddr,
        axum_server::Handle,
        CancellationToken,
        tokio::task::JoinHandle<std::io::Result<()>>,
    )>,
}
impl HttpServer {
    pub async fn new(core: Arc<Core>, force_api: bool) -> Result<Self> {
        if let Some(owner) = crate::core::Ownership::current(&core.config)? {
            anyhow::bail!("listener_owned: ahrara {} (PID {})", owner.mode, owner.pid)
        }
        let ownership =
            crate::core::Ownership::acquire_http(&core.config, &core.identity.fingerprint())?;
        crate::config::private_write(
            &core.config.profile_dir().join("http.config.toml"),
            toml::to_string(&core.config)?.as_bytes(),
            true,
        )?;
        core.web
            .api_forced
            .store(force_api, std::sync::atomic::Ordering::Relaxed);
        let mut server = Self {
            core,
            _ownership: ownership,
            force_api,
            active: None,
        };
        server.sync().await?;
        Ok(server)
    }
    pub async fn sync(&mut self) -> Result<()> {
        let preferences = self.core.web.settings();
        let enabled = self.force_api
            || preferences.api_enabled
            || preferences.web_enabled
            || preferences.mcp_enabled;
        let mut address: std::net::SocketAddr = self.core.config.api_bind.parse()?;
        if !address.ip().is_loopback() {
            anyhow::bail!("api_bind must be loopback; use --lan to enable LAN access explicitly")
        }
        if preferences.lan_enabled {
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            });
        }
        if self.force_api
            || preferences.api_enabled
            || preferences.mcp_enabled && self.core.mcp_auth_required()
        {
            if !self.core.config.api_token_path.exists() {
                auth::generate_token(&self.core.config, false)?;
            }
            token(&self.core).await?;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|(bound, _, _, task)| enabled && *bound == address && !task.is_finished())
        {
            return Ok(());
        }
        self.stop().await;
        if !enabled {
            *self.core.web.status.lock().unwrap() = "Disabled".into();
            return Ok(());
        }
        let tls = if self.core.https_enabled() {
            let _ = rustls::crypto::ring::default_provider().install_default();
            Some(
                axum_server::tls_rustls::RustlsConfig::from_pem_file(
                    &self.core.config.api_tls_cert_path,
                    &self.core.config.api_tls_key_path,
                )
                .await?,
            )
        } else {
            None
        };
        let listener = std::net::TcpListener::bind(address)?;
        let handle = axum_server::Handle::new();
        let serving = handle.clone();
        let shutdown = CancellationToken::new();
        let router = router(self.core.clone())
            .merge(crate::web::router(self.core.clone()))
            .merge(crate::mcp::router(self.core.clone(), shutdown.clone()))
            .layer(Extension(shutdown.clone()));
        let task = tokio::spawn(async move {
            let service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
            match tls {
                Some(tls) => {
                    axum_server::from_tcp_rustls(listener, tls)
                        .handle(serving)
                        .serve(service)
                        .await
                }
                None => {
                    axum_server::from_tcp(listener)
                        .handle(serving)
                        .serve(service)
                        .await
                }
            }
        });
        self.active = Some((address, handle, shutdown, task));
        *self.core.web.status.lock().unwrap() = self.core.web_url();
        Ok(())
    }
    pub async fn stop(&mut self) {
        if let Some((_, handle, shutdown, task)) = self.active.take() {
            shutdown.cancel();
            handle.graceful_shutdown(Some(Duration::from_secs(1)));
            let _ = task.await;
        }
    }
}
impl Drop for HttpServer {
    fn drop(&mut self) {
        if let Some((_, handle, shutdown, task)) = self.active.take() {
            shutdown.cancel();
            handle.shutdown();
            task.abort();
        }
    }
}
pub async fn serve(core: Arc<Core>) -> Result<()> {
    serve_mode(core, "api").await
}
pub(crate) async fn serve_mode(core: Arc<Core>, mode: &str) -> Result<()> {
    let mut events = core.events.subscribe();
    let mut server = HttpServer::new(core.clone(), mode == "api").await?;
    core.start(mode, false).await?;
    eprintln!("Ahrara local server listening on {}", core.web_url());
    if core.web.settings().mcp_enabled {
        eprintln!(
            "MCP Streamable HTTP: {}/mcp ({})",
            core.web_url(),
            if core.mcp_auth_required() {
                "bearer token required"
            } else {
                "authentication disabled"
            }
        );
    }
    let idle = async {
        if mode == "mcp-managed" {
            crate::mcp_stdio::idle(&core.config).await
        } else {
            std::future::pending().await
        }
    };
    tokio::pin!(idle);
    let _idle_guard = loop {
        tokio::select! {
            _=crate::shutdown_signal()=>break None,
            guard=&mut idle=>break Some(guard?),
            event=events.recv()=>if let Ok(event)=event && matches!(event.kind.as_str(), "web_settings_changed" | "mcp_state_changed") && let Err(error)=server.sync().await {
                *core.web.status.lock().unwrap()="Unable to bind local server".into(); eprintln!("Local server: {error}");
            }
        }
    };
    server.stop().await;
    core.stop().await;
    Ok(())
}
