use crate::{
    broker::{Broker, Connection},
    protocol as p,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, FromRequest, Request, State, ws::Message},
    http::{HeaderMap, StatusCode, header::CACHE_CONTROL},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::stream;
use rand::{RngCore, rngs::OsRng};
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

const SESSION_HEADER: &str = "x-ahrara-session";
const MAX_PENDING_CONTROLS: usize = 128;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

struct Session {
    incoming: mpsc::Sender<Message>,
    closed: CancellationToken,
}

pub struct Sessions(Mutex<HashMap<String, Session>>, Semaphore);

impl Default for Sessions {
    fn default() -> Self {
        Self(
            Mutex::new(HashMap::new()),
            Semaphore::new(MAX_PENDING_CONTROLS),
        )
    }
}

// Dropping the streaming HTTP response also cancels authentication/SMTP waits.
struct SessionBody {
    broker: Arc<Broker>,
    token: String,
    outgoing: mpsc::Receiver<Bytes>,
    closed: CancellationToken,
}

impl Drop for SessionBody {
    fn drop(&mut self) {
        self.closed.cancel();
        self.broker.sse.0.lock().unwrap().remove(&self.token);
    }
}

pub async fn open(State(broker): State<Arc<Broker>>, headers: HeaderMap) -> Response {
    if headers.get("origin").is_some_and(|value| !value.is_empty()) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let mut random = [0; 32];
    OsRng.fill_bytes(&mut random);
    let token = URL_SAFE_NO_PAD.encode(random);
    // One control frame and one outbound frame: idle sessions allocate no email buffers.
    let (incoming, input) = mpsc::channel(1);
    let (output, outgoing) = mpsc::channel(1);
    let closed = CancellationToken::new();
    broker.sse.0.lock().unwrap().insert(
        token.clone(),
        Session {
            incoming,
            closed: closed.clone(),
        },
    );
    let body = SessionBody {
        broker: broker.clone(),
        token: token.clone(),
        outgoing,
        closed: closed.clone(),
    };
    let session_broker = broker.clone();
    let session_token = token.clone();
    tokio::spawn(async move {
        session_broker
            .clone()
            .serve_connection(
                Connection::Sse {
                    incoming: input,
                    outgoing: output,
                },
                closed,
            )
            .await;
        session_broker.sse.0.lock().unwrap().remove(&session_token);
    });
    let events = stream::unfold(body, |mut body| async move {
        body.outgoing.recv().await.map(|frame| {
            (
                Ok::<_, Infallible>(
                    Event::default()
                        .event("frame")
                        .data(URL_SAFE_NO_PAD.encode(frame)),
                ),
                body,
            )
        })
    });
    (
        [
            (SESSION_HEADER, token.as_str()),
            (CACHE_CONTROL.as_str(), "no-store"),
        ],
        // POST heartbeats do not keep the separate GET response active at proxies.
        Sse::new(events).keep_alive(KeepAlive::new().interval(if cfg!(test) {
            Duration::from_millis(10)
        } else {
            Duration::from_secs(30)
        })),
    )
        .into_response()
}

