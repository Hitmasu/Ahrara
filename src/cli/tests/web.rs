use ahrara::{
    config::{Config, private_write},
    core::Core,
    web_state::{Settings, verify_password},
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use std::{net::SocketAddr, sync::Arc};
use tower::ServiceExt;
use zeroize::Zeroizing;

fn profile() -> (tempfile::TempDir, Arc<Core>) {
    profile_with_config(|_| {})
}

fn profile_with_config(configure: impl FnOnce(&mut Config)) -> (tempfile::TempDir, Arc<Core>) {
    let dir = tempfile::tempdir().unwrap();
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()]).unwrap();
    let mut cfg = Config {
        database_path: dir.path().join("inbox.db"),
        identity_path: dir.path().join("identity.key"),
        api_token_path: dir.path().join("api.token"),
        api_https: true,
        mcp_auth: true,
        api_tls_cert_path: dir.path().join("api.pem"),
        api_tls_key_path: dir.path().join("api-key.pem"),
        ..Config::default()
    };
    std::fs::write(&cfg.api_tls_cert_path, cert.cert.pem()).unwrap();
    private_write(
        &cfg.api_tls_key_path,
        cert.signing_key.serialize_pem().as_bytes(),
        false,
    )
    .unwrap();
    configure(&mut cfg);
    let (core, _) = Core::open(cfg).unwrap();
    (dir, core)
}

async fn request(
    app: Router,
    method: &str,
    path: &str,
    body: &str,
    cookie: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, "127.0.0.1:8787")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }

    let mut request = request.body(Body::from(body.to_owned())).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    ));
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 12 * 1024 * 1024)
        .await
        .unwrap();

    (status, headers, String::from_utf8_lossy(&bytes).into())
}

fn form(core: &Core, extra: &str) -> String {
    format!("csrf={}&{extra}", core.web.csrf)
}

