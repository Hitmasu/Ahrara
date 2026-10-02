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
use utoipa::{IntoParams, Modify, OpenApi, ToSchema};
use zeroize::Zeroizing;

struct ApiError(anyhow::Error);
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
struct Authorization([u8; 32]);
async fn token(core: &Core) -> Result<Zeroizing<String>> {
    let value = Zeroizing::new(tokio::fs::read_to_string(&core.config.api_token_path).await?);
    auth::validate_api_token(&value)?;
    Ok(value)
}
async fn still_authorized(core: &Core, authorization: &Authorization) -> bool {
    match token(core).await {
        Ok(value) => bool::from(
            Sha256::digest(value.as_bytes())
                .as_slice()
                .ct_eq(&authorization.0),
        ),
        Err(_) => false,
    }
}
fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED,[(header::WWW_AUTHENTICATE,"Bearer")],Json(json!({"error":{"code":"unauthorized","message":"Valid local API bearer token required"}}))).into_response()
}
async fn authorize(State(core): State<Arc<Core>>, mut request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let Ok(expected) = token(&core).await else {
        return unauthorized();
    };
    let Some(supplied) = supplied else {
        return unauthorized();
    };
    if !bool::from(supplied.as_bytes().ct_eq(expected.as_bytes())) {
        return unauthorized();
    }
    request
        .extensions_mut()
        .insert(Authorization(Sha256::digest(expected.as_bytes()).into()));
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
        deleted: core.storage.delete(Some(id)).await?,
    }))
}
/// Destructive: permanently delete every email in this local profile. Aliases remain.
#[utoipa::path(delete,path="/v1/emails",responses((status=200,body=Deleted)))]
async fn clear_inbox(State(core): State<Arc<Core>>) -> ApiResult<Json<Deleted>> {
    Ok(Json(Deleted {
        deleted: core.storage.delete(None).await?,
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
async fn eml(
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
async fn download_attachment(
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
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let mut events = core.events.subscribe();
    let stream = async_stream::stream! {
        let mut heartbeat=tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
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
#[openapi(info(title="Ahrara local API",version="0.1.0",description="HTTPS loopback API. Server stores neither aliases nor email. All inbox data is local SQLite."),paths(status,create_address,list_addresses,get_address,delete_address,enable_address,disable_address,list_emails,get_email,delete_email,clear_inbox,wait,eml,list_attachments,download_attachment,events),components(schemas(Status,crate::core::Owner,Address,Email,ParsedEmail,Attachment,WaitResult,CreateAddress,Deleted)),modifiers(&Security),security(("localToken"=[])))]
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

pub async fn serve(core: Arc<Core>) -> Result<()> {
    let address: std::net::SocketAddr = core.config.api_bind.parse()?;
    if !address.ip().is_loopback() {
        anyhow::bail!("API v1 must bind a loopback address")
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        &core.config.api_tls_cert_path,
        &core.config.api_tls_key_path,
    )
    .await?;
    if !core.config.api_token_path.exists() {
        auth::generate_token(&core.config, false)?;
    }
    token(&core).await?;
    // Bind first: a port conflict must not leave a background listener running.
    let listener = std::net::TcpListener::bind(address)?;
    core.start("api", false).await?;
    let handle = axum_server::Handle::new();
    let shutdown = handle.clone();
    let stopping = core.clone();
    let signal = tokio::spawn(async move {
        crate::shutdown_signal().await;
        stopping.stop().await;
        shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
    });
    eprintln!("Ahrara API listening on https://{address}");
    let result = axum_server::from_tcp_rustls(listener, tls)
        .handle(handle)
        .serve(router(core.clone()).into_make_service())
        .await;
    signal.abort();
    core.stop().await;
    result?;
    Ok(())
}