pub async fn send(State(broker): State<Arc<Broker>>, mut request: Request) -> StatusCode {
    let headers = request.headers();
    if headers.get("origin").is_some_and(|value| !value.is_empty()) {
        return StatusCode::FORBIDDEN;
    }
    let Some(token) = headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::NOT_FOUND;
    };
    let (incoming, closed) = {
        let sessions = broker.sse.0.lock().unwrap();
        let Some(session) = sessions
            .get(token)
            .filter(|session| !session.closed.is_cancelled())
        else {
            return StatusCode::NOT_FOUND;
        };
        (session.incoming.clone(), session.closed.clone())
    };
    // Only short-lived POST bodies consume slots; idle GET sessions have no added buffer.
    let Ok(_upload) = broker.sse.1.try_acquire() else {
        return StatusCode::TOO_MANY_REQUESTS;
    };
    let overhead = if broker.is_encrypted() {
        crate::secure::TAG_LEN
    } else {
        0
    };
    DefaultBodyLimit::max(p::MAX_CONTROL + 6 + overhead).apply(&mut request);
    let body = tokio::select! {
        biased;
        _ = closed.cancelled() => return StatusCode::NOT_FOUND,
        result = tokio::time::timeout(CONTROL_TIMEOUT, Bytes::from_request(request, &())) => {
            match result {
                Ok(Ok(body)) => body,
                Ok(Err(rejection)) => return rejection.status(),
                Err(_) => return StatusCode::REQUEST_TIMEOUT,
            }
        }
    };
    if closed.is_cancelled() {
        return StatusCode::NOT_FOUND;
    }
    let frame = if body.is_empty() {
        Message::Ping(Bytes::new())
    } else {
        // Secure records are authenticated and decoded by the broker after the Noise handshake.
        if !broker.is_encrypted() && p::decode(&body).is_err() {
            closed.cancel();
            return StatusCode::BAD_REQUEST;
        }
        Message::Binary(body)
    };
    match incoming.try_send(frame) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(mpsc::error::TrySendError::Full(_)) => StatusCode::TOO_MANY_REQUESTS,
        Err(mpsc::error::TrySendError::Closed(_)) => StatusCode::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, extract::DefaultBodyLimit, http::Request, routing::get};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        task::Poll,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn secure_sse_posts_authenticate_before_decoding_the_envelope() {
        use crate::{auth, secure};
        use ed25519_dalek::{Signer, SigningKey};
        use futures_util::StreamExt;
        async fn next_frame(body: &mut axum::body::BodyDataStream) -> Vec<u8> {
            loop {
                let bytes = tokio::time::timeout(Duration::from_secs(1), body.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let text = std::str::from_utf8(&bytes).unwrap();
                if let Some(encoded) = text.lines().find_map(|line| line.strip_prefix("data: ")) {
                    return URL_SAFE_NO_PAD.decode(encoded).unwrap();
                }
            }
        }
        let key = snow::Builder::new(secure::PATTERN.parse().unwrap())
            .generate_keypair()
            .unwrap();
        let private = zeroize::Zeroizing::new(key.private);
        let broker = Broker::encrypted(private.as_slice().try_into().unwrap());
        let app = router(broker.clone());
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, response.headers()[SESSION_HEADER].clone());
        let mut body = response.into_body().into_data_stream();
        let mut state = secure::initiator(key.public.as_slice().try_into().unwrap()).unwrap();
        let mut message = [0; secure::MAX_HANDSHAKE];
        let length = state.write_message(&[], &mut message).unwrap();
        assert_eq!(
            app.clone()
                .oneshot(request(
                    headers.clone(),
                    Body::from(message[..length].to_vec())
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            state
                .read_message(&next_frame(&mut body).await, &mut message)
                .unwrap(),
            0
        );
        let binding = state.get_handshake_hash().try_into().unwrap();
        let (mut sender, mut receiver) = secure::into_session(state).unwrap();
        let identity = SigningKey::from_bytes(&[8; 32]);
        let public = identity.verifying_key().to_bytes();
        let cipher = sender
            .seal(
                &p::json(
                    p::HELLO,
                    &serde_json::json!({"public_key":URL_SAFE_NO_PAD.encode(public)}),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(request(headers.clone(), Body::from(cipher)))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let challenge = receiver.open(&next_frame(&mut body).await).unwrap();
        let (kind, payload) = p::decode(&challenge).unwrap();
        assert_eq!(kind, p::CHALLENGE);
        let challenge: serde_json::Value = serde_json::from_slice(payload).unwrap();
        let challenge: [u8; 32] = URL_SAFE_NO_PAD
            .decode(challenge["challenge"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let signature = identity.sign(&secure::auth_preimage(&public, &challenge, &binding));
        let cipher = sender
            .seal(
                &p::json(
                    p::AUTHENTICATE,
                    &serde_json::json!({"signature":URL_SAFE_NO_PAD.encode(signature.to_bytes())}),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            app.oneshot(request(headers, Body::from(cipher)))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        let ready = receiver.open(&next_frame(&mut body).await).unwrap();
        assert_eq!(p::decode(&ready).unwrap().0, p::READY);
        let route = auth::derive(&public);
        assert!(broker.lookup(&route).is_some());
        drop(body);
        tokio::time::timeout(Duration::from_secs(1), async {
            while broker.lookup(&route).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(broker.sse.0.lock().unwrap().is_empty());
    }

    fn active_session() -> (
        Arc<Broker>,
        HeaderMap,
        mpsc::Receiver<Message>,
        CancellationToken,
    ) {
        let broker = Broker::new();
        let (incoming, input) = mpsc::channel(1);
        let closed = CancellationToken::new();
        broker.sse.0.lock().unwrap().insert(
            "active".into(),
            Session {
                incoming,
                closed: closed.clone(),
            },
        );
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, "active".parse().unwrap());
        (broker, headers, input, closed)
    }

    fn router(broker: Arc<Broker>) -> Router {
        Router::new()
            .route("/sse", get(open).post(send))
            .layer(DefaultBodyLimit::max(p::MAX_CONTROL + 6))
            .with_state(broker)
    }

    fn request(headers: HeaderMap, body: Body) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/sse")
            .body(body)
            .unwrap();
        *request.headers_mut() = headers;
        request
    }

    fn pending_body(polls: Arc<AtomicUsize>) -> Body {
        Body::from_stream(stream::poll_fn(move |_| {
            polls.fetch_add(1, Ordering::SeqCst);
            Poll::<Option<Result<Bytes, Infallible>>>::Pending
        }))
    }

    async fn assert_rejected_without_reading(headers: HeaderMap, status: StatusCode) {
        let polls = Arc::new(AtomicUsize::new(0));
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            router(Broker::new()).oneshot(request(headers, pending_body(polls.clone()))),
        )
        .await
        .expect("invalid request must be rejected before awaiting the pending body")
        .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn missing_token_is_rejected_before_pending_body() {
        assert_rejected_without_reading(HeaderMap::new(), StatusCode::NOT_FOUND).await;
    }

    #[tokio::test]
    async fn invalid_token_is_rejected_before_pending_body() {
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, "unknown".parse().unwrap());
        assert_rejected_without_reading(headers, StatusCode::NOT_FOUND).await;
    }

    #[tokio::test]
    async fn forbidden_origin_is_rejected_before_pending_body() {
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://example.test".parse().unwrap());
        assert_rejected_without_reading(headers, StatusCode::FORBIDDEN).await;
    }

    #[tokio::test(start_paused = true)]
    async fn pending_control_has_an_absolute_deadline_without_closing_get() {
        let (broker, headers, mut input, closed) = active_session();
        // Receiving a partial chunk after eight seconds must not restart the deadline.
        let body = Body::from_stream(stream::unfold((), |_| async {
            tokio::time::sleep(Duration::from_secs(8)).await;
            Some((Ok::<_, Infallible>(Bytes::from_static(b"A")), ()))
        }));
        let started = tokio::time::Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(16),
            router(broker.clone()).oneshot(request(headers.clone(), body)),
        )
        .await
        .expect("incomplete upload must expire after fifteen seconds")
        .unwrap();
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert_eq!(started.elapsed(), Duration::from_secs(15));
        assert!(!closed.is_cancelled());
        assert_eq!(broker.sse.1.available_permits(), MAX_PENDING_CONTROLS);
        assert_eq!(
            router(broker)
                .oneshot(request(headers, Body::empty()))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT,
        );
        assert!(matches!(input.recv().await, Some(Message::Ping(_))));
    }

    #[tokio::test]
    async fn closing_get_cancels_its_pending_control_body() {
        let broker = Broker::new();
        let response = open(State(broker.clone()), HeaderMap::new()).await;
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, response.headers()[SESSION_HEADER].clone());
        let polls = Arc::new(AtomicUsize::new(0));
        let upload = tokio::spawn(
            router(broker.clone()).oneshot(request(headers, pending_body(polls.clone()))),
        );
        tokio::time::timeout(Duration::from_millis(100), async {
            while polls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(response);
        let result = tokio::time::timeout(Duration::from_millis(100), upload)
            .await
            .expect("GET drop must cancel body collection without waiting for its deadline")
            .unwrap()
            .unwrap();
        assert_eq!(result.status(), StatusCode::NOT_FOUND);
        assert!(broker.sse.0.lock().unwrap().is_empty());
        assert_eq!(broker.sse.1.available_permits(), MAX_PENDING_CONTROLS);
    }

    #[tokio::test]
    async fn pending_uploads_have_bounded_admission_and_release_capacity() {
        let (broker, headers, _input, closed) = active_session();
        let app = router(broker.clone());
        let mut uploads = Vec::new();
        // The bound applies to uploads across the broker, not to long-lived GETs.
        for _ in 0..128 {
            let polls = Arc::new(AtomicUsize::new(0));
            let mut upload = Box::pin(
                app.clone()
                    .oneshot(request(headers.clone(), pending_body(polls.clone()))),
            );
            assert!(futures_util::poll!(&mut upload).is_pending());
            assert!(polls.load(Ordering::SeqCst) > 0);
            uploads.push(upload);
        }
        let rejected_polls = Arc::new(AtomicUsize::new(0));
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            app.clone().oneshot(request(
                headers.clone(),
                pending_body(rejected_polls.clone()),
            )),
        )
        .await
        .expect("over-capacity upload must be rejected without buffering")
        .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected_polls.load(Ordering::SeqCst), 0);
        let idle_get = app
            .clone()
            .oneshot(Request::builder().uri("/sse").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(idle_get.status(), StatusCode::OK);
        drop(idle_get);
        // Cancel a client POST: its admission permit must be immediately reusable.
        drop(uploads.pop());
        assert_eq!(
            app.clone()
                .oneshot(request(headers.clone(), Body::empty()))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT,
        );
        closed.cancel();
        for upload in uploads {
            let response = tokio::time::timeout(Duration::from_millis(100), upload)
                .await
                .expect("closing GET must release every pending upload")
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        assert_eq!(broker.sse.1.available_permits(), MAX_PENDING_CONTROLS);
        println!(
            "upload_semaphore_bytes={}; session_bytes={}",
            std::mem::size_of::<Semaphore>(),
            std::mem::size_of::<Session>()
        );
    }

    #[tokio::test]
    async fn body_limit_rejects_a_chunked_overflow_without_waiting_for_eof() {
        use futures_util::StreamExt;
        let (broker, headers, mut input, closed) = active_session();
        // The handler enforces its cap even if a caller omits the router's limit layer.
        let app = Router::new()
            .route("/sse", get(open).post(send))
            .with_state(broker.clone());
        let maximum_frame = p::encode(p::ACK, &vec![b'x'; p::MAX_CONTROL]).unwrap();
        let response = app
            .clone()
            .oneshot(request(headers.clone(), Body::from(maximum_frame.clone())))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(matches!(input.recv().await, Some(Message::Binary(_))));
        let body = Body::from_stream(
            stream::iter([
                Ok::<_, Infallible>(Bytes::from(maximum_frame)),
                Ok(Bytes::from_static(b"x")),
            ])
            .chain(stream::pending()),
        );
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            app.clone().oneshot(request(headers.clone(), body)),
        )
        .await
        .expect("oversized body must not wait for EOF")
        .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!closed.is_cancelled());
        assert_eq!(broker.sse.1.available_permits(), MAX_PENDING_CONTROLS);
        assert_eq!(
            app.oneshot(request(headers, Body::empty()))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn body_read_error_preserves_bad_request_and_releases_capacity() {
        let (broker, headers, _input, closed) = active_session();
        let body = Body::from_stream(stream::once(async {
            Err::<Bytes, _>(std::io::Error::other("synthetic body read failure"))
        }));
        let response = router(broker.clone())
            .oneshot(request(headers, body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(!closed.is_cancelled());
        assert_eq!(broker.sse.1.available_permits(), MAX_PENDING_CONTROLS);
    }

    async fn send_control(broker: Arc<Broker>, headers: HeaderMap, body: Bytes) -> StatusCode {
        send(State(broker), request(headers, Body::from(body))).await
    }

    #[tokio::test]
    async fn responses_own_sessions_and_reject_origin() {
        let broker = Broker::new();
        let response = open(State(broker.clone()), HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(broker.sse.0.lock().unwrap().len(), 1);
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, response.headers()[SESSION_HEADER].clone());
        assert_eq!(
            send_control(broker.clone(), headers.clone(), Bytes::new()).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            send_control(broker.clone(), headers.clone(), Bytes::new()).await,
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(response);
        assert!(broker.sse.0.lock().unwrap().is_empty());
        assert_eq!(
            send_control(broker.clone(), headers, Bytes::new()).await,
            StatusCode::NOT_FOUND
        );
        let mut origin = HeaderMap::new();
        origin.insert("origin", "https://example.test".parse().unwrap());
        assert_eq!(
            open(State(broker), origin).await.status(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn malformed_control_frame_cancels_session() {
        let broker = Broker::new();
        let response = open(State(broker.clone()), HeaderMap::new()).await;
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, response.headers()[SESSION_HEADER].clone());
        assert_eq!(
            send_control(broker.clone(), headers.clone(), Bytes::from_static(b"bad")).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send_control(broker, headers, Bytes::new()).await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn idle_response_emits_comments_without_authenticating_and_cleans_up() {
        use futures_util::StreamExt;
        let broker = Broker::new();
        let response = open(State(broker.clone()), HeaderMap::new()).await;
        let mut headers = HeaderMap::new();
        headers.insert(SESSION_HEADER, response.headers()[SESSION_HEADER].clone());
        let mut body = response.into_body().into_data_stream();
        let comment = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
            .await
            .expect("idle GET must send keepalive bytes")
            .unwrap()
            .unwrap();
        assert!(comment.starts_with(b":"));
        assert!(!comment.windows(5).any(|part| part == b"data:"));
        // A keepalive never advances authentication: an ACK before HELLO still fails.
        let ack = p::json(p::ACK, &serde_json::json!({})).unwrap();
        assert_eq!(
            send_control(broker.clone(), headers.clone(), ack.into()).await,
            StatusCode::NO_CONTENT
        );
        let frame = tokio::time::timeout(std::time::Duration::from_secs(1), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let event = std::str::from_utf8(&frame).unwrap();
        let data = event
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .unwrap();
        assert_eq!(
            p::decode(&URL_SAFE_NO_PAD.decode(data).unwrap()).unwrap().0,
            p::FAILURE
        );
        drop(body);
        assert!(broker.sse.0.lock().unwrap().is_empty());
        assert_eq!(
            send_control(broker, headers, Bytes::new()).await,
            StatusCode::NOT_FOUND
        );
    }
}