#[tokio::test]
async fn custom_domain_form_defaults_creation_and_shows_forwarding_metadata() {
    let (_dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let target = core.create_address(None).await.unwrap();
    let app = ahrara::web::router(core.clone());
    let values = format!("action=set&domain=ab.com&forwarding_address={}", target.id);

    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/addresses/domain",
            &format!("csrf=invalid&{values}"),
            None
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/addresses/domain",
            &form(&core, &values),
            None
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/addresses",
            &form(&core, "label=Shopping"),
            None
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let latest = core
        .storage
        .latest_enabled_address(core.identity.fingerprint())
        .await
        .unwrap()
        .unwrap();

    assert!(latest.address.ends_with("@ab.com"));
    let (_, _, page) = request(app.clone(), "GET", "/addresses", "", None).await;

    assert!(
        page.contains(&format!("data-copy=\"{}\"", latest.address))
            && page.contains("Custom domain")
    );
    let raw = format!(
        "To: signup@ab.com\r\nX-Forwarded-To: {}\r\n\r\nHello\r\n",
        target.address
    )
    .into_bytes();
    let saved = core
        .storage
        .insert(
            "forwarded".into(),
            core.identity.fingerprint(),
            vec![target.address.clone()],
            "sender@example.test".into(),
            raw,
        )
        .await
        .unwrap();
    let (_, _, page) = request(
        app.clone(),
        "GET",
        &format!("/emails/{}", saved[0].id),
        "",
        None,
    )
    .await;

    assert!(
        page.contains("Forwarded to")
            && page.contains("signup@ab.com")
            && page.contains(&target.address)
    );
    assert_eq!(
        request(
            app,
            "POST",
            "/addresses/domain",
            &form(&core, "action=remove"),
            None
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );

    assert!(
        core.create_address(None)
            .await
            .unwrap()
            .address
            .ends_with("@ahrara.dev")
    );
}

#[tokio::test]
async fn header_uses_latest_enabled_address_beyond_first_page() {
    let (_dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let app = ahrara::web::router(core.clone());
    let mut addresses = vec![];

    for _ in 0..21 {
        addresses.push(core.create_address(None).await.unwrap());
    }

    let (_other_dir, other) = profile();
    core.storage
        .create_address(other.identity.fingerprint(), other.identity.route(), None)
        .await
        .unwrap();

    for expected in [20, 19] {
        for route in ["/", "/addresses", "/settings"] {
            let (status, _, body) = request(app.clone(), "GET", route, "", None).await;
            assert_eq!(status, StatusCode::OK);
            let header = body
                .split("<header")
                .nth(1)
                .unwrap()
                .split("</header>")
                .next()
                .unwrap();

            assert!(
                header.contains(&format!("data-copy=\"{}\"", addresses[expected].address)),
                "header must use the latest enabled address"
            );
        }

        core.change_address(addresses[expected].id.to_string(), Some(false))
            .await
            .unwrap();
    }

    for address in &addresses[..19] {
        core.change_address(address.id.to_string(), None)
            .await
            .unwrap();
    }

    let (_, _, body) = request(app, "GET", "/", "", None).await;

    assert!(body.contains("Create address</a>"));
}

#[tokio::test]
async fn web_defaults_assets_templates_and_forms() {
    let (_dir, core) = profile();
    assert!(!core.web.settings().lan_enabled);
    let app = ahrara::web::router(core.clone());

    assert_eq!(
        request(app.clone(), "GET", "/", "", None).await.0,
        StatusCode::OK
    );
    let http2 = Request::builder()
        .version(axum::http::Version::HTTP_2)
        .uri("https://127.0.0.1:8787/")
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        app.clone().oneshot(http2).await.unwrap().status(),
        StatusCode::OK,
        "HTTP/2 uses :authority instead of Host"
    );

    for route in ["/", "/addresses", "/settings", "/login"] {
        let (status, headers, body) = request(app.clone(), "GET", route, "", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Ahrara"));
        assert!(headers.contains_key(header::CONTENT_SECURITY_POLICY));
        assert_eq!(headers[header::REFERRER_POLICY], "same-origin");
        assert!(!body.contains("ahrara_sk_v1_"));
        assert!(!body.contains("password_hash"));
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }

    for (path, mime, max) in [
        ("/assets/app.css", "text/css", 30 * 1024),
        ("/assets/app.js", "text/javascript", 15 * 1024),
        ("/assets/logo.png", "image/png", 1024 * 1024),
    ] {
        let (status, headers, body) = request(app.clone(), "GET", path, "", None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with(mime)
        );

        assert!(body.len() < max * 3);
    }

    assert_eq!(
        request(app.clone(), "POST", "/addresses", "csrf=wrong", None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/addresses",
            &form(&core, "label=Personal"),
            None
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    let a = core
        .storage
        .addresses(core.identity.fingerprint(), 0, 20, false)
        .await
        .unwrap()
        .remove(0);

    for action in ["disable", "enable"] {
        assert_eq!(
            request(
                app.clone(),
                "POST",
                &format!("/addresses/{}", a.id),
                &form(&core, &format!("action={action}")),
                None
            )
            .await
            .0,
            StatusCode::SEE_OTHER
        );

        assert_eq!(
            core.storage
                .get_address(core.identity.fingerprint(), a.id.to_string())
                .await
                .unwrap()
                .enabled,
            action == "enable"
        );
    }

    let (status, _, body) = request(app.clone(), "GET", "/addresses", "", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(&a.address));
    let mut bad_host = Request::builder()
        .uri("/")
        .header(header::HOST, "unrelated.example:8787")
        .body(Body::empty())
        .unwrap();
    bad_host
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));

    assert_eq!(
        app.clone().oneshot(bad_host).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let cross = Request::builder()
        .method("POST")
        .uri("/addresses")
        .header(header::HOST, "127.0.0.1:8787")
        .header(header::ORIGIN, "https://unrelated.example")
        .body(Body::from(form(&core, "label=Rejected")))
        .unwrap();

    assert_eq!(
        app.clone().oneshot(cross).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    core.update_interfaces("web", false, false, None)
        .await
        .unwrap();

    assert_eq!(
        request(app, "GET", "/assets/app.css", "", None).await.0,
        StatusCode::NOT_FOUND
    );
    let restored = Core::open(core.config.clone()).unwrap().0;

    assert_eq!(
        request(ahrara::web::router(restored), "GET", "/", "", None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn web_password_sessions_lan_and_persisted_acknowledgement() {
    let (dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();

    assert!(
        core.update_interfaces("lan", true, false, None)
            .await
            .is_err()
    );
    assert!(!core.web.settings().lan_enabled);
    core.update_interfaces("lan", true, true, None)
        .await
        .unwrap();
    let restored = Core::open(core.config.clone()).unwrap().0;

    assert!(restored.web.settings().lan_no_auth_acknowledged);
    core.update_interfaces("lan", false, false, None)
        .await
        .unwrap();
    core.update_interfaces("lan", true, false, None)
        .await
        .unwrap();
    let password = ahrara::web_state::random_secret();
    core.update_interfaces(
        "password",
        true,
        false,
        Some(Zeroizing::new(password.clone())),
    )
    .await
    .unwrap();
    let hash = core.web.settings().password_hash.unwrap();

    assert!(hash.starts_with("$argon2id$"));
    assert!(verify_password(&hash, &password));
    let saved = std::fs::read_to_string(dir.path().join("interfaces.toml")).unwrap();

    assert!(!saved.contains(&password));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("interfaces.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let app = ahrara::web::router(core.clone());

    for path in [
        "/",
        "/addresses",
        "/settings",
        "/emails/1",
        "/emails/1/html",
        "/events",
    ] {
        assert_eq!(
            request(app.clone(), "GET", path, "", None).await.0,
            StatusCode::SEE_OTHER
        );
    }

    let login_page = request(app.clone(), "GET", "/login", "", None).await.2;

    assert!(!login_page.contains(&core.identity.fingerprint()));
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/login",
            &form(&core, "password=incorrect"),
            None
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, headers, _) = request(
        app.clone(),
        "POST",
        "/login",
        &form(&core, &format!("password={password}")),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers[header::SET_COOKIE].to_str().unwrap().to_owned();

    assert!(cookie.contains("Secure; HttpOnly; SameSite=Strict"));
    assert_eq!(
        request(app.clone(), "GET", "/settings", "", Some(&cookie))
            .await
            .0,
        StatusCode::OK
    );
    let new = ahrara::web_state::random_secret();
    core.update_interfaces("password", true, false, Some(Zeroizing::new(new)))
        .await
        .unwrap();

    assert_eq!(
        request(app.clone(), "GET", "/settings", "", Some(&cookie))
            .await
            .0,
        StatusCode::SEE_OTHER
    );
    assert!(
        core.update_interfaces("remove_password", false, false, None)
            .await
            .is_err()
    );
    core.update_interfaces("remove_password", false, true, None)
        .await
        .unwrap();

    assert!(core.web.settings().password_hash.is_none());
    assert!(core.web.settings().lan_no_auth_acknowledged);
    core.update_interfaces("lan", false, false, None)
        .await
        .unwrap();
    core.update_interfaces("mcp", false, false, None)
        .await
        .unwrap();

    assert!(!core.web.settings().mcp_enabled);
    let mut remote = Request::builder()
        .uri("/")
        .header(header::HOST, "192.168.1.10:8787")
        .body(Body::empty())
        .unwrap();
    remote.extensions_mut().insert(ConnectInfo(
        "192.168.1.12:32100".parse::<SocketAddr>().unwrap(),
    ));

    assert_eq!(
        app.oneshot(remote).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let settings: Settings =
        toml::from_str(&std::fs::read_to_string(dir.path().join("interfaces.toml")).unwrap())
            .unwrap();

    assert!(!settings.mcp_enabled);
    let ip = "192.168.1.99".parse().unwrap();

    for _ in 0..4 {
        assert!(core.web.allow_login(ip));
    }

    assert!(!core.web.allow_login(ip));
}

#[tokio::test]
async fn web_metadata_pagination_email_html_attachment_and_delete() {
    let (_dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let a = core
        .create_address(Some("<Personal>".into()))
        .await
        .unwrap();
    let raw = include_bytes!("fixtures/message.eml").to_vec();

    for id in 0..23 {
        let saved = core
            .storage
            .insert(
                format!("web-{id}"),
                core.identity.fingerprint(),
                vec![a.address.clone()],
                "sender@example.test".into(),
                raw.clone(),
            )
            .await
            .unwrap();

        core.received(saved);
    }

    let app = ahrara::web::router(core.clone());
    let (_, _, inbox) = request(app.clone(), "GET", "/", "", None).await;

    assert_eq!(inbox.matches("class=\"subject-cell\"").count(), 20);
    assert!(!inbox.contains("Content-Type: multipart"));
    assert!(inbox.contains("before=4"));
    let (_, _, older) = request(app.clone(), "GET", "/?before=4", "", None).await;

    assert_eq!(older.matches("class=\"subject-cell\"").count(), 3);
    let (_, _, filtered) = request(app.clone(), "GET", "/?q=unmatched-search", "", None).await;

    assert!(filtered.contains("No matching emails"));
    let (status, _, body) = request(app.clone(), "GET", "/emails/23", "", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("sandbox"));
    assert!(body.contains("Attachments"));
    assert!(body.contains("/emails/23/raw"));
    let (status, headers, html) = request(app.clone(), "GET", "/emails/23/html", "", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );
    assert!(!html.contains(" src=\"https://"));
    let (status, headers, attachment) =
        request(app.clone(), "GET", "/emails/23/attachments/1", "", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
    assert!(!attachment.is_empty());
    assert_ne!(
        request(
            app.clone(),
            "GET",
            "/emails/23/attachments/..%2F..%2Ffile",
            "",
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            app.clone(),
            "POST",
            "/emails/23/delete",
            &form(&core, "confirm=true"),
            None
        )
        .await
        .0,
        StatusCode::SEE_OTHER
    );
    assert!(core.storage.email(23).await.is_err());
    request(
        app.clone(),
        "POST",
        &format!("/addresses/{}", a.id),
        &form(&core, "action=delete&confirm=true"),
        None,
    )
    .await;

    assert!(core.storage.email(22).await.is_ok());
    let mut events = core.events.subscribe();
    core.delete_emails(None).await.unwrap();

    assert_eq!(events.recv().await.unwrap().kind, "emails_cleared");
}

#[tokio::test]
async fn web_sse_uses_core_events_and_revokes_anonymous_stream_after_password_setup() {
    use futures_util::StreamExt;
    let (_dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let response = ahrara::web::router(core.clone())
        .oneshot(
            Request::builder()
                .uri("/events")
                .header(header::HOST, "127.0.0.1:8787")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut stream = response.into_body().into_data_stream();

    assert!(String::from_utf8_lossy(&stream.next().await.unwrap().unwrap()).contains("keepalive"));
    core.create_address(Some("SSE".into())).await.unwrap();

    assert!(
        String::from_utf8_lossy(&stream.next().await.unwrap().unwrap())
            .contains("event: address_created")
    );
    core.update_interfaces(
        "password",
        true,
        false,
        Some(Zeroizing::new(ahrara::web_state::random_secret())),
    )
    .await
    .unwrap();

    assert!(
        String::from_utf8_lossy(&stream.next().await.unwrap().unwrap())
            .contains("event: session_expired")
    );

    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn http_shutdown_closes_web_api_and_mcp_streams_promptly() {
    use std::time::Duration;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let (_dir, core) = profile_with_config(|cfg| cfg.api_bind = address.to_string());
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(&core.config.api_tls_cert_path).unwrap())
                .unwrap(),
        )
        .build()
        .unwrap();
    let mut server = ahrara::api::HttpServer::new(core.clone(), true)
        .await
        .unwrap();
    let token = Zeroizing::new(std::fs::read_to_string(&core.config.api_token_path).unwrap());

    // Restarting HTTPS must create a new lifetime; old cancellation cannot affect it.
    for _ in 0..2 {
        server.sync().await.unwrap();
        let mut streams = Vec::new();

        for route in ["/events", "/v1/events"] {
            let mut response = client
                .get(format!("https://{address}{route}"))
                .bearer_auth(token.as_str())
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(
                String::from_utf8_lossy(&response.chunk().await.unwrap().unwrap())
                    .contains("keepalive")
            );

            streams.push(response);
        }

        let initialized = client
            .post(format!("https://{address}/mcp"))
            .bearer_auth(token.as_str())
            .header(header::ACCEPT, "application/json, text/event-stream")
            .json(&mcp_initialize())
            .send()
            .await
            .unwrap();
        let session = initialized.headers()["mcp-session-id"].clone();
        initialized.bytes().await.unwrap();
        client
            .post(format!("https://{address}/mcp"))
            .bearer_auth(token.as_str())
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-session-id", session.clone())
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            }))
            .send()
            .await
            .unwrap();
        let mcp_stream = client
            .get(format!("https://{address}/mcp"))
            .bearer_auth(token.as_str())
            .header(header::ACCEPT, "text/event-stream")
            .header("mcp-session-id", session)
            .send()
            .await
            .unwrap();

        assert_eq!(mcp_stream.status(), StatusCode::OK);
        streams.push(mcp_stream);
        tokio::time::timeout(Duration::from_millis(500), server.stop())
            .await
            .expect("idle SSE must close without exhausting the one-second shutdown deadline");

        for mut response in streams {
            // Buffered heartbeat or MCP events may precede EOF. HTTP chunks
            // can also split an SSE event at any byte boundary.
            tokio::time::timeout(Duration::from_millis(500), async {
                let mut remaining = 0;
                while let Some(chunk) = response.chunk().await.unwrap() {
                    remaining += chunk.len();
                    assert!(remaining <= 64 * 1024, "unexpected output after shutdown");
                }
            })
            .await
            .expect("all SSE streams must reach EOF promptly after shutdown");
        }
    }
}

fn mcp_request(token: &str, payload: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, "127.0.0.1:8787")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json")
        .header("mcp-protocol-version", "2025-03-26")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

fn mcp_initialize() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1"}
        }
    })
}

#[tokio::test]
async fn mcp_defaults_to_http_without_certificates_or_token() {
    let dir = tempfile::tempdir().unwrap();
    let address = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let cfg = Config {
        database_path: dir.path().join("inbox.db"),
        identity_path: dir.path().join("identity.key"),
        api_token_path: dir.path().join("api.token"),
        api_tls_cert_path: dir.path().join("absent.pem"),
        api_tls_key_path: dir.path().join("absent-key.pem"),
        api_bind: address.to_string(),
        ..Config::default()
    };
    let (core, _) = Core::open(cfg).unwrap();
    core.update_interfaces("mcp", true, false, None)
        .await
        .expect("enabling local MCP must not require a certificate");
    let mut server = ahrara::api::HttpServer::new(core.clone(), false)
        .await
        .unwrap();
    let base = format!("http://{address}");

    assert_eq!(core.web_url(), base);
    assert!(!core.config.api_token_path.exists());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let initialize = || {
        client
            .post(format!("{base}/mcp"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .json(&mcp_initialize())
    };

    for (name, value) in [
        ("origin", "https://unrelated.example".to_owned()),
        ("origin", "null".to_owned()),
        ("origin", format!("https://{address}")),
        ("host", format!("unrelated.example:{}", address.port())),
        ("sec-fetch-site", "cross-site".to_owned()),
    ] {
        assert_eq!(
            initialize()
                .header(name, value)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
    }

    let response = initialize()
        .header(header::ORIGIN, &base)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"].clone();

    assert!(response.text().await.unwrap().contains("protocolVersion"));
    let request = |payload| {
        client
            .post(format!("{base}/mcp"))
            .header(header::ACCEPT, "application/json, text/event-stream")
            .header("mcp-session-id", session.clone())
            .json(&payload)
    };

    assert_eq!(
        request(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let response = request(serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "ahrara_create_address", "arguments": {}}
    }))
    .send()
    .await
    .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("@ahrara.dev"));
    assert!(!core.config.api_token_path.exists());

    // Enabling REST on an already-running anonymous MCP server still requires a token.
    core.update_interfaces("api", true, false, None)
        .await
        .unwrap();
    server.sync().await.unwrap();
    let response = client
        .get(format!("{base}/v1/addresses"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let token = Zeroizing::new(std::fs::read_to_string(&core.config.api_token_path).unwrap());

    assert_eq!(
        client
            .get(format!("{base}/v1/addresses"))
            .bearer_auth(token.as_str())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    server.stop().await;
}

#[tokio::test]
async fn mcp_https_and_auth_are_independent_options() {
    for (https, auth) in [(false, true), (true, false), (true, true)] {
        let (_dir, core) = profile_with_config(|cfg| {
            cfg.api_https = https;
            cfg.mcp_auth = auth;
            cfg.api_bind = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .to_string();
        });
        let cfg = &core.config;
        let client = reqwest::Client::builder()
            .add_root_certificate(
                reqwest::Certificate::from_pem(&std::fs::read(&cfg.api_tls_cert_path).unwrap())
                    .unwrap(),
            )
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mut server = ahrara::api::HttpServer::new(core.clone(), false)
            .await
            .unwrap();
        let base = core.web_url();
        let initialize = || {
            client
                .post(format!("{base}/mcp"))
                .header(header::ORIGIN, &base)
                .header(header::ACCEPT, "application/json, text/event-stream")
                .json(&mcp_initialize())
        };
        let response = initialize().send().await.unwrap();
        if auth {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(
                initialize()
                    .bearer_auth("invalid")
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            let token =
                Zeroizing::new(std::fs::read_to_string(&core.config.api_token_path).unwrap());
            let response = initialize()
                .bearer_auth(token.as_str())
                .send()
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.text().await.unwrap().contains("protocolVersion"));
        } else {
            assert_eq!(response.status(), StatusCode::OK);
            assert!(response.text().await.unwrap().contains("protocolVersion"));
            assert!(!core.config.api_token_path.exists());
        }

        // The shared Web server must keep optional password sessions working in either mode.
        core.update_interfaces(
            "password",
            true,
            false,
            Some(Zeroizing::new("test-password".into())),
        )
        .await
        .unwrap();
        let response = client
            .post(format!("{base}/login"))
            .header(header::ORIGIN, &base)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(&core, "password=test-password"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();

        assert_eq!(cookie.contains("Secure;"), https);
        assert_eq!(cookie.starts_with("__Host-"), https);
        assert!(cookie.contains("HttpOnly; SameSite=Strict"));
        assert_eq!(
            client
                .get(format!("{base}/settings"))
                .header(header::COOKIE, cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let response = client
            .post(format!("{base}/logout"))
            .header(header::ORIGIN, &base)
            .header(header::COOKIE, cookie)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form(&core, ""))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert!(
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert_eq!(
            client
                .get(format!("{base}/settings"))
                .header(header::COOKIE, cookie)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SEE_OTHER
        );

        server.stop().await;
    }
}

#[tokio::test]
async fn mcp_lan_still_requires_tls_and_authentication() {
    let (_dir, core) = profile_with_config(|cfg| {
        cfg.api_https = false;
        cfg.mcp_auth = false;
    });
    let shutdown = tokio_util::sync::CancellationToken::new();
    let _cleanup = shutdown.clone().drop_guard();
    let app = ahrara::mcp::router(core.clone(), shutdown);
    let mut req = mcp_request("", mcp_initialize());
    req.headers_mut().remove(header::AUTHORIZATION);
    req.extensions_mut().insert(ConnectInfo(
        "192.168.1.10:4567".parse::<SocketAddr>().unwrap(),
    ));

    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let (status, headers, _) = request(
        ahrara::web::router(core.clone()),
        "POST",
        "/settings",
        &form(&core, "action=lan&enabled=true&acknowledge=true"),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        format!("{}/settings", core.web_url())
    );
    let mut req = mcp_request("", mcp_initialize());
    req.headers_mut().remove(header::AUTHORIZATION);

    assert_eq!(
        app.oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(core.web_url().starts_with("https://"));
    let (status, headers, _) = request(
        ahrara::web::router(core.clone()),
        "POST",
        "/settings",
        &form(&core, "action=lan&enabled=false"),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        headers[header::LOCATION],
        format!("{}/settings", core.web_url())
    );
    std::fs::remove_file(&core.config.api_tls_cert_path).unwrap();

    assert!(
        core.update_interfaces("lan", true, true, None)
            .await
            .is_err()
    );

    assert!(!core.web.settings().lan_enabled);
}

#[tokio::test]
async fn mcp_http_sessions_auth_lan_and_revocation() {
    let (_dir, core) = profile();
    ahrara::auth::generate_token(&core.config, false).unwrap();
    let token = std::fs::read_to_string(&core.config.api_token_path).unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let _cleanup = shutdown.clone().drop_guard();
    let app = ahrara::mcp::router(core.clone(), shutdown.clone());

    for invalid in ["", "invalid"] {
        assert_eq!(
            app.clone()
                .oneshot(mcp_request(invalid, mcp_initialize()))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    for (name, value) in [
        ("origin", "https://unrelated.example"),
        ("host", "unrelated.example:8787"),
        ("origin", "null"),
    ] {
        let mut req = mcp_request(&token, mcp_initialize());
        req.headers_mut().insert(name, value.parse().unwrap());

        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }

    let mut lan = mcp_request(&token, mcp_initialize());
    lan.extensions_mut().insert(ConnectInfo(
        "192.168.1.10:4567".parse::<SocketAddr>().unwrap(),
    ));

    assert_eq!(
        app.clone().oneshot(lan).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    core.update_interfaces("lan", true, true, None)
        .await
        .unwrap();
    let mut lan = mcp_request(&token, mcp_initialize());
    lan.extensions_mut().insert(ConnectInfo(
        "192.168.1.10:4567".parse::<SocketAddr>().unwrap(),
    ));
    let response = app.clone().oneshot(lan).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let session = response.headers()["mcp-session-id"].clone();
    let initialized = to_bytes(response.into_body(), 64 * 1024).await.unwrap();

    assert!(String::from_utf8_lossy(&initialized).contains("\"protocolVersion\":\"2025-03-26\""));
    let mut notification = mcp_request(
        &token,
        serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    notification
        .headers_mut()
        .insert("mcp-session-id", session.clone());

    assert_eq!(
        app.clone().oneshot(notification).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    let mut list = mcp_request(
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    list.headers_mut().insert("mcp-session-id", session.clone());
    let response = app.clone().oneshot(list).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let body = String::from_utf8_lossy(&body);

    assert!(
        body.contains("ahrara_wait_for_email")
            && !body.contains(&token)
            && !body.contains("ahrara_sk_v1_")
    );
    let mut stream = Request::builder()
        .uri("/mcp")
        .header(header::HOST, "127.0.0.1:8787")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::ACCEPT, "text/event-stream")
        .header("mcp-session-id", session.clone())
        .body(Body::empty())
        .unwrap();
    // A session ID never substitutes for bearer authentication.
    stream.headers_mut().remove(header::AUTHORIZATION);

    assert_eq!(
        app.clone().oneshot(stream).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let mut req = mcp_request(
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    );
    req.headers_mut().insert("mcp-session-id", session.clone());
    let response = app.clone().oneshot(req).await.unwrap();
    ahrara::auth::generate_token(&core.config, true).unwrap();

    assert!(
        to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap()
            .is_empty(),
        "rotation suppresses buffered MCP data"
    );
    assert_eq!(
        app.clone()
            .oneshot(mcp_request(&token, mcp_initialize()))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let token = std::fs::read_to_string(&core.config.api_token_path).unwrap();
    let mut delete = mcp_request(&token, serde_json::json!({}));
    *delete.method_mut() = axum::http::Method::DELETE;
    delete
        .headers_mut()
        .insert("mcp-session-id", session.clone());

    assert!(
        app.clone()
            .oneshot(delete)
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let mut unknown = mcp_request(&token, mcp_initialize());
    unknown.headers_mut().insert("mcp-session-id", session);

    assert_eq!(
        app.clone().oneshot(unknown).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let mut large = mcp_request(&token, mcp_initialize());
    *large.body_mut() = Body::from(vec![b' '; 65537]);

    assert_eq!(
        app.clone().oneshot(large).await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let mut unsupported = mcp_request(&token, mcp_initialize());
    unsupported
        .headers_mut()
        .insert("mcp-protocol-version", "unsupported".parse().unwrap());

    assert_eq!(
        app.clone().oneshot(unsupported).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
    core.update_interfaces("mcp", false, false, None)
        .await
        .unwrap();

    assert_eq!(
        app.oneshot(mcp_request(&token, mcp_initialize()))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn html_remote_resources_require_per_view_consent() {
    let (_dir, core) = profile();
    core.update_interfaces("web", true, false, None)
        .await
        .unwrap();
    let address = core.create_address(None).await.unwrap();
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jZ2kAAAAASUVORK5CYII=";
    let raw = format!("From: sender@example.test\r\nSubject: Private images\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=parts\r\n\r\n--parts\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello</p><img src=\"https://example.test/image.png\" alt=\"Company logo\"><img src=\"https://example.test/pixel.png\" width=\"1\" height=\"1\" alt=\"tracking-pixel\"><img src=\"cid:image001\" alt=\"Local image\"><img srcset=\"https://example.test/one.png 1x, https://example.test/two.png 2x\"><picture><source srcset=\"https://example.test/source.png 1x\"><img alt=\"Fallback\"></picture><div style=\"height:40px;background-image:url('https://example.test/background.png')\">Background</div><link rel=\"stylesheet\" href=\"https://example.test/remote.css\"><script>window.executed=true</script><form action=\"https://example.test/form\"><input name=\"secret\"></form><img src=\"javascript:alert(1)\" onerror=\"alert(2)\"><img src=\"data:image/svg+xml,blocked\"><a href=\"https://example.test/navigation\" target=\"_top\">Link</a>\r\n--parts\r\nContent-Type: image/png\r\nContent-ID: <image001>\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--parts\r\nContent-Type: image/png\r\nContent-Disposition: attachment; filename=photo.png\r\nContent-Transfer-Encoding: base64\r\n\r\n{png}\r\n--parts--\r\n").into_bytes();
    let saved = core
        .storage
        .insert(
            "images".into(),
            core.identity.fingerprint(),
            vec![address.address.clone()],
            "sender@example.test".into(),
            raw.clone(),
        )
        .await
        .unwrap();
    let id = saved[0].id;
    let app = ahrara::web::router(core.clone());
    let path = format!("/emails/{id}/html");
    let (_, _, detail) = request(app.clone(), "GET", &format!("/emails/{id}"), "", None).await;

    assert!(
        detail.contains("data-remote-notice")
            && detail.contains("Load images")
            && detail.contains("Keep blocked")
    );
    assert!(detail.contains("photo.png") && detail.contains("target=\"ahrara-email-html\""));
    let (_, headers, blocked) = request(app.clone(), "GET", &path, "", None).await;

    assert!(blocked.contains("Company logo") && blocked.contains("External image blocked"));
    assert!(blocked.contains("src=\"data:image/png;base64,"));
    assert!(!blocked.contains("tracking-pixel"));
    assert!(!blocked.contains(" src=\"https:") && !blocked.contains(" srcset="));
    assert!(!blocked.contains("background-image:url"));
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("img-src data:;")
    );
    assert_eq!(
        request(app.clone(), "POST", &path, "csrf=invalid", None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let (status, headers, allowed) =
        request(app.clone(), "POST", &path, &form(&core, ""), None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(allowed.contains(" src=\"https://example.test/image.png\""));
    assert!(
        allowed.contains(
            " srcset=\"https://example.test/one.png 1x, https://example.test/two.png 2x\""
        )
    );
    assert!(
        allowed.contains("background-image:url")
            && allowed.contains("https://example.test/background.png")
    );
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("img-src data: http: https:;")
    );
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");

    for body in [&blocked, &allowed] {
        for forbidden in [
            "<script",
            "window.executed",
            "<form",
            "<input",
            "<link",
            "remote.css",
            "javascript:",
            "onerror",
            "data:image/svg",
            "href=",
        ] {
            assert!(
                !body.contains(forbidden),
                "forbidden rendered content: {forbidden}"
            );
        }
    }

    let (_, _, reloaded) = request(app.clone(), "GET", &path, "", None).await;

    assert_eq!(blocked, reloaded, "consent must not persist");
    assert_eq!(core.storage.email(id).await.unwrap().1, raw);

    for (index, body) in ["Content-Type: text/plain\r\n\r\nJust text", "Content-Type: text/html\r\n\r\n<p>No images</p>", "Content-Type: multipart/related; boundary=local\r\n\r\n--local\r\nContent-Type: text/html\r\n\r\n<img src=\"cid:local\">\r\n--local\r\nContent-Type: image/png\r\nContent-ID: <local>\r\n\r\nlocal-bytes\r\n--local--\r\n"].iter().enumerate() {
        let saved = core.storage.insert(
            format!("no-remote-{index}"),
            core.identity.fingerprint(),
            vec![address.address.clone()],
            "sender@example.test".into(),
            body.as_bytes().to_vec()
        ).await.unwrap();
        let (_, _, page) = request(app.clone(), "GET", &format!("/emails/{}", saved[0].id), "", None).await;

        assert!(!page.contains("data-remote-notice"));
    }
}
